use crate::controller::{
    AttemptPolicy, BackoffStrategy, ControllerTask, ModelProvider, ProviderOutput, RetryPolicy,
    RetryableErrorClass, TaskKind,
};
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
    max_tokens: u64,
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
            max_tokens: std::env::var("PHILOTIC_OLLAMA_MAX_TOKENS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(256),
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

    fn context_models(&self, task: &ControllerTask) -> Vec<String> {
        vec![
            task.model
                .clone()
                .unwrap_or_else(|| self.default_model.clone()),
        ]
    }

    fn supports(&self, task: &ControllerTask) -> bool {
        task.kind == TaskKind::TextGenerate
    }

    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        if task.kind != TaskKind::TextGenerate {
            bail!(
                "OllamaProvider only supports TextGenerate, got {:?}",
                task.kind
            );
        }

        // Pull model override from task, fall back to configured default.
        let model = task
            .model
            .as_deref()
            .unwrap_or(&self.default_model)
            .to_string();

        let prompt = task
            .composed_prompt_text()
            .context("OllamaProvider: TextGenerate task missing prompt text")?;
        let messages = vec![json!({ "role": "user", "content": prompt })];
        let output_limit = task
            .provider_options
            .get("max_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(self.max_tokens);

        let body = json!({
            "model": model,
            "messages": messages,
            "stream": false,
            "max_tokens": output_limit,
            "options": {
                "num_predict": output_limit
            },
        });

        crate::context_management::account_wire(&body, task)?;
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

    fn attempt_policy(&self) -> AttemptPolicy {
        AttemptPolicy {
            connect_secs: 10,
            idle_secs: 10,
            total_secs: 45,
        }
    }

    fn retry_policy(&self) -> RetryPolicy {
        RetryPolicy {
            max_attempts: 1,
            backoff: BackoffStrategy::None,
            retryable: RetryableErrorClass::default(),
        }
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
        assert_eq!(
            provider.chat_url(),
            "http://localhost:11434/v1/chat/completions"
        );
    }
}
