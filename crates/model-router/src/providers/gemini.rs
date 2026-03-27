use crate::controller::{
    AttachmentInput, ControllerTask, ModelProvider, NativeLiveProvider, NativeLiveTurnOutput,
    ProviderOutput, TaskKind,
};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use futures::{SinkExt, StreamExt};
use media_prep::{PcmPrepPolicy, prepare_audio_ligand_for_pcm};
use serde_json::{Value, json};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::info;

const GEMINI_LIVE_DEFAULT_MODEL: &str = "gemini-3.1-flash-live-preview";
const GEMINI_AUDIO_TRANSCRIBE_DEFAULT_MODEL: &str = "gemini-3-flash-preview";
const GEMINI_LIVE_PROTOCOL: &str = "gemini-live-v1beta";
const GEMINI_LIVE_MESSAGE_TIMEOUT: Duration = Duration::from_secs(30);
type GeminiLiveSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

static GEMINI_LIVE_SESSION_POOL: LazyLock<Mutex<HashMap<String, GeminiLiveSession>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeminiAuth {
    ApiKey(String),
    OAuthBearer {
        access_token: String,
        project_id: Option<String>,
    },
}

pub struct GeminiProvider {
    http_client: reqwest::Client,
    auth: Option<GeminiAuth>,
    default_model: String,
    base_url: String,
}

#[derive(Debug, Default)]
struct LiveTurnAccumulator {
    text_fragments: Vec<String>,
    transcript_fragments: Vec<String>,
    generation_complete: bool,
    turn_complete: bool,
    session_marker: NativeLiveTurnOutputMarker,
}

#[derive(Debug, Default)]
struct NativeLiveTurnOutputMarker {
    resumption_handle: Option<String>,
}

struct GeminiLiveSession {
    ws: GeminiLiveSocket,
}

impl GeminiProvider {
    fn debug_model_requests_enabled() -> bool {
        matches!(
            std::env::var("PHILOTIC_DEBUG_MODEL_REQUESTS")
                .ok()
                .as_deref(),
            Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
        )
    }

    pub fn new(
        http_client: reqwest::Client,
        auth: Option<GeminiAuth>,
        base_url: Option<String>,
    ) -> Self {
        Self {
            http_client,
            auth,
            default_model: "gemini-flash-latest".into(),
            base_url: base_url
                .unwrap_or_else(|| "https://generativelanguage.googleapis.com".into())
                .trim_end_matches('/')
                .to_string(),
        }
    }

    pub fn auth_from_config(
        oauth_access_token: Option<String>,
        oauth_project_id: Option<String>,
        api_key: Option<String>,
    ) -> Option<GeminiAuth> {
        if let Some(access_token) = oauth_access_token
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty())
        {
            let project_id = oauth_project_id
                .map(|project| project.trim().to_string())
                .filter(|project| !project.is_empty());
            return Some(GeminiAuth::OAuthBearer {
                access_token,
                project_id,
            });
        }

        api_key
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
            .map(GeminiAuth::ApiKey)
    }

    fn endpoint_url(&self, model: Option<&str>) -> Result<String> {
        let model = model.unwrap_or(&self.default_model);

        match self
            .auth
            .as_ref()
            .context("Gemini auth missing from config; expected OAuth bearer or API key")?
        {
            GeminiAuth::ApiKey(api_key) => Ok(format!(
                "{}/v1beta/models/{}:generateContent?key={}",
                self.base_url, model, api_key
            )),
            GeminiAuth::OAuthBearer { .. } => Ok(format!(
                "{}/v1beta/models/{}:generateContent",
                self.base_url, model
            )),
        }
    }

    fn request_model<'a>(&'a self, task: &'a ControllerTask) -> &'a str {
        task.model.as_deref().unwrap_or_else(|| match task.kind {
            TaskKind::AudioTranscribe => GEMINI_AUDIO_TRANSCRIBE_DEFAULT_MODEL,
            _ => &self.default_model,
        })
    }

    fn live_endpoint_url(&self) -> Result<reqwest::Url> {
        let mut url = reqwest::Url::parse(&self.base_url)
            .with_context(|| format!("invalid Gemini base_url [{}]", self.base_url))?;
        let live_scheme = match url.scheme() {
            "https" => "wss",
            "http" => "ws",
            other => bail!(
                "Gemini Live requires http(s) base_url so it can derive ws(s); got [{}]",
                other
            ),
        };
        url.set_scheme(live_scheme).map_err(|_| {
            anyhow::anyhow!("failed to convert Gemini base_url to websocket scheme")
        })?;
        url.set_path(
            "/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent",
        );
        url.set_query(None);
        if let Some(GeminiAuth::ApiKey(api_key)) = self.auth.as_ref() {
            url.query_pairs_mut().append_pair("key", api_key);
        }
        Ok(url)
    }

    fn live_connect_request(&self) -> Result<axum::http::Request<()>> {
        let mut request = self
            .live_endpoint_url()?
            .to_string()
            .into_client_request()
            .context("failed to build Gemini Live websocket request")?;

        if let Some(GeminiAuth::OAuthBearer {
            access_token,
            project_id,
        }) = self.auth.as_ref()
        {
            request.headers_mut().insert(
                axum::http::header::AUTHORIZATION,
                axum::http::HeaderValue::from_str(&format!("Bearer {access_token}"))
                    .context("invalid Gemini OAuth authorization header")?,
            );
            if let Some(project_id) = project_id {
                request.headers_mut().insert(
                    "x-goog-user-project",
                    axum::http::HeaderValue::from_str(project_id)
                        .context("invalid Gemini OAuth project header")?,
                );
            }
        }

        Ok(request)
    }

    fn live_model_name(task: &ControllerTask) -> String {
        let model = task
            .model
            .as_deref()
            .or_else(|| task.routing_hints.model_ref.as_deref())
            .unwrap_or(GEMINI_LIVE_DEFAULT_MODEL);
        if model.starts_with("models/") {
            model.to_string()
        } else {
            format!("models/{model}")
        }
    }

    fn request_payload(prompt: &str) -> Value {
        json!({
            "contents": [{"parts": [{"text": prompt}]}]
        })
    }

    fn gemini_function_aliases(tools: &[serde_json::Value]) -> Vec<(String, String)> {
        let mut aliases = Vec::with_capacity(tools.len());
        let mut used = std::collections::HashSet::new();

        for (index, tool) in tools.iter().enumerate() {
            let original = tool
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if original.is_empty() {
                continue;
            }

            let mut alias: String = original
                .chars()
                .map(|ch| {
                    if ch.is_ascii_alphanumeric() || ch == '_' {
                        ch.to_ascii_lowercase()
                    } else {
                        '_'
                    }
                })
                .collect();
            alias = alias.trim_matches('_').to_string();
            if alias.is_empty() {
                alias = format!("tool_{}", index + 1);
            }
            if alias
                .chars()
                .next()
                .map(|ch| ch.is_ascii_digit())
                .unwrap_or(false)
            {
                alias = format!("tool_{}", alias);
            }

            let base = alias.clone();
            let mut suffix = 2usize;
            while !used.insert(alias.clone()) {
                alias = format!("{base}_{suffix}");
                suffix += 1;
            }

            aliases.push((alias, original.to_string()));
        }

        aliases
    }

    fn normalize_function_parameters(schema: &Value) -> Value {
        match schema {
            Value::Object(map) => {
                let mut normalized = serde_json::Map::new();
                for (key, value) in map {
                    let normalized_value = match key.as_str() {
                        "properties" => Value::Object(
                            value
                                .as_object()
                                .map(|props| {
                                    props
                                        .iter()
                                        .map(|(prop_name, prop_schema)| {
                                            (
                                                prop_name.clone(),
                                                Self::normalize_function_parameters(prop_schema),
                                            )
                                        })
                                        .collect::<serde_json::Map<String, Value>>()
                                })
                                .unwrap_or_default(),
                        ),
                        "items" => Self::normalize_function_parameters(value),
                        _ => value.clone(),
                    };
                    normalized.insert(key.clone(), normalized_value);
                }

                if !normalized.contains_key("type") {
                    let inferred = if normalized.contains_key("properties") {
                        "object"
                    } else if normalized.contains_key("items") {
                        "array"
                    } else {
                        "string"
                    };
                    normalized.insert("type".into(), Value::String(inferred.into()));
                }

                Value::Object(normalized)
            }
            _ => schema.clone(),
        }
    }

    fn function_declarations(tools: &[serde_json::Value]) -> Vec<Value> {
        let alias_map = Self::gemini_function_aliases(tools);
        tools
            .iter()
            .filter_map(|tool| {
                let tool_name = tool.get("tool_name").and_then(Value::as_str)?.trim();
                let alias = alias_map
                    .iter()
                    .find_map(|(alias, original)| (original == tool_name).then_some(alias))?;
                let description = tool
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .unwrap_or(tool_name);
                let parameters = tool
                    .get("input_schema")
                    .map(Self::normalize_function_parameters)
                    .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));

                Some(json!({
                    "name": alias,
                    "description": description,
                    "parameters": parameters
                }))
            })
            .collect()
    }

    /// Build a request payload for turns where tools are available.
    ///
    /// The model must output JSON with either:
    /// - `display_text` (and optionally `spoken_text`, `memory_candidate`) for a text response, OR
    /// - `tool_call` with `tool_name` + `arguments` to invoke a tool.
    ///
    /// Exactly one of `display_text` or `tool_call` must be present.
    fn tool_aware_request_payload(
        prompt: &str,
        tools: &[serde_json::Value],
        wants_concept: bool,
        wants_plan: bool,
    ) -> Value {
        let tool_list: String = tools
            .iter()
            .map(|t| {
                let name = t.get("tool_name").and_then(Value::as_str).unwrap_or("?");
                let desc = t.get("description").and_then(Value::as_str).unwrap_or("");
                let required = t
                    .get("input_schema")
                    .and_then(|schema| schema.get("required"))
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .filter(|text| !text.is_empty());
                let brief: String = desc.chars().take(120).collect();
                match required {
                    Some(required) => format!("  {name}: {brief} Required arguments: {required}."),
                    None => format!("  {name}: {brief}"),
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        let memory_instruction = if wants_concept {
            " Also include \"memory_candidate\" with fields: \"concept\" (short kebab-case slug), \
             \"content\" (compact autobiographical memory text for this exchange), and optional \
             \"tags\" (array of short strings)."
        } else {
            ""
        };

        let plan_instruction = if wants_plan {
            " When starting a multi-step task, describe your plan briefly in natural language before or after tool use when helpful."
        } else {
            ""
        };

        let system_text = format!(
            "You are an agent with tools. When a tool is needed, call one of the declared functions \
             instead of writing a JSON tool_call object by hand. Use function parameters exactly as \
             declared and include every required field.{}{}\n\
             When no tool is needed, output a JSON object with \"display_text\" (your reply, markdown fine) \
             and \"spoken_text\" (conversational version for voice, no markdown).\n\n\
             Available tools:\n{}",
            memory_instruction, plan_instruction, tool_list,
        );

        let mut properties = json!({
            "display_text": { "type": "STRING" },
            "spoken_text": { "type": "STRING" }
        });
        let required = vec!["display_text", "spoken_text"];

        if wants_concept {
            properties["memory_candidate"] = json!({
                "type": "OBJECT",
                "properties": {
                    "concept": { "type": "STRING" },
                    "content": { "type": "STRING" },
                    "tags": {
                        "type": "ARRAY",
                        "items": { "type": "STRING" }
                    }
                },
                "required": ["concept", "content"]
            });
        }
        if wants_plan {
            properties["active_plan"] = json!({
                "type": "OBJECT",
                "properties": {
                    "goal": { "type": "STRING" },
                    "status": { "type": "STRING" },
                    "steps": {
                        "type": "ARRAY",
                        "items": {
                            "type": "OBJECT",
                            "properties": {
                                "id": { "type": "INTEGER" },
                                "description": { "type": "STRING" },
                                "tool_name": { "type": "STRING" },
                                "status": { "type": "STRING" }
                            }
                        }
                    }
                }
            });
        }

        json!({
            "system_instruction": {
                "parts": [{ "text": system_text }]
            },
            "contents": [{"parts": [{"text": prompt}]}],
            "generationConfig": {
                "responseMimeType": "application/json",
                "responseSchema": {
                    "type": "OBJECT",
                    "properties": properties,
                    "required": required
                }
            },
            "tools": [
                {
                    "functionDeclarations": Self::function_declarations(tools)
                }
            ],
            "toolConfig": {
                "functionCallingConfig": {
                    "mode": "AUTO"
                }
            }
        })
    }

    fn structured_text_request_payload(
        prompt: &str,
        wants_concept: bool,
        wants_plan: bool,
    ) -> Value {
        let system_text = if wants_concept {
            "When generating your response, produce a JSON object with \"display_text\" \
             (your full response formatted for text display, markdown is fine), \
             \"spoken_text\" (a natural, expressive version for voice delivery — no markdown, \
             conversational tone, written to be heard), and \"memory_candidate\" \
             (an object with \"concept\" as a short kebab-case topic slug, \"content\" as a compact \
             autobiographical memory text for this exchange, and optional \"tags\" as an array of strings)."
        } else {
            "When generating your response, produce a JSON object with two fields: \
             \"display_text\" (your full response formatted for text display, \
             markdown is fine) and \"spoken_text\" (a natural, expressive version \
             for voice delivery — no markdown, conversational tone, written to be heard)."
        };

        let mut properties = json!({
            "display_text": { "type": "STRING" },
            "spoken_text": { "type": "STRING" }
        });
        let mut required = vec!["display_text", "spoken_text"];

        if wants_concept {
            properties["memory_candidate"] = json!({
                "type": "OBJECT",
                "properties": {
                    "concept": { "type": "STRING" },
                    "content": { "type": "STRING" },
                    "tags": {
                        "type": "ARRAY",
                        "items": { "type": "STRING" }
                    }
                },
                "required": ["concept", "content"]
            });
            required.push("memory_candidate");
        }
        if wants_plan {
            properties["active_plan"] = json!({
                "type": "OBJECT",
                "properties": {
                    "goal": { "type": "STRING" },
                    "status": { "type": "STRING" },
                    "steps": {
                        "type": "ARRAY",
                        "items": {
                            "type": "OBJECT",
                            "properties": {
                                "id": { "type": "INTEGER" },
                                "description": { "type": "STRING" },
                                "tool_name": { "type": "STRING" },
                                "status": { "type": "STRING" }
                            }
                        }
                    }
                }
            });
        }

        json!({
            "system_instruction": {
                "parts": [{ "text": system_text }]
            },
            "contents": [{"parts": [{"text": prompt}]}],
            "generationConfig": {
                "responseMimeType": "application/json",
                "responseSchema": {
                    "type": "OBJECT",
                    "properties": properties,
                    "required": required
                }
            }
        })
    }

    /// Parse a structured JSON response from Gemini.
    /// Returns `(display_text, spoken_text, memory_concept, memory_candidate, active_plan)`.
    fn parse_structured_response(
        status: reqwest::StatusCode,
        body: Value,
    ) -> (
        String,
        Option<String>,
        Option<String>,
        Option<Value>,
        Option<Value>,
    ) {
        let raw = Self::parse_response_text(status, body);
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&raw) {
            let display = parsed
                .get("display_text")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string);
            let spoken = parsed
                .get("spoken_text")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string);
            let concept = parsed
                .get("memory_concept")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::to_string);
            let memory_candidate = parsed.get("memory_candidate").cloned();
            let concept = concept.or_else(|| {
                memory_candidate
                    .as_ref()
                    .and_then(|candidate| candidate.get("concept"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string)
            });
            let active_plan = parsed.get("active_plan").cloned();
            if let Some(display) = display {
                return (display, spoken, concept, memory_candidate, active_plan);
            }
        }
        (raw, None, None, None, None)
    }

    async fn media_request_payload(&self, task: &ControllerTask) -> Result<Value> {
        let prompt = task
            .media_prompt()
            .context("Gemini media task missing prompt")?;
        let mut parts = vec![json!({ "text": prompt })];

        for attachment in task.media_attachments().iter().filter(|attachment| {
            attachment
                .url
                .as_deref()
                .map(|url| !url.trim().is_empty())
                .unwrap_or(false)
                && attachment
                    .transport_error
                    .as_deref()
                    .map(|error| error.trim().is_empty())
                    .unwrap_or(true)
        }) {
            let mime_type = attachment_mime_type(attachment)
                .with_context(|| format!("attachment {:?} missing mime type", attachment.kind))?;
            let url = attachment
                .url
                .as_deref()
                .context("media attachment missing download url")?;
            let response = self.http_client.get(url).send().await?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                bail!(
                    "failed to fetch media attachment from {}: HTTP {} {}",
                    url,
                    status,
                    body
                );
            }
            let bytes = response.bytes().await?;
            parts.push(json!({
                "inline_data": {
                    "mime_type": mime_type,
                    "data": BASE64_STANDARD.encode(bytes)
                }
            }));
        }

        Ok(json!({
            "contents": [{"parts": parts}]
        }))
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
            .and_then(|parts| {
                parts
                    .iter()
                    .find_map(|part| part.get("text").and_then(Value::as_str).map(str::to_string))
            })
            .unwrap_or_else(|| "Gemini returned an empty response.".into())
    }

    fn apply_auth_headers(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder> {
        match self
            .auth
            .as_ref()
            .context("Gemini auth missing from config; expected OAuth bearer or API key")?
        {
            GeminiAuth::ApiKey(_) => Ok(builder),
            GeminiAuth::OAuthBearer {
                access_token,
                project_id,
            } => {
                let builder = builder.bearer_auth(access_token);
                if let Some(project_id) = project_id {
                    Ok(builder.header("x-goog-user-project", project_id))
                } else {
                    Ok(builder)
                }
            }
        }
    }

    fn live_response_modalities(task: &ControllerTask) -> Vec<String> {
        if let Some(items) = task
            .provider_options
            .get("response_modalities")
            .and_then(Value::as_array)
        {
            let parsed = items
                .iter()
                .filter_map(Value::as_str)
                .map(|item| item.trim().to_ascii_uppercase())
                .filter(|item| !item.is_empty())
                .collect::<Vec<_>>();
            if !parsed.is_empty() {
                return parsed;
            }
        }

        match task.kind {
            TaskKind::VoiceDialogue => vec!["AUDIO".into()],
            TaskKind::ResponseGenerate => vec!["TEXT".into()],
            _ => vec!["TEXT".into()],
        }
    }

    fn live_setup_payload(&self, task: &ControllerTask) -> Value {
        let response_modalities = Self::live_response_modalities(task);
        let wants_audio_output = response_modalities.iter().any(|item| item == "AUDIO");
        let mut setup = json!({
            "model": Self::live_model_name(task),
            "generationConfig": {
                "responseModalities": response_modalities
            },
            "sessionResumption": {}
        });

        if let Some(handle) = task
            .provider_option_str("resumption_handle")
            .or_else(|| task.provider_option_str("session_resumption_handle"))
        {
            setup["sessionResumption"]["handle"] = Value::String(handle.to_string());
        }

        if !task.tools.is_empty() {
            setup["tools"] = json!([{
                "functionDeclarations": Self::function_declarations(&task.tools)
            }]);
        }

        if task.kind == TaskKind::VoiceDialogue {
            setup["realtimeInputConfig"] = json!({
                "automaticActivityDetection": {
                    "disabled": true
                }
            });
            setup["inputAudioTranscription"] = json!({});
        }

        if wants_audio_output {
            setup["outputAudioTranscription"] = json!({});
        }

        json!({ "setup": setup })
    }

    fn live_session_key(task: &ControllerTask) -> Option<String> {
        Some(format!(
            "{}:{}",
            task.session_id.as_deref()?.trim(),
            task.turn_id.as_deref()?.trim()
        ))
        .filter(|key| !key.contains(':') || !key.starts_with(':') && !key.ends_with(':'))
    }

    async fn store_live_session(task: &ControllerTask, ws: GeminiLiveSocket) {
        if let Some(key) = Self::live_session_key(task) {
            GEMINI_LIVE_SESSION_POOL
                .lock()
                .await
                .insert(key, GeminiLiveSession { ws });
        }
    }

    async fn take_live_session(task: &ControllerTask) -> Option<GeminiLiveSession> {
        let key = Self::live_session_key(task)?;
        GEMINI_LIVE_SESSION_POOL.lock().await.remove(&key)
    }

    fn live_tool_response_payload(task: &ControllerTask) -> Option<Value> {
        let live_tool_response = task.provider_options.get("live_tool_response")?;
        let function_call_id = live_tool_response.get("function_call_id")?.as_str()?.trim();
        let tool_name = live_tool_response.get("tool_name")?.as_str()?.trim();
        let tool_response = live_tool_response.get("tool_response")?.clone();
        if function_call_id.is_empty() || tool_name.is_empty() {
            return None;
        }

        let response = match tool_response {
            Value::Object(_) => tool_response,
            other => json!({ "result": other }),
        };

        Some(json!({
            "toolResponse": {
                "functionResponses": [{
                    "id": function_call_id,
                    "name": tool_name,
                    "response": response,
                }]
            }
        }))
    }

    fn live_client_prompt_message(task: &ControllerTask, turn_complete: bool) -> Option<Value> {
        let prompt = task.composed_prompt_text()?;
        if prompt.trim().is_empty() {
            return None;
        }
        Some(json!({
            "clientContent": {
                "turns": [{
                    "role": "user",
                    "parts": [{ "text": prompt }]
                }],
                "turnComplete": turn_complete
            }
        }))
    }

    async fn live_audio_chunks(&self, task: &ControllerTask) -> Result<Vec<Value>> {
        let mut chunks = Vec::new();
        for attachment in task.media_attachments().iter().filter(|attachment| {
            attachment
                .url
                .as_deref()
                .map(|url| !url.trim().is_empty())
                .unwrap_or(false)
                && attachment
                    .transport_error
                    .as_deref()
                    .map(|error| error.trim().is_empty())
                    .unwrap_or(true)
        }) {
            let mime_type = attachment_mime_type(attachment)
                .with_context(|| format!("attachment {:?} missing mime type", attachment.kind))?;
            let url = attachment
                .url
                .as_deref()
                .context("live audio attachment missing download url")?;
            let response = self.http_client.get(url).send().await?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                bail!(
                    "failed to fetch live audio attachment from {}: HTTP {} {}",
                    url,
                    status,
                    body
                );
            }
            let bytes = response.bytes().await?.to_vec();
            let prepared =
                prepare_audio_ligand_for_pcm(mime_type.as_ref(), bytes, &PcmPrepPolicy::default())
                    .await
                    .with_context(|| {
                        format!(
                            "failed to prepare live audio ligand from [{}] into Gemini PCM input",
                            mime_type
                        )
                    })?;
            chunks.push(json!({
                "realtimeInput": {
                    "audio": {
                        "mimeType": prepared.mime_type,
                        "data": BASE64_STANDARD.encode(prepared.bytes)
                    }
                }
            }));
        }

        if chunks.is_empty() {
            bail!("voice.dialogue requires at least one blob-backed PCM audio attachment");
        }

        Ok(chunks)
    }

    async fn send_live_json<S>(ws: &mut S, payload: &Value) -> Result<()>
    where
        S: futures::Sink<WsMessage, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
    {
        ws.send(WsMessage::Text(
            serde_json::to_string(payload)
                .context("failed to serialize Gemini Live websocket payload")?
                .into(),
        ))
        .await
        .context("failed to send Gemini Live websocket payload")
    }

    async fn recv_live_json<S>(ws: &mut S) -> Result<Value>
    where
        S: futures::Stream<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>> + Unpin,
    {
        loop {
            let message = timeout(GEMINI_LIVE_MESSAGE_TIMEOUT, ws.next())
                .await
                .context("timed out waiting for Gemini Live websocket message")?
                .context("Gemini Live websocket closed before completing the turn")?
                .context("Gemini Live websocket returned an error")?;

            match message {
                WsMessage::Text(text) => {
                    return serde_json::from_str(&text)
                        .context("failed to parse Gemini Live websocket JSON message");
                }
                WsMessage::Binary(bytes) => {
                    return serde_json::from_slice(&bytes)
                        .context("failed to parse Gemini Live websocket binary JSON message");
                }
                WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
                WsMessage::Close(frame) => {
                    bail!(
                        "Gemini Live websocket closed before completing the turn{}",
                        frame
                            .as_ref()
                            .map(|frame| format!(": {}", frame.reason))
                            .unwrap_or_default()
                    )
                }
                other => bail!("unexpected Gemini Live websocket frame: {:?}", other),
            }
        }
    }

    fn parse_live_tool_call(
        task: &ControllerTask,
        body: &Value,
    ) -> Result<Option<(ProviderOutput, Option<String>)>> {
        let alias_map = Self::gemini_function_aliases(&task.tools);
        let Some(function_call) = body
            .get("toolCall")
            .and_then(|value| value.get("functionCalls"))
            .and_then(Value::as_array)
            .and_then(|calls| calls.first())
        else {
            return Ok(None);
        };

        let alias = function_call
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if alias.is_empty() {
            return Ok(None);
        }

        let function_call_id = function_call
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(str::to_string);

        let tool_name = alias_map
            .iter()
            .find_map(|(gemini_alias, original)| {
                (gemini_alias == alias).then_some(original.clone())
            })
            .unwrap_or_else(|| alias.to_string());
        let arguments = function_call
            .get("args")
            .or_else(|| function_call.get("arguments"))
            .cloned()
            .unwrap_or_else(|| json!({}));
        Self::validate_tool_call(task, &tool_name, &arguments)?;

        Ok(Some((
            ProviderOutput::ToolCall {
                tool_name,
                arguments,
            },
            function_call_id,
        )))
    }

    fn absorb_live_server_content(acc: &mut LiveTurnAccumulator, body: &Value) {
        let Some(server_content) = body.get("serverContent") else {
            return;
        };

        if server_content
            .get("generationComplete")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            acc.generation_complete = true;
        }
        if server_content
            .get("turnComplete")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            acc.turn_complete = true;
        }

        if let Some(text) = server_content
            .get("outputTranscription")
            .and_then(|value| value.get("text"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            acc.transcript_fragments.push(text.to_string());
        }

        if let Some(parts) = server_content
            .get("modelTurn")
            .and_then(|value| value.get("parts"))
            .and_then(Value::as_array)
        {
            for part in parts {
                if let Some(text) = part
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                {
                    acc.text_fragments.push(text.to_string());
                }
            }
        }
    }

    fn absorb_live_session_marker(acc: &mut LiveTurnAccumulator, body: &Value) {
        let Some(update) = body.get("sessionResumptionUpdate") else {
            return;
        };

        let resumable = update
            .get("resumable")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let new_handle = update
            .get("newHandle")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|handle| !handle.is_empty());
        if resumable {
            acc.session_marker.resumption_handle = new_handle.map(str::to_string);
        }
    }

    fn finalize_live_output(
        task: &ControllerTask,
        acc: LiveTurnAccumulator,
    ) -> Result<NativeLiveTurnOutput> {
        let display_text = acc
            .text_fragments
            .iter()
            .last()
            .cloned()
            .or_else(|| acc.transcript_fragments.iter().last().cloned())
            .unwrap_or_default();

        if display_text.trim().is_empty() {
            bail!("Gemini Live returned an empty response");
        }

        let spoken_text = acc
            .transcript_fragments
            .iter()
            .last()
            .cloned()
            .or_else(|| (!display_text.is_empty()).then_some(display_text.clone()));

        let partial_text_deltas = if !acc.text_fragments.is_empty() {
            acc.text_fragments.clone()
        } else {
            acc.transcript_fragments.clone()
        };

        let session_marker = acc.session_marker.resumption_handle.map(|handle| {
            crate::controller::NativeLiveSessionMarker {
                provider_session_id: None,
                resumption_handle: Some(handle),
                protocol: Some(GEMINI_LIVE_PROTOCOL.into()),
            }
        });

        Ok(NativeLiveTurnOutput {
            final_output: ProviderOutput::Text {
                content: display_text.clone(),
                display_text: Some(display_text),
                spoken_text,
                partial_replies: partial_text_deltas.clone(),
                working_memory_delta: None,
                follow_up_questions: Vec::new(),
                intent_summary: None,
                memory_concept: None,
                memory_candidate: None,
                active_plan: None,
            },
            partial_text_deltas,
            session_marker,
            pending_function_call_id: None,
            generation_complete: acc.generation_complete,
            turn_complete: acc.turn_complete || task.kind == TaskKind::ResponseGenerate,
        })
    }
}

#[async_trait]
impl ModelProvider for GeminiProvider {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn supports(&self, task: &ControllerTask) -> bool {
        matches!(
            task.kind,
            TaskKind::TextGenerate | TaskKind::MediaAnalyze | TaskKind::AudioTranscribe
        )
    }

    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        let has_tools = !task.tools.is_empty();
        let wants_concept = task.wants_channel("memory_concept");
        let wants_plan = task.wants_channel("active_plan");
        let use_structured = task.kind == TaskKind::TextGenerate
            && (has_tools || task.wants_channel("spoken_text") || wants_concept || wants_plan);

        let payload = match task.kind {
            TaskKind::TextGenerate => {
                let prompt = task
                    .composed_prompt_text()
                    .context("Gemini text task missing prompt")?;
                if has_tools {
                    Self::tool_aware_request_payload(
                        &prompt,
                        &task.tools,
                        wants_concept,
                        wants_plan,
                    )
                } else if use_structured {
                    Self::structured_text_request_payload(&prompt, wants_concept, wants_plan)
                } else {
                    Self::request_payload(&prompt)
                }
            }
            TaskKind::MediaAnalyze | TaskKind::AudioTranscribe => {
                self.media_request_payload(task).await?
            }
            TaskKind::ResponseGenerate => {
                bail!("Gemini native response.generate is not wired yet in this provider")
            }
            TaskKind::VoiceDialogue => {
                bail!("Gemini native voice.dialogue is not wired yet in this provider")
            }
            TaskKind::VoiceSynthesize => bail!("Gemini does not support voice synthesis"),
            TaskKind::Embed => bail!("Gemini does not support local embedding (use OnnxProvider)"),
        };

        if Self::debug_model_requests_enabled() && task.kind == TaskKind::TextGenerate {
            let prompt = task
                .composed_prompt_text()
                .unwrap_or_else(|| "<missing prompt>".into());
            info!(
                "PHILOTIC_DEBUG_MODEL_REQUESTS gemini composed prompt provider={} model={:?}:\n{}",
                ModelProvider::id(self),
                task.model,
                prompt
            );
            match serde_json::to_string_pretty(&payload) {
                Ok(json) => info!(
                    "PHILOTIC_DEBUG_MODEL_REQUESTS gemini provider payload provider={} model={:?}:\n{}",
                    ModelProvider::id(self),
                    task.model,
                    json
                ),
                Err(err) => info!(
                    "PHILOTIC_DEBUG_MODEL_REQUESTS gemini payload serialization failed: {}",
                    err
                ),
            }
        }

        let url = self.endpoint_url(Some(self.request_model(task)))?;
        let req = self.http_client.post(url).json(&payload);
        let response = self.apply_auth_headers(req)?.send().await?;
        let status = response.status();
        let body = response.json::<Value>().await?;

        if use_structured {
            if has_tools {
                if let Some(tool_call) = Self::parse_native_function_call(task, &body)? {
                    return Ok(tool_call);
                }
            }

            let (content, spoken_text, memory_concept, memory_candidate, active_plan) =
                Self::parse_structured_response(status, body.clone());

            if let Ok(parsed) = serde_json::from_str::<Value>(&content) {
                if let Some(tool_call) = Self::parse_tool_call_candidate(task, &parsed)? {
                    return Ok(tool_call);
                }
            }

            if content.trim().is_empty() {
                bail!("Gemini returned an empty response");
            }
            Ok(ProviderOutput::Text {
                display_text: Some(content.clone()),
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
        } else {
            let content = Self::parse_response_text(status, body);
            if content.trim().is_empty() {
                bail!("Gemini returned an empty response");
            }
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
            })
        }
    }
}

#[async_trait]
impl NativeLiveProvider for GeminiProvider {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn supports_live(&self, task: &ControllerTask) -> bool {
        matches!(
            task.kind,
            TaskKind::ResponseGenerate | TaskKind::VoiceDialogue
        )
    }

    async fn invoke_live(&self, task: &ControllerTask) -> Result<NativeLiveTurnOutput> {
        let continuing_tool_response = task.provider_options.contains_key("live_tool_response");
        let mut ws = if let Some(existing_session) = Self::take_live_session(task).await {
            existing_session.ws
        } else {
            if continuing_tool_response {
                bail!(
                    "Gemini Live tool-response continuation requested without an active live session"
                );
            }
            let request = self.live_connect_request()?;
            let (mut ws, _) = connect_async(request)
                .await
                .context("failed to connect to Gemini Live websocket endpoint")?;

            Self::send_live_json(&mut ws, &self.live_setup_payload(task)).await?;

            loop {
                let message = Self::recv_live_json(&mut ws).await?;
                if message.get("setupComplete").is_some() {
                    break;
                }
                if let Some((tool_call, function_call_id)) =
                    Self::parse_live_tool_call(task, &message)?
                {
                    Self::store_live_session(task, ws).await;
                    return Ok(NativeLiveTurnOutput {
                        final_output: tool_call,
                        partial_text_deltas: Vec::new(),
                        session_marker: None,
                        pending_function_call_id: function_call_id,
                        generation_complete: false,
                        turn_complete: false,
                    });
                }
            }

            ws
        };

        if let Some(tool_response_payload) = Self::live_tool_response_payload(task) {
            Self::send_live_json(&mut ws, &tool_response_payload).await?;
        }

        match task.kind {
            TaskKind::ResponseGenerate => {
                if !continuing_tool_response {
                    if let Some(message) = Self::live_client_prompt_message(task, true) {
                        Self::send_live_json(&mut ws, &message).await?;
                    } else {
                        bail!("response.generate task missing prompt for Gemini Live");
                    }
                }
            }
            TaskKind::VoiceDialogue => {
                if !continuing_tool_response {
                    if let Some(message) = Self::live_client_prompt_message(task, false) {
                        Self::send_live_json(&mut ws, &message).await?;
                    }
                    Self::send_live_json(
                        &mut ws,
                        &json!({ "realtimeInput": { "activityStart": {} } }),
                    )
                    .await?;
                    for chunk in self.live_audio_chunks(task).await? {
                        Self::send_live_json(&mut ws, &chunk).await?;
                    }
                    Self::send_live_json(
                        &mut ws,
                        &json!({ "realtimeInput": { "activityEnd": {} } }),
                    )
                    .await?;
                }
            }
            other => bail!(
                "Gemini Live native provider received unsupported live task [{}]",
                other.as_str()
            ),
        }

        let mut acc = LiveTurnAccumulator::default();
        loop {
            let message = Self::recv_live_json(&mut ws).await?;
            Self::absorb_live_session_marker(&mut acc, &message);

            if let Some((tool_call, function_call_id)) = Self::parse_live_tool_call(task, &message)?
            {
                Self::store_live_session(task, ws).await;
                return Ok(NativeLiveTurnOutput {
                    final_output: tool_call,
                    partial_text_deltas: if !acc.text_fragments.is_empty() {
                        acc.text_fragments.clone()
                    } else {
                        acc.transcript_fragments.clone()
                    },
                    session_marker: acc.session_marker.resumption_handle.map(|handle| {
                        crate::controller::NativeLiveSessionMarker {
                            provider_session_id: None,
                            resumption_handle: Some(handle),
                            protocol: Some(GEMINI_LIVE_PROTOCOL.into()),
                        }
                    }),
                    pending_function_call_id: function_call_id,
                    generation_complete: acc.generation_complete,
                    turn_complete: acc.turn_complete,
                });
            }

            Self::absorb_live_server_content(&mut acc, &message);

            if acc.turn_complete {
                break;
            }
        }

        Self::finalize_live_output(task, acc)
    }
}

impl GeminiProvider {
    fn parse_native_function_call(
        task: &ControllerTask,
        body: &Value,
    ) -> Result<Option<ProviderOutput>> {
        let alias_map = Self::gemini_function_aliases(&task.tools);
        let Some(parts) = body
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|candidates| candidates.first())
            .and_then(|candidate| candidate.get("content"))
            .and_then(|content| content.get("parts"))
            .and_then(Value::as_array)
        else {
            return Ok(None);
        };

        for part in parts {
            let Some(function_call) = part
                .get("functionCall")
                .or_else(|| part.get("function_call"))
            else {
                continue;
            };

            let alias = function_call
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if alias.is_empty() {
                continue;
            }

            let tool_name = alias_map
                .iter()
                .find_map(|(gemini_alias, original)| {
                    (gemini_alias == alias).then_some(original.clone())
                })
                .unwrap_or_else(|| alias.to_string());
            let arguments = function_call
                .get("args")
                .or_else(|| function_call.get("arguments"))
                .cloned()
                .context("functionCall.args missing from Gemini response")?;
            Self::validate_tool_call(task, &tool_name, &arguments)?;
            return Ok(Some(ProviderOutput::ToolCall {
                tool_name,
                arguments,
            }));
        }

        Ok(None)
    }

    fn parse_tool_call_candidate(
        task: &ControllerTask,
        parsed: &Value,
    ) -> Result<Option<ProviderOutput>> {
        let Some(tc) = parsed.get("tool_call") else {
            return Ok(None);
        };

        let tool_name = tc
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if tool_name.is_empty() {
            return Ok(None);
        }

        let arguments = tc
            .get("arguments")
            .cloned()
            .context("tool_call.arguments missing from Gemini response")?;
        Self::validate_tool_call(task, &tool_name, &arguments)?;

        Ok(Some(ProviderOutput::ToolCall {
            tool_name,
            arguments,
        }))
    }

    fn validate_tool_call(task: &ControllerTask, tool_name: &str, arguments: &Value) -> Result<()> {
        let Some(tool_def) = task.tools.iter().find(|tool| {
            tool.get("tool_name")
                .and_then(Value::as_str)
                .map(|name| name == tool_name)
                .unwrap_or(false)
        }) else {
            bail!("Gemini returned unsupported tool_call [{}]", tool_name);
        };

        let args_obj = arguments.as_object().with_context(|| {
            format!("tool_call.arguments for [{}] must be an object", tool_name)
        })?;

        let required = tool_def
            .get("input_schema")
            .and_then(|schema| schema.get("required"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        for field in required.iter().filter_map(Value::as_str) {
            let value = args_obj.get(field);
            if value.is_none() || value.is_some_and(Value::is_null) {
                bail!(
                    "Gemini returned invalid tool_call [{}]: missing required argument [{}]",
                    tool_name,
                    field
                );
            }
        }

        if let Some(properties) = tool_def
            .get("input_schema")
            .and_then(|schema| schema.get("properties"))
            .and_then(Value::as_object)
        {
            for (field, value) in args_obj {
                let Some(expected_type) = properties
                    .get(field)
                    .and_then(|property| property.get("type"))
                    .and_then(Value::as_str)
                else {
                    continue;
                };

                let type_ok = match expected_type {
                    "string" => value.is_string(),
                    "integer" => value.as_i64().is_some(),
                    "number" => value.as_f64().is_some(),
                    "boolean" => value.is_boolean(),
                    "array" => value.is_array(),
                    "object" => value.is_object(),
                    _ => true,
                };

                if !type_ok {
                    bail!(
                        "Gemini returned invalid tool_call [{}]: argument [{}] did not match expected type [{}]",
                        tool_name,
                        field,
                        expected_type
                    );
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{GeminiAuth, GeminiProvider};
    use crate::controller::{
        AttachmentInput, ContextEnvelope, ControllerTask, NativeLiveProvider, RequestClass,
        RoutingHints, TaskKind,
    };

    fn minimal_text_task_with_tools(tools: Vec<serde_json::Value>) -> ControllerTask {
        ControllerTask {
            kind: TaskKind::TextGenerate,
            request_class: RequestClass::Cognitive,
            session_id: None,
            turn_id: None,
            provider: None,
            model: None,
            prompt: Some("test prompt".into()),
            text: None,
            spoken_text: None,
            display_text: None,
            voice: None,
            voice_id: None,
            output_format: None,
            language_code: None,
            response_contract: Default::default(),
            context: Default::default(),
            context_projection: Default::default(),
            affordances: Default::default(),
            routing_hints: RoutingHints::default(),
            provider_options: Default::default(),
            effective_rights: Vec::new(),
            tools,
        }
    }

    #[test]
    fn prefers_oauth_bearer_over_api_key() {
        let auth = GeminiProvider::auth_from_config(
            Some(" oauth-token ".into()),
            Some(" test-project ".into()),
            Some("api-key".into()),
        )
        .unwrap();

        assert_eq!(
            auth,
            GeminiAuth::OAuthBearer {
                access_token: "oauth-token".into(),
                project_id: Some("test-project".into())
            }
        );
    }

    #[test]
    fn falls_back_to_api_key_when_oauth_missing() {
        let auth = GeminiProvider::auth_from_config(
            None,
            Some("ignored-project".into()),
            Some(" api ".into()),
        )
        .unwrap();

        assert_eq!(auth, GeminiAuth::ApiKey("api".into()));
    }

    #[test]
    fn oauth_endpoint_omits_query_api_key() {
        let provider = GeminiProvider::new(
            reqwest::Client::new(),
            Some(GeminiAuth::OAuthBearer {
                access_token: "oauth-token".into(),
                project_id: Some("proj".into()),
            }),
            None,
        );

        let url = provider.endpoint_url(Some("gemini-2.5-pro")).unwrap();
        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent"
        );
    }

    #[test]
    fn oauth_auth_adds_bearer_and_project_headers() {
        let provider = GeminiProvider::new(
            reqwest::Client::new(),
            Some(GeminiAuth::OAuthBearer {
                access_token: "oauth-token".into(),
                project_id: Some("proj-123".into()),
            }),
            None,
        );

        let request = provider
            .apply_auth_headers(reqwest::Client::new().post("https://example.com"))
            .unwrap()
            .build()
            .unwrap();

        assert_eq!(
            request.headers().get("authorization").unwrap(),
            "Bearer oauth-token"
        );
        assert_eq!(
            request.headers().get("x-goog-user-project").unwrap(),
            "proj-123"
        );
    }

    #[test]
    fn api_key_endpoint_keeps_query_key() {
        let provider = GeminiProvider::new(
            reqwest::Client::new(),
            Some(GeminiAuth::ApiKey("api-key".into())),
            None,
        );

        let url = provider.endpoint_url(Some("gemini-2.5-flash")).unwrap();
        assert_eq!(
            url,
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:generateContent?key=api-key"
        );
    }

    #[test]
    fn base_url_override_changes_endpoint_host() {
        let provider = GeminiProvider::new(
            reqwest::Client::new(),
            Some(GeminiAuth::OAuthBearer {
                access_token: "oauth-token".into(),
                project_id: Some("proj".into()),
            }),
            Some("http://127.0.0.1:40123".into()),
        );

        let url = provider.endpoint_url(Some("gemini-2.5-flash")).unwrap();
        assert_eq!(
            url,
            "http://127.0.0.1:40123/v1beta/models/gemini-2.5-flash:generateContent"
        );
    }

    #[test]
    fn live_api_key_endpoint_uses_ws_path_and_query_key() {
        let provider = GeminiProvider::new(
            reqwest::Client::new(),
            Some(GeminiAuth::ApiKey("api-key".into())),
            None,
        );

        let url = provider.live_endpoint_url().unwrap();
        assert_eq!(
            url.as_str(),
            "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key=api-key"
        );
    }

    #[test]
    fn live_oauth_request_uses_bearer_headers() {
        let provider = GeminiProvider::new(
            reqwest::Client::new(),
            Some(GeminiAuth::OAuthBearer {
                access_token: "oauth-token".into(),
                project_id: Some("proj-123".into()),
            }),
            None,
        );

        let request = provider.live_connect_request().unwrap();
        assert_eq!(
            request.headers().get("authorization").unwrap(),
            "Bearer oauth-token"
        );
        assert_eq!(
            request.headers().get("x-goog-user-project").unwrap(),
            "proj-123"
        );
    }

    #[test]
    fn image_attachment_defaults_to_jpeg() {
        let attachment = AttachmentInput {
            kind: Some("photo".into()),
            file_id: Some("photo-1".into()),
            mime_type: None,
            url: Some("http://127.0.0.1:9001/download/sha256-1".into()),
            blob_ref: Some("sha256-1".into()),
            transport_error: None,
        };

        assert_eq!(
            super::attachment_mime_type(&attachment).as_deref(),
            Some("image/jpeg")
        );
    }

    #[test]
    fn markdown_document_mime_normalizes_to_text_plain() {
        let attachment = AttachmentInput {
            kind: Some("document".into()),
            file_id: Some("doc-1".into()),
            mime_type: Some("text/x-web-markdown".into()),
            url: Some("http://127.0.0.1:9001/download/sha256-doc".into()),
            blob_ref: Some("sha256-doc".into()),
            transport_error: None,
        };

        assert_eq!(
            super::attachment_mime_type(&attachment).as_deref(),
            Some("text/plain")
        );
    }

    #[test]
    fn gemini_supports_media_analysis_tasks() {
        let provider = GeminiProvider::new(
            reqwest::Client::new(),
            Some(GeminiAuth::ApiKey("api-key".into())),
            None,
        );
        let task = ControllerTask {
            kind: TaskKind::MediaAnalyze,
            request_class: RequestClass::Transform,
            session_id: None,
            turn_id: None,
            provider: None,
            model: None,
            prompt: Some("Describe this media".into()),
            text: None,
            spoken_text: None,
            display_text: None,
            voice: None,
            voice_id: None,
            output_format: None,
            language_code: None,
            response_contract: Default::default(),
            context: ContextEnvelope {
                attachments: vec![AttachmentInput {
                    kind: Some("voice".into()),
                    file_id: Some("voice-1".into()),
                    mime_type: Some("audio/ogg".into()),
                    url: Some("http://127.0.0.1:9001/download/sha256-2".into()),
                    blob_ref: Some("sha256-2".into()),
                    transport_error: None,
                }],
                ..Default::default()
            },
            context_projection: Default::default(),
            affordances: Default::default(),
            routing_hints: RoutingHints::default(),
            provider_options: Default::default(),
            effective_rights: Vec::new(),
            tools: vec![],
        };

        assert!(crate::controller::ModelProvider::supports(&provider, &task));
    }

    #[test]
    fn gemini_supports_audio_transcribe_tasks() {
        let provider = GeminiProvider::new(
            reqwest::Client::new(),
            Some(GeminiAuth::ApiKey("api-key".into())),
            None,
        );
        let task = ControllerTask {
            kind: TaskKind::AudioTranscribe,
            request_class: RequestClass::Transform,
            session_id: None,
            turn_id: None,
            provider: None,
            model: None,
            prompt: Some("Transcribe this audio verbatim.".into()),
            text: None,
            spoken_text: None,
            display_text: None,
            voice: None,
            voice_id: None,
            output_format: None,
            language_code: None,
            response_contract: Default::default(),
            context: ContextEnvelope {
                attachments: vec![AttachmentInput {
                    kind: Some("voice".into()),
                    file_id: Some("voice-1".into()),
                    mime_type: Some("audio/ogg".into()),
                    url: Some("http://127.0.0.1:9001/download/sha256-voice-1".into()),
                    blob_ref: Some("sha256-voice-1".into()),
                    transport_error: None,
                }],
                ..Default::default()
            },
            context_projection: Default::default(),
            affordances: Default::default(),
            routing_hints: RoutingHints::default(),
            provider_options: Default::default(),
            effective_rights: Vec::new(),
            tools: vec![],
        };

        assert!(crate::controller::ModelProvider::supports(&provider, &task));
    }

    #[test]
    fn audio_transcribe_defaults_to_gemini_3_flash_preview() {
        let provider = GeminiProvider::new(
            reqwest::Client::new(),
            Some(GeminiAuth::ApiKey("api-key".into())),
            None,
        );
        let task = ControllerTask {
            kind: TaskKind::AudioTranscribe,
            request_class: RequestClass::Transform,
            session_id: None,
            turn_id: None,
            provider: None,
            model: None,
            prompt: Some("Transcribe this audio verbatim.".into()),
            text: None,
            spoken_text: None,
            display_text: None,
            voice: None,
            voice_id: None,
            output_format: None,
            language_code: None,
            response_contract: Default::default(),
            context: ContextEnvelope::default(),
            context_projection: Default::default(),
            affordances: Default::default(),
            routing_hints: RoutingHints::default(),
            provider_options: Default::default(),
            effective_rights: Vec::new(),
            tools: vec![],
        };

        assert_eq!(
            provider.request_model(&task),
            super::GEMINI_AUDIO_TRANSCRIBE_DEFAULT_MODEL
        );
    }

    #[test]
    fn gemini_supports_native_live_task_kinds_on_live_provider_seam() {
        let provider = GeminiProvider::new(
            reqwest::Client::new(),
            Some(GeminiAuth::ApiKey("api-key".into())),
            None,
        );
        let response_generate = ControllerTask {
            kind: TaskKind::ResponseGenerate,
            request_class: RequestClass::Cognitive,
            session_id: None,
            turn_id: None,
            provider: None,
            model: Some("gemini-3.1-flash-live-preview".into()),
            prompt: Some("Respond with native audio and text.".into()),
            text: None,
            spoken_text: None,
            display_text: None,
            voice: None,
            voice_id: None,
            output_format: None,
            language_code: None,
            response_contract: Default::default(),
            context: Default::default(),
            context_projection: Default::default(),
            affordances: Default::default(),
            routing_hints: RoutingHints::default(),
            provider_options: Default::default(),
            effective_rights: Vec::new(),
            tools: vec![],
        };
        let voice_dialogue = ControllerTask {
            kind: TaskKind::VoiceDialogue,
            request_class: RequestClass::Cognitive,
            session_id: None,
            turn_id: None,
            provider: None,
            model: Some("gemini-3.1-flash-live-preview".into()),
            prompt: Some("Continue this live conversation.".into()),
            text: None,
            spoken_text: None,
            display_text: None,
            voice: None,
            voice_id: None,
            output_format: None,
            language_code: None,
            response_contract: Default::default(),
            context: ContextEnvelope {
                attachments: vec![AttachmentInput {
                    kind: Some("voice".into()),
                    file_id: Some("voice-1".into()),
                    mime_type: Some("audio/ogg".into()),
                    url: Some("http://127.0.0.1:9001/download/sha256-voice-1".into()),
                    blob_ref: Some("sha256-voice-1".into()),
                    transport_error: None,
                }],
                ..Default::default()
            },
            context_projection: Default::default(),
            affordances: Default::default(),
            routing_hints: RoutingHints::default(),
            provider_options: Default::default(),
            effective_rights: Vec::new(),
            tools: vec![],
        };

        assert!(provider.supports_live(&response_generate));
        assert!(provider.supports_live(&voice_dialogue));
    }

    #[test]
    fn live_setup_payload_enables_audio_transcription_for_voice_dialogue() {
        let task = ControllerTask {
            kind: TaskKind::VoiceDialogue,
            request_class: RequestClass::Cognitive,
            session_id: None,
            turn_id: None,
            provider: None,
            model: Some("gemini-3.1-flash-live-preview".into()),
            prompt: Some("Continue this live conversation.".into()),
            text: None,
            spoken_text: None,
            display_text: None,
            voice: None,
            voice_id: None,
            output_format: None,
            language_code: None,
            response_contract: Default::default(),
            context: ContextEnvelope {
                attachments: vec![AttachmentInput {
                    kind: Some("voice".into()),
                    file_id: Some("voice-1".into()),
                    mime_type: Some("audio/pcm;rate=16000".into()),
                    url: Some("http://127.0.0.1:9001/download/sha256-voice-1".into()),
                    blob_ref: Some("sha256-voice-1".into()),
                    transport_error: None,
                }],
                ..Default::default()
            },
            context_projection: Default::default(),
            affordances: Default::default(),
            routing_hints: RoutingHints::default(),
            provider_options: Default::default(),
            effective_rights: Vec::new(),
            tools: vec![],
        };

        let provider = GeminiProvider::new(
            reqwest::Client::new(),
            Some(GeminiAuth::ApiKey("api-key".into())),
            None,
        );
        let payload = provider.live_setup_payload(&task);

        assert_eq!(
            payload["setup"]["generationConfig"]["responseModalities"],
            serde_json::json!(["AUDIO"])
        );
        assert!(payload["setup"]["inputAudioTranscription"].is_object());
        assert!(payload["setup"]["outputAudioTranscription"].is_object());
        assert_eq!(
            payload["setup"]["realtimeInputConfig"]["automaticActivityDetection"]["disabled"],
            serde_json::json!(true)
        );
    }

    #[test]
    fn rejects_tool_call_missing_required_argument() {
        let task = minimal_text_task_with_tools(vec![serde_json::json!({
            "tool_name": "echo",
            "description": "Echo text",
            "input_schema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                },
                "required": ["text"]
            }
        })]);

        let parsed = serde_json::json!({
            "tool_call": {
                "tool_name": "echo",
                "arguments": {}
            }
        });

        let err = GeminiProvider::parse_tool_call_candidate(&task, &parsed)
            .expect_err("missing required argument should fail");
        assert!(err.to_string().contains("missing required argument [text]"));
    }

    #[test]
    fn accepts_tool_call_with_required_argument() {
        let task = minimal_text_task_with_tools(vec![serde_json::json!({
            "tool_name": "echo",
            "description": "Echo text",
            "input_schema": {
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                },
                "required": ["text"]
            }
        })]);

        let parsed = serde_json::json!({
            "tool_call": {
                "tool_name": "echo",
                "arguments": { "text": "hello" }
            }
        });

        let output = GeminiProvider::parse_tool_call_candidate(&task, &parsed)
            .expect("valid tool call should parse")
            .expect("tool call should be present");
        assert_eq!(
            output,
            crate::controller::ProviderOutput::ToolCall {
                tool_name: "echo".into(),
                arguments: serde_json::json!({ "text": "hello" })
            }
        );
    }

    #[test]
    fn tool_aware_payload_uses_function_declarations_with_required_fields() {
        let payload = GeminiProvider::tool_aware_request_payload(
            "hello",
            &[serde_json::json!({
                "tool_name": "echo",
                "description": "Echo text",
                "input_schema": {
                    "type": "object",
                    "properties": {
                        "text": {
                            "type": "string",
                            "description": "The text to echo back."
                        }
                    },
                    "required": ["text"]
                }
            })],
            false,
            false,
        );

        let declaration = &payload["tools"][0]["functionDeclarations"][0];
        assert_eq!(declaration["name"], "echo");
        assert_eq!(
            declaration["parameters"]["required"],
            serde_json::json!(["text"])
        );
        assert_eq!(
            declaration["parameters"]["properties"]["text"]["type"],
            serde_json::json!("string")
        );
    }

    #[test]
    fn native_function_call_maps_alias_back_to_original_tool_name() {
        let task = minimal_text_task_with_tools(vec![serde_json::json!({
            "tool_name": "session.status",
            "description": "Session status",
            "input_schema": {
                "type": "object",
                "properties": {}
            }
        })]);

        let body = serde_json::json!({
            "candidates": [
                {
                    "content": {
                        "parts": [
                            {
                                "functionCall": {
                                    "name": "session_status",
                                    "args": {}
                                }
                            }
                        ]
                    }
                }
            ]
        });

        let output = GeminiProvider::parse_native_function_call(&task, &body)
            .expect("native function call should parse")
            .expect("tool call should be present");
        assert_eq!(
            output,
            crate::controller::ProviderOutput::ToolCall {
                tool_name: "session.status".into(),
                arguments: serde_json::json!({})
            }
        );
    }

    #[test]
    fn live_tool_call_maps_alias_back_to_original_tool_name() {
        let task = minimal_text_task_with_tools(vec![serde_json::json!({
            "tool_name": "session.status",
            "description": "Session status",
            "input_schema": {
                "type": "object",
                "properties": {}
            }
        })]);

        let body = serde_json::json!({
            "toolCall": {
                "functionCalls": [{
                    "id": "call-1",
                    "name": "session_status",
                    "args": {}
                }]
            }
        });

        let output = GeminiProvider::parse_live_tool_call(&task, &body)
            .expect("live tool call should parse")
            .expect("tool call should be present");
        assert_eq!(
            output.0,
            crate::controller::ProviderOutput::ToolCall {
                tool_name: "session.status".into(),
                arguments: serde_json::json!({})
            }
        );
        assert_eq!(output.1.as_deref(), Some("call-1"));
    }

    #[test]
    fn absorb_live_session_marker_captures_latest_resumption_handle() {
        let mut acc = super::LiveTurnAccumulator::default();
        GeminiProvider::absorb_live_session_marker(
            &mut acc,
            &serde_json::json!({
                "sessionResumptionUpdate": {
                    "newHandle": "resume-123",
                    "resumable": true
                }
            }),
        );

        assert_eq!(
            acc.session_marker.resumption_handle.as_deref(),
            Some("resume-123")
        );
    }

    #[test]
    fn live_tool_response_payload_wraps_json_tool_result_for_function_response() {
        let mut provider_options = serde_json::Map::new();
        provider_options.insert(
            "live_tool_response".into(),
            serde_json::json!({
                "function_call_id": "call-1",
                "tool_name": "session.status",
                "tool_response": { "ok": true }
            }),
        );
        let task = ControllerTask {
            kind: TaskKind::ResponseGenerate,
            request_class: RequestClass::Cognitive,
            session_id: Some("session-1".into()),
            turn_id: Some("turn-1".into()),
            provider: None,
            model: Some("gemini-3.1-flash-live-preview".into()),
            prompt: Some("Continue.".into()),
            text: None,
            spoken_text: None,
            display_text: None,
            voice: None,
            voice_id: None,
            output_format: None,
            language_code: None,
            response_contract: Default::default(),
            context: Default::default(),
            context_projection: Default::default(),
            affordances: Default::default(),
            routing_hints: RoutingHints::default(),
            provider_options,
            effective_rights: Vec::new(),
            tools: vec![],
        };

        let payload =
            GeminiProvider::live_tool_response_payload(&task).expect("payload should exist");
        assert_eq!(
            payload["toolResponse"]["functionResponses"][0]["id"],
            serde_json::json!("call-1")
        );
        assert_eq!(
            payload["toolResponse"]["functionResponses"][0]["response"]["ok"],
            serde_json::json!(true)
        );
    }
}

fn attachment_mime_type(attachment: &AttachmentInput) -> Option<Cow<'_, str>> {
    attachment
        .mime_type
        .as_deref()
        .map(normalize_attachment_mime_type)
        .or_else(|| match attachment.kind.as_deref() {
            Some("photo") | Some("image") => Some(Cow::Borrowed("image/jpeg")),
            Some("voice") => Some(Cow::Borrowed("audio/ogg")),
            Some("sticker") => Some(Cow::Borrowed("image/webp")),
            _ => None,
        })
}

fn normalize_attachment_mime_type(mime_type: &str) -> Cow<'_, str> {
    let normalized = mime_type.trim();
    if normalized.eq_ignore_ascii_case("text/x-web-markdown")
        || normalized.eq_ignore_ascii_case("text/markdown")
        || normalized.eq_ignore_ascii_case("text/x-markdown")
    {
        Cow::Borrowed("text/plain")
    } else {
        Cow::Borrowed(normalized)
    }
}
