use anyhow::{Context, Result};
use ansible_mesh_core::whisper_training::{SqliteWhisperTrainingStorage, WhisperTrainingSample, WhisperTrainingStorage};
use philotic_client::{GuestIdentity, IpcResponse, PhiloticClient, is_ipc_disconnect};
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};
use ulid::Ulid;

const ROLE: &str = "router-listener";
const GUEST_ID: &str = "router-listener-01";

/// Envelope received from model-router fan-out after AudioTranscribe succeeds.
#[derive(Debug, Deserialize)]
struct TranscriptionCapture {
    session_id: String,
    turn_id: String,
    agent_id: String,
    transcript: String,
    model_gen: Option<String>,
    blob_download_url: Option<String>,
}

/// Envelope emitted by philote /correct slash command.
#[derive(Debug, Deserialize)]
struct TranscriptionCorrection {
    turn_id: String,
    corrected_transcript: String,
    #[serde(default = "default_correction_source")]
    correction_source: String,
}

fn default_correction_source() -> String {
    "operator".to_string()
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let db_path = std::env::var("PHILOTIC_TRAINING_DB")
        .unwrap_or_else(|_| "whisper_training.db".to_string());
    let audio_dir = PathBuf::from(
        std::env::var("PHILOTIC_TRAINING_AUDIO_DIR")
            .unwrap_or_else(|_| "training_audio".to_string()),
    );
    tokio::fs::create_dir_all(&audio_dir)
        .await
        .context("failed to create PHILOTIC_TRAINING_AUDIO_DIR")?;

    let store: Arc<dyn WhisperTrainingStorage> =
        Arc::new(SqliteWhisperTrainingStorage::open(&db_path)?);

    let http = reqwest::Client::new();

    info!("router-listener starting, db={db_path}");

    loop {
        match run_listener_loop(&store, &http, &audio_dir).await {
            Ok(()) => {
                info!("router-listener IPC loop exited cleanly — shutting down");
                break;
            }
            Err(e) if is_ipc_disconnect(&e) => {
                warn!("router-listener IPC disconnected, reconnecting in 5s…");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
            Err(e) => {
                error!("router-listener fatal error: {e:#}");
                return Err(e);
            }
        }
    }

    Ok(())
}

async fn run_listener_loop(
    store: &Arc<dyn WhisperTrainingStorage>,
    http: &reqwest::Client,
    audio_dir: &PathBuf,
) -> Result<()> {
    let identity = GuestIdentity {
        guest_id: GUEST_ID.to_string(),
        role: ROLE.to_string(),
        supported_tools: Vec::new(),
    };
    let mut ipc = PhiloticClient::connect(identity).await?;
    info!("router-listener connected to hotel IPC");

    loop {
        let msg = ipc.recv_task().await?;
        let IpcResponse::InboundTask { task_json, .. } = msg else {
            continue;
        };

        let envelope: serde_json::Value = match serde_json::from_str(&task_json) {
            Ok(v) => v,
            Err(e) => {
                warn!("router-listener: failed to parse inbound task JSON: {e}");
                continue;
            }
        };

        let kind = envelope
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match kind {
            "transcription_capture" => {
                let capture: TranscriptionCapture =
                    match serde_json::from_value(envelope.clone()) {
                        Ok(c) => c,
                        Err(e) => {
                            warn!("router-listener: malformed transcription_capture: {e}");
                            continue;
                        }
                    };
                handle_capture(store, http, audio_dir, capture).await;
            }
            "transcription_correction" => {
                let correction: TranscriptionCorrection =
                    match serde_json::from_value(envelope.clone()) {
                        Ok(c) => c,
                        Err(e) => {
                            warn!("router-listener: malformed transcription_correction: {e}");
                            continue;
                        }
                    };
                handle_correction(store, correction).await;
            }
            other => {
                warn!("router-listener: unknown task kind [{other}], ignoring");
            }
        }
    }
}

async fn handle_capture(
    store: &Arc<dyn WhisperTrainingStorage>,
    http: &reqwest::Client,
    audio_dir: &PathBuf,
    capture: TranscriptionCapture,
) {
    let sample_id = Ulid::new().to_string();

    // Download WAV from blob store if URL is present.
    let audio_path = if let Some(ref url) = capture.blob_download_url {
        let dest = audio_dir.join(format!("{}.wav", capture.turn_id));
        match http.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                match resp.bytes().await {
                    Ok(bytes) => match tokio::fs::write(&dest, &bytes).await {
                        Ok(()) => {
                            info!(
                                turn_id = %capture.turn_id,
                                path = %dest.display(),
                                "router-listener: audio saved"
                            );
                            Some(dest.to_string_lossy().to_string())
                        }
                        Err(e) => {
                            warn!("router-listener: failed to write audio file: {e}");
                            None
                        }
                    },
                    Err(e) => {
                        warn!("router-listener: failed to read audio response body: {e}");
                        None
                    }
                }
            }
            Ok(resp) => {
                warn!(
                    "router-listener: blob fetch HTTP {}: {}",
                    resp.status(),
                    url
                );
                None
            }
            Err(e) => {
                warn!("router-listener: blob fetch failed: {e}");
                None
            }
        }
    } else {
        None
    };

    let auto_eligible = std::env::var("PHILOTIC_TRAINING_AUTO_ELIGIBLE").as_deref() == Ok("true");
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let sample = WhisperTrainingSample {
        sample_id,
        turn_id: capture.turn_id.clone(),
        session_id: capture.session_id,
        agent_id: capture.agent_id,
        audio_path,
        raw_transcript: capture.transcript,
        corrected_transcript: None,
        correction_source: None,
        model_gen: capture.model_gen.unwrap_or_default(),
        confidence: None,
        training_eligible: auto_eligible,
        timestamp,
    };

    match store.insert_sample(&sample) {
        Ok(()) => info!(turn_id = %capture.turn_id, "router-listener: sample stored"),
        Err(e) => error!("router-listener: failed to store sample: {e}"),
    }
}

async fn handle_correction(
    store: &Arc<dyn WhisperTrainingStorage>,
    correction: TranscriptionCorrection,
) {
    match store.update_correction(
        &correction.turn_id,
        &correction.corrected_transcript,
        &correction.correction_source,
    ) {
        Ok(true) => info!(
            turn_id = %correction.turn_id,
            "router-listener: correction applied, sample marked training_eligible"
        ),
        Ok(false) => warn!(
            turn_id = %correction.turn_id,
            "router-listener: correction received for unknown turn_id"
        ),
        Err(e) => error!("router-listener: failed to apply correction: {e}"),
    }
}
