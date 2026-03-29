use anyhow::{bail, Context, Result};
use rustfft::{num_complex::Complex, FftPlanner};
use std::f32::consts::PI;

// ── Whisper audio constants ──────────────────────────────────────────────────
pub const SAMPLE_RATE: u32 = 16_000;
pub const N_FFT: usize = 512; // window size — 32 ms at 16 kHz
pub const HOP_LENGTH: usize = 160; // 10 ms at 16 kHz
pub const N_MELS: usize = 80;
pub const N_SAMPLES: usize = 480_000; // 30 s × 16 kHz
pub const N_FRAMES: usize = 3_000; // N_SAMPLES / HOP_LENGTH

// ── WAV decoding ─────────────────────────────────────────────────────────────

/// Decode a WAV file from bytes and return mono PCM at 16 kHz as `f32`.
///
/// Only 16 kHz mono WAV is accepted in Slice 2; multi-channel / non-16 kHz
/// inputs return an error (resampling / downmix deferred to Slice 3).
pub fn decode_wav(bytes: &[u8]) -> Result<Vec<f32>> {
    let mut reader =
        hound::WavReader::new(std::io::Cursor::new(bytes)).context("failed to parse WAV header")?;
    let spec = reader.spec();

    if spec.sample_rate != SAMPLE_RATE {
        bail!(
            "expected 16 kHz WAV, got {} Hz (resampling not supported in this slice)",
            spec.sample_rate
        );
    }
    if spec.channels != 1 {
        bail!(
            "expected mono WAV, got {} channels (downmix not supported in this slice)",
            spec.channels
        );
    }

    let samples: Result<Vec<f32>, _> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect(),
        hound::SampleFormat::Int => {
            let max_val = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|r| r.map(|s| s as f32 / max_val))
                .collect()
        }
    };

    samples.context("failed to decode WAV samples")
}

// ── Log-mel spectrogram ───────────────────────────────────────────────────────

/// Compute the log-mel spectrogram of `samples`.
///
/// Returns a flat `Vec<f32>` in **row-major** order, shape `[N_MELS, N_FRAMES]`.
/// The values are normalised to the range expected by the Whisper encoder:
/// `(log10(mel) + 4) / 4`, clipped at `[-1, 1]` (global max − 8 dB floor).
pub fn log_mel_spectrogram(samples: &[f32]) -> Vec<f32> {
    // Pad or trim to exactly N_SAMPLES.
    let mut pcm = samples.to_vec();
    if pcm.len() < N_SAMPLES {
        pcm.resize(N_SAMPLES, 0.0);
    } else {
        pcm.truncate(N_SAMPLES);
    }

    // Hann window of length N_FFT.
    let window: Vec<f32> = (0..N_FFT)
        .map(|n| 0.5 * (1.0 - (2.0 * PI * n as f32 / (N_FFT - 1) as f32).cos()))
        .collect();

    let filters = mel_filterbank();
    let n_freqs = N_FFT / 2 + 1; // 257

    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(N_FFT);

    let mut mel_spec = vec![0.0f32; N_MELS * N_FRAMES];

    for frame in 0..N_FRAMES {
        let start = frame * HOP_LENGTH;

        let mut buf: Vec<Complex<f32>> = (0..N_FFT)
            .map(|i| {
                let s = if start + i < pcm.len() {
                    pcm[start + i]
                } else {
                    0.0
                };
                Complex::new(s * window[i], 0.0)
            })
            .collect();

        fft.process(&mut buf);

        // Power spectrum over positive frequencies.
        let power: Vec<f32> = buf[..n_freqs].iter().map(|c| c.norm_sqr()).collect();

        // Apply mel filterbank → log mel energy.
        for m in 0..N_MELS {
            let energy: f32 = (0..n_freqs)
                .map(|k| filters[m * n_freqs + k] * power[k])
                .sum();
            mel_spec[m * N_FRAMES + frame] = energy.max(1e-10).log10();
        }
    }

    // Whisper normalisation: global max − 8 dB floor, then (val + 4) / 4.
    let global_max = mel_spec.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let floor = global_max - 8.0;
    for v in mel_spec.iter_mut() {
        *v = ((*v).max(floor) + 4.0) / 4.0;
    }

    mel_spec
}

// ── Mel filterbank ────────────────────────────────────────────────────────────

/// Build the triangular mel filterbank matrix, shape `[N_MELS, N_FFT/2+1]`.
/// Row-major: index `[m * n_freqs + k]`.
fn mel_filterbank() -> Vec<f32> {
    let n_freqs = N_FFT / 2 + 1;
    let f_max = SAMPLE_RATE as f32 / 2.0;

    let mel_min = hz_to_mel(0.0);
    let mel_max = hz_to_mel(f_max);

    // N_MELS + 2 equally-spaced mel-scale reference points.
    let n_pts = N_MELS + 2;
    let mel_pts: Vec<f32> = (0..n_pts)
        .map(|i| mel_min + (mel_max - mel_min) * i as f32 / (n_pts - 1) as f32)
        .collect();

    // Convert to fractional FFT bin indices.
    let bin_pts: Vec<f32> = mel_pts
        .iter()
        .map(|&m| mel_to_hz(m) / f_max * (n_freqs - 1) as f32)
        .collect();

    let mut filters = vec![0.0f32; N_MELS * n_freqs];

    for m in 0..N_MELS {
        let f_left = bin_pts[m];
        let f_center = bin_pts[m + 1];
        let f_right = bin_pts[m + 2];

        for k in 0..n_freqs {
            let f = k as f32;
            let w = if f >= f_left && f <= f_center && f_center > f_left {
                (f - f_left) / (f_center - f_left)
            } else if f > f_center && f <= f_right && f_right > f_center {
                (f_right - f) / (f_right - f_center)
            } else {
                0.0
            };
            filters[m * n_freqs + k] = w;
        }
    }

    filters
}

fn hz_to_mel(hz: f32) -> f32 {
    2595.0 * (1.0 + hz / 700.0).log10()
}

fn mel_to_hz(mel: f32) -> f32 {
    700.0 * (10.0f32.powf(mel / 2595.0) - 1.0)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mel_filterbank_shape() {
        let f = mel_filterbank();
        assert_eq!(f.len(), N_MELS * (N_FFT / 2 + 1));
    }

    #[test]
    fn mel_filterbank_non_negative() {
        for v in mel_filterbank() {
            assert!(v >= 0.0, "filter weight {v} is negative");
        }
    }

    #[test]
    fn log_mel_spectrogram_shape() {
        let silence = vec![0.0f32; N_SAMPLES];
        let spec = log_mel_spectrogram(&silence);
        assert_eq!(spec.len(), N_MELS * N_FRAMES);
    }

    #[test]
    fn log_mel_short_input_padded() {
        // Input shorter than 30 s — should not panic.
        let short = vec![0.1f32; 1000];
        let spec = log_mel_spectrogram(&short);
        assert_eq!(spec.len(), N_MELS * N_FRAMES);
    }

    #[test]
    fn hz_to_mel_roundtrip() {
        for hz in [0.0f32, 440.0, 8000.0, 16000.0] {
            let roundtrip = mel_to_hz(hz_to_mel(hz));
            assert!((hz - roundtrip).abs() < 1e-2, "{hz} → mel → {roundtrip}");
        }
    }
}
