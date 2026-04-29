use crate::controller::{AudioArtifact, ControllerTask, ModelProvider, ProviderOutput, TaskKind};
use anyhow::{Context, Result};
use async_trait::async_trait;
use onnx_runner::{
    EmbeddingsBackend, EmbeddingsConfig, KokoroBackend, KokoroConfig, ModelCache, TranscribeConfig,
    WhisperBackend,
};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

/// Configuration for the OnnxProvider.
#[derive(Debug, Clone)]
pub struct OnnxProviderConfig {
    pub embeddings: EmbeddingsConfig,
    pub transcribe: TranscribeConfig,
    pub kokoro: KokoroConfig,
    /// Prefer quantized ONNX variants when available.
    pub prefer_quantized: bool,
}

impl Default for OnnxProviderConfig {
    fn default() -> Self {
        Self {
            embeddings: EmbeddingsConfig::default(),
            transcribe: TranscribeConfig::default(),
            kokoro: KokoroConfig::default(),
            prefer_quantized: true,
        }
    }
}

/// ModelProvider implementation backed by local ONNX inference via `onnx-runner`.
///
/// Supports:
/// - `TaskKind::Embed`            (embeddings)
/// - `TaskKind::AudioTranscribe`  (Whisper)
/// - `TaskKind::VoiceSynthesize`  (Kokoro TTS)
pub struct OnnxProvider {
    embeddings: Arc<RwLock<Option<EmbeddingsBackend>>>,
    whisper: Arc<RwLock<Option<WhisperBackend>>>,
    kokoro: Arc<RwLock<Option<KokoroBackend>>>,
    config: OnnxProviderConfig,
    http_client: reqwest::Client,
}

impl OnnxProvider {
    /// Create an OnnxProvider and eagerly load both the embedding and Whisper
    /// models from HF Hub.
    pub fn load(config: OnnxProviderConfig) -> Result<Self> {
        let cache = ModelCache::new().context("failed to initialise HF Hub model cache")?;

        // ── Embeddings ──────────────────────────────────────────────────────
        let emb_backend = cache
            .pull(&config.embeddings.repo_id, config.prefer_quantized)
            .and_then(|h| EmbeddingsBackend::load(&h, config.embeddings.max_seq_len))
            .map(|b| { info!(model_gen = %b.model_gen(), "OnnxProvider embeddings loaded"); b })
            .map_err(|e| tracing::warn!(err = %e, "EmbeddingsBackend failed to load — Embed will be unavailable"))
            .ok();

        // ── Whisper ─────────────────────────────────────────────────────────
        let whisper_backend = cache
            .pull_whisper(&config.transcribe.repo_id, config.prefer_quantized)
            .and_then(|h| WhisperBackend::load(&h, &config.transcribe))
            .map(|b| { info!(model_gen = %b.model_gen(), "OnnxProvider Whisper loaded"); b })
            .map_err(|e| tracing::warn!(err = %e, "WhisperBackend failed to load — AudioTranscribe will be unavailable"))
            .ok();

        // ── Kokoro TTS ──────────────────────────────────────────────────────
        let kokoro_voices: Vec<&str> = {
            let mut v = vec![config.kokoro.default_voice.as_str()];
            v.extend(config.kokoro.extra_voices.iter().map(|s| s.as_str()));
            v
        };
        let kokoro_backend = match cache.pull_kokoro(
            &config.kokoro.repo_id,
            config.prefer_quantized,
            &kokoro_voices,
        ) {
            Ok(handle) => match KokoroBackend::load(&handle, &config.kokoro) {
                Ok(b) => {
                    info!(model_gen = %b.model_gen(), "OnnxProvider Kokoro TTS loaded");
                    Some(b)
                }
                Err(e) => {
                    tracing::warn!(err = %e, "KokoroBackend failed to load — VoiceSynthesize will be unavailable");
                    None
                }
            },
            Err(e) => {
                tracing::warn!(err = %e, "Kokoro model pull failed — VoiceSynthesize will be unavailable");
                None
            }
        };

        Ok(Self {
            embeddings: Arc::new(RwLock::new(emb_backend)),
            whisper: Arc::new(RwLock::new(whisper_backend)),
            kokoro: Arc::new(RwLock::new(kokoro_backend)),
            config,
            http_client: reqwest::Client::new(),
        })
    }

    /// Returns the shared embedding backend handle so the HTTP sidecar can
    /// serve the same backend instance without duplication.
    pub fn shared_embeddings(&self) -> Arc<RwLock<Option<EmbeddingsBackend>>> {
        Arc::clone(&self.embeddings)
    }

    /// Returns the shared Whisper backend handle for the HTTP sidecar.
    pub fn shared_whisper(&self) -> Arc<RwLock<Option<WhisperBackend>>> {
        Arc::clone(&self.whisper)
    }

    /// Returns the shared Kokoro backend handle.
    pub fn shared_kokoro(&self) -> Arc<RwLock<Option<KokoroBackend>>> {
        Arc::clone(&self.kokoro)
    }

    /// Hot-swap the embedding model. Downloads the new revision and replaces
    /// the backend atomically.
    pub async fn swap_embeddings(&self, repo_id: &str) -> Result<String> {
        let cache = ModelCache::new()?;
        let handle = cache.pull(repo_id, self.config.prefer_quantized)?;
        let new_backend = EmbeddingsBackend::load(&handle, self.config.embeddings.max_seq_len)?;
        let model_gen = new_backend.model_gen().to_string();
        *self.embeddings.write().await = Some(new_backend);
        info!(%model_gen, "embedding model hot-swapped");
        Ok(model_gen)
    }
}

#[async_trait]
impl ModelProvider for OnnxProvider {
    fn id(&self) -> &'static str {
        "onnx"
    }

    fn supports(&self, task: &ControllerTask) -> bool {
        matches!(
            task.kind,
            TaskKind::Embed | TaskKind::AudioTranscribe | TaskKind::VoiceSynthesize
        )
    }

    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        match task.kind {
            // ── Embed ───────────────────────────────────────────────────────
            TaskKind::Embed => {
                let text = task
                    .composed_prompt_text()
                    .context("text.embed task missing input text")?;

                let guard = self.embeddings.read().await;
                let backend = guard.as_ref().context("embedding backend not loaded")?;

                let output = backend
                    .embed(&text)
                    .context("ONNX embedding inference failed")?;

                Ok(ProviderOutput::Embedding {
                    vector: output.vector,
                    model_gen: output.model_gen,
                })
            }

            // ── AudioTranscribe ─────────────────────────────────────────────
            TaskKind::AudioTranscribe => {
                // Fetch the first audio attachment from blob storage.
                let attachment = task
                    .media_attachments()
                    .into_iter()
                    .find(|a| {
                        a.url
                            .as_deref()
                            .map(|u| !u.trim().is_empty())
                            .unwrap_or(false)
                    })
                    .context("audio.transcribe task has no media attachment with a download URL")?;

                let url = attachment
                    .url
                    .as_deref()
                    .context("attachment missing url")?;

                let response = self
                    .http_client
                    .get(url)
                    .send()
                    .await
                    .with_context(|| format!("failed to fetch audio from {url}"))?;

                let status = response.status();
                if !status.is_success() {
                    let body = response.text().await.unwrap_or_default();
                    anyhow::bail!(
                        "failed to fetch audio attachment from {url}: HTTP {status} {body}"
                    );
                }

                let wav_bytes = response
                    .bytes()
                    .await
                    .context("failed to read audio response body")?;

                // Run Whisper in a blocking task to avoid blocking the async executor.
                let guard = self.whisper.read().await;
                let backend = guard.as_ref().context("Whisper backend not loaded")?;

                // SAFETY: backend lives inside Arc<RwLock<…>> so the pointer is
                // stable for the duration of this call.  We convert to a raw ptr
                // so the closure can cross the `spawn_blocking` boundary without
                // forcing `Send` on the backend itself.
                let backend_ptr = backend as *const WhisperBackend as usize;
                let wav_bytes_owned = wav_bytes.to_vec();

                let output = tokio::task::spawn_blocking(move || {
                    // SAFETY: the RwLock read guard is held for the full duration
                    // of this block, so the backend is alive.
                    let backend = unsafe { &*(backend_ptr as *const WhisperBackend) };
                    backend.transcribe(&wav_bytes_owned)
                })
                .await
                .context("Whisper blocking task panicked")??;

                Ok(ProviderOutput::Text {
                    display_text: Some(output.text.clone()),
                    content: output.text,
                    spoken_text: None,
                    partial_replies: Vec::new(),
                    working_memory_delta: None,
                    follow_up_questions: Vec::new(),
                    intent_summary: None,
                    memory_concept: None,
                    memory_candidate: None,
                    active_plan: None,
                    model_gen: Some(output.model_gen),
                })
            }

            // ── VoiceSynthesize ─────────────────────────────────────────────
            TaskKind::VoiceSynthesize => {
                let text = task
                    .voice_text()
                    .map(|s| s.to_string())
                    .or_else(|| task.composed_prompt_text())
                    .context("voice.synthesize task missing text")?;

                let voice = task.requested_voice().map(|s| s.to_string());
                let speed = task
                    .provider_options
                    .get("speed")
                    .and_then(|v| v.as_f64())
                    .map(|s| s as f32);

                let guard = self.kokoro.read().await;
                let backend = guard
                    .as_ref()
                    .context("Kokoro backend not loaded — is espeak-ng installed and the model downloaded?")?;

                // Run synthesis in a blocking thread (CPU-bound ONNX + subprocess).
                let backend_ptr = backend as *const KokoroBackend as usize;
                let output = tokio::task::spawn_blocking(move || {
                    // SAFETY: the RwLock read guard is held for the full duration
                    // of the spawn_blocking call, keeping the backend alive.
                    let backend = unsafe { &*(backend_ptr as *const KokoroBackend) };
                    backend.synthesize(&text, voice.as_deref(), speed)
                })
                .await
                .context("Kokoro blocking task panicked")??;

                Ok(ProviderOutput::Audio(AudioArtifact {
                    provider: "onnx/kokoro".into(),
                    model: "kokoro-82m".into(),
                    voice_id: output.voice,
                    mime_type: "audio/wav".into(),
                    output_format: "wav".into(),
                    audio_bytes: output.wav_bytes,
                }))
            }

            other => anyhow::bail!(
                "OnnxProvider does not support task kind [{}]",
                other.as_str()
            ),
        }
    }
}
