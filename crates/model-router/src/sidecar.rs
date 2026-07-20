/// Ollama-compatible HTTP sidecar for the ONNX provider.
///
/// Exposes embedding and transcription inference directly over HTTP so clients
/// that speak the Ollama API (Muninn, vector stores, tooling) can use local
/// ONNX models without going through the IPC routing plane.
///
/// Port: 11435 (leaves Ollama's 11434 free for optional proxy fallback).
use anyhow::Result;
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use onnx_runner::{EmbeddingsBackend, KokoroBackend, ModelCache, WhisperBackend};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info};

// ── State ─────────────────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct SidecarState {
    embeddings: Arc<RwLock<Option<EmbeddingsBackend>>>,
    whisper: Arc<RwLock<Option<WhisperBackend>>>,
    kokoro: Arc<RwLock<Option<KokoroBackend>>>,
}

impl SidecarState {
    pub fn new(
        embeddings: Arc<RwLock<Option<EmbeddingsBackend>>>,
        whisper: Arc<RwLock<Option<WhisperBackend>>>,
        kokoro: Arc<RwLock<Option<KokoroBackend>>>,
    ) -> Self {
        Self {
            embeddings,
            whisper,
            kokoro,
        }
    }
}

// ── /api/health ───────────────────────────────────────────────────────────────

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({"status": "ok"}))
}

// ── /api/embeddings ───────────────────────────────────────────────────────────

/// Ollama `POST /api/embeddings` request body.
#[derive(Debug, Deserialize)]
pub struct EmbedRequest {
    pub model: Option<String>,
    pub prompt: String,
}

/// Ollama `POST /api/embeddings` response body.
#[derive(Debug, Serialize)]
pub struct EmbedResponse {
    pub embedding: Vec<f32>,
    /// Non-standard extension: model generation provenance token.
    /// Serialized as "model" for Ollama API compatibility.
    #[serde(rename = "model")]
    pub model_gen: String,
}

async fn embed(
    State(state): State<SidecarState>,
    Json(req): Json<EmbedRequest>,
) -> impl IntoResponse {
    let guard = state.embeddings.read().await;
    let Some(backend) = guard.as_ref() else {
        error!("sidecar: embedding backend not loaded");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "embedding backend not loaded"})),
        )
            .into_response();
    };

    match backend.embed(&req.prompt) {
        Ok(output) => Json(EmbedResponse {
            embedding: output.vector,
            model_gen: output.model_gen,
        })
        .into_response(),
        Err(err) => {
            error!(%err, "sidecar: embedding inference failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": err.to_string()})),
            )
                .into_response()
        }
    }
}

// ── /api/embeddings/batch ────────────────────────────────────────────────────

/// Hard cap on prompts accepted per `/api/embeddings/batch` request. Sized so
/// one over-budget caller fails fast with a 400 instead of the sidecar
/// silently accepting an unbounded batch and blocking the executor for a
/// long stretch of sequential inference calls.
pub const EMBED_BATCH_MAX_PROMPTS: usize = 64;

/// `POST /api/embeddings/batch` request body.
#[derive(Debug, Deserialize)]
pub struct EmbedBatchRequest {
    pub prompts: Vec<String>,
}

/// `POST /api/embeddings/batch` response body: one embedding per input
/// prompt, in the same order, plus the shared model generation token (the
/// whole batch runs through the same loaded backend).
#[derive(Debug, Serialize)]
pub struct EmbedBatchResponse {
    pub embeddings: Vec<Vec<f32>>,
    #[serde(rename = "model")]
    pub model_gen: String,
}

/// Pure request-shape validation: non-empty, at most [`EMBED_BATCH_MAX_PROMPTS`].
/// Split out from the handler so it's testable without spinning up axum.
fn validate_batch_prompt_count(count: usize) -> Result<(), String> {
    if count == 0 {
        return Err("prompts must be a non-empty array".to_string());
    }
    if count > EMBED_BATCH_MAX_PROMPTS {
        return Err(format!(
            "batch of {count} prompts exceeds the {EMBED_BATCH_MAX_PROMPTS}-item cap"
        ));
    }
    Ok(())
}

async fn embed_batch(
    State(state): State<SidecarState>,
    Json(req): Json<EmbedBatchRequest>,
) -> impl IntoResponse {
    if let Err(msg) = validate_batch_prompt_count(req.prompts.len()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": msg})),
        )
            .into_response();
    }

    let guard = state.embeddings.read().await;
    let Some(backend) = guard.as_ref() else {
        error!("sidecar: embedding backend not loaded");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "embedding backend not loaded"})),
        )
            .into_response();
    };

    let mut embeddings = Vec::with_capacity(req.prompts.len());
    let mut model_gen = String::new();
    for prompt in &req.prompts {
        match backend.embed(prompt) {
            Ok(output) => {
                model_gen = output.model_gen;
                embeddings.push(output.vector);
            }
            Err(err) => {
                error!(%err, "sidecar: batch embedding inference failed");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": err.to_string()})),
                )
                    .into_response();
            }
        }
    }

    Json(EmbedBatchResponse {
        embeddings,
        model_gen,
    })
    .into_response()
}

// ── /api/transcribe ───────────────────────────────────────────────────────────

/// `POST /api/transcribe` response body.
#[derive(Debug, Serialize)]
pub struct TranscribeResponse {
    pub text: String,
    /// Model generation provenance token.
    pub model_gen: String,
}

/// Accept raw WAV bytes in the request body (Content-Type: audio/wav or
/// application/octet-stream).  Returns the transcript as JSON.
async fn transcribe(State(state): State<SidecarState>, body: Bytes) -> impl IntoResponse {
    let guard = state.whisper.read().await;
    let Some(backend) = guard.as_ref() else {
        error!("sidecar: Whisper backend not loaded");
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error": "Whisper backend not loaded"})),
        )
            .into_response();
    };

    // Run in a blocking task to avoid stalling the async executor.
    let backend_ptr = backend as *const WhisperBackend as usize;
    let wav_bytes = body.to_vec();

    let result = tokio::task::spawn_blocking(move || {
        // SAFETY: the RwLock read guard is held for the duration of this block.
        let backend = unsafe { &*(backend_ptr as *const WhisperBackend) };
        backend.transcribe(&wav_bytes)
    })
    .await;

    match result {
        Ok(Ok(output)) => Json(TranscribeResponse {
            text: output.text,
            model_gen: output.model_gen,
        })
        .into_response(),
        Ok(Err(err)) => {
            error!(%err, "sidecar: Whisper transcription failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": err.to_string()})),
            )
                .into_response()
        }
        Err(panic) => {
            error!(?panic, "sidecar: Whisper blocking task panicked");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": "internal error"})),
            )
                .into_response()
        }
    }
}

// ── /api/synthesize ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SynthesizeRequest {
    pub text: String,
    pub voice: Option<String>,
    pub speed: Option<f32>,
}

async fn synthesize(
    State(state): State<SidecarState>,
    Json(req): Json<SynthesizeRequest>,
) -> impl IntoResponse {
    let guard = state.kokoro.read().await;
    let Some(backend) = guard.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Kokoro backend not loaded".to_string(),
        )
            .into_response();
    };

    let backend_ptr = backend as *const KokoroBackend as usize;
    let voice = req.voice.clone();
    let speed = req.speed;
    let text = req.text.clone();

    let result = tokio::task::spawn_blocking(move || {
        let backend = unsafe { &*(backend_ptr as *const KokoroBackend) };
        backend.synthesize(&text, voice.as_deref(), speed)
    })
    .await;

    drop(guard);

    match result {
        Ok(Ok(output)) => axum::response::Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "audio/wav")
            .header("X-Voice", output.voice)
            .header("X-Model-Gen", output.model_gen)
            .body(axum::body::Body::from(output.wav_bytes))
            .unwrap()
            .into_response(),
        Ok(Err(e)) => {
            error!(err = %e, "Kokoro synthesis failed");
            (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")).into_response()
        }
        Err(e) => {
            error!(err = %e, "Kokoro blocking task panicked");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "synthesis task panicked".to_string(),
            )
                .into_response()
        }
    }
}

// ── Router & runner ───────────────────────────────────────────────────────────

pub fn router(state: SidecarState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/embeddings", post(embed))
        .route("/api/embeddings/batch", post(embed_batch))
        .route("/api/transcribe", post(transcribe))
        .route("/api/synthesize", post(synthesize))
        .with_state(state)
}

/// Run the Ollama-compat HTTP sidecar on `addr` (e.g. `"0.0.0.0:11435"`).
pub async fn run_sidecar(
    addr: &str,
    embeddings: Arc<RwLock<Option<EmbeddingsBackend>>>,
    whisper: Arc<RwLock<Option<WhisperBackend>>>,
    kokoro: Arc<RwLock<Option<KokoroBackend>>>,
) -> Result<()> {
    let state = SidecarState::new(embeddings, whisper, kokoro);
    let app = router(state);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| anyhow::anyhow!("sidecar failed to bind {}: {}", addr, e))?;

    info!("ONNX sidecar listening on http://{} (Ollama-compat)", addr);

    axum::serve(listener, app)
        .await
        .map_err(|e| anyhow::anyhow!("sidecar error: {}", e))
}

/// Pull a fresh model into the shared backend slot.
/// Called on startup and during hot-swap.
pub fn load_embeddings_into(
    slot: &std::sync::Mutex<Option<EmbeddingsBackend>>,
    repo_id: &str,
    prefer_quantized: bool,
    max_seq_len: usize,
) -> Result<()> {
    let cache = ModelCache::new()?;
    let handle = cache.pull(repo_id, prefer_quantized)?;
    let backend = EmbeddingsBackend::load(&handle, max_seq_len)?;
    *slot.lock().unwrap() = Some(backend);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_prompt_count_rejects_empty() {
        let err = validate_batch_prompt_count(0).expect_err("empty batch must be rejected");
        assert!(err.contains("non-empty"), "{err}");
    }

    #[test]
    fn batch_prompt_count_accepts_the_cap_exactly() {
        assert!(validate_batch_prompt_count(EMBED_BATCH_MAX_PROMPTS).is_ok());
    }

    #[test]
    fn batch_prompt_count_rejects_one_over_the_cap() {
        let err = validate_batch_prompt_count(EMBED_BATCH_MAX_PROMPTS + 1)
            .expect_err("over-cap batch must be rejected");
        assert!(
            err.contains(&EMBED_BATCH_MAX_PROMPTS.to_string()),
            "cap must be named in the error: {err}"
        );
    }

    #[test]
    fn batch_prompt_count_accepts_a_single_prompt() {
        assert!(validate_batch_prompt_count(1).is_ok());
    }
}
