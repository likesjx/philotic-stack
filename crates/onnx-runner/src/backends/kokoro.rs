//! Local Kokoro-82M TTS backend via ONNX Runtime.
//!
//! Pipeline:
//!   text → espeak-ng IPA phonemes → token IDs (Kokoro vocab)
//!        → [ONNX] → f32 waveform (24 kHz) → WAV bytes
//!
//! Requires `espeak-ng` to be installed on the system:
//!   macOS: `brew install espeak`
//!   Linux: `apt install espeak-ng`
//!
//! Default model: `onnx-community/Kokoro-82M-v1.0-ONNX` (int8 quantised, ~92 MB)

use crate::hub::KokoroHandle;
use anyhow::{bail, Context, Result};
use hound::{SampleFormat, WavSpec, WavWriter};
use ndarray::{Array1, Array2};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use std::collections::HashMap;
use std::io::Cursor;
#[allow(unused_imports)]
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

// ── Kokoro phoneme vocabulary ─────────────────────────────────────────────────
// Stable mapping from Kokoro-82M config.json (hexgrad/Kokoro-82M).
// Embedded as a constant so we don't need to download config.json separately.
static KOKORO_VOCAB: OnceLock<HashMap<String, i64>> = OnceLock::new();

fn vocab() -> &'static HashMap<String, i64> {
    KOKORO_VOCAB.get_or_init(|| {
        [
            (";", 1i64),
            (":", 2),
            (",", 3),
            (".", 4),
            ("!", 5),
            ("?", 6),
            ("—", 9),
            ("…", 10),
            ("\"", 11),
            ("(", 12),
            (")", 13),
            ("\u{201C}", 14),
            ("\u{201D}", 15),
            (" ", 16),
            ("\u{0303}", 17),
            ("ʣ", 18),
            ("ʥ", 19),
            ("ʦ", 20),
            ("ʨ", 21),
            ("ᵝ", 22),
            ("\u{AB67}", 23),
            ("A", 24),
            ("I", 25),
            ("O", 31),
            ("Q", 33),
            ("S", 35),
            ("T", 36),
            ("W", 39),
            ("Y", 41),
            ("ᵊ", 42),
            ("a", 43),
            ("b", 44),
            ("c", 45),
            ("d", 46),
            ("e", 47),
            ("f", 48),
            ("h", 50),
            ("i", 51),
            ("j", 52),
            ("k", 53),
            ("l", 54),
            ("m", 55),
            ("n", 56),
            ("o", 57),
            ("p", 58),
            ("q", 59),
            ("r", 60),
            ("s", 61),
            ("t", 62),
            ("u", 63),
            ("v", 64),
            ("w", 65),
            ("x", 66),
            ("y", 67),
            ("z", 68),
            ("ɑ", 69),
            ("ɐ", 70),
            ("ɒ", 71),
            ("æ", 72),
            ("β", 75),
            ("ɔ", 76),
            ("ɕ", 77),
            ("ç", 78),
            ("ɖ", 80),
            ("ð", 81),
            ("ʤ", 82),
            ("ə", 83),
            ("ɚ", 85),
            ("ɛ", 86),
            ("ɜ", 87),
            ("ɟ", 90),
            ("ɡ", 92),
            ("ɥ", 99),
            ("ɨ", 101),
            ("ɪ", 102),
            ("ʝ", 103),
            ("ɯ", 110),
            ("ɰ", 111),
            ("ŋ", 112),
            ("ɳ", 113),
            ("ɲ", 114),
            ("ɴ", 115),
            ("ø", 116),
            ("ɸ", 118),
            ("θ", 119),
            ("œ", 120),
            ("ɹ", 123),
            ("ɾ", 125),
            ("ɻ", 126),
            ("ʁ", 128),
            ("ɽ", 129),
            ("ʂ", 130),
            ("ʃ", 131),
            ("ʈ", 132),
            ("ʧ", 133),
            ("ʊ", 135),
            ("ʋ", 136),
            ("ʌ", 138),
            ("ɣ", 139),
            ("ɤ", 140),
            ("χ", 142),
            ("ʎ", 143),
            ("ʒ", 147),
            ("ʔ", 148),
            ("ˈ", 156),
            ("ˌ", 157),
            ("ː", 158),
            ("ʰ", 162),
            ("ʲ", 164),
            ("↓", 169),
            ("→", 171),
            ("↗", 172),
            ("↘", 173),
            ("ᵻ", 177),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect()
    })
}

// ── Config ────────────────────────────────────────────────────────────────────

/// Configuration for [`KokoroBackend`].
#[derive(Debug, Clone)]
pub struct KokoroConfig {
    /// HuggingFace repo id for the Kokoro ONNX model.
    pub repo_id: String,
    /// Default voice name (e.g. `"af_heart"`). Must have a matching `{voice}.bin`
    /// in the voices directory.
    pub default_voice: String,
    /// Speech rate multiplier (1.0 = normal).
    pub default_speed: f32,
    /// Prefer the quantised model variant.
    pub prefer_quantized: bool,
    /// Voices to download at startup in addition to `default_voice`.
    pub extra_voices: Vec<String>,
}

impl Default for KokoroConfig {
    fn default() -> Self {
        Self {
            repo_id: "onnx-community/Kokoro-82M-v1.0-ONNX".into(),
            default_voice: "af_heart".into(),
            default_speed: 1.0,
            prefer_quantized: true,
            extra_voices: vec!["af".into(), "bm_george".into(), "bf_emma".into()],
        }
    }
}

// ── Output ────────────────────────────────────────────────────────────────────

/// Result of a single [`KokoroBackend::synthesize`] call.
#[derive(Debug)]
pub struct SynthesizeOutput {
    /// Raw WAV bytes (PCM f32, mono, 24 kHz).
    pub wav_bytes: Vec<u8>,
    /// Always 24 000 Hz.
    pub sample_rate: u32,
    /// Provenance token for the model.
    pub model_gen: String,
    /// Voice name actually used.
    pub voice: String,
}

// ── Backend ───────────────────────────────────────────────────────────────────

/// Local Kokoro ONNX TTS backend.
pub struct KokoroBackend {
    session: Arc<Mutex<Session>>,
    voices_dir: PathBuf,
    default_voice: String,
    default_speed: f32,
    model_gen: String,
}

impl KokoroBackend {
    /// Load the backend from a resolved [`KokoroHandle`].
    pub fn load(handle: &KokoroHandle, config: &KokoroConfig) -> Result<Self> {
        let session = Session::builder()
            .map_err(|e| anyhow::anyhow!("Kokoro session builder: {e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| anyhow::anyhow!("Kokoro opt level: {e}"))?
            .commit_from_file(&handle.model_path)
            .with_context(|| format!("load Kokoro ONNX from {}", handle.model_path.display()))?;

        tracing::info!(
            model_gen = %handle.model_gen,
            vocab_size = vocab().len(),
            voices_dir = %handle.voices_dir.display(),
            "KokoroBackend loaded"
        );

        Ok(Self {
            session: Arc::new(Mutex::new(session)),
            voices_dir: handle.voices_dir.clone(),
            default_voice: config.default_voice.clone(),
            default_speed: config.default_speed,
            model_gen: handle.model_gen.clone(),
        })
    }

    /// Synthesise `text` with the given `voice` (falls back to default) and
    /// `speed` (falls back to 1.0).  Blocks the calling thread.
    pub fn synthesize(
        &self,
        text: &str,
        voice: Option<&str>,
        speed: Option<f32>,
    ) -> Result<SynthesizeOutput> {
        let voice_name = voice.unwrap_or(&self.default_voice);
        let speed_val = speed.unwrap_or(self.default_speed);

        // ── Step 1: text → IPA via espeak-ng ─────────────────────────────────
        let ipa = phonemize_espeak(text)?;
        tracing::debug!(text, ipa, "Kokoro IPA phonemes");

        // ── Step 2: IPA → token ID sequence ──────────────────────────────────
        let vocab = vocab();
        let mut tokens: Vec<i64> = vec![0]; // leading pad
        for ch in ipa.chars() {
            let key = ch.to_string();
            if let Some(&id) = vocab.get(&key) {
                tokens.push(id);
            }
            // Unknown characters (e.g. unsupported diacritics) are silently skipped.
        }
        tokens.push(0); // trailing pad

        // Kokoro ONNX model has a hard max of 512 tokens (encoder Expand node).
        // Truncate to 511 content tokens + trailing pad to stay within the limit.
        const MAX_TOKENS: usize = 512;
        if tokens.len() > MAX_TOKENS {
            tracing::warn!(
                n_tokens = tokens.len(),
                max = MAX_TOKENS,
                "Kokoro input exceeds model capacity; truncating"
            );
            tokens.truncate(MAX_TOKENS - 1);
            tokens.push(0); // re-add trailing pad
        }

        let n_tokens = tokens.len();
        if n_tokens <= 2 {
            bail!(
                "Kokoro phonemizer produced no valid tokens for: {:?} (IPA: {:?})",
                text,
                ipa
            );
        }

        // ── Step 3: Load voice style embedding ───────────────────────────────
        let style = self
            .load_style(voice_name, n_tokens)
            .with_context(|| format!("load Kokoro style for voice '{voice_name}'"))?;

        // ── Step 4: Build ONNX input tensors ─────────────────────────────────
        let ids_arr =
            Array2::from_shape_vec((1, n_tokens), tokens).context("shape Kokoro input_ids")?;
        let ids_tensor = Tensor::<i64>::from_array(ids_arr)
            .map_err(|e| anyhow::anyhow!("Kokoro input_ids tensor: {e}"))?;

        let style_arr = Array2::from_shape_vec((1, 256), style).context("shape Kokoro style")?;
        let style_tensor = Tensor::<f32>::from_array(style_arr)
            .map_err(|e| anyhow::anyhow!("Kokoro style tensor: {e}"))?;

        let speed_arr = Array1::from_vec(vec![speed_val]);
        let speed_tensor = Tensor::<f32>::from_array(speed_arr)
            .map_err(|e| anyhow::anyhow!("Kokoro speed tensor: {e}"))?;

        // ── Step 5: Run ONNX ──────────────────────────────────────────────────
        let waveform: Vec<f32> = {
            let mut sess = self
                .session
                .lock()
                .map_err(|_| anyhow::anyhow!("Kokoro session mutex poisoned"))?;
            let output = sess
                .run(ort::inputs![
                    "input_ids" => ids_tensor,
                    "style"     => style_tensor,
                    "speed"     => speed_tensor,
                ])
                .map_err(|e| anyhow::anyhow!("Kokoro ONNX run: {e}"))?;
            let (_shape, data) = output[0]
                .try_extract_tensor::<f32>()
                .map_err(|e| anyhow::anyhow!("extract Kokoro waveform: {e}"))?;
            data.to_vec()
        };

        tracing::debug!(
            samples = waveform.len(),
            duration_secs = waveform.len() as f32 / 24_000.0,
            "Kokoro synthesis complete"
        );

        // ── Step 6: WAV encode ────────────────────────────────────────────────
        let wav_bytes = encode_wav_f32(&waveform, 24_000)?;

        Ok(SynthesizeOutput {
            wav_bytes,
            sample_rate: 24_000,
            model_gen: self.model_gen.clone(),
            voice: voice_name.to_string(),
        })
    }

    /// Load the style embedding for `voice_name`, selecting the row at index
    /// `min(n_tokens - 1, n_rows - 1)` per the Kokoro reference implementation.
    fn load_style(&self, voice_name: &str, n_tokens: usize) -> Result<Vec<f32>> {
        let bin_path = self.voices_dir.join(format!("{voice_name}.bin"));
        if !bin_path.exists() {
            // Try the default voice as a fallback so an unknown voice name
            // degrades gracefully rather than hard-failing.
            let fallback = self.voices_dir.join(format!("{}.bin", self.default_voice));
            if fallback.exists() && voice_name != self.default_voice {
                tracing::warn!(
                    voice = voice_name,
                    fallback = %self.default_voice,
                    "voice .bin not found, using default"
                );
                return self.load_style(&self.default_voice.clone(), n_tokens);
            }
            bail!("voice file not found: {}", bin_path.display());
        }

        let bytes = std::fs::read(&bin_path)
            .with_context(|| format!("read voice bin: {}", bin_path.display()))?;

        // Layout: float32 array, reshaped as [-1, 1, 256] in NumPy terms.
        // Each row is 1 × 256 = 256 floats = 1024 bytes.
        const FLOATS_PER_ROW: usize = 256;
        const BYTES_PER_ROW: usize = FLOATS_PER_ROW * 4;
        anyhow::ensure!(
            bytes.len() % BYTES_PER_ROW == 0,
            "voice bin length {} is not a multiple of 1024",
            bytes.len()
        );
        let n_rows = bytes.len() / BYTES_PER_ROW;
        let idx = (n_tokens.saturating_sub(1)).min(n_rows.saturating_sub(1));
        let offset = idx * BYTES_PER_ROW;

        let floats: Vec<f32> = bytes[offset..offset + BYTES_PER_ROW]
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();

        Ok(floats)
    }

    /// The model generation token for the currently loaded model.
    pub fn model_gen(&self) -> &str {
        &self.model_gen
    }
}

// ── Phonemizer ────────────────────────────────────────────────────────────────

/// Convert `text` to an IPA phoneme string using the system `espeak-ng` binary.
///
/// Requires `espeak-ng` to be installed:
/// - macOS: `brew install espeak`
/// - Linux: `apt install espeak-ng`
fn phonemize_espeak(text: &str) -> Result<String> {
    // Try espeak-ng first (Linux), fall back to espeak (macOS Homebrew installs as "espeak").
    let output = Command::new("espeak-ng")
        .args(["--ipa", "-q", "-v", "en-us", text])
        .output()
        .or_else(|_| {
            Command::new("espeak")
                .args(["--ipa", "-q", "-v", "en-us", text])
                .output()
        })
        .context(
            "failed to run espeak-ng or espeak — install with `brew install espeak` (macOS) \
             or `apt install espeak-ng` (Linux)",
        )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("espeak exited with error: {stderr}");
    }

    let raw = String::from_utf8(output.stdout).context("espeak output was not valid UTF-8")?;

    // espeak-ng adds a leading space per word and trailing newline(s).
    // Trim to get a clean IPA string.
    Ok(raw.trim().to_string())
}

// ── WAV encoder ───────────────────────────────────────────────────────────────

/// Encode `samples` (f32 PCM, mono) into WAV bytes at `sample_rate` Hz.
fn encode_wav_f32(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>> {
    let spec = WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 32,
        sample_format: SampleFormat::Float,
    };
    let mut buf = Cursor::new(Vec::new());
    let mut writer = WavWriter::new(&mut buf, spec).context("create WavWriter")?;
    for &s in samples {
        writer.write_sample(s).context("write WAV sample")?;
    }
    writer.finalize().context("finalize WAV")?;
    Ok(buf.into_inner())
}
