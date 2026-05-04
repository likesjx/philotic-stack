use crate::controller::{ControllerTask, ModelProvider, ProviderOutput, TaskKind};
use anyhow::{Context, Result};
use async_trait::async_trait;
use parakeet_runner::ParakeetHandle;
use std::sync::Arc;
use tracing::warn;

/// ModelProvider backed by a NeMo Parakeet ASR subprocess.
/// Supports `TaskKind::AudioTranscribe` only.
pub struct ParakeetProvider {
    handles: Vec<Arc<ParakeetHandle>>,
    python_path: String,
}

impl ParakeetProvider {
    pub fn new(
        model_name: impl Into<String>,
        python_path: impl Into<String>,
        priority: u32,
    ) -> Self {
        let handle = Arc::new(ParakeetHandle::new(model_name, priority));
        Self {
            handles: vec![handle],
            python_path: python_path.into(),
        }
    }

    fn best_handle(&self) -> Option<&Arc<ParakeetHandle>> {
        self.handles.iter().max_by_key(|h| h.priority)
    }
}

#[async_trait]
impl ModelProvider for ParakeetProvider {
    fn id(&self) -> &'static str {
        "parakeet"
    }

    fn supports(&self, task: &ControllerTask) -> bool {
        matches!(task.kind, TaskKind::AudioTranscribe) && !self.handles.is_empty()
    }

    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        let handle = self
            .best_handle()
            .context("no Parakeet handle configured")?;

        let audio_path = task
            .provider_options
            .get("audio_path")
            .and_then(|v| v.as_str())
            .context("AudioTranscribe task missing provider_options.audio_path")?;

        let python_path = self.python_path.clone();
        let path = std::path::PathBuf::from(audio_path);
        let model_name = handle.model_name.clone();
        let priority = handle.priority;

        // Run blocking subprocess off the async executor.
        let result = tokio::task::spawn_blocking(move || {
            let h = ParakeetHandle::new(model_name, priority);
            h.transcribe(&python_path, &path)
        })
        .await;

        let output = match result {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                warn!("parakeet transcription failed: {e}");
                return Err(e.context("Parakeet transcription failed"));
            }
            Err(e) => anyhow::bail!("Parakeet blocking task panicked: {e}"),
        };

        Ok(ProviderOutput::Text {
            content: output.text.clone(),
            display_text: Some(output.text.clone()),
            spoken_text: None,
            partial_replies: Vec::new(),
            working_memory_delta: None,
            follow_up_questions: Vec::new(),
            intent_summary: None,
            memory_concept: None,
            memory_candidate: None,
            active_plan: None,
            model_gen: Some(output.model_gen),
        })
    }
}
