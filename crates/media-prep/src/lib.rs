use anyhow::{Context, Result, bail};
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
    use super::{AudioLigand, PcmPrepPolicy, classify_audio_ligand, prepare_audio_ligand_for_pcm};

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
}
