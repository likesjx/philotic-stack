use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use tokio::time::{Duration, sleep};
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = std::env::var("PHILOTIC_HOTEL_SOCKET")
        .context("PHILOTIC_HOTEL_SOCKET must be set for webrtc_smoke_driver")?;
    let target_node_id = std::env::var("PHILOTIC_WEBRTC_TARGET_NODE")
        .or_else(|_| std::env::var("PHILOTIC_TARGET_NODE"))
        .context("PHILOTIC_WEBRTC_TARGET_NODE or PHILOTIC_TARGET_NODE must be set")?;
    let target_guest_id = std::env::var("PHILOTIC_WEBRTC_TARGET_GUEST").ok();
    let session_id = std::env::var("PHILOTIC_WEBRTC_SESSION_ID")
        .unwrap_or_else(|_| format!("webrtc-smoke-{}", Uuid::new_v4()));
    let expected_log = format!("Received WebRTC pong for session {}", session_id);

    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: "webrtc-smoke-driver".into(),
        role: "membrane".into(),
        supported_tools: Vec::new(),
    })
    .await
    .with_context(|| format!("failed to connect webrtc smoke driver to {socket_path}"))?;

    let response = client
        .send_request(IpcRequest::StartWebRtcSession {
            target_node_id,
            target_guest_id,
            session_id: Some(session_id.clone()),
        })
        .await?;

    match response {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("unexpected start session response: {other:?}"),
    }

    for _ in 0..20 {
        let response = client
            .send_request(IpcRequest::GetWebRtcSessionStatus {
                session_id: session_id.clone(),
            })
            .await?;
        let IpcResponse::Standard {
            ok: true,
            data: Some(data),
            ..
        } = response
        else {
            bail!("unexpected webrtc status response");
        };

        let status = data
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if status == "completed" {
            println!("webrtc smoke ok: observed {:?}", expected_log);
            return Ok(());
        }

        sleep(Duration::from_secs(1)).await;
    }

    bail!(
        "timed out waiting for WebRTC pong marker {:?} in local hotel logs",
        expected_log
    );
}
