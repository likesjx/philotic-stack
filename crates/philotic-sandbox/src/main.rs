use anyhow::{Context, Result};
use clap::Parser;
use philotic_sandbox::executor;
use philotic_sandbox::ipc::{read_frame, write_frame};
use philotic_sandbox::policy::SandboxPolicy;
use philotic_sandbox::ExecuteCommandRequest;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio::sync::Semaphore;
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(
    name = "philotic-sandbox-worker",
    about = "Sandbox worker for philotic-stack"
)]
struct Args {
    /// Path to the sandbox policy TOML file.
    #[arg(long)]
    policy: PathBuf,

    /// Path for the Unix domain socket.
    #[arg(long)]
    socket: PathBuf,

    /// Require OS-level enforcement (Landlock + seccomp). Fail if unavailable.
    #[arg(long, default_value_t = false)]
    strict: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    // 1. Load and validate policy
    let policy = SandboxPolicy::from_file(&args.policy)
        .with_context(|| format!("failed to load policy from {:?}", args.policy))?;
    info!(
        name = %policy.sandbox.name,
        version = %policy.sandbox.version,
        max_timeout_ms = policy.sandbox.max_timeout_ms,
        max_concurrent = policy.sandbox.max_concurrent,
        "loaded sandbox policy"
    );

    // 2. Create UDS listener
    // Remove stale socket file if it exists
    if args.socket.exists() {
        std::fs::remove_file(&args.socket)
            .with_context(|| format!("failed to remove stale socket {:?}", args.socket))?;
    }

    let listener = UnixListener::bind(&args.socket)
        .with_context(|| format!("failed to bind UDS at {:?}", args.socket))?;
    info!(socket = %args.socket.display(), "listening for connections");

    // 3. Apply OS-level restrictions (Linux only)
    apply_os_restrictions(&policy, args.strict)?;

    // 4. Enter event loop
    let policy = Arc::new(policy);
    let semaphore = Arc::new(Semaphore::new(policy.sandbox.max_concurrent));

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let policy = Arc::clone(&policy);
                let permit = match semaphore.clone().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => {
                        warn!("max concurrent executions reached, rejecting connection");
                        continue;
                    }
                };

                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(e) = handle_connection(stream, &policy).await {
                        error!("connection handler error: {}", e);
                    }
                });
            }
            Err(e) => {
                error!("accept error: {}", e);
            }
        }
    }
}

async fn handle_connection(stream: tokio::net::UnixStream, policy: &SandboxPolicy) -> Result<()> {
    let (mut reader, mut writer) = stream.into_split();

    // Read requests until the client disconnects
    loop {
        let request: Option<ExecuteCommandRequest> = read_frame(&mut reader).await?;

        let Some(request) = request else {
            // Client disconnected cleanly
            break;
        };

        let request_id = request.request_id.clone();
        info!(request_id = %request_id, command = %request.command, "received request");

        let response = executor::execute(policy, request).await;

        info!(
            request_id = %request_id,
            status = ?response.status,
            duration_ms = response.duration_ms,
            "sending response"
        );

        write_frame(&mut writer, &response).await?;
    }

    Ok(())
}

fn apply_os_restrictions(policy: &SandboxPolicy, strict: bool) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        // Apply Landlock filesystem restrictions
        match philotic_sandbox::landlock::apply_landlock(policy, strict) {
            Ok(true) => info!("landlock restrictions applied"),
            Ok(false) => {
                if strict {
                    anyhow::bail!("landlock not available but --strict was specified");
                }
                warn!("landlock not available, continuing with application-level enforcement");
            }
            Err(e) => {
                if strict {
                    return Err(e.context("failed to apply landlock restrictions"));
                }
                warn!(
                    "landlock failed: {}, continuing with application-level enforcement",
                    e
                );
            }
        }

        // Apply seccomp syscall filter
        match philotic_sandbox::seccomp::apply_seccomp(&policy.seccomp.profile, strict) {
            Ok(true) => info!("seccomp filter applied"),
            Ok(false) => {
                if strict {
                    anyhow::bail!("seccomp not available but --strict was specified");
                }
                warn!("seccomp not available, continuing with application-level enforcement");
            }
            Err(e) => {
                if strict {
                    return Err(e.context("failed to apply seccomp filter"));
                }
                warn!(
                    "seccomp failed: {}, continuing with application-level enforcement",
                    e
                );
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        if strict {
            anyhow::bail!(
                "OS-level sandbox enforcement (landlock, seccomp) is only available on Linux"
            );
        }
        warn!("not running on Linux — OS-level sandbox restrictions unavailable");
        warn!("application-level policy enforcement will still apply");
        let _ = policy; // suppress unused warning
    }

    Ok(())
}
