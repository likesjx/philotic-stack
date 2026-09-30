use ansible_mesh_core::mcp_upstream::{McpEgressPolicy, McpStdioAllowEntry, McpStdioAllowlist};
use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use std::path::{Path, PathBuf};
use tokio::process::Command;

use crate::start::socket_path;

#[derive(Subcommand, Debug)]
pub enum McpAction {
    /// List registered upstream MCP servers (client fabric) and their state.
    Upstreams,

    /// Show the current egress policy and stdio command allowlist.
    ShowPolicy,

    /// Allow an outbound HTTP host (exact or `*.suffix`) beyond the
    /// loopback + tailnet default. Operator ceremony.
    AllowHost {
        /// Hostname or `*.suffix` pattern (e.g. `api.example.com`, `*.corp.net`).
        host: String,
    },

    /// Allow a stdio upstream command. Fail-closed by default: nothing runs
    /// until an operator adds it here. Operator ceremony.
    AllowCommand {
        /// Exact command string the transport must use (e.g. `muninn`,
        /// `/usr/bin/python3`). Compared verbatim — no PATH/basename matching.
        command: String,
        /// Required args prefix. For an interpreter, pin the script:
        /// `--args-prefix scripts/muninn_mcp.py`. Space-separated.
        #[arg(long, num_args = 0.., value_delimiter = ' ')]
        args_prefix: Vec<String>,
    },

    /// Run MCP client UAT through scripts/mcp-client-uat.sh.
    ///
    /// Defaults to strict live mode, which requires Perplexity and LifeGraph
    /// bearer tokens through env vars or token files.
    Uat {
        /// UAT mode passed to scripts/mcp-client-uat.sh.
        #[arg(default_value = "live")]
        mode: String,

        /// File containing the Perplexity/frontdoor bearer token.
        #[arg(long)]
        perplexity_token_file: Option<PathBuf>,

        /// File containing the LifeGraph readonly bearer token.
        #[arg(long)]
        lifegraph_token_file: Option<PathBuf>,

        /// Override the Perplexity/frontdoor MCP URL.
        #[arg(long)]
        perplexity_url: Option<String>,

        /// Override the LifeGraph MCP URL.
        #[arg(long)]
        lifegraph_url: Option<String>,

        /// Include remote native Muninn private smoke where the script supports it.
        #[arg(long)]
        run_remote: bool,

        /// Override the UAT script path.
        #[arg(long)]
        script: Option<PathBuf>,
    },
}

pub async fn run(action: McpAction) -> Result<()> {
    match action {
        McpAction::Upstreams => return upstreams().await,
        McpAction::ShowPolicy => return show_policy().await,
        McpAction::AllowHost { host } => return allow_host(host).await,
        McpAction::AllowCommand {
            command,
            args_prefix,
        } => return allow_command(command, args_prefix).await,
        McpAction::Uat {
            mode,
            perplexity_token_file,
            lifegraph_token_file,
            perplexity_url,
            lifegraph_url,
            run_remote,
            script,
        } => {
            run_uat(
                mode,
                perplexity_token_file,
                lifegraph_token_file,
                perplexity_url,
                lifegraph_url,
                run_remote,
                script,
            )
            .await
        }
    }
}

async fn run_uat(
    mode: String,
    perplexity_token_file: Option<PathBuf>,
    lifegraph_token_file: Option<PathBuf>,
    perplexity_url: Option<String>,
    lifegraph_url: Option<String>,
    run_remote: bool,
    script: Option<PathBuf>,
) -> Result<()> {
    let script = match script {
        Some(path) => path,
        None => find_uat_script()?,
    };
    ensure_readable(&script).with_context(|| format!("UAT script {}", script.display()))?;

    let mut cmd = Command::new(&script);
    cmd.arg(&mode);

    if let Some(path) = perplexity_token_file {
        ensure_readable(&path)
            .with_context(|| format!("Perplexity token file {}", path.display()))?;
        cmd.env("PERPLEXITY_MCP_TOKEN_FILE", path);
    }
    if let Some(path) = lifegraph_token_file {
        ensure_readable(&path)
            .with_context(|| format!("LifeGraph token file {}", path.display()))?;
        cmd.env("LIFEGRAPH_MCP_TOKEN_FILE", path);
    }
    if let Some(url) = perplexity_url {
        cmd.env("PERPLEXITY_MCP_URL", url);
    }
    if let Some(url) = lifegraph_url {
        cmd.env("LIFEGRAPH_MCP_URL", url);
    }
    if run_remote {
        cmd.env("RUN_REMOTE", "1");
    }

    println!(
        "Running MCP client UAT mode '{mode}' via {}",
        script.display()
    );
    println!("Token values are passed by environment or token files and are not printed.");

    let status = cmd.status().await.context("run MCP client UAT script")?;
    if !status.success() {
        anyhow::bail!("MCP client UAT failed with status {status}");
    }

    Ok(())
}

fn find_uat_script() -> Result<PathBuf> {
    let current = std::env::current_dir().context("resolve current directory")?;
    for dir in current.ancestors() {
        let candidate = dir.join("scripts").join("mcp-client-uat.sh");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    anyhow::bail!(
        "could not find scripts/mcp-client-uat.sh from {}; run inside philotic-stack or pass --script",
        current.display()
    )
}

fn ensure_readable(path: &Path) -> Result<()> {
    let meta =
        std::fs::metadata(path).with_context(|| format!("read metadata for {}", path.display()))?;
    if !meta.is_file() {
        anyhow::bail!("{} is not a file", path.display());
    }
    std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    Ok(())
}

// ── Client-fabric operator ceremony (upstreams + egress/stdio policy) ──────────

async fn ipc_client() -> Result<PhiloticClient> {
    let socket = socket_path("aiua");
    let identity = GuestIdentity {
        guest_id: "phil-mcp".into(),
        role: "management".into(),
        supported_tools: vec![],
    };
    PhiloticClient::connect_at(&socket, identity)
        .await
        .with_context(|| format!("connect to aiua at {socket}"))
}

async fn get_config<T: serde::de::DeserializeOwned + Default>(
    client: &mut PhiloticClient,
    key: &str,
) -> Result<T> {
    match client
        .send_request(IpcRequest::GetConfig { key: key.into() })
        .await?
    {
        IpcResponse::ConfigData { value_json, .. } => Ok(value_json
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()),
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected GetConfig response: {other:?}")),
    }
}

async fn set_config(
    client: &mut PhiloticClient,
    key: &str,
    value: &impl serde::Serialize,
) -> Result<()> {
    let value_json = serde_json::to_string(value)?;
    match client
        .send_request(IpcRequest::SetConfig {
            key: key.into(),
            value_json,
        })
        .await?
    {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected SetConfig response: {other:?}")),
    }
}

async fn upstreams() -> Result<()> {
    use ansible_mesh_core::mcp_upstream::McpUpstreamTransport;
    let mut client = ipc_client().await?;
    match client.send_request(IpcRequest::GetMcpUpstreams {}).await? {
        IpcResponse::McpUpstreamsState { mcp_upstreams } => {
            if mcp_upstreams.is_empty() {
                println!("no upstream MCP servers registered");
                return Ok(());
            }
            for entry in &mcp_upstreams {
                let cfg = &entry.config;
                let transport = match &cfg.transport {
                    McpUpstreamTransport::Http { url } => url.clone(),
                    McpUpstreamTransport::Stdio { command, args } => {
                        format!("stdio: {command} {}", args.join(" "))
                    }
                };
                let (state, projected, stale) = entry
                    .catalog
                    .as_ref()
                    .map(|c| {
                        (
                            format!("{:?}", c.state),
                            c.tools.len(),
                            c.stale_grants.len(),
                        )
                    })
                    .unwrap_or_else(|| ("Pending".into(), 0, 0));
                println!(
                    "{}  owner={}  [{}]  projected={} stale={}\n    {}",
                    cfg.upstream_id, cfg.owner_agent_id, state, projected, stale, transport
                );
            }
            Ok(())
        }
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected upstreams response: {other:?}")),
    }
}

async fn show_policy() -> Result<()> {
    let mut client = ipc_client().await?;
    let egress: McpEgressPolicy = get_config(&mut client, "mcp_egress_policy").await?;
    let stdio: McpStdioAllowlist = get_config(&mut client, "mcp_stdio_allowlist").await?;

    println!("egress policy (HTTP hosts): loopback + tailnet always allowed");
    if egress.allowed_hosts.is_empty() {
        println!("  (no additional hosts allowed)");
    } else {
        for h in &egress.allowed_hosts {
            println!("  + {h}");
        }
    }
    println!("stdio command allowlist (fail-closed):");
    if stdio.entries.is_empty() {
        println!("  (empty — no stdio upstreams may run)");
    } else {
        for e in &stdio.entries {
            println!("  + {} {}", e.command, e.args_prefix.join(" "));
        }
    }
    Ok(())
}

async fn allow_host(host: String) -> Result<()> {
    let host = host.trim().to_string();
    if host.is_empty() {
        anyhow::bail!("host cannot be empty");
    }
    let mut client = ipc_client().await?;
    let mut policy: McpEgressPolicy = get_config(&mut client, "mcp_egress_policy").await?;
    if policy.allowed_hosts.iter().any(|h| h == &host) {
        println!("host '{host}' already allowed");
        return Ok(());
    }
    policy.allowed_hosts.push(host.clone());
    set_config(&mut client, "mcp_egress_policy", &policy).await?;
    println!("allowed HTTP egress host '{host}'");
    Ok(())
}

async fn allow_command(command: String, args_prefix: Vec<String>) -> Result<()> {
    let command = command.trim().to_string();
    if command.is_empty() {
        anyhow::bail!("command cannot be empty");
    }
    let mut client = ipc_client().await?;
    let mut allowlist: McpStdioAllowlist = get_config(&mut client, "mcp_stdio_allowlist").await?;
    if allowlist
        .entries
        .iter()
        .any(|e| e.command == command && e.args_prefix == args_prefix)
    {
        println!(
            "stdio command already allowed: {command} {}",
            args_prefix.join(" ")
        );
        return Ok(());
    }
    allowlist.entries.push(McpStdioAllowEntry {
        command: command.clone(),
        args_prefix: args_prefix.clone(),
    });
    set_config(&mut client, "mcp_stdio_allowlist", &allowlist).await?;
    println!("allowed stdio command: {command} {}", args_prefix.join(" "));
    Ok(())
}
