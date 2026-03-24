use crate::controller::{ControllerTask, ModelProvider, ProviderOutput, TaskKind};
use anyhow::{Context, Result};
use async_trait::async_trait;
use mlx_runner::{
    MlxFleetConfig, MlxModelClass, MlxModelInstance, MlxWhisperHandle,
};
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
use tracing::{info, warn};

/// ModelProvider implementation backed by a fleet of local `mlx_lm.server` instances.
///
/// Supports `TaskKind::TextGenerate` (Phase 1) and `TaskKind::AudioTranscribe` (Phase 2).
/// Multimodal (`MediaAnalyze`) is designed in but deferred.
pub struct MlxProvider {
    text_models: Vec<Arc<MlxModelInstance>>,
    multimodal_models: Vec<Arc<MlxModelInstance>>,
    transcribe_handles: Vec<Arc<MlxWhisperHandle>>,
}

impl MlxProvider {
    /// Build the fleet from a `MlxFleetConfig`. Starts all managed instances
    /// and performs startup health checks in parallel.
    pub async fn from_config(config: MlxFleetConfig) -> Result<Self> {
        let mut text_models: Vec<Arc<MlxModelInstance>> = Vec::new();
        let mut multimodal_models: Vec<Arc<MlxModelInstance>> = Vec::new();
        let mut transcribe_handles: Vec<Arc<MlxWhisperHandle>> = Vec::new();

        let mut text_cfgs = Vec::new();
        let mut mm_cfgs = Vec::new();

        for model_cfg in config.models {
            match model_cfg.class {
                MlxModelClass::Transcribe => {
                    transcribe_handles.push(Arc::new(MlxWhisperHandle::new(
                        &model_cfg.repo_id,
                        model_cfg.priority,
                    )));
                }
                MlxModelClass::Text => {
                    text_cfgs.push(model_cfg);
                }
                MlxModelClass::Multimodal => {
                    mm_cfgs.push(model_cfg);
                }
            }
        }

        // Start text and multimodal instances in parallel.
        let text_results = futures::future::join_all(
            text_cfgs.into_iter().map(|cfg| async move {
                let repo_id = cfg.repo_id.clone();
                MlxModelInstance::start(cfg)
                    .await
                    .map(Arc::new)
                    .map_err(|e| (repo_id, e))
            }),
        )
        .await;

        let mm_results = futures::future::join_all(
            mm_cfgs.into_iter().map(|cfg| async move {
                let repo_id = cfg.repo_id.clone();
                MlxModelInstance::start(cfg)
                    .await
                    .map(Arc::new)
                    .map_err(|e| (repo_id, e))
            }),
        )
        .await;

        for result in text_results {
            match result {
                Ok(inst) => text_models.push(inst),
                Err((repo_id, e)) => warn!(repo_id, "failed to start text instance: {e}"),
            }
        }
        for result in mm_results {
            match result {
                Ok(inst) => multimodal_models.push(inst),
                Err((repo_id, e)) => warn!(repo_id, "failed to start multimodal instance: {e}"),
            }
        }

        info!(
            text = text_models.len(),
            multimodal = multimodal_models.len(),
            transcribe = transcribe_handles.len(),
            "MlxProvider fleet started"
        );

        Ok(Self {
            text_models,
            multimodal_models,
            transcribe_handles,
        })
    }

    /// Spawn the background health-check ticker.
    pub fn spawn_health_ticker(provider: Arc<MlxProvider>, interval_secs: u64) {
        let interval = Duration::from_secs(interval_secs);
        tokio::spawn(async move {
            let mut ticker = time::interval(interval);
            ticker.tick().await; // skip first immediate tick
            loop {
                ticker.tick().await;
                for inst in provider.text_models.iter().chain(provider.multimodal_models.iter()) {
                    inst.run_health_check().await;
                }
            }
        });
    }

    /// Select the highest-priority healthy instance from a class slice.
    async fn select_instance<'a>(
        instances: &'a [Arc<MlxModelInstance>],
        prefer_repo: Option<&str>,
    ) -> Option<&'a Arc<MlxModelInstance>> {
        let mut healthy: Vec<&Arc<MlxModelInstance>> = Vec::new();
        for inst in instances {
            if inst.is_healthy().await {
                healthy.push(inst);
            }
        }
        if healthy.is_empty() {
            return None;
        }
        // Prefer explicit model if requested and healthy.
        if let Some(repo) = prefer_repo {
            if let Some(inst) = healthy.iter().find(|i| i.config.repo_id == repo) {
                return Some(inst);
            }
        }
        // Otherwise pick highest priority.
        healthy.into_iter().max_by_key(|i| i.priority())
    }

    /// True if at least one whisper handle is configured.
    fn any_transcribe(&self) -> bool {
        !self.transcribe_handles.is_empty()
    }

    async fn invoke_text(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        let prefer_repo = task
            .provider_options
            .get("prefer_model")
            .and_then(|v| v.as_str());

        let inst = Self::select_instance(&self.text_models, prefer_repo)
            .await
            .context("no healthy MLX text instance available")?;

        // Build messages from the composed prompt text.
        let prompt = task
            .composed_prompt_text()
            .context("MLX text task missing prompt")?;

        let messages = vec![mlx_runner::client::ChatMessage {
            role: "user".to_string(),
            content: prompt,
        }];

        let tools = if task.tools.is_empty() {
            None
        } else {
            Some(
                task.tools
                    .iter()
                    .map(|t| {
                        serde_json::json!({
                            "type": "function",
                            "function": {
                                "name": t.get("tool_name").and_then(|v| v.as_str()).unwrap_or(""),
                                "description": t.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                                "parameters": t.get("input_schema").cloned().unwrap_or(serde_json::json!({})),
                            }
                        })
                    })
                    .collect::<Vec<_>>(),
            )
        };

        let temperature = task
            .provider_options
            .get("temperature")
            .and_then(|v| v.as_f64())
            .map(|f| f as f32);
        let max_tokens = task
            .provider_options
            .get("max_tokens")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);

        let result = inst.chat(messages, tools, temperature, max_tokens).await;

        if let Err(ref e) = result {
            warn!(repo_id = %inst.config.repo_id, "invoke_text failed, marking degraded: {e}");
            inst.mark_degraded().await;
        }

        let response = result.context("MLX text generation failed")?;

        let choice = response
            .choices
            .into_iter()
            .next()
            .context("mlx_lm.server returned no choices")?;

        // Check for tool calls first.
        if let Some(tool_calls) = choice.message.tool_calls {
            if let Some(tc) = tool_calls.into_iter().next() {
                let arguments: serde_json::Value =
                    serde_json::from_str(&tc.function.arguments).unwrap_or(serde_json::json!({}));
                return Ok(ProviderOutput::ToolCall {
                    tool_name: tc.function.name,
                    arguments,
                });
            }
        }

        let content = choice.message.content.unwrap_or_default();
        Ok(ProviderOutput::Text {
            content: content.clone(),
            display_text: Some(content.clone()),
            spoken_text: Some(content.clone()),
            partial_replies: Vec::new(),
            working_memory_delta: None,
            follow_up_questions: Vec::new(),
            intent_summary: None,
            memory_concept: None,
            memory_candidate: None,
            active_plan: None,
        })
    }

    async fn invoke_transcribe(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        let handle = self
            .transcribe_handles
            .iter()
            .max_by_key(|h| h.priority)
            .context("no MLX whisper handle configured")?;

        // Audio path is expected in provider_options["audio_path"].
        let audio_path = task
            .provider_options
            .get("audio_path")
            .and_then(|v| v.as_str())
            .context("AudioTranscribe task missing provider_options.audio_path")?;

        let path = std::path::Path::new(audio_path);
        let output = handle.transcribe(path).context("mlx_whisper transcription failed")?;

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
        })
    }
}

#[async_trait]
impl ModelProvider for MlxProvider {
    fn id(&self) -> &'static str {
        "mlx"
    }

    fn supports(&self, task: &ControllerTask) -> bool {
        match task.kind {
            TaskKind::TextGenerate => !self.text_models.is_empty(),
            TaskKind::AudioTranscribe => self.any_transcribe(),
            _ => false,
        }
    }

    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        match task.kind {
            TaskKind::TextGenerate => self.invoke_text(task).await,
            TaskKind::AudioTranscribe => self.invoke_transcribe(task).await,
            other => anyhow::bail!(
                "MlxProvider does not support task kind [{}]",
                other.as_str()
            ),
        }
    }
}
