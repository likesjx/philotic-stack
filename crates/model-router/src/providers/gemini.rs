use crate::controller::{ControllerTask, ModelProvider, ProviderOutput, TaskKind};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};

pub struct GeminiProvider {
    http_client: reqwest::Client,
    api_key: Option<String>,
    default_model: String,
}

impl GeminiProvider {
    pub fn new(http_client: reqwest::Client, api_key: Option<String>) -> Self {
        Self {
            http_client,
            api_key,
            default_model: "gemini-flash-latest".into(),
        }
    }

    fn endpoint_url(&self, model: Option<&str>) -> Result<String> {
        let api_key = self
            .api_key
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .context("Gemini API key missing from config")?;
        let model = model.unwrap_or(&self.default_model);

        Ok(format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            model, api_key
        ))
    }

    fn request_payload(prompt: &str) -> Value {
        json!({
            "contents": [{"parts": [{"text": prompt}]}]
        })
    }

    fn parse_response_text(status: reqwest::StatusCode, body: Value) -> String {
        if !status.is_success() {
            if let Some(message) = body
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
            {
                return format!("Gemini API Error ({}): {}", status.as_u16(), message);
            }

            return format!("Gemini API Error ({}): {}", status.as_u16(), body);
        }

        body.get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first())
            .and_then(|candidate| candidate.get("content"))
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array)
            .and_then(|parts| parts.first())
            .and_then(|part| part.get("text"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| "Gemini returned an empty response.".into())
    }
}

#[async_trait]
impl ModelProvider for GeminiProvider {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn supports(&self, task: &ControllerTask) -> bool {
        task.kind == TaskKind::TextGenerate
    }

    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        let prompt = task
            .prompt_text()
            .context("Gemini text task missing prompt")?;
        let response = self
            .http_client
            .post(self.endpoint_url(task.model.as_deref())?)
            .json(&Self::request_payload(prompt))
            .send()
            .await?;
        let status = response.status();
        let body = response.json::<Value>().await?;
        let content = Self::parse_response_text(status, body);

        if content.trim().is_empty() {
            bail!("Gemini returned an empty response");
        }

        Ok(ProviderOutput::Text { content })
    }
}
