use anyhow::{Context, Result, bail};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::Value;
use tokio::time::{Duration, sleep, timeout};
use uuid::Uuid;

async fn seed_session_bindings(
    client: &mut PhiloticClient,
    session_id: &str,
    remote_hotel_id: &str,
) -> Result<()> {
    let response = client
        .send_request(IpcRequest::UpdateTask {
            task_id: Uuid::new_v4(),
            state: "session_bindings_updated".into(),
            payload: serde_json::json!({
                "session_id": session_id,
                "bindings": {
                    "effective_model_controller": "gemini-flash",
                    "preferred_hotel_id": remote_hotel_id
                }
            }),
        })
        .await
        .context("failed to seed session bindings")?;

    match response {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        other => bail!("unexpected response while seeding session bindings: {other:?}"),
    }
}

async fn wait_for_remote_model_route(
    client: &mut PhiloticClient,
    session_id: &str,
    expected_remote_node: &str,
    expected_incarnation: &str,
    timeout_duration: Duration,
) -> Result<()> {
    let started = std::time::Instant::now();
    loop {
        let response = client
            .send_request(IpcRequest::GetConfig {
                key: format!("__session_snapshot__:{session_id}"),
            })
            .await
            .context("failed to fetch session snapshot")?;

        let snapshot = match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => serde_json::from_str::<Value>(&value_json)
                .context("failed to decode session snapshot")?,
            other => bail!("unexpected session snapshot response: {other:?}"),
        };
        let route = &snapshot["component_route_assembly"]["execution_routes"]["text.generate"];
        if route["target_node"].as_str() == Some(expected_remote_node)
            && route["incarnation_id"].as_str() == Some(expected_incarnation)
        {
            return Ok(());
        }

        if started.elapsed() >= timeout_duration {
            bail!(
                "timed out waiting for remote model route {:?}/{:?}; last route was node={:?} inc={:?}; last snapshot={}",
                expected_remote_node,
                expected_incarnation,
                route["target_node"],
                route["incarnation_id"],
                snapshot
            );
        }

        sleep(Duration::from_millis(250)).await;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = std::env::var("PHILOTIC_HOTEL_SOCKET")
        .context("PHILOTIC_HOTEL_SOCKET must be set for remote_model_smoke_driver")?;
    let remote_hotel_id = std::env::var("PHILOTIC_REMOTE_HOTEL_ID")
        .unwrap_or_else(|_| "aria-architect-hotel".to_string());
    let expected_remote_node = std::env::var("PHILOTIC_REMOTE_NODE_ID")
        .unwrap_or_else(|_| "aria-architect-hotel-ansible-01".to_string());
    let expected_remote_incarnation = std::env::var("PHILOTIC_REMOTE_MODEL_INCID")
        .unwrap_or_else(|_| "aria-architect-hotel:model-controller-gemini".to_string());
    let expected_reply = std::env::var("PHILOTIC_SMOKE_EXPECTED_REPLY")
        .unwrap_or_else(|_| "remote model smoke ok".to_string());
    let session_id = std::env::var("PHILOTIC_SMOKE_SESSION_ID")
        .unwrap_or_else(|_| "smoke:remote-model:agent-jane-01".to_string());
    let turn_id = std::env::var("PHILOTIC_SMOKE_TURN_ID")
        .unwrap_or_else(|_| "remote-model-turn-1".to_string());
    let chat_id = std::env::var("PHILOTIC_SMOKE_CHAT_ID")
        .unwrap_or_else(|_| "remote-model-chat-1".to_string());
    let content = std::env::var("PHILOTIC_SMOKE_USER_CONTENT")
        .unwrap_or_else(|_| "explain the remote model placement smoke".to_string());

    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: "remote-model-smoke-hegemon".into(),
        role: "hegemon".into(),
        supported_tools: Vec::new(),
    })
    .await
    .with_context(|| format!("failed to connect smoke driver to {socket_path}"))?;

    seed_session_bindings(&mut client, &session_id, &remote_hotel_id).await?;
    wait_for_remote_model_route(
        &mut client,
        &session_id,
        &expected_remote_node,
        &expected_remote_incarnation,
        Duration::from_secs(15),
    )
    .await?;

    let response = client
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
                "final_reply_role": "hegemon",
                "final_reply_guest_id": "remote-model-smoke-hegemon"
            })
            .to_string(),
        })
        .await?;

    match response {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("unexpected emit response: {other:?}"),
    }

    let final_reply = timeout(Duration::from_secs(15), client.recv_task())
        .await
        .context("timed out waiting for final remote model smoke reply")??;
    let IpcResponse::InboundTask { task_json, .. } = final_reply else {
        bail!("unexpected final reply envelope: {final_reply:?}");
    };
    let payload: Value =
        serde_json::from_str(&task_json).context("failed to decode final reply json")?;
    let action = payload.get("action").and_then(Value::as_str);
    let reply_content = payload
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();

    if action != Some("send_reply") {
        bail!("unexpected final reply action: {:?}", action);
    }
    if reply_content != expected_reply {
        bail!(
            "expected final reply {:?}, got {:?}",
            expected_reply,
            reply_content
        );
    }

    println!(
        "remote model smoke ok: received {:?} via {}",
        reply_content, expected_remote_node
    );
    Ok(())
}
