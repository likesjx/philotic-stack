use anyhow::{bail, Context, Result};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use tokio::time::{timeout, Duration};

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
            task_json: serde_json::json!({
                "source": "smoke",
                "session_id": session_id,
                "turn_id": turn_id,
                "chat_id": chat_id,
                "content": content,
                "final_reply_to": "local-ansible-01",
                "final_reply_role": "hegemon"
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
        serde_json::from_str(&task_json).context("failed to decode session binding reply")?;
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
        .context("PHILOTIC_HOTEL_SOCKET must be set for session_bindings_smoke_driver")?;
    let session_id = "smoke:session-bindings:agent-jane-01";
    let chat_id = "smoke-session-bindings-chat";

    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: "session-bindings-smoke-hegemon".into(),
        role: "hegemon".into(),
        supported_tools: Vec::new(),
    })
    .await
    .with_context(|| format!("failed to connect session bindings smoke driver to {socket_path}"))?;

    emit_and_expect(
        &mut client,
        session_id,
        chat_id,
        "bindings-turn-1",
        "/tools add echo",
        "Tool bindings updated: echo.",
    )
    .await?;
    emit_and_expect(
        &mut client,
        session_id,
        chat_id,
        "bindings-turn-2",
        "/skills add planning",
        "Skill bindings updated: planning.",
    )
    .await?;
    emit_and_expect(
        &mut client,
        session_id,
        chat_id,
        "bindings-turn-3",
        "/workspace set workspace://main",
        "Workspace binding updated: workspace://main.",
    )
    .await?;
    emit_and_expect(
        &mut client,
        session_id,
        chat_id,
        "bindings-turn-4",
        "/status",
        "Skillset: planning.",
    )
    .await?;

    println!("session bindings smoke ok");
    Ok(())
}
