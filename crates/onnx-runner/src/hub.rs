use anyhow::{Context, Result};
use hf_hub::api::sync::Api;
use std::path::PathBuf;

/// A resolved local path set for a Whisper ONNX model downloaded from HF Hub.
#[derive(Debug, Clone)]
pub struct WhisperHandle {
    /// Local path to the encoder ONNX file.
    pub encoder_path: PathBuf,
    /// Local path to the decoder ONNX file (non-merged, no past KV).
    pub decoder_path: PathBuf,
    /// Local path to `tokenizer.json`.
    pub tokenizer_path: PathBuf,
    /// Provenance token: `"{repo}@{sha8}"`.
    pub model_gen: String,
}

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
    /// - `onnx/model.onnx` (fp32, ~1.2GB)
    /// - `onnx/model_q4.onnx` (q4 quant, ~197MB) - recommended for M1
    /// - `onnx/model_quantized.onnx` (legacy quantized)
    /// - `tokenizer.json`
    ///
    /// Returns a [`ModelHandle`] with local paths and a `model_gen` token.
    pub fn pull(&self, repo_id: &str, prefer_quantized: bool) -> Result<ModelHandle> {
        let repo = self.api.model(repo_id.to_string());

        // Priority: model_q4 (best for M1) > model_quantized > model_fp32
        let model_filename = if prefer_quantized {
            "onnx/model_q4.onnx"
        } else {
            "onnx/model.onnx"
        };

        let model_path = repo
            .get(model_filename)
            .or_else(|_| {
                // Try fallback variants in order of preference
                let fallbacks = if prefer_quantized {
                    vec!["onnx/model_quantized.onnx", "onnx/model.onnx"]
                } else {
                    vec!["onnx/model_q4.onnx", "onnx/model_quantized.onnx"]
                };
                for fallback in fallbacks {
                    if let Ok(path) = repo.get(fallback) {
                        return Ok(path);
                    }
                }
                Err(anyhow::anyhow!("no suitable model found"))
            })
            .with_context(|| format!("could not download ONNX model from {}", repo_id))?;

        // Also fetch the .onnx_data file if present (for split models like q4)
        let model_data_filename = model_path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| format!("onnx/{}_data", s))
            .unwrap_or_else(|| "onnx/model.onnx_data".to_string());

        if let Ok(data_path) = repo.get(&model_data_filename) {
            tracing::info!(?data_path, "downloaded companion weights file");
        }

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

    /// Download (or serve from cache) the Whisper encoder + decoder for `repo_id`.
    ///
    /// Expects the repo to follow the `onnx-community/whisper-*` layout:
    /// - `onnx/encoder_model.onnx` (or `_quantized`)
    /// - `onnx/decoder_model.onnx` (without past KV cache)
    /// - `tokenizer.json`
    ///
    /// Returns a [`WhisperHandle`] with local paths and a `model_gen` token.
    pub fn pull_whisper(&self, repo_id: &str, prefer_quantized: bool) -> Result<WhisperHandle> {
        let repo = self.api.model(repo_id.to_string());

        let encoder_primary = if prefer_quantized {
            "onnx/encoder_model_quantized.onnx"
        } else {
            "onnx/encoder_model.onnx"
        };
        let encoder_fallback = if prefer_quantized {
            "onnx/encoder_model.onnx"
        } else {
            "onnx/encoder_model_quantized.onnx"
        };

        let encoder_path = repo
            .get(encoder_primary)
            .or_else(|_| repo.get(encoder_fallback))
            .with_context(|| format!("could not download Whisper encoder from {}", repo_id))?;

        let decoder_primary = if prefer_quantized {
            "onnx/decoder_model_quantized.onnx"
        } else {
            "onnx/decoder_model.onnx"
        };
        let decoder_fallback = if prefer_quantized {
            "onnx/decoder_model.onnx"
        } else {
            "onnx/decoder_model_quantized.onnx"
        };

        let decoder_path = repo
            .get(decoder_primary)
            .or_else(|_| repo.get(decoder_fallback))
            .with_context(|| format!("could not download Whisper decoder from {}", repo_id))?;

        let tokenizer_path = repo
            .get("tokenizer.json")
            .with_context(|| format!("could not download tokenizer.json from {}", repo_id))?;

        let sha8 = encoder_path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(|s| s.chars().take(8).collect::<String>())
            .unwrap_or_else(|| "unknown".into());

        let model_gen = format!("{}@{}", repo_id, sha8);

        tracing::info!(
            repo_id,
            model_gen,
            ?encoder_path,
            ?decoder_path,
            "whisper model resolved from hub cache"
        );

        Ok(WhisperHandle {
            encoder_path,
            decoder_path,
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

// ── Kokoro ────────────────────────────────────────────────────────────────────

/// Resolved local paths for a Kokoro ONNX TTS model downloaded from HF Hub.
#[derive(Debug, Clone)]
pub struct KokoroHandle {
    /// Local path to the Kokoro ONNX model file.
    pub model_path: PathBuf,
    /// Local path to `config.json` (contains the phoneme vocab map).
    pub config_path: PathBuf,
    /// Local directory containing `{voice}.bin` style embedding files.
    pub voices_dir: PathBuf,
    /// Provenance token: `"{repo}@{sha8}"`.
    pub model_gen: String,
}

impl ModelCache {
    /// Download (or serve from cache) the Kokoro-82M ONNX model.
    ///
    /// Downloads:
    /// - `onnx/model_quantized.onnx` (int8, ~92 MB) or `onnx/model.onnx`
    /// - `config.json` (phoneme vocabulary)
    /// - `voices/{voice}.bin` for each name in `voices_to_prefetch`
    pub fn pull_kokoro(
        &self,
        repo_id: &str,
        prefer_quantized: bool,
        voices_to_prefetch: &[&str],
    ) -> Result<KokoroHandle> {
        let repo = self.api.model(repo_id.to_string());

        // ── Model ────────────────────────────────────────────────────────────
        let model_path = if prefer_quantized {
            repo.get("onnx/model_quantized.onnx")
                .or_else(|_| repo.get("onnx/model.onnx"))
        } else {
            repo.get("onnx/model.onnx")
                .or_else(|_| repo.get("onnx/model_quantized.onnx"))
        }
        .with_context(|| format!("download Kokoro model from {repo_id}"))?;

        // ── Config (vocab) ───────────────────────────────────────────────────
        let config_path = repo
            .get("config.json")
            .with_context(|| format!("download config.json from {repo_id}"))?;

        // ── Voices ───────────────────────────────────────────────────────────
        // Download the requested voices; derive the voices_dir from the first hit.
        let mut voices_dir: Option<PathBuf> = None;
        for voice in voices_to_prefetch {
            let filename = format!("voices/{voice}.bin");
            match repo.get(&filename) {
                Ok(p) => {
                    if voices_dir.is_none() {
                        voices_dir = p.parent().map(|d| d.to_path_buf());
                    }
                    tracing::info!(voice, "Kokoro voice cached");
                }
                Err(e) => tracing::warn!(voice, err = %e, "could not prefetch Kokoro voice"),
            }
        }

        let voices_dir = voices_dir.with_context(|| {
            format!("none of the requested voices could be downloaded from {repo_id}")
        })?;

        let sha8 = model_path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .map(|s| s.chars().take(8).collect::<String>())
            .unwrap_or_else(|| "unknown".into());

        let model_gen = format!("{repo_id}@{sha8}");

        tracing::info!(
            repo_id,
            model_gen,
            ?model_path,
            ?voices_dir,
            "Kokoro model resolved from hub cache"
        );

        Ok(KokoroHandle {
            model_path,
            config_path,
            voices_dir,
            model_gen,
        })
    }
}
