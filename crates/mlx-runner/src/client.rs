use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

/// Lightweight OpenAI-compatible HTTP client for `mlx_lm.server`.
#[derive(Clone)]
pub struct MlxClient {
    base_url: String,
    http: reqwest::Client,
}

/// The model currently loaded in an mlx_lm.server instance.
#[derive(Debug, Clone)]
pub struct LoadedModelInfo {
    /// The model id as reported by `/v1/models`.
    pub model_id: String,
}

#[derive(Debug, Serialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct ChatResponse {
    pub choices: Vec<ChatChoice>,
    pub model: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChatChoice {
    pub message: ChatChoiceMessage,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChatChoiceMessage {
    pub role: String,
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCallResponse>>,
}

#[derive(Debug, Deserialize)]
pub struct ToolCallResponse {
    pub id: Option<String>,
    #[serde(rename = "type")]
    pub call_type: Option<String>,
    pub function: ToolCallFunction,
}

#[derive(Debug, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    data: Vec<ModelObject>,
}

#[derive(Debug, Deserialize)]
struct ModelObject {
    id: String,
}

impl MlxClient {
    pub fn new(host: &str, port: u16) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .context("failed to build reqwest client for mlx_lm.server")?;
        Ok(Self {
            base_url: format!("http://{}:{}", host, port),
            http,
        })
    }

    /// Query `/v1/models` and return the first model id reported by the server.
    /// Used at startup to verify what is actually loaded (trust but verify).
    pub async fn discover_loaded_model(&self) -> Result<LoadedModelInfo> {
        let url = format!("{}/v1/models", self.base_url);
        let resp = self
            .http
            .get(&url)
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .context("GET /v1/models failed")?;
        let status = resp.status();
        if !status.is_success() {
            anyhow::bail!("GET /v1/models returned HTTP {status}");
        }
        let body: ModelsResponse = resp.json().await.context("failed to parse /v1/models response")?;
        let model_id = body
            .data
            .into_iter()
            .next()
            .map(|m| m.id)
            .unwrap_or_default();
        Ok(LoadedModelInfo { model_id })
    }

    /// Health-check: returns Ok if the server responds to /v1/models.
    pub async fn health_check(&self) -> Result<()> {
        self.discover_loaded_model().await.map(|_| ())
    }

    /// Send a chat completions request and return the full response.
    pub async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let url = format!("{}/v1/chat/completions", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(req)
            .send()
            .await
            .context("POST /v1/chat/completions failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("mlx_lm.server chat error HTTP {status}: {body}");
        }
        resp.json::<ChatResponse>()
            .await
            .context("failed to parse chat completions response")
    }
}
