use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use std::path::PathBuf;
use tokio::time::{Duration, timeout};

#[tokio::main]
async fn main() -> Result<()> {
    let socket_path = std::env::var("PHILOTIC_HOTEL_SOCKET")
        .unwrap_or_else(|_| "/tmp/philotic-ansible.sock".to_string());
    let session_id = std::env::var("PHILOTIC_VOICE_SAMPLE_SESSION_ID")
        .unwrap_or_else(|_| "voice-sample:test-client".to_string());
    let turn_id = std::env::var("PHILOTIC_VOICE_SAMPLE_TURN_ID")
        .unwrap_or_else(|_| "voice-sample-turn-1".to_string());
    let chat_id = std::env::var("PHILOTIC_VOICE_SAMPLE_CHAT_ID")
        .unwrap_or_else(|_| "voice-sample-chat".to_string());
    let text = std::env::var("PHILOTIC_VOICE_SAMPLE_TEXT")
        .unwrap_or_else(|_| "Hello from Philotic. This is a hotel-routed voice test.".to_string());
    let output_path = std::env::var("PHILOTIC_VOICE_SAMPLE_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("tmp/voice-samples/hotel-elevenlabs-sample.mp3"));
    let target_role = std::env::var("PHILOTIC_VOICE_SAMPLE_TARGET_ROLE")
        .unwrap_or_else(|_| "model.elevenlabs".to_string());
    let model = std::env::var("PHILOTIC_VOICE_SAMPLE_MODEL")
        .unwrap_or_else(|_| "eleven_multilingual_v2".to_string());
    let output_format = std::env::var("PHILOTIC_VOICE_SAMPLE_FORMAT")
        .unwrap_or_else(|_| "mp3_44100_128".to_string());
    let voice_id = std::env::var("PHILOTIC_VOICE_SAMPLE_VOICE_ID").ok();

    let mut client = PhiloticClient::connect(GuestIdentity {
        guest_id: "voice-sample-client".into(),
        role: "voice-sample-client".into(),
        supported_tools: Vec::new(),
    })
    .await
    .with_context(|| format!("failed to connect voice sample driver to {socket_path}"))?;

    let mut payload = serde_json::json!({
        "kind": "voice.synthesize",
        "session_id": session_id,
        "turn_id": turn_id,
        "chat_id": chat_id,
        "text": text,
        "model": model,
        "output_format": output_format,
        "reply_to": "local-ansible-01",
        "reply_role": "voice-sample-client",
        "final_reply_to": "local-ansible-01",
        "final_reply_role": "voice-sample-client",
        "final_reply_guest_id": "voice-sample-client"
    });

    if let Some(voice_id) = voice_id {
        payload["voice_id"] = serde_json::Value::String(voice_id);
    }

    let response = client
        .send_request(IpcRequest::EmitTask {
            target_node: "local-ansible-01".into(),
            target_role,
            target_guest_id: None,
            task_json: payload.to_string(),
        })
        .await?;

    match response {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("unexpected emit response: {other:?}"),
    }

    let reply = timeout(Duration::from_secs(30), client.recv_task())
        .await
        .context("timed out waiting for voice sample reply")??;
    let IpcResponse::InboundTask { task_json, .. } = reply else {
        bail!("unexpected voice sample envelope: {reply:?}");
    };

    let payload: serde_json::Value =
        serde_json::from_str(&task_json).context("failed to decode voice sample reply")?;
    if payload.get("action").and_then(serde_json::Value::as_str) != Some("model_response") {
        bail!("unexpected reply action: {:?}", payload.get("action"));
    }

    if let Some(message) = payload
        .get("agent_action")
        .and_then(|value| value.get("message"))
        .and_then(serde_json::Value::as_str)
    {
        bail!("model controller returned failure: {message}");
    }

    let content = payload
        .get("content")
        .and_then(serde_json::Value::as_str)
        .context("voice sample reply was missing string content")?;
    let artifact: serde_json::Value = serde_json::from_str(content)
        .context("voice sample content was not audio artifact json")?;

    if artifact.get("kind").and_then(serde_json::Value::as_str) != Some("audio_artifact") {
        bail!("unexpected artifact kind: {:?}", artifact.get("kind"));
    }

    let audio_base64 = artifact
        .get("audio_base64")
        .and_then(serde_json::Value::as_str)
        .context("audio artifact missing audio_base64")?;
    let audio_bytes = BASE64_STANDARD
        .decode(audio_base64)
        .context("failed to decode audio artifact base64")?;

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create output directory {}", parent.display()))?;
    }
    std::fs::write(&output_path, audio_bytes)
        .with_context(|| format!("failed to write {}", output_path.display()))?;

    println!(
        "voice sample ok: wrote {} using voice {}",
        output_path.display(),
        artifact
            .get("voice_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
    );

    Ok(())
}
