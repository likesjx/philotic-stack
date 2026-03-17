use anyhow::{Context, Result};
use hf_hub::api::sync::Api;
use std::path::PathBuf;

/// A resolved local path to a downloaded model file, with provenance.
#[derive(Debug, Clone)]
pub struct ModelHandle {
    /// The local filesystem path to the ONNX model file.
    pub model_path: PathBuf,
    /// The local filesystem path to the tokenizer.json file.
    pub tokenizer_path: PathBuf,
    /// Provenance token: `"{repo}@{sha8}"` — identifies the embedding space.
    pub model_gen: String,
}

/// Manages a local cache of HuggingFace Hub models.
pub struct ModelCache {
    api: Api,
}

impl ModelCache {
    /// Create a new cache backed by the default HF Hub cache directory.
    pub fn new() -> Result<Self> {
        Ok(Self {
            api: Api::new().context("failed to initialise HuggingFace Hub API")?,
        })
    }

    /// Download (or serve from cache) the ONNX model and tokenizer for `repo_id`.
    ///
    /// Expects the repo to contain:
    /// - `onnx/model.onnx` or `onnx/model_quantized.onnx`
    /// - `tokenizer.json`
    ///
    /// Returns a [`ModelHandle`] with local paths and a `model_gen` token.
    pub fn pull(&self, repo_id: &str, prefer_quantized: bool) -> Result<ModelHandle> {
        let repo = self.api.model(repo_id.to_string());

        let model_filename = if prefer_quantized {
            "onnx/model_quantized.onnx"
        } else {
            "onnx/model.onnx"
        };

        let model_path = repo
            .get(model_filename)
            .or_else(|_| {
                // Fall back to the other variant if the preferred one is missing.
                let fallback = if prefer_quantized {
                    "onnx/model.onnx"
                } else {
                    "onnx/model_quantized.onnx"
                };
                repo.get(fallback)
            })
            .with_context(|| format!("could not download ONNX model from {}", repo_id))?;

        let tokenizer_path = repo
            .get("tokenizer.json")
            .with_context(|| format!("could not download tokenizer.json from {}", repo_id))?;

        // Derive model_gen from the repo id and the last 8 chars of the local path hash.
        // A proper implementation would use the HF Hub commit SHA; for Slice 1 we use
        // the filename stem of the resolved cache path as a stable proxy.
        let sha8 = model_path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(|s| s.chars().take(8).collect::<String>())
            .unwrap_or_else(|| "unknown".into());

        let model_gen = format!("{}@{}", repo_id, sha8);

        tracing::info!(
            repo_id,
            model_gen,
            ?model_path,
            "onnx model resolved from hub cache"
        );

        Ok(ModelHandle {
            model_path,
            tokenizer_path,
            model_gen,
        })
    }
}

impl Default for ModelCache {
    fn default() -> Self {
        Self::new().expect("failed to initialise ModelCache")
    }
}
