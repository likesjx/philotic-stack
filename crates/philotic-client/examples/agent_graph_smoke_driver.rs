/// Live smoke driver for agent-graph-runner (Seams 3 & 4).
///
/// Connects to the hotel and fires three `agent.graph.*` tool calls at the
/// `agent-graph` role inbox.  Because agent-graph-runner uses `CompleteTask`
/// (not `EmitTask`) to acknowledge results, this driver does not wait for
/// replies — side effects are verified by the shell script via sqlite3.
///
/// Environment:
///   PHILOTIC_HOTEL_SOCKET   — hotel UDS socket path (required)
///   PHILOTIC_TARGET_NODE    — hotel node ID (default: "local-aiua-01")
///   PHILOTIC_AGENT_ID       — agent ID expected by the runner (default: "smoke-agent")
use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};

const DRIVER_GUEST_ID: &str = "agent-graph-smoke-driver";
const DRIVER_ROLE: &str = "agent-graph-smoke-driver";

#[tokio::main]
async fn main() -> Result<()> {
    let target_node =
        std::env::var("PHILOTIC_TARGET_NODE").unwrap_or_else(|_| "local-aiua-01".to_string());
    let agent_id =
        std::env::var("PHILOTIC_AGENT_ID").unwrap_or_else(|_| "smoke-agent".to_string());

    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: DRIVER_GUEST_ID.into(),
        role: DRIVER_ROLE.into(),
        supported_tools: Vec::new(),
    })
    .await
    .context("failed to connect agent-graph smoke driver to hotel")?;

    // ── agent.graph.write — write a tool preference ───────────────────────────

    emit_fire_and_forget(
        &mut client,
        &target_node,
        "agent.graph.write",
        serde_json::json!({
            "entity": "tool_preference",
            "tool_name": "smoke.tool",
            "preference_level": 9,
            "config": { "smoke": true }
        }),
    )
    .await
    .context("agent.graph.write emit")?;
    println!("agent.graph.write dispatched");

    // ── agent.graph.declare — declare a resource ──────────────────────────────

    emit_fire_and_forget(
        &mut client,
        &target_node,
        "agent.graph.declare",
        serde_json::json!({
            "resource_type": "router_listener",
            "config_hint": "smoke-router"
        }),
    )
    .await
    .context("agent.graph.declare emit")?;
    println!("agent.graph.declare dispatched");

    // ── agent.graph.sync — apply a LWW snapshot (Seam 4) ─────────────────────
    //
    // Snapshot carries a "sync.tool" preference with a far-future updated_at
    // so it wins LWW against any local value, and a new declaration entry.

    emit_fire_and_forget(
        &mut client,
        &target_node,
        "agent.graph.sync",
        serde_json::json!({
            "agent_id": agent_id,
            "source_node_id": target_node,
            "exported_at": 0u64,
            "preferences": [
                {
                    "tool_name": "sync.tool",
                    "preference_level": 5,
                    "config_json": { "from_sync": true },
                    "updated_at": 9_999_999_999u64
                }
            ],
            "declarations": []
        }),
    )
    .await
    .context("agent.graph.sync emit")?;
    println!("agent.graph.sync dispatched");

    println!("agent-graph smoke driver: all tasks dispatched — side effects will be verified via sqlite3");
    Ok(())
}

/// Emit a tool call to the `agent-graph` role inbox and verify the hotel
/// accepted it (returns `ok: true`).  Does not wait for the runner's reply.
async fn emit_fire_and_forget(
    client: &mut PhiloticClient,
    target_node: &str,
    tool_name: &str,
    args: serde_json::Value,
) -> Result<()> {
    let response = client
        .send_request(IpcRequest::EmitTask {
            target_node: target_node.to_string(),
            target_role: "agent-graph".to_string(),
            target_guest_id: None,
            task_json: serde_json::json!({
                "tool_name": tool_name,
                "args": args
            })
            .to_string(),
        })
        .await
        .with_context(|| format!("send EmitTask for {tool_name}"))?;

    match response {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        other => bail!("{tool_name}: hotel rejected EmitTask: {other:?}"),
    }
}
