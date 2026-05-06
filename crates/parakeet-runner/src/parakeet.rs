use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;
use tracing::info;

/// Drives NeMo `nemo_asr` as a one-shot Python subprocess for audio transcription.
/// Each invocation spawns a new process (stateless; model is reloaded per call).
pub struct ParakeetHandle {
    pub model_name: String,
    pub priority: u32,
}

#[derive(Debug)]
pub struct TranscribeOutput {
    pub text: String,
    pub model_gen: String,
}

/// Inline Python script: loads the model, transcribes one file, prints JSON.
const TRANSCRIBE_SCRIPT: &str = r#"
import sys, json
try:
    import nemo.collections.asr as nemo_asr
    model_name = sys.argv[1]
    audio_path = sys.argv[2]
    model = nemo_asr.models.ASRModel.from_pretrained(model_name)
    result = model.transcribe([audio_path])[0]
    text = result.text if hasattr(result, 'text') else str(result)
    print(json.dumps({"text": text.strip()}))
except Exception as e:
    print(json.dumps({"error": str(e)}), file=sys.stderr)
    sys.exit(1)
"#;

impl ParakeetHandle {
    pub fn new(model_name: impl Into<String>, priority: u32) -> Self {
        Self {
            model_name: model_name.into(),
            priority,
        }
    }

    /// Transcribe a WAV file via a one-shot NeMo Python subprocess.
    pub fn transcribe(&self, python_path: &str, audio_path: &Path) -> Result<TranscribeOutput> {
        info!(
            model = %self.model_name,
            path = %audio_path.display(),
            python_path,
            "running parakeet transcription"
        );

        let output = Command::new(python_path)
            .args([
                "-c",
                TRANSCRIBE_SCRIPT,
                &self.model_name,
                audio_path
                    .to_str()
                    .context("audio path is not valid UTF-8")?,
            ])
            .output()
            .context("failed to invoke nemo_asr transcription subprocess")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("parakeet subprocess exited with error: {stderr}");
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: serde_json::Value =
            serde_json::from_str(stdout.trim()).context("failed to parse parakeet JSON output")?;

        if let Some(err) = parsed.get("error").and_then(|v| v.as_str()) {
            anyhow::bail!("parakeet transcription error: {err}");
        }

        let text = parsed["text"]
            .as_str()
            .unwrap_or_default()
            .trim()
            .to_string();

        let model_gen = format!("parakeet:{}", self.model_name);

        Ok(TranscribeOutput { text, model_gen })
    }
}
