use ansible_mesh_core::whisper_training::{
    SqliteWhisperTrainingStorage, WhisperTrainingSample, WhisperTrainingStorage,
};
use anyhow::{Context, Result};
use philotic_client::{
    GuestIdentity, IpcRequest, IpcResponse, PhiloticClient, is_graceful_shutdown, is_ipc_disconnect,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};
use ulid::Ulid;

const ROLE: &str = "router-listener";
const GUEST_ID: &str = "router-listener-01";
/// Config key in hotel node_config that holds the listener configuration JSON.
const CONFIG_KEY: &str = "router_listener.config";

// ── Config schema ─────────────────────────────────────────────────────────────

/// Per-event-kind handler specification.
#[derive(Debug, Clone, Deserialize, Default)]
struct EventHandlerConfig {
    /// "whisper_training" | "table_insert" | "ignore"
    #[serde(default)]
    mode: String,
    // whisper_training fields
    #[serde(default)]
    whisper_db: Option<String>,
    #[serde(default)]
    audio_dir: Option<String>,
    // table_insert fields
    #[serde(default)]
    target_role: Option<String>,
    #[serde(default)]
    table_name: Option<String>,
    /// Maps event JSON field names to table column names.
    #[serde(default)]
    schema_map: Option<serde_json::Map<String, Value>>,
    /// Optional Python adapter script path — stdin: raw event JSON, stdout: transformed row JSON.
    #[serde(default)]
    adapter_script: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct ListenerConfig {
    /// Filter: only process events where `filter_keys` fields match. Empty = accept all.
    #[serde(default)]
    filter_keys: serde_json::Map<String, Value>,
    /// Per-event-kind handler map. Key = `kind` field value in inbound envelope.
    #[serde(default)]
    event_kinds: std::collections::HashMap<String, EventHandlerConfig>,
}

impl ListenerConfig {
    fn matches(&self, envelope: &Value) -> bool {
        for (k, expected) in &self.filter_keys {
            match envelope.get(k) {
                Some(v) if v == expected => {}
                _ => return false,
            }
        }
        true
    }
}

// ── Legacy structs (kept for backward compat with existing whisper path) ──────

#[derive(Debug, Deserialize)]
struct TranscriptionCapture {
    session_id: String,
    turn_id: String,
    agent_id: String,
    transcript: String,
    model_gen: Option<String>,
    blob_download_url: Option<String>,
}

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

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    info!("router-listener starting");

    loop {
        match run_connect_and_listen().await {
            Ok(()) => {
                info!("router-listener IPC loop exited cleanly — shutting down");
                break;
            }
            Err(e) if is_ipc_disconnect(&e) => {
                warn!("router-listener IPC disconnected, reconnecting in 5s…");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            Err(e) => {
                error!("router-listener fatal error: {e:#}");
                return Err(e);
            }
        }
    }

    Ok(())
}

/// Connect, load config from hotel, then enter the event loop.
async fn run_connect_and_listen() -> Result<()> {
    let identity = GuestIdentity {
        guest_id: GUEST_ID.to_string(),
        role: ROLE.to_string(),
        supported_tools: Vec::new(),
    };
    let mut ipc = PhiloticClient::connect(identity).await?;
    info!("router-listener connected to hotel IPC");

    let config = load_config(&mut ipc).await;
    info!(
        has_config = config.is_some(),
        "router-listener config loaded"
    );

    // Fall back to env-based whisper config when no structured config is present.
    let whisper_store = build_legacy_whisper_store().await.ok();
    let whisper_audio_dir = PathBuf::from(
        std::env::var("PHILOTIC_TRAINING_AUDIO_DIR")
            .unwrap_or_else(|_| "training_audio".to_string()),
    );
    let http = reqwest::Client::new();

    loop {
        let msg = ipc.recv_task().await?;
        if is_graceful_shutdown(&msg) {
            info!("router-listener: received graceful shutdown from hotel");
            return Ok(());
        }
        let IpcResponse::InboundTask { task_json, .. } = msg else {
            continue;
        };

        let envelope: Value = match serde_json::from_str(&task_json) {
            Ok(v) => v,
            Err(e) => {
                warn!("router-listener: failed to parse inbound task JSON: {e}");
                continue;
            }
        };

        let kind = envelope.get("kind").and_then(|v| v.as_str()).unwrap_or("");

        // Config-driven path.
        if let Some(ref cfg) = config {
            if !cfg.matches(&envelope) {
                continue;
            }
            if let Some(handler) = cfg.event_kinds.get(kind) {
                handle_event(&mut ipc, &http, handler, &envelope).await;
                continue;
            }
        }

        // Legacy fallback path for pre-config whisper events.
        match kind {
            "transcription_capture" => {
                if let Some(ref store) = whisper_store {
                    if let Ok(capture) = serde_json::from_value(envelope.clone()) {
                        handle_whisper_capture(store, &http, &whisper_audio_dir, capture).await;
                    } else {
                        warn!("router-listener: malformed transcription_capture");
                    }
                }
            }
            "transcription_correction" => {
                if let Some(ref store) = whisper_store {
                    if let Ok(correction) = serde_json::from_value(envelope.clone()) {
                        handle_whisper_correction(store, correction).await;
                    } else {
                        warn!("router-listener: malformed transcription_correction");
                    }
                }
            }
            other => {
                warn!("router-listener: unhandled event kind [{other}]");
            }
        }
    }
}

// ── Config loading ────────────────────────────────────────────────────────────

async fn load_config(ipc: &mut PhiloticClient) -> Option<ListenerConfig> {
    let resp = ipc
        .send_request(IpcRequest::GetConfig {
            key: CONFIG_KEY.to_string(),
        })
        .await
        .ok()?;

    let raw = match resp {
        IpcResponse::ConfigData { value_json, .. } => value_json?,
        _ => return None,
    };

    match serde_json::from_str::<ListenerConfig>(&raw) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            warn!("router-listener: failed to parse config: {e}");
            None
        }
    }
}

// ── Config-driven event handler ───────────────────────────────────────────────

async fn handle_event(
    ipc: &mut PhiloticClient,
    http: &reqwest::Client,
    handler: &EventHandlerConfig,
    envelope: &Value,
) {
    match handler.mode.as_str() {
        "table_insert" => {
            let row = build_row(handler, envelope);
            emit_table_insert(ipc, handler, row).await;
        }
        "whisper_training" => {
            let db = handler
                .whisper_db
                .as_deref()
                .unwrap_or("whisper_training.db");
            let audio_dir = PathBuf::from(handler.audio_dir.as_deref().unwrap_or("training_audio"));
            if let Ok(store) = SqliteWhisperTrainingStorage::open(db) {
                let store: Arc<dyn WhisperTrainingStorage> = Arc::new(store);
                if let Ok(capture) = serde_json::from_value(envelope.clone()) {
                    handle_whisper_capture(&store, http, &audio_dir, capture).await;
                }
            }
        }
        "ignore" => {}
        other => {
            warn!("router-listener: unknown handler mode [{other}]");
        }
    }
}

/// Map event fields to table columns using schema_map (or pass through all fields).
fn build_row(handler: &EventHandlerConfig, envelope: &Value) -> Value {
    if let Some(ref script) = handler.adapter_script {
        if let Some(adapted) = run_adapter(script, envelope) {
            return adapted;
        }
    }

    let Some(obj) = envelope.as_object() else {
        return envelope.clone();
    };

    let Some(ref schema_map) = handler.schema_map else {
        return envelope.clone();
    };

    let mut row = serde_json::Map::new();
    for (event_field, col_name_val) in schema_map {
        let col = col_name_val.as_str().unwrap_or(event_field.as_str());
        if let Some(v) = obj.get(event_field) {
            row.insert(col.to_string(), v.clone());
        }
    }
    Value::Object(row)
}

/// Run an adapter subprocess: stdin = event JSON, stdout = transformed row JSON.
fn run_adapter(script: &str, envelope: &Value) -> Option<Value> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("python3")
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| warn!("router-listener: adapter spawn failed: {e}"))
        .ok()?;

    if let Some(stdin) = child.stdin.as_mut() {
        let _ = write!(stdin, "{}", envelope);
    }

    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        warn!("router-listener: adapter exited {}", output.status);
        return None;
    }

    serde_json::from_slice(&output.stdout)
        .map_err(|e| warn!("router-listener: adapter output parse failed: {e}"))
        .ok()
}

async fn emit_table_insert(ipc: &mut PhiloticClient, handler: &EventHandlerConfig, row: Value) {
    let target_role = handler.target_role.as_deref().unwrap_or("table-datasource");
    let table_name = match handler.table_name.as_deref() {
        Some(t) => t.to_string(),
        None => {
            warn!("router-listener: table_insert handler missing table_name");
            return;
        }
    };

    let local_node =
        std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string());

    let task_json = json!({
        "kind": "table.insert",
        "provider": "table",
        "graph_id": table_name,
        "parameters": row,
    })
    .to_string();

    if let Err(e) = ipc
        .send_request(IpcRequest::EmitTask {
            target_node: local_node,
            target_role: target_role.to_string(),
            target_guest_id: None,
            task_json,
        })
        .await
    {
        warn!("router-listener: emit table.insert failed: {e}");
    }
}

// ── Legacy whisper helpers ────────────────────────────────────────────────────

async fn build_legacy_whisper_store() -> Result<Arc<dyn WhisperTrainingStorage>> {
    let db_path =
        std::env::var("PHILOTIC_TRAINING_DB").unwrap_or_else(|_| "whisper_training.db".to_string());
    let audio_dir = PathBuf::from(
        std::env::var("PHILOTIC_TRAINING_AUDIO_DIR")
            .unwrap_or_else(|_| "training_audio".to_string()),
    );
    tokio::fs::create_dir_all(&audio_dir)
        .await
        .context("failed to create PHILOTIC_TRAINING_AUDIO_DIR")?;
    Ok(Arc::new(SqliteWhisperTrainingStorage::open(&db_path)?))
}

async fn handle_whisper_capture(
    store: &Arc<dyn WhisperTrainingStorage>,
    http: &reqwest::Client,
    audio_dir: &PathBuf,
    capture: TranscriptionCapture,
) {
    let sample_id = Ulid::new().to_string();

    let audio_path = if let Some(ref url) = capture.blob_download_url {
        let dest = audio_dir.join(format!("{}.wav", capture.turn_id));
        match http.get(url).send().await {
            Ok(resp) if resp.status().is_success() => match resp.bytes().await {
                Ok(bytes) => match tokio::fs::write(&dest, &bytes).await {
                    Ok(()) => {
                        info!(turn_id = %capture.turn_id, path = %dest.display(), "audio saved");
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
            },
            Ok(resp) => {
                warn!("blob fetch HTTP {}: {}", resp.status(), url);
                None
            }
            Err(e) => {
                warn!("blob fetch failed: {e}");
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
        exported_at: None,
    };

    match store.insert_sample(&sample) {
        Ok(()) => info!(turn_id = %capture.turn_id, "whisper sample stored"),
        Err(e) => error!("failed to store whisper sample: {e}"),
    }
}

async fn handle_whisper_correction(
    store: &Arc<dyn WhisperTrainingStorage>,
    correction: TranscriptionCorrection,
) {
    match store.update_correction(
        &correction.turn_id,
        &correction.corrected_transcript,
        &correction.correction_source,
    ) {
        Ok(true) => info!(turn_id = %correction.turn_id, "correction applied"),
        Ok(false) => warn!(turn_id = %correction.turn_id, "correction for unknown turn_id"),
        Err(e) => error!("failed to apply correction: {e}"),
    }
}
