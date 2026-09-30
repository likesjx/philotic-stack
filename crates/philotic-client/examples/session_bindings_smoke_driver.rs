use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use tokio::time::{Duration, timeout};

async fn emit_and_expect(
    client: &mut PhiloticClient,
    session_id: &str,
    chat_id: &str,
    turn_id: &str,
    content: &str,
    expected: &str,
    target_node: &str,
) -> Result<()> {
    client
        .send_request(IpcRequest::EmitTask {
            target_node: target_node.into(),
            target_role: "agent".into(),
            target_guest_id: None,
            task_json: serde_json::json!({
                "source": "smoke",
                "session_id": session_id,
                "turn_id": turn_id,
                "chat_id": chat_id,
                "content": content,
                "final_reply_to": target_node,
                "final_reply_role": "membrane",
                "final_reply_guest_id": "session-bindings-smoke-membrane"
            })
            .to_string(),
        })
        .await?;

    let mut actual = String::new();
    for _ in 0..10 {
        let reply = timeout(Duration::from_secs(5), client.recv_task())
            .await
            .with_context(|| format!("timed out waiting for reply to {content}"))??;
        let IpcResponse::InboundTask { task_json, .. } = reply else {
            continue;
        };
        let payload: serde_json::Value =
            serde_json::from_str(&task_json).context("failed to decode session binding reply")?;
        if let Some(c) = payload.get("content").and_then(serde_json::Value::as_str)
            && !c.is_empty()
        {
            actual = c.to_string();
            break;
        }
    }

    if !actual.contains(expected) {
        bail!("expected reply containing {:?}, got {:?}", expected, actual);
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = std::env::var("PHILOTIC_HOTEL_SOCKET")
        .context("PHILOTIC_HOTEL_SOCKET must be set for session_bindings_smoke_driver")?;
    let session_id = "smoke:session-bindings:agent-jane-01";
    let chat_id = "smoke-session-bindings-chat";

    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: "session-bindings-smoke-membrane".into(),
        role: "membrane".into(),
        supported_tools: Vec::new(),
    })
    .await
    .with_context(|| format!("failed to connect session bindings smoke driver to {socket_path}"))?;

    let target_node =
        std::env::var("PHILOTIC_TARGET_NODE").unwrap_or_else(|_| "local-aiua-01".to_string());

    emit_and_expect(
        &mut client,
        session_id,
        chat_id,
        "bindings-turn-1",
        "/tools add echo",
        "Tool bindings updated: echo.",
        &target_node,
    )
    .await?;
    emit_and_expect(
        &mut client,
        session_id,
        chat_id,
        "bindings-turn-2",
        "/skills add planning",
        "Skill bindings updated: planning.",
        &target_node,
    )
    .await?;
    emit_and_expect(
        &mut client,
        session_id,
        chat_id,
        "bindings-turn-3",
        "/workspace set workspace://main",
        "Workspace binding updated: workspace://main.",
        &target_node,
    )
    .await?;
    emit_and_expect(
        &mut client,
        session_id,
        chat_id,
        "bindings-turn-4",
        "/status",
        "Skills: planning.",
        &target_node,
    )
    .await?;

    println!("session bindings smoke ok");
    Ok(())
}
