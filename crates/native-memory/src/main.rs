//! Explicitly configured native runner. No secret fetching or automatic startup.
use clap::Parser;
use native_memory::{Envelope, MAX_WIRE, Policy, ROLE, Runner};
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
            guest_id: args.guest,
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
        let Ok(envelope) = serde_json::from_str::<Envelope>(&task_json) else {
            continue;
        };
        let current = || {
            let policy: Policy = serde_json::from_slice(
                &std::fs::read(&args.policy).map_err(|_| native_memory::Error::Unavailable)?,
            )
            .map_err(|_| native_memory::Error::Invalid)?;
            Ok((policy, now()))
        };
        let cancelled = AtomicBool::new(false);
        let result = if let Some(graph) = &graph {
            graph.execute_current(&envelope, current, &cancelled).await
        } else if let Some(fixture) = &mut fixture {
            fixture.execute_current(&envelope, current, &cancelled)
        } else {
            Err(native_memory::Error::Unavailable)
        };
        // Never send even an error to an unverified return route. Valid failures
        // are bounded by the ingress timeout; success uses the signed route only.
        let Ok(response) = result else { continue };
        let payload = serde_json::json!({"action":"datasource_response","turn_id":envelope.claims.nonce,"result":response});
        ipc.send_request(IpcRequest::EmitTask {
            target_node: envelope.claims.reply_node,
            target_role: "mcp-membrane".into(),
            target_guest_id: Some(envelope.claims.reply_guest),
            task_json: payload.to_string(),
        })
        .await?;
    }
}
