mod access;
mod graph;
mod store;
mod tools;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use philotic_client::{is_ipc_disconnect, GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::json;
use store::sqlite::SqliteGraphStore;
use store::GraphTableStore;
use tracing::{info, warn};

use graph::Identity;

// ── Config ────────────────────────────────────────────────────────────────────

fn db_path() -> PathBuf {
    std::env::var("PHILOTIC_GRAPH_DB_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".philotic").join("graph-runner.db")
        })
}

fn instance_id() -> String {
    std::env::var("PHILOTIC_GRAPH_RUNNER_ID").unwrap_or_else(|_| "graph-runner-01".into())
}

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(author, version, about = "Philotic Context Graph Tool Runner")]
struct Args {}

// ── Supported tools ───────────────────────────────────────────────────────────

const GRAPH_TOOLS: &[&str] = &[
    "graph.create",
    "graph.list",
    "graph.schema.get",
    "graph.schema.update",
    "graph.node.upsert",
    "graph.node.get",
    "graph.node.list",
    "graph.node.delete",
    "graph.edge.upsert",
    "graph.edge.get",
    "graph.edge.list",
    "graph.edge.delete",
    "graph.traverse",
    "graph.search",
    "graph.export",
];

const TABLE_TOOLS: &[&str] = &[
    "table.create",
    "table.schema.get",
    "table.update",
    "table.drop",
    "table.list",
    "table.row.insert",
    "table.row.get",
    "table.row.update",
    "table.row.delete",
    "table.query",
];

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let _args = Args::parse();

    let path = db_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    info!(path = %path.display(), "Opening graph store");
    let store: &'static dyn GraphTableStore =
        Box::leak(Box::new(SqliteGraphStore::open(&path)?));

    let iid = instance_id();
    let all_tools: Vec<String> = GRAPH_TOOLS.iter().chain(TABLE_TOOLS.iter())
        .map(|s| s.to_string())
        .collect();
    let identity = GuestIdentity {
        guest_id: iid.clone(),
        role: "tool.graph".into(),
        supported_tools: all_tools.clone(),
    };

    let mut ipc_client = PhiloticClient::connect(identity).await?;

    // Subscribe to inbox roles for all graph + table tools.
    for tool in &all_tools {
        let role = format!("tool.{}", tool);
        let _ = ipc_client
            .send_request(IpcRequest::SubscribeInbox { role })
            .await?;
    }
    // Also subscribe on the general role so the hotel can direct-route by guest ID.
    let _ = ipc_client
        .send_request(IpcRequest::SubscribeInbox { role: "tool.graph".into() })
        .await?;

    // Register all graphs that already exist in this instance's store.
    for graph_meta in store.list_graphs().unwrap_or_default() {
        let _ = ipc_client
            .send_request(IpcRequest::RegisterGraphInstance {
                graph_id: graph_meta.graph_id.clone(),
                instance_id: iid.clone(),
            })
            .await;
    }

    info!(instance_id = %iid, tools = GRAPH_TOOLS.len(), "Graph runner ready");

    loop {
        match tokio::time::timeout(Duration::from_secs(5), ipc_client.recv_task()).await {
            Ok(Ok(IpcResponse::InboundTask { task_json, .. })) => {
                let task: serde_json::Value = match serde_json::from_str(&task_json) {
                    Ok(v) => v,
                    Err(_) => {
                        warn!("Could not parse graph tool task");
                        continue;
                    }
                };

                if task.get("action").and_then(serde_json::Value::as_str) != Some("execute_tool") {
                    continue;
                }

                let tool_name = task
                    .get("tool_name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let arguments = task.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let session_id = task
                    .get("session_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let turn_id = task
                    .get("turn_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let chat_id = task
                    .get("chat_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let reply_to = task
                    .get("reply_to")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&iid)
                    .to_string();
                let reply_role = task
                    .get("reply_role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("agent")
                    .to_string();
                let final_reply_to = task
                    .get("final_reply_to")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(&iid)
                    .to_string();
                let final_reply_role = task
                    .get("final_reply_role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("membrane")
                    .to_string();

                // Build the caller identity from the task envelope.
                let caller_id = task
                    .get("agent_id")
                    .or_else(|| task.get("caller_id"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let caller_roles: Vec<String> = task
                    .get("caller_roles")
                    .and_then(serde_json::Value::as_array)
                    .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                let identity = Identity { id: caller_id, roles: caller_roles };

                // Dispatch is synchronous (rusqlite). Spawn a blocking task so the
                // async loop is not stalled during heavier graph operations.
                let result_content = tokio::task::spawn_blocking({
                    let tool_name = tool_name.clone();
                    let arguments = arguments.clone();
                    let identity = identity.clone();
                    move || tools::dispatch(store, &tool_name, &arguments, &identity)
                })
                .await
                .unwrap_or_else(|e| format!("graph runner internal error: {e}"));

                // When a new graph is created, register it in the hotel ODS.
                if tool_name == "graph.create" {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&result_content) {
                        if v["ok"].as_bool() == Some(true) {
                            if let Some(gid) = v["graph_id"].as_str() {
                                let _ = ipc_client
                                    .send_request(IpcRequest::RegisterGraphInstance {
                                        graph_id: gid.to_string(),
                                        instance_id: iid.clone(),
                                    })
                                    .await;
                            }
                        }
                    }
                }

                let _ = ipc_client
                    .send_request(IpcRequest::EmitTask {
                        target_node: reply_to,
                        target_role: reply_role,
                        target_guest_id: None,
                        task_json: json!({
                            "action": "tool_result",
                            "session_id": session_id,
                            "turn_id": turn_id,
                            "chat_id": chat_id,
                            "tool_name": tool_name,
                            "content": result_content,
                            "final_reply_to": final_reply_to,
                            "final_reply_role": final_reply_role,
                        })
                        .to_string(),
                    })
                    .await;
            }
            Ok(Ok(_)) => {}
            Ok(Err(err)) => {
                if is_ipc_disconnect(&err) {
                    info!("Hotel IPC disconnected; graph-runner exiting.");
                    return Ok(());
                }
                warn!("IPC recv error: {err}");
            }
            Err(_) => {} // timeout, loop
        }
    }
}
