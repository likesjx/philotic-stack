use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;
use tracing::info;

/// Drives `mlx_whisper` as a subprocess for audio transcription.
/// Each invocation spawns a one-shot process (stateless).
pub struct MlxWhisperHandle {
    pub repo_id: String,
    pub priority: u32,
}

#[derive(Debug)]
pub struct TranscribeOutput {
    pub text: String,
}

impl MlxWhisperHandle {
    pub fn new(repo_id: impl Into<String>, priority: u32) -> Self {
        Self {
            repo_id: repo_id.into(),
            priority,
        }
    }

    /// Transcribe an audio file. Returns the full transcript text.
    /// Spawns `python -m mlx_whisper --model <repo> --output-format json <audio_path>`.
    pub fn transcribe(&self, audio_path: &Path) -> Result<TranscribeOutput> {
        info!(
            repo_id = %self.repo_id,
            path = %audio_path.display(),
            "running mlx_whisper transcription"
        );
        let output = Command::new("python")
            .args([
                "-m",
                "mlx_whisper",
                "--model",
                &self.repo_id,
                "--output-format",
                "json",
                audio_path
                    .to_str()
                    .context("audio path is not valid UTF-8")?,
            ])
            .output()
            .context("failed to invoke mlx_whisper")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("mlx_whisper exited with error: {stderr}");
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        // mlx_whisper JSON output has a "text" field at top level.
        let parsed: serde_json::Value =
            serde_json::from_str(&stdout).context("failed to parse mlx_whisper JSON output")?;
        let text = parsed["text"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string();
        Ok(TranscribeOutput { text })
    }
}
