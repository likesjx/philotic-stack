use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use tokio::time::{Duration, timeout};

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = std::env::var("PHILOTIC_HOTEL_SOCKET")
        .context("PHILOTIC_HOTEL_SOCKET must be set for smoke_driver")?;
    let expected =
        std::env::var("PHILOTIC_SMOKE_EXPECTED_REPLY").unwrap_or_else(|_| "pong".to_string());
    let target_node =
        std::env::var("PHILOTIC_TARGET_NODE").unwrap_or_else(|_| "local-aiua-01".to_string());
    let final_reply_to =
        std::env::var("PHILOTIC_FINAL_REPLY_TO").unwrap_or_else(|_| target_node.clone());
    let session_id = std::env::var("PHILOTIC_SMOKE_SESSION_ID")
        .unwrap_or_else(|_| "smoke:chat-1:agent-jane-01".to_string());
    let turn_id =
        std::env::var("PHILOTIC_SMOKE_TURN_ID").unwrap_or_else(|_| "smoke-turn-1".to_string());
    let chat_id =
        std::env::var("PHILOTIC_SMOKE_CHAT_ID").unwrap_or_else(|_| "smoke-chat-1".to_string());
    let content =
        std::env::var("PHILOTIC_SMOKE_USER_CONTENT").unwrap_or_else(|_| "/ping".to_string());

    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: "smoke-driver-membrane".into(),
        role: "membrane".into(),
        supported_tools: Vec::new(),
    })
    .await
    .with_context(|| format!("failed to connect smoke driver to {socket_path}"))?;

    let response = client
        .send_request(IpcRequest::EmitTask {
            target_node,
            target_role: "agent".into(),
            target_guest_id: None,
            task_json: serde_json::json!({
                "source": "smoke",
                "session_id": session_id,
                "turn_id": turn_id,
                "chat_id": chat_id,
                "content": content,
                "final_reply_to": final_reply_to,
                "final_reply_role": "membrane",
                "final_reply_guest_id": "smoke-driver-membrane"
            })
            .to_string(),
        })
        .await?;

    match response {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("unexpected emit response: {other:?}"),
    }

    let deadline = Duration::from_secs(10);
    let mut reply_content = None;
    for _ in 0..30 {
        let inbound = timeout(deadline, client.recv_task())
            .await
            .context("timed out waiting for final reply")??;

        let IpcResponse::InboundTask { task_json, .. } = inbound else {
            continue;
        };

        let payload: serde_json::Value =
            serde_json::from_str(&task_json).context("failed to decode final reply json")?;
        let action = payload.get("action").and_then(serde_json::Value::as_str);
        if action != Some("send_reply") {
            continue;
        }

        reply_content = payload
            .get("content")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        break;
    }

    let reply_content = reply_content.context("did not receive send_reply before timeout")?;
    if reply_content != expected {
        bail!(
            "expected final reply {:?}, got {:?}",
            expected,
            reply_content
        );
    }

    println!("smoke ok: received final reply {:?}", reply_content);
    Ok(())
}
