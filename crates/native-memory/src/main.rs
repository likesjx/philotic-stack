//! Explicitly configured native runner. No secret fetching or automatic startup.
use clap::Parser;
use native_memory::{MAX_WIRE, Policy, ROLE, RoutedRequest, Runner};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use std::{
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    socket: String,
    #[arg(long)]
    node: String,
    #[arg(long)]
    guest: String,
    #[arg(long)]
    sqlite_fixture: Option<PathBuf>,
    /// Local private Bolt config. No credential discovery; no schema auto-creation.
    #[arg(
        long,
        conflicts_with = "sqlite_fixture",
        required_unless_present = "sqlite_fixture"
    )]
    memgraph_config: Option<PathBuf>,
    /// Local public-key pins and grants; reread for every task. Contains no secrets.
    #[arg(long)]
    policy: PathBuf,
    /// Private local result-signing key; never routed or fetched from a vault.
    #[arg(long)]
    response_signing_config: PathBuf,
    /// Opt-in hotel OAuth enforcer. Every task then requires a live bound permit.
    #[arg(long, conflicts_with = "sqlite_fixture")]
    authority_socket: Option<PathBuf>,
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let mut fixture = args
        .sqlite_fixture
        .as_ref()
        .map(|path| Runner::open(path, args.node.clone(), args.guest.clone()))
        .transpose()?;
    let graph = if let Some(path) = &args.memgraph_config {
        Some(
            native_memory::memgraph::MemgraphConfig::load(path)?
                .connect(args.node.clone(), args.guest.clone())
                .await?,
        )
    } else {
        None
    };
    let mut ipc = PhiloticClient::connect_at(
        &args.socket,
        GuestIdentity {
            guest_id: args.guest.clone(),
            role: ROLE.into(),
            supported_tools: vec![
                "native-memory.remember".into(),
                "native-memory.recall".into(),
            ],
        },
    )
    .await?;
    loop {
        let message = ipc.recv_task().await?;
        let IpcResponse::InboundTask { task_json, .. } = message else {
            continue;
        };
        if task_json.len() > MAX_WIRE {
            continue;
        }
        let Ok(task) = serde_json::from_str::<RoutedRequest>(&task_json) else {
            continue;
        };
        if task.action != "native_memory_request" {
            continue;
        }
        if task.authority_handle.is_some() && args.authority_socket.is_none() {
            continue;
        }
        let envelope = task.signed_request;
        let current = || {
            let policy: Policy = serde_json::from_slice(
                &std::fs::read(&args.policy).map_err(|_| native_memory::Error::Unavailable)?,
            )
            .map_err(|_| native_memory::Error::Invalid)?;
            Ok((policy, now()))
        };
        let cancelled = AtomicBool::new(false);
        let mut reservation = if let Some(path) = &args.authority_socket {
            use native_memory::authority_control::{Client, Reply, Request};
            let Some(handle) = task.authority_handle else {
                continue;
            };
            let Ok((policy, time)) = current() else {
                continue;
            };
            let Ok(verified) = native_memory::VerifiedRequest::validate(
                &envelope,
                &policy,
                &args.node,
                &args.guest,
                time,
            ) else {
                continue;
            };
            let Ok(mut client) = Client::connect(path).await else {
                continue;
            };
            if !matches!(
                client
                    .send(&Request::Reserve {
                        guest: args.guest.clone(),
                        handle,
                        digest: verified.binding()
                    })
                    .await,
                Ok(Reply::Ready)
            ) {
                continue;
            }
            Some((client, verified.binding()))
        } else {
            None
        };
        let result = if let Some(graph) = &graph {
            if let Some((client, digest)) = &mut reservation {
                graph
                    .execute_authorized(&envelope, current, &cancelled, async || {
                        client.validate(*digest).await
                    })
                    .await
            } else {
                graph.execute_current(&envelope, current, &cancelled).await
            }
        } else if let Some(fixture) = &mut fixture {
            fixture.execute_current(&envelope, current, &cancelled)
        } else {
            Err(native_memory::Error::Unavailable)
        };
        if let Some((client, _)) = reservation {
            if matches!(result, Err(native_memory::Error::Uncertain)) {
                continue;
            }
            if client.finish(result.is_ok()).await.is_err() {
                continue;
            }
        }
        // Never send even an error to an unverified return route. Valid failures
        // are bounded by the ingress timeout; success uses the signed route only.
        let Ok(response) = result else { continue };
        let Ok(signer) =
            native_memory::response::ResponseSigner::load(&args.response_signing_config)
        else {
            continue;
        };
        let Ok(response) = signer.sign(&envelope, response, now()) else {
            continue;
        };
        let payload = serde_json::json!({"action":"native_memory_response","turn_id":envelope.claims.nonce,"signed_response":response});
        ipc.send_request(IpcRequest::EmitTask {
            target_node: envelope.claims.reply_node,
            target_role: "mcp-membrane".into(),
            target_guest_id: Some(envelope.claims.reply_guest),
            task_json: payload.to_string(),
        })
        .await?;
    }
}
