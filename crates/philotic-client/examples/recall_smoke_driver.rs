use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::Value;
use tokio::time::{Duration, timeout};

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = std::env::var("PHILOTIC_HOTEL_SOCKET")
        .context("PHILOTIC_HOTEL_SOCKET must be set for recall_smoke_driver")?;
    let target_node =
        std::env::var("PHILOTIC_TARGET_NODE").unwrap_or_else(|_| "local-aiua-01".to_string());
    let final_reply_to =
        std::env::var("PHILOTIC_FINAL_REPLY_TO").unwrap_or_else(|_| target_node.clone());
    let target_guest_id = std::env::var("PHILOTIC_SMOKE_TARGET_GUEST_ID").ok();
    let session_id = std::env::var("PHILOTIC_SMOKE_SESSION_ID")
        .unwrap_or_else(|_| "smoke:recall:agent-jane-01".to_string());
    let turn_id =
        std::env::var("PHILOTIC_SMOKE_TURN_ID").unwrap_or_else(|_| "recall-smoke-turn-1".into());
    let chat_id =
        std::env::var("PHILOTIC_SMOKE_CHAT_ID").unwrap_or_else(|_| "recall-smoke-chat-1".into());
    let content = std::env::var("PHILOTIC_SMOKE_USER_CONTENT")
        .unwrap_or_else(|_| "continue the memory architecture work".to_string());
    let expected_event = std::env::var("PHILOTIC_EXPECT_TURN_EVENT")
        .unwrap_or_else(|_| "memory_auto_recall_completed".to_string());

    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: "recall-smoke-driver".into(),
        role: "membrane".into(),
        supported_tools: Vec::new(),
    })
    .await
    .with_context(|| format!("failed to connect recall smoke driver to {socket_path}"))?;

    let response = client
        .send_request(IpcRequest::EmitTask {
            target_node,
            target_role: "agent".into(),
            target_guest_id,
            task_json: serde_json::json!({
                "source": "smoke",
                "session_id": session_id,
                "turn_id": turn_id,
                "chat_id": chat_id,
                "content": content,
                "final_reply_to": final_reply_to,
                "final_reply_role": "membrane",
                "final_reply_guest_id": "recall-smoke-driver"
            })
            .to_string(),
        })
        .await?;

    match response {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("unexpected emit response: {other:?}"),
    }

    for _ in 0..10 {
        let inbound = timeout(Duration::from_secs(10), client.recv_task())
            .await
            .context("timed out waiting for recall smoke event")??;

        let IpcResponse::InboundTask { task_json, .. } = inbound else {
            continue;
        };
        let payload: Value =
            serde_json::from_str(&task_json).context("failed to decode recall smoke payload")?;
        let action = payload.get("action").and_then(Value::as_str);
        if action != Some("turn_event") {
            continue;
        }

        let event = payload
            .get("event")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if event == expected_event {
            println!("recall smoke ok: observed turn event {event:?}");
            return Ok(());
        }
    }

    bail!("did not observe expected turn event {:?}", expected_event);
}
