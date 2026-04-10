use crate::controller::{ControllerTask, ModelProvider, ProviderOutput, TaskKind};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use tracing::info;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenAIAuth {
    ApiKey(String),
    OAuthBearer(String),
}

pub struct OpenAIProvider {
    http_client: reqwest::Client,
    auth: Option<OpenAIAuth>,
    base_url: String,
    project_id: Option<String>,
    default_model: String,
    default_embedding_model: String,
}

impl OpenAIProvider {
    pub fn new(
        http_client: reqwest::Client,
        auth: Option<OpenAIAuth>,
        base_url: Option<String>,
        project_id: Option<String>,
        default_model: Option<String>,
        default_embedding_model: Option<String>,
    ) -> Self {
        Self {
            http_client,
            auth,
            base_url: base_url
                .unwrap_or_else(|| "https://api.openai.com".into())
                .trim_end_matches('/')
                .to_string(),
            project_id,
            default_model: default_model.unwrap_or_else(|| "gpt-4.1-mini".into()),
            default_embedding_model: default_embedding_model
                .unwrap_or_else(|| "text-embedding-3-small".into()),
        }
    }

    pub fn auth_from_config(
        oauth_access_token: Option<String>,
        api_key: Option<String>,
    ) -> Option<OpenAIAuth> {
        if let Some(access_token) = oauth_access_token
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty())
        {
            return Some(OpenAIAuth::OAuthBearer(access_token));
        }

        api_key
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
            .map(OpenAIAuth::ApiKey)
    }

    fn endpoint_url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }

    fn auth_header(&self) -> Option<String> {
        match self.auth.as_ref()? {
            OpenAIAuth::ApiKey(key) | OpenAIAuth::OAuthBearer(key) => {
                Some(format!("Bearer {}", key))
            }
        }
    }

    fn default_model<'a>(&'a self, task: &'a ControllerTask) -> &'a str {
        task.model.as_deref().unwrap_or(&self.default_model)
    }

    fn default_embedding_model<'a>(&'a self, task: &'a ControllerTask) -> &'a str {
        task.model
            .as_deref()
            .unwrap_or(&self.default_embedding_model)
    }

    fn prompt_text(task: &ControllerTask) -> Result<String> {
        match task.kind {
            TaskKind::MediaAnalyze => task
                .media_prompt()
                .map(str::to_string)
                .context("media.analyze task missing prompt"),
            TaskKind::TextGenerate => task
                .composed_prompt_text()
                .context("text.generate task missing prompt"),
            other => bail!(
                "OpenAIProvider does not support task kind [{}]",
                other.as_str()
            ),
        }
    }

    fn attachment_looks_like_image(attachment: &crate::controller::AttachmentInput) -> bool {
        attachment
            .mime_type
            .as_deref()
            .map(|mime| mime.starts_with("image/"))
            .unwrap_or(false)
            || attachment
                .kind
                .as_deref()
                .map(|kind| kind.contains("image"))
                .unwrap_or(false)
    }

    fn user_message(task: &ControllerTask) -> Result<Value> {
        let prompt = Self::prompt_text(task)?;

        let attachments = task.media_attachments();
        if attachments.is_empty() {
            return Ok(json!({
                "role": "user",
                "content": prompt,
            }));
        }

        let mut parts = vec![json!({
            "type": "text",
            "text": prompt,
        })];

        for attachment in attachments {
            let Some(url) = attachment
                .url
                .as_deref()
                .map(str::trim)
                .filter(|url| !url.is_empty())
            else {
                continue;
            };

            let treat_as_image = Self::attachment_looks_like_image(attachment)
                || matches!(task.kind, TaskKind::MediaAnalyze);

            if treat_as_image {
                parts.push(json!({
                    "type": "image_url",
                    "image_url": {
                        "url": url,
                        "detail": "auto"
                    }
                }));
            }
        }

        if parts.len() == 1 {
            Ok(json!({
                "role": "user",
                "content": prompt,
            }))
        } else {
            Ok(json!({
                "role": "user",
                "content": parts,
            }))
        }
    }

    fn function_declarations(tools: &[Value]) -> Vec<Value> {
        tools
            .iter()
            .filter_map(|tool| {
                let tool_name = tool.get("tool_name").and_then(Value::as_str)?.trim();
                if tool_name.is_empty() {
                    return None;
                }

                let description = tool
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .unwrap_or(tool_name);
                let parameters = tool
                    .get("input_schema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}}));

                Some(json!({
                    "type": "function",
                    "function": {
                        "name": tool_name,
                        "description": description,
                        "parameters": parameters,
                    }
                }))
            })
            .collect()
    }

    fn response_format(task: &ControllerTask) -> Option<Value> {
        if let Some(value) = task.provider_options.get("response_format") {
            return Some(value.clone());
        }

        match task.response_contract.style.as_deref() {
            Some("json") | Some("structured") => Some(json!({ "type": "json_object" })),
            _ => None,
        }
    }

    fn numeric_provider_option(task: &ControllerTask, key: &str) -> Option<Value> {
        task.provider_options.get(key).and_then(|value| {
            if value.is_number() {
                Some(value.clone())
            } else if let Some(raw) = value.as_str() {
                raw.parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number)
            } else {
                None
            }
        })
    }

    fn string_provider_option(task: &ControllerTask, key: &str) -> Option<String> {
        task.provider_option_str(key).map(str::to_string)
    }

    fn bool_provider_option(task: &ControllerTask, key: &str) -> Option<bool> {
        task.provider_options.get(key).and_then(|value| {
            if let Some(flag) = value.as_bool() {
                Some(flag)
            } else if let Some(raw) = value.as_str() {
                match raw.trim().to_ascii_lowercase().as_str() {
                    "1" | "true" | "yes" | "on" => Some(true),
                    "0" | "false" | "no" | "off" => Some(false),
                    _ => None,
                }
            } else {
                None
            }
        })
    }

    fn chat_request_body(&self, task: &ControllerTask) -> Result<Value> {
        let mut body = json!({
            "model": self.default_model(task),
            "messages": [Self::user_message(task)?],
        });

        if !task.tools.is_empty() {
            body["tools"] = Value::Array(Self::function_declarations(&task.tools));
            body["tool_choice"] = Value::String("auto".into());
        }

        if let Some(response_format) = Self::response_format(task) {
            body["response_format"] = response_format;
        }

        for key in ["reasoning_effort", "verbosity"] {
            if let Some(value) = Self::string_provider_option(task, key) {
                body[key] = Value::String(value);
            }
        }

        if let Some(background) = Self::bool_provider_option(task, "background") {
            body["background"] = Value::Bool(background);
        }

        for key in [
            "temperature",
            "top_p",
            "frequency_penalty",
            "presence_penalty",
            "max_tokens",
            "seed",
        ] {
            if let Some(value) = Self::numeric_provider_option(task, key) {
                body[key] = value;
            }
        }

        if let Some(stop) = task.provider_options.get("stop") {
            body["stop"] = stop.clone();
        }

        if let Some(stream) = task.provider_options.get("stream") {
            body["stream"] = stream.clone();
        }

        if let Some(extra_tools) = task.provider_options.get("openai_builtin_tools") {
            let extras = match extra_tools {
                Value::Array(items) => items.clone(),
                Value::Object(_) => vec![extra_tools.clone()],
                _ => Vec::new(),
            };
            if !extras.is_empty() {
                match body.get_mut("tools") {
                    Some(Value::Array(tools)) => tools.extend(extras),
                    _ => {
                        body["tools"] = Value::Array(extras);
                    }
                }
            }
        }

        Ok(body)
    }

    fn parse_message_content(message: &Value) -> Option<String> {
        let content = message.get("content")?;

        if let Some(text) = content.as_str() {
            return Some(text.to_string());
        }

        let parts = content.as_array()?;
        let mut segments = Vec::new();
        for part in parts {
            let Some(text) = part
                .get("text")
                .and_then(Value::as_str)
                .or_else(|| part.get("content").and_then(Value::as_str))
            else {
                continue;
            };

            let trimmed = text.trim();
            if !trimmed.is_empty() {
                segments.push(trimmed.to_string());
            }
        }

        if segments.is_empty() {
            None
        } else {
            Some(segments.join("\n"))
        }
    }

    fn parse_structured_text(
        content: &str,
    ) -> (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<Value>,
        Option<Value>,
    ) {
        let Ok(value) = serde_json::from_str::<Value>(content) else {
            return (None, None, None, None, None);
        };

        let Some(object) = value.as_object() else {
            return (None, None, None, None, None);
        };

        let display_text = object
            .get("display_text")
            .or_else(|| object.get("content"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let spoken_text = object
            .get("spoken_text")
            .and_then(Value::as_str)
            .map(str::to_string);
        let memory_concept = object
            .get("memory_concept")
            .and_then(Value::as_str)
            .map(str::to_string);
        let memory_candidate = object
            .get("memory_candidate")
            .cloned()
            .or_else(|| object.get("memory").cloned());
        let active_plan = object.get("active_plan").cloned();

        (
            display_text,
            spoken_text,
            memory_concept,
            memory_candidate,
            active_plan,
        )
    }

    fn parse_tool_arguments(arguments: Option<&Value>) -> Value {
        let Some(arguments) = arguments else {
            return Value::Object(Map::new());
        };

        if let Some(raw) = arguments.as_str() {
            return serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()));
        }

        arguments.clone()
    }

    fn parse_tool_call(task: &ControllerTask, body: &Value) -> Result<Option<ProviderOutput>> {
        let Some(tool_calls) = body
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("message"))
            .and_then(|message| message.get("tool_calls"))
            .and_then(Value::as_array)
        else {
            return Ok(None);
        };

        let Some(tool_call) = tool_calls.first() else {
            return Ok(None);
        };

        let function = tool_call
            .get("function")
            .context("OpenAI tool_call missing function payload")?;
        let tool_name = function
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if tool_name.is_empty() {
            bail!("OpenAI tool_call missing function.name");
        }

        let arguments = Self::parse_tool_arguments(function.get("arguments"));
        if !arguments.is_object() {
            bail!(
                "OpenAI tool_call.arguments for [{}] must be an object",
                tool_name
            );
        }

        let allowed = task.tools.iter().any(|tool| {
            tool.get("tool_name")
                .and_then(Value::as_str)
                .map(|name| name == tool_name)
                .unwrap_or(false)
        });
        if !allowed {
            bail!("OpenAI returned unsupported tool_call [{}]", tool_name);
        }

        Ok(Some(ProviderOutput::ToolCall {
            tool_name,
            arguments,
        }))
    }

    fn parse_chat_response(task: &ControllerTask, body: &Value) -> Result<ProviderOutput> {
        if let Some(tool_call) = Self::parse_tool_call(task, body)? {
            return Ok(tool_call);
        }

        let choice = body
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .context("OpenAI response missing choices[0]")?;
        let message = choice
            .get("message")
            .context("OpenAI response missing message")?;

        let content = Self::parse_message_content(message).unwrap_or_default();
        if content.trim().is_empty() {
            bail!("OpenAI returned an empty response");
        }

        let (display_text, spoken_text, memory_concept, memory_candidate, active_plan) =
            Self::parse_structured_text(&content);

        Ok(ProviderOutput::Text {
            display_text: display_text.or_else(|| Some(content.clone())),
            content,
            spoken_text,
            partial_replies: Vec::new(),
            working_memory_delta: None,
            follow_up_questions: Vec::new(),
            intent_summary: None,
            memory_concept,
            memory_candidate,
            active_plan,
        })
    }

    fn parse_embedding_response(body: &Value, model: &str) -> Result<ProviderOutput> {
        let vector = body
            .get("data")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .and_then(|item| item.get("embedding"))
            .and_then(Value::as_array)
            .context("OpenAI embedding response missing data[0].embedding")?
            .iter()
            .map(|value| {
                value
                    .as_f64()
                    .map(|number| number as f32)
                    .context("OpenAI embedding response contains a non-numeric vector element")
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(ProviderOutput::Embedding {
            vector,
            model_gen: model.to_string(),
        })
    }

    async fn send_json(&self, path: &str, body: &Value) -> Result<reqwest::Response> {
        let mut request = self.http_client.post(self.endpoint_url(path)).json(body);
        if let Some(auth_header) = self.auth_header() {
            request = request.header(reqwest::header::AUTHORIZATION, auth_header);
        }
        if let Some(project_id) = self
            .project_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            request = request.header("OpenAI-Project", project_id);
        }
        let response = request.send().await?;
        Ok(response)
    }
}

#[async_trait]
impl ModelProvider for OpenAIProvider {
    fn id(&self) -> &'static str {
        "openai"
    }

    fn supports(&self, task: &ControllerTask) -> bool {
        matches!(
            task.kind,
            TaskKind::TextGenerate | TaskKind::MediaAnalyze | TaskKind::Embed
        )
    }

    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        match task.kind {
            TaskKind::TextGenerate | TaskKind::MediaAnalyze => {
                let body = self.chat_request_body(task)?;
                if std::env::var("PHILOTIC_DEBUG_MODEL_REQUESTS")
                    .ok()
                    .as_deref()
                    .map(|value| matches!(value, "1" | "true" | "TRUE" | "yes" | "YES"))
                    .unwrap_or(false)
                {
                    info!(
                        provider = self.id(),
                        model = ?task.model,
                        request = %serde_json::to_string_pretty(&body).unwrap_or_default(),
                        "OpenAI request payload"
                    );
                }

                let response = self.send_json("/v1/chat/completions", &body).await?;
                let status = response.status();
                let body = response.json::<Value>().await?;

                if !status.is_success() {
                    let error_message = body
                        .get("error")
                        .and_then(|error| error.get("message"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| body.to_string());
                    bail!("OpenAI API error ({}): {}", status.as_u16(), error_message);
                }

                Self::parse_chat_response(task, &body)
            }
            TaskKind::Embed => {
                let text = task
                    .composed_prompt_text()
                    .context("text.embed task missing input text")?;
                let model = self.default_embedding_model(task).to_string();
                let body = json!({
                    "model": model,
                    "input": text,
                });

                let response = self.send_json("/v1/embeddings", &body).await?;
                let status = response.status();
                let body = response.json::<Value>().await?;

                if !status.is_success() {
                    let error_message = body
                        .get("error")
                        .and_then(|error| error.get("message"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| body.to_string());
                    bail!(
                        "OpenAI embeddings API error ({}): {}",
                        status.as_u16(),
                        error_message
                    );
                }

                Self::parse_embedding_response(&body, &model)
            }
            other => bail!(
                "OpenAIProvider does not support task kind [{}]",
                other.as_str()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{OpenAIAuth, OpenAIProvider};
    use crate::controller::{ControllerTask, ModelProvider, ProviderOutput};
    use axum::{Json, Router, routing::post};
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;

    async fn spawn_test_server(handler: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, handler).await.unwrap();
        });
        (format!("http://{}", addr), handle)
    }

    #[test]
    fn auth_prefers_oauth_bearer_over_api_key() {
        assert_eq!(
            OpenAIProvider::auth_from_config(Some(" bearer-token ".into()), Some("key".into())),
            Some(OpenAIAuth::OAuthBearer("bearer-token".into()))
        );
        assert_eq!(
            OpenAIProvider::auth_from_config(None, Some(" key ".into())),
            Some(OpenAIAuth::ApiKey("key".into()))
        );
    }

    #[tokio::test]
    async fn chat_request_includes_tools_and_image_parts() {
        let captured = Arc::new(Mutex::new(None::<Value>));
        let captured_for_handler = Arc::clone(&captured);
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let captured_for_handler = Arc::clone(&captured_for_handler);
                async move {
                    *captured_for_handler.lock().await = Some(body);
                    Json(json!({
                        "choices": [{
                            "message": {
                                "content": "done"
                            }
                        }]
                    }))
                }
            }),
        );
        let (base_url, _handle) = spawn_test_server(app).await;

        let provider = OpenAIProvider::new(
            reqwest::Client::new(),
            Some(OpenAIAuth::ApiKey("secret".into())),
            Some(base_url),
            None,
            Some("gpt-test".into()),
            Some("text-embedding-3-small".into()),
        );
        let task = ControllerTask::from_value(&json!({
            "kind": "media.analyze",
            "prompt": "Describe the image",
            "attachments": [{
                "kind": "image",
                "mime_type": "image/png",
                "url": "https://example.com/test.png"
            }],
            "tools_for_model": [{
                "tool_name": "workspace.read",
                "description": "Read a file",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    }
                }
            }],
            "provider_options": {
                "response_format": { "type": "json_object" },
                "temperature": 0.2
            }
        }))
        .unwrap();

        let output = provider.invoke(&task).await.unwrap();
        assert!(matches!(output, ProviderOutput::Text { .. }));

        let body = captured
            .lock()
            .await
            .clone()
            .expect("request body captured");
        assert_eq!(body["model"], "gpt-test");
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["response_format"]["type"], "json_object");
        assert_eq!(body["messages"][0]["content"][0]["type"], "text");
        assert_eq!(body["messages"][0]["content"][1]["type"], "image_url");
        assert_eq!(body["tools"][0]["function"]["name"], "workspace.read");
    }

    #[tokio::test]
    async fn chat_request_includes_openai_capability_overrides() {
        let captured = Arc::new(Mutex::new(None::<Value>));
        let captured_for_handler = Arc::clone(&captured);
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<Value>| {
                let captured_for_handler = Arc::clone(&captured_for_handler);
                async move {
                    *captured_for_handler.lock().await = Some(body);
                    Json(json!({
                        "choices": [{
                            "message": {
                                "content": "done"
                            }
                        }]
                    }))
                }
            }),
        );
        let (base_url, _handle) = spawn_test_server(app).await;

        let provider = OpenAIProvider::new(
            reqwest::Client::new(),
            Some(OpenAIAuth::ApiKey("secret".into())),
            Some(base_url),
            None,
            None,
            None,
        );
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "prompt": "Summarize this",
            "provider_options": {
                "reasoning_effort": "high",
                "verbosity": "low",
                "background": true,
                "openai_builtin_tools": [{
                    "type": "web_search_preview"
                }]
            }
        }))
        .unwrap();

        let output = provider.invoke(&task).await.unwrap();
        assert!(matches!(output, ProviderOutput::Text { .. }));

        let body = captured
            .lock()
            .await
            .clone()
            .expect("request body captured");
        assert_eq!(body["reasoning_effort"], "high");
        assert_eq!(body["verbosity"], "low");
        assert_eq!(body["background"], true);
        assert_eq!(body["tools"][0]["type"], "web_search_preview");
    }

    #[tokio::test]
    async fn parses_tool_call_response() {
        let app = Router::new().route(
            "/v1/chat/completions",
            post(|| async move {
                Json(json!({
                    "choices": [{
                        "message": {
                            "tool_calls": [{
                                "id": "call_1",
                                "type": "function",
                                "function": {
                                    "name": "workspace.read",
                                    "arguments": "{\"path\":\"docs/task.md\"}"
                                }
                            }]
                        }
                    }]
                }))
            }),
        );
        let (base_url, _handle) = spawn_test_server(app).await;

        let provider = OpenAIProvider::new(
            reqwest::Client::new(),
            Some(OpenAIAuth::ApiKey("secret".into())),
            Some(base_url),
            None,
            None,
            None,
        );
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "context": { "active_turn": { "text": "run a tool" } },
            "tools_for_model": [{
                "tool_name": "workspace.read",
                "description": "Read a file",
                "input_schema": { "type": "object", "properties": { "path": { "type": "string" } } }
            }]
        }))
        .unwrap();

        let output = provider.invoke(&task).await.unwrap();
        assert_eq!(
            output,
            ProviderOutput::ToolCall {
                tool_name: "workspace.read".into(),
                arguments: json!({ "path": "docs/task.md" }),
            }
        );
    }

    #[tokio::test]
    async fn parses_structured_text_response() {
        let app = Router::new().route(
            "/v1/chat/completions",
            post(|| async move {
                Json(json!({
                    "choices": [{
                        "message": {
                            "content": "{\"display_text\":\"Hello\",\"spoken_text\":\"Hello there\",\"active_plan\":{\"goal\":\"test\"}}"
                        }
                    }]
                }))
            }),
        );
        let (base_url, _handle) = spawn_test_server(app).await;

        let provider = OpenAIProvider::new(
            reqwest::Client::new(),
            Some(OpenAIAuth::ApiKey("secret".into())),
            Some(base_url),
            None,
            None,
            None,
        );
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "context": { "active_turn": { "text": "summarize" } }
        }))
        .unwrap();

        let output = provider.invoke(&task).await.unwrap();
        match output {
            ProviderOutput::Text {
                content,
                display_text,
                spoken_text,
                active_plan,
                ..
            } => {
                assert_eq!(
                    content,
                    "{\"display_text\":\"Hello\",\"spoken_text\":\"Hello there\",\"active_plan\":{\"goal\":\"test\"}}"
                );
                assert_eq!(display_text.as_deref(), Some("Hello"));
                assert_eq!(spoken_text.as_deref(), Some("Hello there"));
                assert_eq!(active_plan, Some(json!({ "goal": "test" })));
            }
            other => panic!("unexpected output: {:?}", other),
        }
    }

    #[tokio::test]
    async fn parses_embedding_response() {
        let app = Router::new().route(
            "/v1/embeddings",
            post(|| async move {
                Json(json!({
                    "data": [{
                        "embedding": [0.1, 0.2, 0.3]
                    }]
                }))
            }),
        );
        let (base_url, _handle) = spawn_test_server(app).await;

        let provider = OpenAIProvider::new(
            reqwest::Client::new(),
            Some(OpenAIAuth::ApiKey("secret".into())),
            Some(base_url),
            None,
            None,
            None,
        );
        let task = ControllerTask::from_value(&json!({
            "kind": "text.embed",
            "prompt": "hello world"
        }))
        .unwrap();

        let output = provider.invoke(&task).await.unwrap();
        match output {
            ProviderOutput::Embedding { vector, model_gen } => {
                assert_eq!(vector, vec![0.1, 0.2, 0.3]);
                assert_eq!(model_gen, "text-embedding-3-small");
            }
            other => panic!("unexpected output: {:?}", other),
        }
    }
}
