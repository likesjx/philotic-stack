use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioLigand {
    NativePcm {
        bytes: Vec<u8>,
        mime_type: String,
    },
    ForeignEncoding {
        bytes: Vec<u8>,
        source_mime_type: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedAudioLigand {
    pub bytes: Vec<u8>,
    pub mime_type: String,
    pub source_mime_type: String,
    pub prep_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioArtifactEnvelope {
    pub kind: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub voice_id: Option<String>,
    pub mime_type: String,
    #[serde(default)]
    pub output_format: Option<String>,
    #[serde(default, alias = "data_b64")]
    pub audio_base64: String,
}

impl AudioArtifactEnvelope {
    pub fn from_bytes(
        provider: Option<&str>,
        model: Option<&str>,
        voice_id: Option<&str>,
        mime_type: &str,
        output_format: Option<&str>,
        audio_bytes: &[u8],
    ) -> Self {
        Self {
            kind: "audio_artifact".into(),
            provider: provider.map(str::to_string),
            model: model.map(str::to_string),
            voice_id: voice_id.map(str::to_string),
            mime_type: mime_type.to_string(),
            output_format: output_format.map(str::to_string),
            audio_base64: BASE64_STANDARD.encode(audio_bytes),
        }
    }

    pub fn to_json_string(&self) -> Result<String> {
        serde_json::to_string(self).context("failed to serialize audio artifact envelope")
    }

    pub fn decode_audio_bytes(&self) -> Result<Vec<u8>> {
        BASE64_STANDARD
            .decode(&self.audio_base64)
            .context("failed to decode audio artifact base64 payload")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcmPrepPolicy {
    pub target_mime_type: String,
    pub sample_rate_hz: u32,
    pub channels: u8,
    pub ffmpeg_bin: String,
}

impl Default for PcmPrepPolicy {
    fn default() -> Self {
        Self {
            target_mime_type: "audio/pcm;rate=16000".into(),
            sample_rate_hz: 16_000,
            channels: 1,
            ffmpeg_bin: "ffmpeg".into(),
        }
    }
}

pub fn classify_audio_ligand(mime_type: &str, bytes: Vec<u8>) -> AudioLigand {
    let normalized = mime_type.trim();
    if normalized.to_ascii_lowercase().starts_with("audio/pcm") {
        AudioLigand::NativePcm {
            bytes,
            mime_type: normalized.to_string(),
        }
    } else {
        AudioLigand::ForeignEncoding {
            bytes,
            source_mime_type: normalized.to_string(),
        }
    }
}

pub fn serialize_audio_artifact_envelope(
    provider: Option<&str>,
    model: Option<&str>,
    voice_id: Option<&str>,
    mime_type: &str,
    output_format: Option<&str>,
    audio_bytes: &[u8],
) -> Result<String> {
    AudioArtifactEnvelope::from_bytes(
        provider,
        model,
        voice_id,
        mime_type,
        output_format,
        audio_bytes,
    )
    .to_json_string()
}

pub fn parse_audio_artifact_json(raw: &str) -> Result<AudioArtifactEnvelope> {
    let envelope: AudioArtifactEnvelope =
        serde_json::from_str(raw).context("failed to parse audio artifact envelope JSON")?;
    if envelope.kind != "audio_artifact" {
        bail!(
            "expected audio artifact envelope kind [audio_artifact], got [{}]",
            envelope.kind
        );
    }
    if envelope.mime_type.trim().is_empty() {
        bail!("audio artifact envelope missing mime_type");
    }
    if envelope.audio_base64.trim().is_empty() {
        bail!("audio artifact envelope missing audio_base64 payload");
    }
    Ok(envelope)
}

pub fn extract_audio_artifact_json(model_result: Option<&Value>) -> Option<String> {
    let artifacts = model_result?.get("artifacts")?.as_array()?;
    let payload = artifacts.iter().find_map(|artifact| {
        let artifact_kind = artifact.get("kind").and_then(Value::as_str);
        let payload = artifact.get("payload")?;
        let payload_kind = payload.get("kind").and_then(Value::as_str);
        if artifact_kind == Some("audio") || payload_kind == Some("audio_artifact") {
            Some(payload)
        } else {
            None
        }
    })?;
    let serialized = serde_json::to_string(payload).ok()?;
    parse_audio_artifact_json(&serialized).ok()?;
    Some(serialized)
}

/// Transcode any audio format (OGG, MP3, etc.) to a 16 kHz mono WAV using
/// ffmpeg.  Returns raw WAV bytes ready for `hound::WavReader`.
///
/// If the source is already WAV (`mime_type` starts with `"audio/wav"` or
/// `"audio/x-wav"`), the bytes are returned unchanged to avoid a re-encode.
pub async fn transcode_to_wav_16k(
    source_bytes: Vec<u8>,
    mime_type: &str,
    ffmpeg_bin: &str,
) -> Result<Vec<u8>> {
    let normalized = mime_type.trim().to_ascii_lowercase();
    if normalized.starts_with("audio/wav") || normalized.starts_with("audio/x-wav") {
        return Ok(source_bytes);
    }
    transcode_raw_to_wav_16k(source_bytes, mime_type, ffmpeg_bin).await
}

async fn transcode_raw_to_wav_16k(
    source_bytes: Vec<u8>,
    source_mime_type: &str,
    ffmpeg_bin: &str,
) -> Result<Vec<u8>> {
    let mut child = tokio::process::Command::new(ffmpeg_bin)
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg("pipe:0")
        .arg("-f")
        .arg("wav")
        .arg("-acodec")
        .arg("pcm_s16le")
        .arg("-ac")
        .arg("1")
        .arg("-ar")
        .arg("16000")
        .arg("pipe:1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| {
            format!("failed to spawn [{ffmpeg_bin}] for WAV transcode from [{source_mime_type}]")
        })?;

    let mut stdin = child.stdin.take().context("WAV transcoder missing stdin")?;
    tokio::io::AsyncWriteExt::write_all(&mut stdin, &source_bytes)
        .await
        .context("failed to stream source audio into WAV transcoder")?;
    drop(stdin);

    let output = child
        .wait_with_output()
        .await
        .context("failed to await WAV transcoder")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "WAV transcoder [{ffmpeg_bin}] failed for [{source_mime_type}]: {}",
            stderr.trim()
        );
    }
    if output.stdout.is_empty() {
        bail!("WAV transcoder [{ffmpeg_bin}] returned empty output for [{source_mime_type}]");
    }
    // ffmpeg writes 0xffffffff for RIFF chunk size and data chunk size when the
    // output is a pipe (size unknown at write time). hound validates that the
    // data chunk size is a multiple of bytes_per_sample, which fails for
    // 0xffffffff with 16-bit samples (odd). Patch both sizes to the real values.
    Ok(patch_wav_streaming_sizes(output.stdout))
}

/// Patch the RIFF chunk size and the first `data` chunk size in a WAV byte
/// vector whose sizes were written as `0xffffffff` (streaming/pipe mode).
fn patch_wav_streaming_sizes(mut bytes: Vec<u8>) -> Vec<u8> {
    let total = bytes.len();
    if total < 44 {
        return bytes;
    }
    // RIFF chunk size lives at bytes[4..8].
    let riff_size = (total.saturating_sub(8)) as u32;
    bytes[4..8].copy_from_slice(&riff_size.to_le_bytes());

    // Scan for the first "data" chunk whose stored size is 0xffffffff.
    let mut i = 12usize;
    while i + 8 <= total {
        if &bytes[i..i + 4] == b"data" {
            let stored = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().unwrap());
            if stored == u32::MAX {
                let data_size = (total.saturating_sub(i + 8)) as u32;
                bytes[i + 4..i + 8].copy_from_slice(&data_size.to_le_bytes());
            }
            break;
        }
        // Skip to the next chunk: 4-byte tag + 4-byte length + chunk payload.
        let chunk_len = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().unwrap_or([0; 4]));
        i += 8 + chunk_len as usize;
    }
    bytes
}

pub async fn prepare_audio_ligand_for_pcm(
    mime_type: &str,
    bytes: Vec<u8>,
    policy: &PcmPrepPolicy,
) -> Result<PreparedAudioLigand> {
    match classify_audio_ligand(mime_type, bytes) {
        AudioLigand::NativePcm { bytes, mime_type } => {
            let source_mime_type = mime_type.clone();
            Ok(PreparedAudioLigand {
                bytes,
                mime_type,
                source_mime_type,
                prep_path: "native_pcm".into(),
            })
        }
        AudioLigand::ForeignEncoding {
            bytes,
            source_mime_type,
        } => {
            let pcm_bytes = transcode_audio_ligand_to_pcm(bytes, &source_mime_type, policy).await?;
            Ok(PreparedAudioLigand {
                bytes: pcm_bytes,
                mime_type: policy.target_mime_type.clone(),
                source_mime_type,
                prep_path: "ffmpeg_transcode".into(),
            })
        }
    }
}

async fn transcode_audio_ligand_to_pcm(
    source_bytes: Vec<u8>,
    source_mime_type: &str,
    policy: &PcmPrepPolicy,
) -> Result<Vec<u8>> {
    let mut child = Command::new(&policy.ffmpeg_bin)
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg("pipe:0")
        .arg("-f")
        .arg("s16le")
        .arg("-acodec")
        .arg("pcm_s16le")
        .arg("-ac")
        .arg(policy.channels.to_string())
        .arg("-ar")
        .arg(policy.sample_rate_hz.to_string())
        .arg("pipe:1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "failed to spawn [{}] for audio ligand transcoding from [{}]",
                policy.ffmpeg_bin, source_mime_type
            )
        })?;

    let mut stdin = child
        .stdin
        .take()
        .context("audio ligand transcoder missing stdin")?;
    stdin
        .write_all(&source_bytes)
        .await
        .context("failed to stream source audio into transcoder")?;
    drop(stdin);

    let output = child
        .wait_with_output()
        .await
        .context("failed to await audio ligand transcoder")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "audio ligand transcoder [{}] failed for [{}]: {}",
            policy.ffmpeg_bin,
            source_mime_type,
            stderr.trim()
        );
    }
    if output.stdout.is_empty() {
        bail!(
            "audio ligand transcoder [{}] returned empty PCM output for [{}]",
            policy.ffmpeg_bin,
            source_mime_type
        );
    }

    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::{
        AudioArtifactEnvelope, AudioLigand, PcmPrepPolicy, classify_audio_ligand,
        extract_audio_artifact_json, parse_audio_artifact_json, prepare_audio_ligand_for_pcm,
        serialize_audio_artifact_envelope,
    };

    #[test]
    fn classify_audio_ligand_preserves_pcm() {
        let ligand = classify_audio_ligand("audio/pcm;rate=16000", vec![1, 2, 3]);
        assert_eq!(
            ligand,
            AudioLigand::NativePcm {
                bytes: vec![1, 2, 3],
                mime_type: "audio/pcm;rate=16000".into()
            }
        );
    }

    #[test]
    fn classify_audio_ligand_marks_foreign_encoding() {
        let ligand = classify_audio_ligand("audio/ogg", vec![4, 5, 6]);
        assert_eq!(
            ligand,
            AudioLigand::ForeignEncoding {
                bytes: vec![4, 5, 6],
                source_mime_type: "audio/ogg".into()
            }
        );
    }

    #[tokio::test]
    async fn prepare_audio_ligand_for_pcm_keeps_native_pcm() {
        let prepared = prepare_audio_ligand_for_pcm(
            "audio/pcm;rate=16000",
            vec![7, 8, 9],
            &PcmPrepPolicy::default(),
        )
        .await
        .expect("native pcm should pass through");

        assert_eq!(prepared.bytes, vec![7, 8, 9]);
        assert_eq!(prepared.mime_type, "audio/pcm;rate=16000");
        assert_eq!(prepared.prep_path, "native_pcm");
    }

    #[test]
    fn serialize_and_parse_audio_artifact_roundtrip() {
        let serialized = serialize_audio_artifact_envelope(
            Some("elevenlabs"),
            Some("eleven_multilingual_v2"),
            Some("voice-123"),
            "audio/mpeg",
            Some("mp3_44100_128"),
            b"hello",
        )
        .expect("audio artifact should serialize");

        let parsed = parse_audio_artifact_json(&serialized).expect("audio artifact should parse");
        assert_eq!(parsed.kind, "audio_artifact");
        assert_eq!(parsed.provider.as_deref(), Some("elevenlabs"));
        assert_eq!(
            parsed.decode_audio_bytes().expect("audio should decode"),
            b"hello".to_vec()
        );
    }

    #[test]
    fn parse_audio_artifact_accepts_legacy_data_b64_alias() {
        let parsed = parse_audio_artifact_json(
            r#"{"kind":"audio_artifact","mime_type":"audio/wav","data_b64":"AQID"}"#,
        )
        .expect("legacy alias should parse");

        assert_eq!(parsed.mime_type, "audio/wav");
        assert_eq!(
            parsed.decode_audio_bytes().expect("audio should decode"),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn extract_audio_artifact_json_reads_valid_audio_payload() {
        let model_result = serde_json::json!({
            "artifacts": [{
                "kind": "audio",
                "payload": AudioArtifactEnvelope::from_bytes(
                    Some("gemini"),
                    Some("gemini-3.1-flash-live"),
                    Some("voice-1"),
                    "audio/wav",
                    Some("wav"),
                    &[1, 2, 3],
                )
            }]
        });

        let extracted = extract_audio_artifact_json(Some(&model_result))
            .expect("audio artifact should extract");
        let parsed = parse_audio_artifact_json(&extracted).expect("extracted payload should parse");
        assert_eq!(parsed.mime_type, "audio/wav");
    }
}
