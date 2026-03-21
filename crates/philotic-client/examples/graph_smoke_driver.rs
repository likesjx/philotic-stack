/// Smoke driver for the graph-runner.
///
/// Connects to the hotel as a fake agent, emits an `execute_tool` task for
/// `graph.create`, and asserts that the `tool_result` response contains `"ok": true`.
/// Optionally follows up with `graph.node.upsert` + `graph.node.get` to verify
/// the full create→write→read round-trip.
use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use tokio::time::{Duration, timeout};

const DRIVER_GUEST_ID: &str = "graph-smoke-driver";
const DRIVER_ROLE: &str = "graph.smoke.reply";

#[tokio::main]
async fn main() -> Result<()> {
    let target_node =
        std::env::var("PHILOTIC_TARGET_NODE").unwrap_or_else(|_| "local-aiua-01".to_string());
    let graph_name = format!("smoke-graph-{}", uuid::Uuid::new_v4());

    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: DRIVER_GUEST_ID.into(),
        role: DRIVER_ROLE.into(),
        supported_tools: Vec::new(),
    })
    .await
    .context("failed to connect graph smoke driver")?;

    let subscribe = client
        .send_request(IpcRequest::SubscribeInbox {
            role: DRIVER_ROLE.into(),
        })
        .await
        .context("failed to subscribe graph smoke driver inbox")?;
    match subscribe {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("unexpected subscribe response: {other:?}"),
    }

    // ── graph.create ─────────────────────────────────────────────────────────

    let graph_id = emit_tool(
        &mut client,
        &target_node,
        "graph.create",
        serde_json::json!({
            "name": graph_name,
            "default_visibility": "public",
            "caller_id": DRIVER_GUEST_ID
        }),
    )
    .await
    .context("graph.create round-trip")?;

    let gid = graph_id["graph_id"]
        .as_str()
        .context("graph.create response missing graph_id")?
        .to_string();
    println!("graph.create ok  graph_id={gid}");

    // ── graph.node.upsert ─────────────────────────────────────────────────────

    let upsert_result = emit_tool(
        &mut client,
        &target_node,
        "graph.node.upsert",
        serde_json::json!({
            "graph_id": gid,
            "node_type": "Smoke",
            "label": "smoke node",
            "visibility": ["public"],
            "caller_id": DRIVER_GUEST_ID
        }),
    )
    .await
    .context("graph.node.upsert round-trip")?;

    let nid = upsert_result["node_id"]
        .as_str()
        .context("graph.node.upsert response missing node_id")?
        .to_string();
    println!("graph.node.upsert ok  node_id={nid}");

    // ── graph.node.get ────────────────────────────────────────────────────────

    let get_result = emit_tool(
        &mut client,
        &target_node,
        "graph.node.get",
        serde_json::json!({
            "graph_id": gid,
            "node_id": nid,
            "caller_id": DRIVER_GUEST_ID
        }),
    )
    .await
    .context("graph.node.get round-trip")?;

    let label = get_result["node"]["label"]
        .as_str()
        .context("graph.node.get response missing node.label")?;

    if label != "smoke node" {
        bail!("expected label 'smoke node', got {label:?}");
    }
    println!("graph.node.get ok  label={label:?}");

    // ── graph.export ──────────────────────────────────────────────────────────

    let export_result = emit_tool(
        &mut client,
        &target_node,
        "graph.export",
        serde_json::json!({
            "graph_id": gid,
            "caller_id": DRIVER_GUEST_ID
        }),
    )
    .await
    .context("graph.export round-trip")?;

    let node_count = export_result["node_count"]
        .as_u64()
        .context("graph.export response missing node_count")?;

    if node_count < 1 {
        bail!("expected at least 1 node in export, got {node_count}");
    }
    println!("graph.export ok  node_count={node_count}");

    println!("graph smoke round-trip succeeded.");
    Ok(())
}

/// Emit an `execute_tool` task to the hotel and wait for the `tool_result` reply.
/// Returns the parsed result JSON, asserting `"ok": true`.
async fn emit_tool(
    client: &mut PhiloticClient,
    target_node: &str,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value> {
    let session_id = format!("smoke:graph:{tool_name}");
    let turn_id = format!("smoke-turn-{tool_name}");

    let response = client
        .send_request(IpcRequest::EmitTask {
            target_node: target_node.to_string(),
            target_role: format!("tool.{tool_name}"),
            target_guest_id: None,
            task_json: serde_json::json!({
                "action": "execute_tool",
                "tool_name": tool_name,
                "arguments": arguments,
                "session_id": session_id,
                "turn_id": turn_id,
                "chat_id": "smoke-chat",
                "agent_id": DRIVER_GUEST_ID,
                "caller_roles": [DRIVER_ROLE],
                "reply_to": target_node,
                "reply_role": DRIVER_ROLE,
                "final_reply_to": target_node,
                "final_reply_role": DRIVER_ROLE,
            })
            .to_string(),
        })
        .await?;

    match response {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("{tool_name}: unexpected emit response: {other:?}"),
    }

    let reply = timeout(Duration::from_secs(15), client.recv_task())
        .await
        .with_context(|| format!("{tool_name}: timed out waiting for tool_result"))??;

    let IpcResponse::InboundTask { task_json, .. } = reply else {
        bail!("{tool_name}: unexpected reply envelope: {reply:?}");
    };

    let payload: serde_json::Value =
        serde_json::from_str(&task_json).context("failed to decode tool_result json")?;

    let content_str = payload["content"]
        .as_str()
        .with_context(|| format!("{tool_name}: tool_result missing content"))?;

    let content: serde_json::Value = serde_json::from_str(content_str)
        .with_context(|| format!("{tool_name}: content is not valid JSON: {content_str}"))?;

    if content["ok"].as_bool() != Some(true) {
        bail!(
            "{tool_name}: tool returned ok=false: {}",
            content
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or(content_str)
        );
    }

    Ok(content)
}
