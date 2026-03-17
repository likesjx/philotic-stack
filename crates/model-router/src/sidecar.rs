/// Ollama-compatible HTTP sidecar for the ONNX provider.
///
/// Exposes embedding inference directly over HTTP so clients that speak the
/// Ollama API (Muninn, vector stores, tooling) can use local ONNX models
/// without going through the IPC routing plane.
///
/// Port: 11435 (leaves Ollama's 11434 free for optional proxy fallback).
use anyhow::Result;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::post,
};
use onnx_runner::{EmbeddingsBackend, ModelCache};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{error, info};

#[derive(Clone)]
pub struct SidecarState {
    embeddings: Arc<RwLock<Option<EmbeddingsBackend>>>,
}

impl SidecarState {
    pub fn new(embeddings: Arc<RwLock<Option<EmbeddingsBackend>>>) -> Self {
        Self { embeddings }
    }
}

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

pub fn router(state: SidecarState) -> Router {
    Router::new()
        .route("/api/embeddings", post(embed))
        .with_state(state)
}

/// Run the Ollama-compat HTTP sidecar on `addr` (e.g. `"0.0.0.0:11435"`).
pub async fn run_sidecar(
    addr: &str,
    embeddings: Arc<RwLock<Option<EmbeddingsBackend>>>,
) -> Result<()> {
    let state = SidecarState::new(embeddings);
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
