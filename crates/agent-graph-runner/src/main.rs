//! agent-graph-runner — per-agent cognitive graph tool-runner guest.
//!
//! Materialised by the hotel as a required resource for each active agent.
//! Connects to the hotel IPC socket, registers as an `agent-graph` guest,
//! and services `agent.graph.*` tool calls against a per-agent SQLite store.
//!
//! # Configuration (environment variables)
//!
//! | Variable                   | Default                            | Purpose                        |
//! |----------------------------|------------------------------------|--------------------------------|
//! | `PHILOTIC_AGENT_ID`        | *(required)*                       | Owning agent's identity        |
//! | `PHILOTIC_GRAPH_RUNNER_ID` | `agent-graph-{agent_id}`           | Guest ID for IPC registration  |
//! | `PHILOTIC_IPC_SOCKET`      | `/tmp/philotic-aiua.sock`          | Hotel UDS socket path          |
//! | `PHILOTIC_AGENT_GRAPH_DB`  | `~/.philotic/agent-graph-{id}.db` | Per-agent SQLite path          |

use agent_graph_runner::tools;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ansible_mesh_core::agent_graph_storage::SqliteAgentGraphStorage;
use anyhow::{Context, Result};
use clap::Parser;
use philotic_client::{is_ipc_disconnect, GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::json;
use tracing::{error, info, warn};

// ── Config helpers ────────────────────────────────────────────────────────────

fn agent_id() -> String {
    std::env::var("PHILOTIC_AGENT_ID").unwrap_or_else(|_| "unknown-agent".into())
}

fn guest_id() -> String {
    std::env::var("PHILOTIC_GRAPH_RUNNER_ID")
        .unwrap_or_else(|_| format!("agent-graph-{}", agent_id()))
}

fn ipc_socket() -> String {
    std::env::var("PHILOTIC_IPC_SOCKET").unwrap_or_else(|_| "/tmp/philotic-aiua.sock".into())
}

fn db_path() -> PathBuf {
    let path = std::env::var("PHILOTIC_AGENT_GRAPH_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home)
                .join(".philotic")
                .join(format!("agent-graph-{}.db", agent_id()))
        });
    // SQLite cannot create intermediate directories; on hosts where
    // `$HOME/.philotic/` does not pre-exist (e.g. the vps `philotic` service
    // user) a missing parent crashlooped this runner until its respawn budget
    // exhausted (2026-07-12..20, count 255 on agent-beacon). Best-effort:
    // startup still fails loudly if creation is truly impossible.
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(dir = %parent.display(), "could not create agent-graph db dir: {e}");
        }
    }
    path
}

const AGENT_GRAPH_TOOLS: &[&str] = &[
    "agent.graph.read",
    "agent.graph.write",
    "agent.graph.declare",
    "agent.graph.recall",
    "agent.graph.sync",
];

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(author, version, about = "Philotic Agent Graph Tool-Runner Guest")]
struct Args {}

// ── Main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let _args = Args::parse();

    let agent_id = agent_id();
    let guest_id = guest_id();
    let socket = ipc_socket();
    let db = db_path();

    info!(
        agent_id = %agent_id,
        guest_id = %guest_id,
        db = %db.display(),
        "agent-graph-runner starting"
    );

    if let Some(parent) = db.parent() {
        std::fs::create_dir_all(parent).context("Failed to create agent graph db directory")?;
    }

    let storage = Arc::new(
        SqliteAgentGraphStorage::open(&agent_id, &db)
            .context("Failed to open agent graph SQLite database")?,
    );

    let identity = GuestIdentity {
        guest_id: guest_id.clone(),
        role: "agent-graph".to_string(),
        supported_tools: AGENT_GRAPH_TOOLS.iter().map(|s| s.to_string()).collect(),
    };

    run_ipc_loop(agent_id, guest_id, socket, identity, storage).await
}

async fn run_ipc_loop(
    agent_id: String,
    guest_id: String,
    socket: String,
    identity: GuestIdentity,
    storage: Arc<SqliteAgentGraphStorage>,
) -> Result<()> {
    loop {
        match PhiloticClient::connect_at(&socket, identity.clone()).await {
            Ok(mut client) => {
                info!(guest_id = %guest_id, "Connected to hotel IPC");
                loop {
                    match client.recv_task().await {
                        Ok(msg) => {
                            handle_inbound_task(
                                &agent_id,
                                &guest_id,
                                &msg,
                                storage.as_ref(),
                                &mut client,
                            )
                            .await;
                        }
                        Err(e) if is_ipc_disconnect(&e) => {
                            warn!(guest_id = %guest_id, "IPC disconnected: {e}");
                            break;
                        }
                        Err(e) => {
                            error!(guest_id = %guest_id, "IPC recv error: {e}");
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                warn!(guest_id = %guest_id, "Failed to connect to hotel IPC: {e}");
            }
        }

        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn handle_inbound_task(
    agent_id: &str,
    guest_id: &str,
    msg: &IpcResponse,
    storage: &SqliteAgentGraphStorage,
    client: &mut PhiloticClient,
) {
    let IpcResponse::InboundTask {
        task_json, task_id, ..
    } = msg
    else {
        return;
    };

    let task: serde_json::Value = match serde_json::from_str(task_json) {
        Ok(v) => v,
        Err(e) => {
            warn!(guest_id, "Failed to parse inbound task JSON: {e}");
            return;
        }
    };

    let tool_name = task
        .get("tool_name")
        .or_else(|| task.get("tool"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");

    let args = task.get("args").cloned().unwrap_or_else(|| json!({}));

    info!(
        agent_id,
        guest_id,
        tool = tool_name,
        "agent.graph tool call"
    );

    let result_str = tools::dispatch(storage, tool_name, &args, agent_id);
    let result_json: serde_json::Value =
        serde_json::from_str(&result_str).unwrap_or_else(|_| json!({"raw": result_str}));

    let outcome = if result_json
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
    {
        "success"
    } else {
        "failure"
    };
    tools::record_tool_call_trace(storage, agent_id, tool_name, &args, &result_json, outcome);

    let ack = IpcRequest::CompleteTask {
        task_id: *task_id,
        result: result_json,
    };
    if let Err(e) = client.send_request(ack).await {
        warn!(guest_id, "Failed to send CompleteTask ack: {e}");
    }
}
