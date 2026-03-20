use crate::controller::{ControllerTask, ModelProvider, ProviderOutput, TaskKind};
use anyhow::{Context, Result};
use async_trait::async_trait;
use onnx_runner::{EmbeddingsBackend, EmbeddingsConfig, ModelCache};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

/// Configuration for the OnnxProvider.
#[derive(Debug, Clone)]
pub struct OnnxProviderConfig {
    pub embeddings: EmbeddingsConfig,
    /// Prefer quantized ONNX variants when available.
    pub prefer_quantized: bool,
}

impl Default for OnnxProviderConfig {
    fn default() -> Self {
        Self {
            embeddings: EmbeddingsConfig::default(),
            prefer_quantized: true,
        }
    }
}

/// ModelProvider implementation backed by local ONNX inference via `onnx-runner`.
///
/// Supports `TaskKind::Embed` in Slice 1. `TextGenerate` and `AudioTranscribe`
/// support will be added in Slices 3 and 2 respectively.
pub struct OnnxProvider {
    embeddings: Arc<RwLock<Option<EmbeddingsBackend>>>,
    config: OnnxProviderConfig,
}

impl OnnxProvider {
    /// Create an OnnxProvider and load the embedding model from HF Hub.
    pub fn load(config: OnnxProviderConfig) -> Result<Self> {
        let cache = ModelCache::new().context("failed to initialise HF Hub model cache")?;
        let handle = cache
            .pull(&config.embeddings.repo_id, config.prefer_quantized)
            .with_context(|| {
                format!(
                    "failed to pull embedding model {}",
                    config.embeddings.repo_id
                )
            })?;

        let backend = EmbeddingsBackend::load(&handle, config.embeddings.max_seq_len)
            .context("failed to load EmbeddingsBackend")?;

        info!(
            model_gen = %backend.model_gen(),
            "OnnxProvider embedding backend loaded"
        );

        Ok(Self {
            embeddings: Arc::new(RwLock::new(Some(backend))),
            config,
        })
    }

    /// Returns the shared embedding backend handle so the HTTP sidecar can
    /// serve the same backend instance without duplication.
    pub fn shared_embeddings(&self) -> Arc<RwLock<Option<EmbeddingsBackend>>> {
        Arc::clone(&self.embeddings)
    }

    /// Hot-swap the embedding model. Downloads the new revision and replaces
    /// the backend atomically. Emitting `model.swapped` is the caller's responsibility.
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
        task.kind == TaskKind::Embed
    }

    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        match task.kind {
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
            other => anyhow::bail!(
                "OnnxProvider does not support task kind [{}] in this slice",
                other.as_str()
            ),
        }
    }
}
