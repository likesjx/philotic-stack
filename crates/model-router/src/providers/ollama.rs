use crate::controller::{ControllerTask, ModelProvider, ProviderOutput, TaskKind};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use tracing::info;

/// ModelProvider backed by a local Ollama server (OpenAI-compatible API).
///
/// Handles `TaskKind::TextGenerate` only. Embeddings are handled by the ONNX
/// sidecar on :11435 (Ollama-compat), which is separate from this provider.
///
/// Default endpoint: `http://localhost:11434`.
/// Default model: `gemma4:e4b`.
pub struct OllamaProvider {
    http_client: reqwest::Client,
    base_url: String,
    default_model: String,
}

impl OllamaProvider {
    pub fn new(
        http_client: reqwest::Client,
        base_url: Option<String>,
        model: Option<String>,
    ) -> Self {
        Self {
            http_client,
            base_url: base_url
                .unwrap_or_else(|| "http://localhost:11434".into())
                .trim_end_matches('/')
                .to_string(),
            default_model: model.unwrap_or_else(|| "gemma4:e4b".into()),
        }
    }

    fn chat_url(&self) -> String {
        format!("{}/v1/chat/completions", self.base_url)
    }
}

#[async_trait]
impl ModelProvider for OllamaProvider {
    fn id(&self) -> &'static str {
        "ollama"
    }

    fn supports(&self, task: &ControllerTask) -> bool {
        task.kind == TaskKind::TextGenerate
    }

    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        if task.kind != TaskKind::TextGenerate {
            bail!("OllamaProvider only supports TextGenerate, got {:?}", task.kind);
        }

        // Pull model override from task, fall back to configured default.
        let model = task
            .model
            .as_deref()
            .unwrap_or(&self.default_model)
            .to_string();

        // Build messages array from composed prompt.
        // System text goes into a system role message; the prompt is the user turn.
        let mut messages: Vec<Value> = Vec::new();

        // Inject composed context (identity, instructions, memory) as system message.
        if let Some(system_text) = task.composed_prompt_text() {
            if !system_text.trim().is_empty() {
                messages.push(json!({ "role": "system", "content": system_text }));
            }
        }

        // Active user prompt — required for text generation.
        let prompt = task
            .prompt_text()
            .context("OllamaProvider: TextGenerate task missing prompt text")?;
        messages.push(json!({ "role": "user", "content": prompt }));

        let body = json!({
            "model": model,
            "messages": messages,
            "stream": false,
        });

        info!(
            model = %model,
            base_url = %self.base_url,
            "OllamaProvider: invoking chat completions"
        );

        let resp = self
            .http_client
            .post(self.chat_url())
            .json(&body)
            .send()
            .await
            .context("OllamaProvider: HTTP request to Ollama failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("OllamaProvider: Ollama returned {} — {}", status, body);
        }

        let json_resp: Value = resp
            .json()
            .await
            .context("OllamaProvider: failed to parse Ollama response JSON")?;

        let content = json_resp
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .map(|s| s.to_string())
            .context("OllamaProvider: response missing choices[0].message.content")?;

        Ok(ProviderOutput::Text {
            display_text: Some(content.clone()),
            content,
            spoken_text: None,
            partial_replies: Vec::new(),
            working_memory_delta: None,
            follow_up_questions: Vec::new(),
            intent_summary: None,
            memory_concept: None,
            memory_candidate: None,
            active_plan: None,
            model_gen: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let provider = OllamaProvider::new(reqwest::Client::new(), None, None);
        assert_eq!(provider.base_url, "http://localhost:11434");
        assert_eq!(provider.default_model, "gemma4:e4b");
        assert_eq!(provider.id(), "ollama");
    }

    #[test]
    fn trailing_slash_stripped_from_base_url() {
        let provider = OllamaProvider::new(
            reqwest::Client::new(),
            Some("http://localhost:11434/".into()),
            None,
        );
        assert_eq!(provider.chat_url(), "http://localhost:11434/v1/chat/completions");
    }
}
