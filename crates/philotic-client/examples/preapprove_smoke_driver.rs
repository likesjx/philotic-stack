use anyhow::{bail, Context, Result};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use tokio::time::{timeout, Duration};

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = std::env::var("PHILOTIC_HOTEL_SOCKET")
        .context("PHILOTIC_HOTEL_SOCKET must be set for preapprove_smoke_driver")?;
    let session_id = std::env::var("PHILOTIC_SMOKE_SESSION_ID")
        .unwrap_or_else(|_| "smoke:preapprove:agent-jane-01".to_string());
    let chat_id =
        std::env::var("PHILOTIC_SMOKE_CHAT_ID").unwrap_or_else(|_| "smoke-preapprove-chat".to_string());
    let expected_preapprove = std::env::var("PHILOTIC_SMOKE_EXPECTED_PREAPPROVE")
        .unwrap_or_else(|_| "Approval policy updated: this session is now pre-approved.".to_string());
    let expected_final = std::env::var("PHILOTIC_SMOKE_EXPECTED_FINAL")
        .unwrap_or_else(|_| "Approved: deploy the thing".to_string());

    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: "preapprove-smoke-hegemon".into(),
        role: "hegemon".into(),
    })
    .await
    .with_context(|| format!("failed to connect preapprove smoke driver to {socket_path}"))?;

    client
        .send_request(IpcRequest::EmitTask {
            target_node: "local-ansible-01".into(),
            target_role: "agent".into(),
            task_json: serde_json::json!({
                "source": "smoke",
                "session_id": session_id,
                "turn_id": "preapprove-turn-1",
                "chat_id": chat_id,
                "content": "/preapprove this-session",
                "final_reply_to": "local-ansible-01",
                "final_reply_role": "hegemon"
            })
            .to_string(),
        })
        .await?;

    let preapprove_reply = timeout(Duration::from_secs(10), client.recv_task())
        .await
        .context("timed out waiting for preapprove reply")??;
    let IpcResponse::InboundTask { task_json, .. } = preapprove_reply else {
        bail!("unexpected preapprove reply envelope: {preapprove_reply:?}");
    };
    let payload: serde_json::Value =
        serde_json::from_str(&task_json).context("failed to decode preapprove reply")?;
    let preapprove_content = payload
        .get("content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if preapprove_content != expected_preapprove {
        bail!(
            "expected preapprove reply {:?}, got {:?}",
            expected_preapprove,
            preapprove_content
        );
    }

    client
        .send_request(IpcRequest::EmitTask {
            target_node: "local-ansible-01".into(),
            target_role: "agent".into(),
            task_json: serde_json::json!({
                "source": "smoke",
                "session_id": session_id,
                "turn_id": "preapprove-turn-2",
                "chat_id": chat_id,
                "content": "need approval deploy the thing",
                "final_reply_to": "local-ansible-01",
                "final_reply_role": "hegemon"
            })
            .to_string(),
        })
        .await?;

    let final_reply = timeout(Duration::from_secs(10), client.recv_task())
        .await
        .context("timed out waiting for preapproved final reply")??;
    let IpcResponse::InboundTask { task_json, .. } = final_reply else {
        bail!("unexpected preapproved reply envelope: {final_reply:?}");
    };
    let payload: serde_json::Value =
        serde_json::from_str(&task_json).context("failed to decode preapproved final reply")?;
    let final_content = payload
        .get("content")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if final_content != expected_final {
        bail!("expected final preapproved reply {:?}, got {:?}", expected_final, final_content);
    }

    println!(
        "preapprove smoke ok: preapprove={:?} final={:?}",
        preapprove_content, final_content
    );
    Ok(())
}
