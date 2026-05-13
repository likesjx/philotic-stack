use crate::controller::{
    ControllerTask, ModelProvider, ProviderOutput, ResponseRouteBucket, TaskKind,
};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use serde_json::{Map, Value, json};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::header::{AUTHORIZATION, HeaderValue},
    },
};
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
            base_url: {
                let b = base_url.unwrap_or_else(|| "https://api.openai.com".into());
                let b = b.trim_end_matches('/');
                let b = b.trim_end_matches("/v1");
                b.trim_end_matches('/').to_string()
            },
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
        if Self::realtime_websocket_requested(task) && task.model.is_none() {
            return "gpt-realtime";
        }

        if Self::native_audio_requested(task) && task.model.is_none() {
            return "gpt-realtime-1.5";
        }
        task.model.as_deref().unwrap_or(&self.default_model)
    }

    fn default_embedding_model<'a>(&'a self, task: &'a ControllerTask) -> &'a str {
        task.model
            .as_deref()
            .unwrap_or(&self.default_embedding_model)
    }

    fn native_audio_requested(task: &ControllerTask) -> bool {
        matches!(
            task.response_route_bucket(),
            ResponseRouteBucket::AudioMultimodal
        )
    }

    fn realtime_websocket_requested(task: &ControllerTask) -> bool {
        matches!(
            task.response_route_bucket(),
            ResponseRouteBucket::RealtimeWebsocket
        )
    }

    fn websocket_url(&self, model: &str) -> String {
        let base = self
            .base_url
            .replace("https://", "wss://")
            .replace("http://", "ws://");
        format!("{}/v1/realtime?model={}", base.trim_end_matches('/'), model)
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
            "stream": false,
        });

        if Self::native_audio_requested(task) {
            let modalities = if task.response_contract.modalities.is_empty() {
                vec!["text", "audio"]
            } else {
                task.response_contract
                    .modalities
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
            };
            body["modalities"] = json!(modalities);

            let voice = task
                .provider_option_str("voice_id")
                .or(task.voice.as_deref())
                .unwrap_or("alloy");
            let audio_format = task.provider_option_str("audio_format").unwrap_or("mp3");
            body["audio"] = json!({
                "voice": voice,
                "format": audio_format,
            });
        }

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

    fn realtime_request_body(&self, task: &ControllerTask) -> Result<Value> {
        let prompt = Self::prompt_text(task)?;
        let output_modalities = if task.response_contract.modalities.is_empty() {
            vec!["text".to_string()]
        } else {
            task.response_contract.modalities.clone()
        };
        let mut response = json!({
            "conversation": "none",
            "output_modalities": output_modalities,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": prompt,
                }]
            }],
        });

        if !task.tools.is_empty() {
            response["tools"] = Value::Array(Self::function_declarations(&task.tools));
            response["tool_choice"] = Value::String("auto".into());
        }

        if let Some(response_format) = Self::response_format(task) {
            response["response_format"] = response_format;
        }

        for key in ["reasoning_effort", "verbosity"] {
            if let Some(value) = Self::string_provider_option(task, key) {
                response[key] = Value::String(value);
            }
        }

        if let Some(background) = Self::bool_provider_option(task, "background") {
            response["background"] = Value::Bool(background);
        }

        Ok(json!({
            "type": "response.create",
            "response": response,
        }))
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

    fn parse_realtime_event(
        task: &ControllerTask,
        event: &Value,
        text_fragments: &mut Vec<String>,
        spoken_fragments: &mut Vec<String>,
        final_text: &mut Option<String>,
        final_spoken_text: &mut Option<String>,
        tool_call: &mut Option<(String, Value)>,
    ) -> Result<bool> {
        let Some(event_type) = event.get("type").and_then(Value::as_str) else {
            return Ok(false);
        };

        match event_type {
            "session.created" | "session.updated" => Ok(false),
            "response.output_text.delta" => {
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    let trimmed = delta.trim();
                    if !trimmed.is_empty() {
                        text_fragments.push(trimmed.to_string());
                    }
                }
                Ok(false)
            }
            "response.output_text.done" => {
                if let Some(text) = event.get("text").and_then(Value::as_str) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        *final_text = Some(trimmed.to_string());
                    }
                }
                Ok(false)
            }
            "response.output_audio_transcript.delta" => {
                if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                    let trimmed = delta.trim();
                    if !trimmed.is_empty() {
                        spoken_fragments.push(trimmed.to_string());
                    }
                }
                Ok(false)
            }
            "response.output_audio_transcript.done" => {
                if let Some(transcript) = event.get("transcript").and_then(Value::as_str) {
                    let trimmed = transcript.trim();
                    if !trimmed.is_empty() {
                        *final_spoken_text = Some(trimmed.to_string());
                    }
                }
                Ok(false)
            }
            "response.function_call_arguments.done" => {
                let tool_name = event
                    .get("name")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        event
                            .get("item")
                            .and_then(|item| item.get("name"))
                            .and_then(Value::as_str)
                    })
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if tool_name.is_empty() {
                    bail!("OpenAI realtime tool_call missing tool name");
                }

                let arguments = event
                    .get("arguments")
                    .cloned()
                    .or_else(|| {
                        event
                            .get("item")
                            .and_then(|item| item.get("arguments"))
                            .cloned()
                    })
                    .unwrap_or_else(|| Value::Object(Map::new()));
                let arguments = Self::parse_tool_arguments(Some(&arguments));
                if !arguments.is_object() {
                    bail!(
                        "OpenAI realtime tool_call.arguments for [{}] must be an object",
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
                    bail!(
                        "OpenAI realtime returned unsupported tool_call [{}]",
                        tool_name
                    );
                }

                *tool_call = Some((tool_name, arguments));
                Ok(false)
            }
            "response.output_item.done" => {
                if let Some(item) = event.get("item") {
                    if let Some(content) = item.get("content").and_then(Value::as_array) {
                        for part in content {
                            if let Some(text) = part
                                .get("text")
                                .and_then(Value::as_str)
                                .or_else(|| part.get("transcript").and_then(Value::as_str))
                            {
                                let trimmed = text.trim();
                                if !trimmed.is_empty() {
                                    text_fragments.push(trimmed.to_string());
                                }
                            }
                        }
                    }
                }
                Ok(false)
            }
            "response.done" => Ok(true),
            "error" => {
                let message = event
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("OpenAI realtime websocket returned an error");
                bail!("{}", message);
            }
            _ => Ok(false),
        }
    }

    async fn invoke_realtime_websocket(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        let model = self.default_model(task).to_string();
        let mut request = self.websocket_url(&model).into_client_request()?;
        if let Some(auth_header) = self.auth_header() {
            request
                .headers_mut()
                .insert(AUTHORIZATION, HeaderValue::from_str(&auth_header)?);
        }
        if let Some(project_id) = self
            .project_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            request
                .headers_mut()
                .insert("OpenAI-Project", HeaderValue::from_str(project_id)?);
        }

        let (ws_stream, _) = connect_async(request).await?;
        let (mut write, mut read) = ws_stream.split();

        let request = self.realtime_request_body(task)?;
        write
            .send(Message::Text(serde_json::to_string(&request)?.into()))
            .await?;

        let mut text_fragments = Vec::new();
        let mut spoken_fragments = Vec::new();
        let mut final_text = None;
        let mut final_spoken_text = None;
        let mut tool_call = None;

        while let Some(message) = read.next().await {
            let message = message?;
            let Message::Text(text) = message else {
                continue;
            };

            let event: Value = serde_json::from_str(&text).with_context(|| {
                format!("OpenAI realtime websocket returned invalid JSON: {}", text)
            })?;
            let done = Self::parse_realtime_event(
                task,
                &event,
                &mut text_fragments,
                &mut spoken_fragments,
                &mut final_text,
                &mut final_spoken_text,
                &mut tool_call,
            )?;
            if done {
                break;
            }
        }

        let content = final_text
            .or_else(|| {
                if text_fragments.is_empty() {
                    None
                } else {
                    Some(text_fragments.join(""))
                }
            })
            .or_else(|| final_spoken_text.clone())
            .or_else(|| {
                if spoken_fragments.is_empty() {
                    None
                } else {
                    Some(spoken_fragments.join(""))
                }
            })
            .context("OpenAI realtime websocket returned no text content")?;

        if let Some((tool_name, arguments)) = tool_call {
            return Ok(ProviderOutput::ToolCall {
                tool_name,
                arguments,
            });
        }

        Ok(ProviderOutput::Text {
            display_text: Some(content.clone()),
            content,
            spoken_text: final_spoken_text.or_else(|| {
                if spoken_fragments.is_empty() {
                    None
                } else {
                    Some(spoken_fragments.join(""))
                }
            }),
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
            model_gen: None,
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
                if Self::realtime_websocket_requested(task)
                    && matches!(task.kind, TaskKind::TextGenerate)
                {
                    return self.invoke_realtime_websocket(task).await;
                }

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
    use futures::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;
    use tokio_tungstenite::{accept_async, tungstenite::Message};

    async fn spawn_test_server(handler: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(listener, handler).await.unwrap();
        });
        (format!("http://{}", addr), handle)
    }

    #[test]
    fn base_url_strips_trailing_v1() {
        let provider = OpenAIProvider::new(
            reqwest::Client::new(),
            None,
            Some("http://localhost:11434/v1".into()),
            None,
            None,
            None,
        );
        assert_eq!(provider.base_url, "http://localhost:11434");
        // endpoint_url must not double-append /v1
        assert_eq!(
            provider.endpoint_url("/v1/chat/completions"),
            "http://localhost:11434/v1/chat/completions"
        );
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
    async fn chat_request_enables_native_audio_response_mode() {
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
                                "content": "{\"display_text\":\"Hello\",\"spoken_text\":\"Hello there\"}"
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
            "prompt": "Say hello",
            "response_contract": {
                "modalities": ["text", "audio"]
            },
            "provider_options": {
                "response_mode": "native_audio",
                "voice_id": "alloy",
                "audio_format": "mp3"
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
        assert_eq!(body["model"], "gpt-realtime-1.5");
        assert_eq!(body["modalities"], json!(["text", "audio"]));
        assert_eq!(body["audio"]["voice"], "alloy");
        assert_eq!(body["audio"]["format"], "mp3");
    }

    #[tokio::test]
    async fn realtime_websocket_request_streams_text_and_audio_transcript() {
        let captured = Arc::new(Mutex::new(None::<Value>));
        let captured_for_server = Arc::clone(&captured);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws_stream = accept_async(stream).await.unwrap();
            let (mut write, mut read) = ws_stream.split();

            write
                .send(Message::Text(
                    json!({
                        "type": "session.created",
                        "session": {
                            "model": "gpt-realtime"
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();

            let first = read.next().await.unwrap().unwrap();
            let Message::Text(text) = first else {
                panic!("expected text message from client");
            };
            *captured_for_server.lock().await = Some(serde_json::from_str(&text).unwrap());

            write
                .send(Message::Text(
                    json!({
                        "type": "response.output_text.delta",
                        "delta": "Hello "
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            write
                .send(Message::Text(
                    json!({
                        "type": "response.output_text.done",
                        "text": "Hello websocket"
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            write
                .send(Message::Text(
                    json!({
                        "type": "response.output_audio_transcript.done",
                        "transcript": "Hello websocket"
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
            write
                .send(Message::Text(
                    json!({
                        "type": "response.done",
                        "response": {
                            "status": "completed"
                        }
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .unwrap();
        });

        let provider = OpenAIProvider::new(
            reqwest::Client::new(),
            Some(OpenAIAuth::ApiKey("secret".into())),
            Some(format!("http://{}", addr)),
            None,
            None,
            None,
        );
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "prompt": "Say hello",
            "response_contract": {
                "modalities": ["text", "audio"]
            },
            "provider_options": {
                "response_mode": "realtime_websocket"
            }
        }))
        .unwrap();

        let output = provider.invoke(&task).await.unwrap();
        match output {
            ProviderOutput::Text {
                content,
                display_text,
                spoken_text,
                ..
            } => {
                assert_eq!(content, "Hello websocket");
                assert_eq!(display_text.as_deref(), Some("Hello websocket"));
                assert_eq!(spoken_text.as_deref(), Some("Hello websocket"));
            }
            other => panic!("unexpected output: {:?}", other),
        }

        let body = captured
            .lock()
            .await
            .clone()
            .expect("request body captured");
        assert_eq!(body["type"], "response.create");
        assert_eq!(
            body["response"]["output_modalities"],
            json!(["text", "audio"])
        );
        assert_eq!(
            body["response"]["input"][0]["content"][0]["type"],
            "input_text"
        );

        server.await.unwrap();
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
