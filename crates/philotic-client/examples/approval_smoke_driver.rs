use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use tokio::time::{Duration, timeout};

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = std::env::var("PHILOTIC_HOTEL_SOCKET")
        .context("PHILOTIC_HOTEL_SOCKET must be set for approval_smoke_driver")?;
    let session_id = std::env::var("PHILOTIC_SMOKE_SESSION_ID")
        .unwrap_or_else(|_| "smoke:approval:agent-jane-01".to_string());
    let chat_id = std::env::var("PHILOTIC_SMOKE_CHAT_ID")
        .unwrap_or_else(|_| "smoke-approval-chat".to_string());
    let approval_request = std::env::var("PHILOTIC_SMOKE_APPROVAL_REQUEST")
        .unwrap_or_else(|_| "need approval deploy the thing".to_string());
    let expected_wait = std::env::var("PHILOTIC_SMOKE_EXPECTED_WAIT").unwrap_or_else(|_| {
        "Approval required: deploy the thing. Reply /approve or /deny.".to_string()
    });
    let expected_final = std::env::var("PHILOTIC_SMOKE_EXPECTED_FINAL")
        .unwrap_or_else(|_| "Approved: deploy the thing".to_string());
    let approval_command =
        std::env::var("PHILOTIC_SMOKE_APPROVAL_COMMAND").unwrap_or_else(|_| "/approve".to_string());

    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: "approval-smoke-hegemon".into(),
        role: "hegemon".into(),
        supported_tools: Vec::new(),
    })
    .await
    .with_context(|| format!("failed to connect approval smoke driver to {socket_path}"))?;

    let first_turn_id = "approval-turn-1";
    let first_response = client
        .send_request(IpcRequest::EmitTask {
            target_node: "local-ansible-01".into(),
            target_role: "agent".into(),
            task_json: serde_json::json!({
                "source": "smoke",
                "session_id": session_id,
                "turn_id": first_turn_id,
                "chat_id": chat_id,
                "content": approval_request,
                "final_reply_to": "local-ansible-01",
                "final_reply_role": "hegemon"
            })
            .to_string(),
        })
        .await?;

    match first_response {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("unexpected approval emit response: {other:?}"),
    }

    let wait_reply = timeout(Duration::from_secs(10), client.recv_task())
        .await
        .context("timed out waiting for approval prompt")??;
    let IpcResponse::InboundTask { task_json, .. } = wait_reply else {
        bail!("unexpected approval prompt envelope: {wait_reply:?}");
    };
    let payload: serde_json::Value =
        serde_json::from_str(&task_json).context("failed to decode approval prompt")?;
    let wait_content = payload
        .get("content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if wait_content != expected_wait {
        bail!(
            "expected approval prompt {:?}, got {:?}",
            expected_wait,
            wait_content
        );
    }

    let second_turn_id = "approval-turn-2";
    let second_response = client
        .send_request(IpcRequest::EmitTask {
            target_node: "local-ansible-01".into(),
            target_role: "agent".into(),
            task_json: serde_json::json!({
                "source": "smoke",
                "session_id": session_id,
                "turn_id": second_turn_id,
                "chat_id": chat_id,
                "content": approval_command,
                "final_reply_to": "local-ansible-01",
                "final_reply_role": "hegemon"
            })
            .to_string(),
        })
        .await?;

    match second_response {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("unexpected approval resolution emit response: {other:?}"),
    }

    let final_reply = timeout(Duration::from_secs(10), client.recv_task())
        .await
        .context("timed out waiting for approved response")??;
    let IpcResponse::InboundTask { task_json, .. } = final_reply else {
        bail!("unexpected approved reply envelope: {final_reply:?}");
    };
    let payload: serde_json::Value =
        serde_json::from_str(&task_json).context("failed to decode approved reply")?;
    let final_content = payload
        .get("content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if final_content != expected_final {
        bail!(
            "expected final approved reply {:?}, got {:?}",
            expected_final,
            final_content
        );
    }

    println!(
        "approval smoke ok: wait={:?} final={:?}",
        wait_content, final_content
    );
    Ok(())
}
