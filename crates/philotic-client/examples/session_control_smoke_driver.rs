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
) -> Result<()> {
    client
        .send_request(IpcRequest::EmitTask {
            target_node: "local-ansible-01".into(),
            target_role: "agent".into(),
            target_guest_id: None,
            task_json: serde_json::json!({
                "source": "smoke",
                "session_id": session_id,
                "turn_id": turn_id,
                "chat_id": chat_id,
                "content": content,
                "final_reply_to": "local-ansible-01",
                "final_reply_role": "membrane",
                "final_reply_guest_id": "session-control-smoke-membrane"
            })
            .to_string(),
        })
        .await?;

    let reply = timeout(Duration::from_secs(10), client.recv_task())
        .await
        .with_context(|| format!("timed out waiting for reply to {content}"))??;
    let IpcResponse::InboundTask { task_json, .. } = reply else {
        bail!("unexpected envelope while waiting for {content}: {reply:?}");
    };
    let payload: serde_json::Value =
        serde_json::from_str(&task_json).context("failed to decode session control reply")?;
    let actual = payload
        .get("content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if !actual.contains(expected) {
        bail!("expected reply containing {:?}, got {:?}", expected, actual);
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = std::env::var("PHILOTIC_HOTEL_SOCKET")
        .context("PHILOTIC_HOTEL_SOCKET must be set for session_control_smoke_driver")?;
    let session_id = "smoke:session-control:agent-jane-01";
    let chat_id = "smoke-session-control-chat";

    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: "session-control-smoke-membrane".into(),
        role: "membrane".into(),
        supported_tools: Vec::new(),
    })
    .await
    .with_context(|| format!("failed to connect session control smoke driver to {socket_path}"))?;

    emit_and_expect(
        &mut client,
        session_id,
        chat_id,
        "status-turn-1",
        "/status",
        "Session status: active.",
    )
    .await?;
    emit_and_expect(
        &mut client,
        session_id,
        chat_id,
        "pause-turn-1",
        "/pause",
        "Session paused.",
    )
    .await?;
    emit_and_expect(
        &mut client,
        session_id,
        chat_id,
        "status-turn-2",
        "/status",
        "Session status: paused.",
    )
    .await?;
    emit_and_expect(
        &mut client,
        session_id,
        chat_id,
        "blocked-turn-1",
        "hello while paused",
        "Session is paused. Use /resume to continue.",
    )
    .await?;
    emit_and_expect(
        &mut client,
        session_id,
        chat_id,
        "resume-turn-1",
        "/resume",
        "Session resumed.",
    )
    .await?;
    emit_and_expect(
        &mut client,
        session_id,
        chat_id,
        "status-turn-3",
        "/status",
        "Session status: active.",
    )
    .await?;

    println!("session control smoke ok");
    Ok(())
}
