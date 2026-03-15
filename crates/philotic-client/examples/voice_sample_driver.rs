use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::{Map, Value, json};
use tokio::time::{Duration, timeout};

const DEFAULT_SOCKET: &str = "/tmp/philotic-aiua.sock";
const DEFAULT_TARGET_NODE: &str = "local-aiua-01";
const DEFAULT_TARGET_ROLE: &str = "model.elevenlabs";
const DEFAULT_REPLY_ROLE: &str = "voice.sample";
const DEFAULT_OUTPUT_PATH: &str = "/tmp/philotic-voice-sample.mp3";

#[derive(Debug)]
struct Args {
    socket_path: String,
    target_node: String,
    target_role: String,
    display_text: Option<String>,
    spoken_text: Option<String>,
    voice: Option<String>,
    model: Option<String>,
    output_format: Option<String>,
    output_path: String,
    provider_options: Map<String, Value>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse()?;
    let mut client = PhiloticClient::connect_at(
        &args.socket_path,
        GuestIdentity {
            guest_id: "voice-sample-driver".into(),
            role: DEFAULT_REPLY_ROLE.into(),
            supported_tools: Vec::new(),
        },
    )
    .await
    .with_context(|| {
        format!(
            "failed to connect voice sample driver to {}",
            args.socket_path
        )
    })?;

    let display_text = args
        .display_text
        .clone()
        .or_else(|| args.spoken_text.clone())
        .context("voice sample requires --display-text or --spoken-text")?;

    let task_json = json!({
        "kind": "voice.synthesize",
        "provider": "elevenlabs",
        "text": display_text,
        "display_text": args.display_text,
        "spoken_text": args.spoken_text,
        "voice": args.voice,
        "model": args.model,
        "output_format": args.output_format,
        "provider_options": args.provider_options,
        "reply_to": args.target_node,
        "reply_role": DEFAULT_REPLY_ROLE,
        "final_reply_to": args.target_node,
        "final_reply_role": DEFAULT_REPLY_ROLE,
        "final_reply_guest_id": "voice-sample-driver",
        "session_id": "voice-sample-session",
        "turn_id": "voice-sample-turn",
        "chat_id": "voice-sample-chat"
    })
    .to_string();

    let response = client
        .send_request(IpcRequest::EmitTask {
            target_node: args.target_node.clone(),
            target_role: args.target_role.clone(),
            target_guest_id: None,
            task_json,
        })
        .await?;

    match response {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("unexpected emit response: {other:?}"),
    }

    let inbound = timeout(Duration::from_secs(30), client.recv_task())
        .await
        .context("timed out waiting for voice sample reply")??;

    let IpcResponse::InboundTask { task_json, .. } = inbound else {
        bail!("unexpected voice sample envelope: {inbound:?}");
    };

    let payload: Value =
        serde_json::from_str(&task_json).context("failed to decode model reply json")?;
    let action = payload.get("action").and_then(Value::as_str);
    if action != Some("model_response") {
        bail!("unexpected reply action: {:?}", action);
    }

    if payload
        .get("agent_action")
        .and_then(|value| value.get("kind"))
        .and_then(Value::as_str)
        == Some("fail")
    {
        let message = payload
            .get("agent_action")
            .and_then(|value| value.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("unknown model-controller failure");
        bail!("voice sample failed: {}", message);
    }

    let artifact_json = payload
        .get("content")
        .and_then(Value::as_str)
        .context("voice sample reply missing serialized audio artifact content")?;
    let artifact: Value =
        serde_json::from_str(artifact_json).context("failed to decode audio artifact json")?;
    if artifact.get("kind").and_then(Value::as_str) != Some("audio_artifact") {
        bail!("unexpected reply content kind: {:?}", artifact);
    }

    let audio_base64 = artifact
        .get("audio_base64")
        .and_then(Value::as_str)
        .context("audio artifact missing audio_base64")?;
    let audio_bytes = BASE64_STANDARD
        .decode(audio_base64)
        .context("failed to decode audio artifact bytes")?;
    std::fs::write(&args.output_path, audio_bytes)
        .with_context(|| format!("failed to write {}", args.output_path))?;

    println!(
        "voice sample ok: wrote {} using model {:?} voice {:?}",
        args.output_path,
        artifact.get("model").and_then(Value::as_str),
        artifact.get("voice_id").and_then(Value::as_str)
    );
    Ok(())
}

impl Args {
    fn parse() -> Result<Self> {
        let mut socket_path =
            std::env::var("PHILOTIC_HOTEL_SOCKET").unwrap_or_else(|_| DEFAULT_SOCKET.to_string());
        let mut target_node = DEFAULT_TARGET_NODE.to_string();
        let mut target_role = DEFAULT_TARGET_ROLE.to_string();
        let mut display_text = None;
        let mut spoken_text = None;
        let mut voice = None;
        let mut model = None;
        let mut output_format = None;
        let mut output_path = DEFAULT_OUTPUT_PATH.to_string();
        let mut provider_options = Map::new();

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--socket" => socket_path = next_arg(&mut args, "--socket")?,
                "--target-node" => target_node = next_arg(&mut args, "--target-node")?,
                "--target-role" => target_role = next_arg(&mut args, "--target-role")?,
                "--display-text" | "--text" => {
                    display_text = Some(next_arg(&mut args, arg.as_str())?)
                }
                "--spoken-text" => spoken_text = Some(next_arg(&mut args, "--spoken-text")?),
                "--voice" => voice = Some(next_arg(&mut args, "--voice")?),
                "--model" => model = Some(next_arg(&mut args, "--model")?),
                "--output-format" => output_format = Some(next_arg(&mut args, "--output-format")?),
                "--output" => output_path = next_arg(&mut args, "--output")?,
                "--provider-option" => {
                    let raw = next_arg(&mut args, "--provider-option")?;
                    let (key, value) = raw.split_once('=').with_context(|| {
                        format!("provider option must be key=value, got {}", raw)
                    })?;
                    provider_options.insert(key.to_string(), parse_provider_value(value));
                }
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => bail!("unknown argument: {}", other),
            }
        }

        Ok(Self {
            socket_path,
            target_node,
            target_role,
            display_text,
            spoken_text,
            voice,
            model,
            output_format,
            output_path,
            provider_options,
        })
    }
}

fn next_arg(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    args.next()
        .with_context(|| format!("missing value for {}", flag))
}

fn parse_provider_value(raw: &str) -> Value {
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

fn print_help() {
    println!(
        "voice_sample_driver \
--display-text <text> \
[--spoken-text <text>] \
[--voice <voice-id>] \
[--model <provider-model>] \
[--output-format <format>] \
[--provider-option key=value]... \
[--output <path>] \
[--socket <path>] \
[--target-node <node>] \
[--target-role <role>]"
    );
}
