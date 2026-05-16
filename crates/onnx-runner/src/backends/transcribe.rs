use crate::audio::{decode_wav, log_mel_spectrogram, N_FRAMES, N_MELS};
use crate::hub::WhisperHandle;
use anyhow::{Context, Result};
use ndarray::{Array2, Array3};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use std::sync::{Arc, Mutex};
use tokenizers::Tokenizer;

// ── Whisper special token IDs ─────────────────────────────────────────────────
// These are stable across all Whisper model sizes.
const SOT: i64 = 50258; // <|startoftranscript|>
const EOT: i64 = 50257; // <|endoftext|>
const LANG_EN: i64 = 50259; // <|en|>
const TASK_TRANSCRIBE: i64 = 50358; // <|transcribe|>
const NO_TIMESTAMPS: i64 = 50363; // <|notimestamps|>

// Number of prompt tokens prepended before the first generated token.
const PROMPT_LEN: usize = 4;

// ── Config ────────────────────────────────────────────────────────────────────

/// Configuration for the `WhisperBackend`.
#[derive(Debug, Clone)]
pub struct TranscribeConfig {
    /// HuggingFace repo id, e.g. `"onnx-community/whisper-small"`.
    pub repo_id: String,
    /// Prefer quantized ONNX variants when available.
    pub prefer_quantized: bool,
    /// Maximum tokens to generate per transcription call.
    pub max_new_tokens: usize,
}

impl Default for TranscribeConfig {
    fn default() -> Self {
        Self {
            repo_id: "onnx-community/whisper-small".into(),
            prefer_quantized: true,
            max_new_tokens: 448,
        }
    }
}

// ── Output ────────────────────────────────────────────────────────────────────

/// Output from a single `WhisperBackend::transcribe` call.
#[derive(Debug, Clone)]
pub struct TranscribeOutput {
    /// Decoded transcript text (special tokens stripped).
    pub text: String,
    /// Model generation token — `"{repo}@{sha8}"`.
    pub model_gen: String,
}

// ── Backend ───────────────────────────────────────────────────────────────────

/// Local ONNX Whisper backend.
///
/// Runs the Whisper encoder once per audio chunk, then greedily decodes with
/// the non-merged decoder (`decoder_model.onnx`).  Each decode step runs a
/// full forward pass over the growing token sequence — no KV-cache is used in
/// this slice. This is correct but O(n²); KV-cache optimisation is deferred.
pub struct WhisperBackend {
    encoder: Arc<Mutex<Session>>,
    decoder: Arc<Mutex<Session>>,
    tokenizer: Arc<Tokenizer>,
    model_gen: String,
    max_new_tokens: usize,
}

impl WhisperBackend {
    /// Load the backend from a resolved [`WhisperHandle`].
    pub fn load(handle: &WhisperHandle, config: &TranscribeConfig) -> Result<Self> {
        let encoder = Session::builder()
            .map_err(|e| anyhow::anyhow!("failed to create encoder session builder: {}", e))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!("failed to set encoder opt level: {}", e))?
            .commit_from_file(&handle.encoder_path)
            .with_context(|| {
                format!(
                    "failed to load Whisper encoder from {}",
                    handle.encoder_path.display()
                )
            })?;

        let decoder = Session::builder()
            .map_err(|e| anyhow::anyhow!("failed to create decoder session builder: {}", e))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!("failed to set decoder opt level: {}", e))?
            .commit_from_file(&handle.decoder_path)
            .with_context(|| {
                format!(
                    "failed to load Whisper decoder from {}",
                    handle.decoder_path.display()
                )
            })?;

        let tokenizer = Tokenizer::from_file(&handle.tokenizer_path)
            .map_err(|e| anyhow::anyhow!("failed to load Whisper tokenizer: {}", e))?;

        tracing::info!(
            model_gen = %handle.model_gen,
            encoder = %handle.encoder_path.display(),
            decoder = %handle.decoder_path.display(),
            "WhisperBackend loaded"
        );

        Ok(Self {
            encoder: Arc::new(Mutex::new(encoder)),
            decoder: Arc::new(Mutex::new(decoder)),
            tokenizer: Arc::new(tokenizer),
            model_gen: handle.model_gen.clone(),
            max_new_tokens: config.max_new_tokens,
        })
    }

    /// Transcribe raw WAV bytes. Returns a [`TranscribeOutput`] with the text
    /// and provenance token.
    pub fn transcribe(&self, wav_bytes: &[u8]) -> Result<TranscribeOutput> {
        // Step 1 — Decode WAV → PCM.
        let pcm = decode_wav(wav_bytes)?;

        // Step 2 — Compute log-mel spectrogram → [1, N_MELS, N_FRAMES].
        let mel_flat = log_mel_spectrogram(&pcm);
        let mel_arr = Array3::from_shape_vec((1, N_MELS, N_FRAMES), mel_flat)
            .context("failed to shape mel spectrogram array")?;
        let mel_tensor = Tensor::<f32>::from_array(mel_arr)
            .map_err(|e| anyhow::anyhow!("failed to create mel tensor: {}", e))?;

        // Step 3 — Run encoder once.
        let (enc_data, enc_seq, enc_hidden) = {
            let mut enc = self
                .encoder
                .lock()
                .map_err(|_| anyhow::anyhow!("encoder session mutex poisoned"))?;
            let enc_out = enc
                .run(ort::inputs!["input_features" => mel_tensor])
                .map_err(|e| anyhow::anyhow!("encoder run failed: {}", e))?;
            let (shape, data) = enc_out[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow::anyhow!("failed to extract encoder output: {}", e))?;
            // shape: [1, enc_seq, enc_hidden] e.g. [1, 1500, 384] for whisper-small
            let dims: &[i64] = &shape;
            anyhow::ensure!(
                dims.len() == 3,
                "expected 3D encoder output [1, enc_seq, hidden], got {}D",
                dims.len()
            );
            (data.to_vec(), dims[1] as usize, dims[2] as usize)
        };

        // Step 4 — Greedy decode.
        // Initial prompt: [SOT, <lang>, <task>, <no_timestamps>]
        let mut token_ids: Vec<i64> = vec![SOT, LANG_EN, TASK_TRANSCRIBE, NO_TIMESTAMPS];

        for _ in 0..self.max_new_tokens {
            let seq_len = token_ids.len();

            let ids_arr = Array2::from_shape_vec((1, seq_len), token_ids.clone())
                .context("failed to shape input_ids")?;
            let ids_tensor = Tensor::<i64>::from_array(ids_arr)
                .map_err(|e| anyhow::anyhow!("failed to create input_ids tensor: {}", e))?;

            // Rebuild encoder hidden states tensor each step.
            // TODO(slice3): eliminate this per-step clone with KV-cache.
            let enc_arr = Array3::from_shape_vec((1, enc_seq, enc_hidden), enc_data.clone())
                .context("failed to shape encoder_hidden_states")?;
            let enc_tensor = Tensor::<f32>::from_array(enc_arr).map_err(|e| {
                anyhow::anyhow!("failed to create encoder_hidden_states tensor: {}", e)
            })?;

            let logits_vec = {
                let mut dec = self
                    .decoder
                    .lock()
                    .map_err(|_| anyhow::anyhow!("decoder session mutex poisoned"))?;
                let dec_out = dec
                    .run(ort::inputs![
                        "input_ids" => ids_tensor,
                        "encoder_hidden_states" => enc_tensor,
                    ])
                    .map_err(|e| anyhow::anyhow!("decoder run failed: {}", e))?;
                let (shape, data) = dec_out[0]
                    .try_extract_tensor::<f32>()
                    .map_err(|e| anyhow::anyhow!("failed to extract logits: {}", e))?;
                let dims: &[i64] = &shape;
                let vocab = dims[2] as usize;
                // Take logits at the last sequence position.
                let offset = (seq_len - 1) * vocab;
                data[offset..offset + vocab].to_vec()
            };

            // Argmax → next token id.
            let next_token = logits_vec
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(i, _)| i as i64)
                .unwrap_or(EOT);

            if next_token == EOT {
                break;
            }
            token_ids.push(next_token);
        }

        // Step 5 — Decode generated token ids to text (skip the prompt prefix).
        let generated_ids: Vec<u32> = token_ids[PROMPT_LEN..].iter().map(|&t| t as u32).collect();

        let text = self
            .tokenizer
            .decode(&generated_ids, /* skip_special_tokens */ true)
            .map_err(|e| anyhow::anyhow!("tokenizer decode failed: {}", e))?;

        tracing::debug!(
            model_gen = %self.model_gen,
            tokens = generated_ids.len(),
            "WhisperBackend transcription complete"
        );

        Ok(TranscribeOutput {
            text: text.trim().to_string(),
            model_gen: self.model_gen.clone(),
        })
    }

    /// The model generation token for the currently loaded model.
    pub fn model_gen(&self) -> &str {
        &self.model_gen
    }
}
