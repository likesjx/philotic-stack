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

/// Only the supported Ollama error shape can classify model absence. A404
/// proxy/endpoint failure, malicious arbitrary body or other status cannot.
fn missing_model_response(status: u16, raw: &[u8]) -> bool {
    if status != 404 || raw.len() > 4096 { return false; }
    serde_json::from_slice::<Value>(raw).ok()
        .and_then(|value| value.get("error").and_then(Value::as_str).or_else(|| value.pointer("/error/message").and_then(Value::as_str)).map(str::to_owned))
        .is_some_and(|error| error.starts_with("model ") && error.ends_with(" not found"))
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

        // Inventory presence is not proof of tool/structured/media capability.
        // The proposed small local fallback is admitted only for bounded plain
        // text until an independent capability acceptance proves more.
        if model == "qwen2.5:1.5b" && (!task.tools.is_empty()
            || !task.media_attachments().is_empty()
            || !task.affordances.tools.is_empty()
            || !task.response_contract.channels.is_empty()
            || task.response_contract.modalities.iter().any(|mode| mode != "text")
            || task.provider_options.contains_key("response_format")
            || task.provider_options.contains_key("json_schema")
            || task.output_format.as_deref().is_some_and(|format| format != "text")) {
            bail!("OLLAMA_FALLBACK_CAPABILITY_UNVERIFIED");
        }

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

        if model == "qwen2.5:1.5b" && (output_limit > 256 || serde_json::to_vec(&body)?.len() > 8192) {
            bail!("OLLAMA_FALLBACK_BUDGET_EXCEEDED");
        }
        crate::context_management::account_wire(&body, task)?;
        info!(
            model = %model,
            base_url = %self.base_url,
            "OllamaProvider: invoking chat completions"
        );

        let mut resp = self
            .http_client
            .post(self.chat_url())
            .json(&body)
            .send()
            .await
            .context("OllamaProvider: HTTP request to Ollama failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            // Never interpolate provider bodies into failures/logs. Bound the
            // diagnostic read; an oversized or malformed body stays generic.
            let mut raw = Vec::new();
            while let Ok(Some(chunk)) = resp.chunk().await {
                if raw.len() + chunk.len() > 4096 { raw.clear(); break; }
                raw.extend_from_slice(&chunk);
            }
            if missing_model_response(status.as_u16(), &raw) {
                bail!("OLLAMA_MODEL_UNAVAILABLE: HTTP 404");
            }
            bail!("OllamaProvider: HTTP {}", status.as_u16());
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

    async fn synthetic_http(status: u16, body: &'static str) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = axum::Router::new().route("/v1/chat/completions", axum::routing::post(move || async move {
            (axum::http::StatusCode::from_u16(status).unwrap(), body)
        }));
        let worker = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        (base, worker)
    }

    #[tokio::test]
    async fn supported_available_model_succeeds_and_missing_model_is_sanitized() {
        let task = ControllerTask::from_value(&json!({"action":"generate_text", "prompt":"PUBLIC_SYNTHETIC", "model":"qwen2.5:1.5b"})).unwrap();
        let (base, worker) = synthetic_http(200, r#"{"choices":[{"message":{"content":"PUBLIC_SYNTHETIC_REPLY"}}]}"#).await;
        let provider = OllamaProvider::new(reqwest::Client::builder().no_proxy().build().unwrap(), Some(base), None);
        assert_eq!(provider.context_models(&task), vec!["qwen2.5:1.5b"]);
        assert!(provider.invoke(&task).await.is_ok());
        worker.abort();
        let (base, worker) = synthetic_http(404, r#"{"error":{"message":"model 'private-provider-text' not found"},"prompt":"PRIVATE_PROVIDER_BODY"}"#).await;
        let provider = OllamaProvider::new(reqwest::Client::builder().no_proxy().build().unwrap(), Some(base), None);
        let error = provider.invoke(&task).await.unwrap_err().to_string();
        assert_eq!(error, "OLLAMA_MODEL_UNAVAILABLE: HTTP 404");
        assert!(!error.contains("private-provider-text"));
        assert!(!error.contains("PRIVATE_PROVIDER_BODY"));
        worker.abort();
    }

    #[tokio::test]
    async fn unverified_tools_structured_output_and_large_context_refuse_before_network() {
        let provider = OllamaProvider::new(reqwest::Client::builder().no_proxy().build().unwrap(), Some("http://127.0.0.1:1".into()), None);
        let plain = ControllerTask::from_value(&json!({"action":"generate_text", "prompt":"PUBLIC", "model":"qwen2.5:1.5b"})).unwrap();
        let mut tools = plain.clone(); tools.tools.push(json!({"tool_name":"synthetic"}));
        assert_eq!(provider.invoke(&tools).await.unwrap_err().to_string(), "OLLAMA_FALLBACK_CAPABILITY_UNVERIFIED");
        let mut structured = plain.clone(); structured.response_contract.channels.push("active_plan".into());
        assert_eq!(provider.invoke(&structured).await.unwrap_err().to_string(), "OLLAMA_FALLBACK_CAPABILITY_UNVERIFIED");
        for (kind, mime_type) in [("photo", "image/jpeg"), ("audio", "audio/ogg")] {
            let media = ControllerTask::from_value(&json!({
                "action": "generate_text", "prompt": "PUBLIC", "model": "qwen2.5:1.5b",
                "attachments": [{"kind": kind, "mime_type": mime_type,
                    "file_id": "synthetic", "blob_id": "synthetic",
                    "blob_download_url": "http://127.0.0.1:1/synthetic"}]
            })).unwrap();
            assert_eq!(media.kind, TaskKind::TextGenerate);
            assert_eq!(media.media_attachments().len(), 1);
            assert_eq!(provider.invoke(&media).await.unwrap_err().to_string(), "OLLAMA_FALLBACK_CAPABILITY_UNVERIFIED");
        }
        let mut budget = plain.clone(); budget.provider_options.insert("max_tokens".into(), json!(257));
        assert_eq!(provider.invoke(&budget).await.unwrap_err().to_string(), "OLLAMA_FALLBACK_BUDGET_EXCEEDED");
        let mut context = plain; context.prompt = Some("x".repeat(9000));
        assert_eq!(provider.invoke(&context).await.unwrap_err().to_string(), "OLLAMA_FALLBACK_BUDGET_EXCEEDED");
    }

    #[test]
    fn missing_models_are_distinct_from_endpoint_errors() {
        assert!(missing_model_response(404, br#"{"error":"model 'synthetic-absent' not found"}"#));
        assert!(!missing_model_response(404, br#"{"error":"endpoint not found"}"#));
        assert!(!missing_model_response(500, br#"{"error":"model 'synthetic' not found"}"#));
        assert!(!missing_model_response(404, b"Bearer private prompt not found"));
        assert!(!missing_model_response(404, &vec![b'x'; 4097]));
    }

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
