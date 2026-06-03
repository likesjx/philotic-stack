use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use tokio::time::{Duration, timeout};

const DRIVER_GUEST_ID: &str = "datasource-smoke-driver";
const DRIVER_ROLE: &str = "datasource.smoke.reply";

#[tokio::main]
async fn main() -> Result<()> {
    let target_node =
        std::env::var("PHILOTIC_TARGET_NODE").unwrap_or_else(|_| "local-aiua-01".to_string());
    let graph_id = format!("datasource-smoke-{}", uuid::Uuid::new_v4());

    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: DRIVER_GUEST_ID.into(),
        role: DRIVER_ROLE.into(),
        supported_tools: Vec::new(),
    })
    .await
    .context("failed to connect datasource smoke driver")?;

    let subscribe = client
        .send_request(IpcRequest::SubscribeInbox {
            role: DRIVER_ROLE.into(),
        })
        .await
        .context("failed to subscribe datasource smoke driver inbox")?;
    match subscribe {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("unexpected subscribe response: {other:?}"),
    }

    let response = client
        .send_request(IpcRequest::EmitTask {
            target_node: target_node.clone(),
            target_role: "graph-datasource".into(),
            target_guest_id: None,
            task_json: serde_json::json!({
                "action": "execute_tool",
                "tool_name": "graph.create",
                "graph_id": graph_id,
                "session_id": "smoke:datasource:graph.create",
                "turn_id": "smoke-turn-datasource-graph-create",
                "chat_id": "smoke-chat",
                "agent_id": DRIVER_GUEST_ID,
                "reply_to": target_node,
                "reply_role": DRIVER_ROLE,
            })
            .to_string(),
        })
        .await?;

    match response {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("graph.create: unexpected emit response: {other:?}"),
    }

    let reply = timeout(Duration::from_secs(15), client.recv_task())
        .await
        .context("graph.create: timed out waiting for datasource_response")??;
    let IpcResponse::InboundTask { task_json, .. } = reply else {
        bail!("graph.create: unexpected reply envelope: {reply:?}");
    };
    let payload: serde_json::Value =
        serde_json::from_str(&task_json).context("failed to decode datasource_response json")?;

    if payload["action"].as_str() != Some("datasource_response") {
        bail!("expected datasource_response, got {payload}");
    }
    if payload.get("error").is_some() {
        bail!("datasource returned error: {}", payload["error"]);
    }
    if payload["capability"].as_str() != Some("graph.create") {
        bail!("expected graph.create capability, got {payload}");
    }

    let returned_graph_id = payload["result"]["graph_id"]
        .as_str()
        .context("datasource_response missing result.graph_id")?;
    if returned_graph_id != graph_id {
        bail!("expected graph_id {graph_id}, got {returned_graph_id}");
    }

    println!("datasource graph.create ok  graph_id={returned_graph_id}");
    Ok(())
}
