use crate::controller::{
    AttemptPolicy, AttachmentInput, ControllerTask, ModelProvider, NativeLiveProvider,
    NativeLiveTurnOutput, ProviderOutput, RetryPolicy, TaskKind,
};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use futures::{SinkExt, StreamExt};
use media_codec::{AudioProvider as CodecProvider, CodecCache, normalize_audio};
use media_prep::{PcmPrepPolicy, prepare_audio_ligand_for_pcm};
use serde_json::{Value, json};
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tracing::{info, warn};

/// Seconds without a byte chunk from the SSE stream before we abort and escalate.
const STREAMING_IDLE_SECS: u64 = 8;
/// Seconds to wait for the initial HTTP response headers from Gemini before aborting.
/// Large contexts (100KB+) can take 20–30s before the first SSE byte arrives.
/// Reduced from 60s: the runtime now enforces an outer total_secs timeout per attempt
/// so the internal connect timeout only needs to cover legitimate network latency.
const STREAMING_CONNECT_SECS: u64 = 25;
/// Hard wall-clock cap on the entire SSE session (post-connect). Gemini can drip
/// keep-alive SSE bytes every <8s indefinitely, which prevents STREAMING_IDLE_SECS
/// from firing. Must be <= AttemptPolicy::total_secs to ensure the provider returns
/// before the runtime outer timeout fires. Reduced from 120s to eliminate the race
/// with the philote WaitingModel watchdog (also 120s).
const STREAMING_TOTAL_SECS: u64 = 32;

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
    codec_cache: Option<CodecCache>,
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
        let codec_cache = std::env::var("PHILOTIC_CODEC_CACHE_DB")
            .ok()
            .and_then(|path| match CodecCache::open(&path) {
                Ok(c) => Some(c),
                Err(e) => {
                    warn!("GeminiProvider: failed to open codec cache at {path}: {e:#}");
                    None
                }
            });
        Self {
            http_client,
            auth,
            default_model: "gemini-3.5-flash".into(),
            base_url: base_url
                .unwrap_or_else(|| "https://generativelanguage.googleapis.com".into())
                .trim_end_matches('/')
                .to_string(),
            codec_cache,
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
            " If — and only if — this exchange contains something genuinely worth remembering \
             (a user preference, a decision made, a fact learned, or a pattern worth recalling later), \
             include \"memory_candidate\" with fields: \"concept\" (short kebab-case slug), \
             \"content\" (one or two sentences distilling what is worth keeping), and optional \
             \"tags\" (array of short strings). Omit memory_candidate entirely for routine \
             exchanges, simple questions, greetings, or transient state."
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
                "nullable": true,
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
            // Not pushed to required — model omits it when there's nothing worth saving.
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
             (your full response formatted for text display, markdown is fine) and \
             \"spoken_text\" (a natural, expressive version for voice delivery — no markdown, \
             conversational tone, written to be heard). If — and only if — this exchange contains \
             something genuinely worth remembering (a user preference, a decision, a fact learned, \
             or a recurring pattern), also include \"memory_candidate\" (an object with \"concept\" \
             as a short kebab-case slug, \"content\" as one or two sentences distilling what is \
             worth keeping, and optional \"tags\"). Omit memory_candidate entirely for routine \
             exchanges, greetings, or transient state."
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
        let required = vec!["display_text", "spoken_text"];

        if wants_concept {
            properties["memory_candidate"] = json!({
                "type": "OBJECT",
                "nullable": true,
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
            // Not pushed to required — model omits when nothing is worth saving.
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
                .transport_error
                .as_deref()
                .map(|error| error.trim().is_empty())
                .unwrap_or(true)
        }) {
            // Inline PCM audio (Discord voice bridge) — convert to WAV and include directly.
            if let Some(inline_b64) = &attachment.inline_audio_b64 {
                let sample_rate = attachment.inline_audio_sample_rate.unwrap_or(48_000);
                let channels = attachment.inline_audio_channels.unwrap_or(2);
                let wav_bytes = pcm_i16_b64_to_wav(inline_b64, sample_rate, channels)
                    .context("failed to build WAV from inline PCM audio")?;
                parts.push(json!({
                    "inline_data": {
                        "mime_type": "audio/wav",
                        "data": BASE64_STANDARD.encode(&wav_bytes)
                    }
                }));
                continue;
            }

            // Blob-backed attachment — fetch from URL.
            let url = match attachment.url.as_deref().filter(|u| !u.trim().is_empty()) {
                Some(u) => u,
                None => continue,
            };
            let mime_type = attachment_mime_type(attachment)
                .with_context(|| format!("attachment {:?} missing mime type", attachment.kind))?;
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
            let bytes = response.bytes().await?.to_vec();
            let normalized =
                normalize_audio(bytes, &mime_type, CodecProvider::Gemini, self.codec_cache.as_ref(), "ffmpeg")
                    .await
                    .with_context(|| {
                        format!("media-codec: failed to normalize audio [{mime_type}] for Gemini")
                    })?;
            parts.push(json!({
                "inline_data": {
                    "mime_type": normalized.mime_type,
                    "data": BASE64_STANDARD.encode(normalized.bytes)
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
                model_gen: None,
            },
            partial_text_deltas,
            session_marker,
            pending_function_call_id: None,
            generation_complete: acc.generation_complete,
            turn_complete: acc.turn_complete || task.kind == TaskKind::ResponseGenerate,
        })
    }
    // ── Streaming helpers ──────────────────────────────────────────────────────

    fn streaming_endpoint_url(&self, model: Option<&str>) -> Result<String> {
        let model = model.unwrap_or(&self.default_model);
        match self
            .auth
            .as_ref()
            .context("Gemini auth missing from config; expected OAuth bearer or API key")?
        {
            GeminiAuth::ApiKey(api_key) => Ok(format!(
                "{}/v1beta/models/{}:streamGenerateContent?key={}&alt=sse",
                self.base_url, model, api_key
            )),
            GeminiAuth::OAuthBearer { .. } => Ok(format!(
                "{}/v1beta/models/{}:streamGenerateContent?alt=sse",
                self.base_url, model
            )),
        }
    }

    /// Parse one SSE line (`data: {...}`) and extract the text fragment from
    /// `candidates[0].content.parts[0].text`. Returns `None` for non-data lines
    /// or lines with no text part (e.g. finish-reason-only chunks).
    fn parse_sse_text_chunk(line: &str) -> Option<String> {
        let json_str = if let Some(rest) = line.strip_prefix("data: ") {
            rest
        } else if let Some(rest) = line.strip_prefix("data:") {
            rest
        } else {
            return None;
        };
        if json_str.trim() == "[DONE]" {
            return None;
        }
        let chunk: Value = serde_json::from_str(json_str).ok()?;
        let parts = chunk
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(Value::as_array)?;
        // Collect text from all parts — Gemini can send [{functionCall:...}, {text:...}]
        // in a single chunk, so scanning only the first part misses text after a tool call.
        let combined: String = parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("");
        if combined.is_empty() { None } else { Some(combined) }
    }

    /// Parse one SSE line and return the full chunk Value if it contains a `functionCall`
    /// part in `candidates[0].content.parts`. Gemini emits function calls via streaming
    /// without any accompanying text, which causes `parse_sse_text_chunk` to return None
    /// and `full_text` to stay empty. This helper detects those chunks so `invoke_streaming`
    /// can return them correctly instead of bailing with a streaming_timeout error.
    fn parse_sse_function_call_chunk(line: &str) -> Option<Value> {
        let json_str = if let Some(rest) = line.strip_prefix("data: ") {
            rest
        } else if let Some(rest) = line.strip_prefix("data:") {
            rest
        } else {
            return None;
        };
        if json_str.trim() == "[DONE]" {
            return None;
        }
        let chunk: Value = serde_json::from_str(json_str).ok()?;
        let has_fc = chunk
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
            .and_then(|c| c.get("content"))
            .and_then(|c| c.get("parts"))
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .any(|p| p.get("functionCall").is_some() || p.get("function_call").is_some())
            })
            .unwrap_or(false);
        if has_fc { Some(chunk) } else { None }
    }

    /// Scan `accumulated` for the display_text JSON string value, starting where we left off.
    /// Appends any newly revealed display_text characters to `out` and calls `on_token`.
    /// Returns the updated extraction cursor.
    ///
    /// The function handles the three phases:
    /// 1. Searching for `"display_text":` key + opening quote
    /// 2. Inside the display_text value (until unescaped closing `"`)
    /// 3. Done (returns immediately)
    fn extract_display_text_tokens(
        accumulated: &str,
        cursor: &mut DisplayTextCursor,
        out: &mut String,
    ) -> Vec<String> {
        let mut tokens = Vec::new();
        match cursor {
            DisplayTextCursor::Done => {}
            DisplayTextCursor::Searching => {
                // Find `"display_text":` then skip optional whitespace to the opening `"`.
                // Gemini's structured-JSON output uses `"display_text": "` (space after
                // colon), so we cannot anchor on the quote directly.
                const KEY: &str = "\"display_text\":";
                if let Some(key_pos) = accumulated.find(KEY) {
                    let after_colon = key_pos + KEY.len();
                    // Skip any whitespace between `:` and the opening `"`.
                    if let Some(quote_offset) = accumulated[after_colon..].find('"') {
                        let value_start = after_colon + quote_offset + 1; // +1 past the `"`
                        *cursor = DisplayTextCursor::InValue { pos: value_start };
                        return Self::extract_display_text_tokens(accumulated, cursor, out);
                    }
                    // Key found but opening quote not yet in buffer — stay Searching.
                }
            }
            DisplayTextCursor::InValue { pos } => {
                let s = &accumulated[*pos..];
                let mut new_text = String::new();
                let mut escaping = false;
                let mut bytes_consumed = 0usize;
                for ch in s.chars() {
                    bytes_consumed += ch.len_utf8();
                    match ch {
                        '\\' if !escaping => {
                            escaping = true;
                            // Skip the backslash — we'll include the next char literally.
                        }
                        '"' if !escaping => {
                            // Closing quote — extraction complete.
                            out.push_str(&new_text);
                            *cursor = DisplayTextCursor::Done;
                            if !new_text.is_empty() {
                                tokens.push(new_text);
                            }
                            return tokens;
                        }
                        ch => {
                            // Decode JSON escape sequences when preceded by a backslash.
                            // The backslash was consumed without being pushed; we handle
                            // the escape character here so \n → newline, \t → tab, etc.
                            let actual = if escaping {
                                match ch {
                                    'n' => '\n',
                                    'r' => '\r',
                                    't' => '\t',
                                    '\\' => '\\',
                                    '"' => '"',
                                    '/' => '/',
                                    _ => ch,
                                }
                            } else {
                                ch
                            };
                            escaping = false;
                            new_text.push(actual);
                        }
                    }
                }
                // Stream not complete yet — emit what we found and advance the cursor.
                *pos += bytes_consumed;
                if !new_text.is_empty() {
                    out.push_str(&new_text);
                    tokens.push(new_text);
                }
            }
        }
        tokens
    }
}

/// Cursor tracking state for display_text extraction during Gemini SSE streaming.
#[derive(Debug)]
enum DisplayTextCursor {
    /// Still searching for the `"display_text":"` key in the accumulated buffer.
    Searching,
    /// Inside the display_text value; `pos` is the byte offset in `accumulated` of the
    /// next unread character.
    InValue { pos: usize },
    /// Extraction complete (closing `"` was found).
    Done,
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

    fn attempt_policy(&self) -> AttemptPolicy {
        // total_secs must be >= STREAMING_TOTAL_SECS (provider's internal cap).
        // Invariant: total_secs × retry_policy.max_attempts < philote watchdog (120s).
        // 35 × 2 = 70s < 120s ✓
        AttemptPolicy { connect_secs: STREAMING_CONNECT_SECS, idle_secs: STREAMING_IDLE_SECS, total_secs: 35 }
    }

    fn retry_policy(&self) -> RetryPolicy {
        use crate::controller::{BackoffStrategy, RetryableErrorClass};
        RetryPolicy {
            max_attempts: 2,
            backoff: BackoffStrategy::Linear { step_ms: 800 },
            retryable: RetryableErrorClass { network_reset: true, streaming_timeout: true, provider_5xx: true, rate_limit: false },
        }
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

        if !status.is_success() {
            let message = body
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            bail!("Gemini API error ({}): {}", status.as_u16(), message);
        }

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
                model_gen: None,
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
                model_gen: None,
            })
        }
    }

    fn supports_streaming(&self, task: &ControllerTask) -> bool {
        // Only stream TextGenerate — MediaAnalyze/AudioTranscribe are batch by nature.
        task.kind == TaskKind::TextGenerate
    }

    async fn invoke_streaming(
        &self,
        task: &ControllerTask,
        token_tx: tokio::sync::mpsc::Sender<String>,
    ) -> Result<ProviderOutput> {
        use futures::StreamExt;

        if task.kind != TaskKind::TextGenerate {
            // Non-text kinds fall back to batch invoke (no tokens emitted).
            return self.invoke(task).await;
        }

        let has_tools = !task.tools.is_empty();
        let wants_concept = task.wants_channel("memory_concept");
        let wants_plan = task.wants_channel("active_plan");
        let use_structured =
            has_tools || task.wants_channel("spoken_text") || wants_concept || wants_plan;

        let payload = {
            let prompt = task
                .composed_prompt_text()
                .context("Gemini streaming text task missing prompt")?;
            if has_tools {
                Self::tool_aware_request_payload(&prompt, &task.tools, wants_concept, wants_plan)
            } else if use_structured {
                Self::structured_text_request_payload(&prompt, wants_concept, wants_plan)
            } else {
                Self::request_payload(&prompt)
            }
        };

        let url = self.streaming_endpoint_url(Some(self.request_model(task)))?;
        let req = self.http_client.post(url).json(&payload);
        // Wrap send() in a timeout — for large contexts Gemini can take 20–30s
        // before returning the first response byte, leaving send().await hung forever.
        let response = timeout(
            Duration::from_secs(STREAMING_CONNECT_SECS),
            self.apply_auth_headers(req)?.send(),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "streaming_timeout: Gemini did not respond within {}s (large context?)",
                STREAMING_CONNECT_SECS
            )
        })??;
        let status = response.status();

        if !status.is_success() {
            // On HTTP error, read the full body and propagate as a failure so
            // the philote tier-escalation logic can route to a fallback provider.
            let body = response.json::<Value>().await.unwrap_or_default();
            let message = body
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            bail!("Gemini API error ({}): {}", status.as_u16(), message);
        }

        // Read the SSE byte stream, splitting on newlines.
        // IMPORTANT: accumulate raw bytes and decode as UTF-8 at line boundaries.
        // Using `*byte as char` would corrupt multi-byte UTF-8 sequences (any
        // non-ASCII character in the model response), producing garbled text.
        let mut byte_stream = response.bytes_stream();
        let mut full_text = String::new();
        let mut line_buf: Vec<u8> = Vec::new();
        let is_structured = use_structured;
        let mut cursor = if is_structured {
            DisplayTextCursor::Searching
        } else {
            DisplayTextCursor::Done
        };
        let mut _display_text_out = String::new();

        let idle_dur = Duration::from_secs(STREAMING_IDLE_SECS);
        let sse_start = Instant::now();
        // Stash a function-call response found in SSE chunks. Gemini delivers tool
        // calls via streaming without any text content; we capture them here so the
        // empty-full_text check below can return them instead of bailing.
        let mut pending_function_call: Option<ProviderOutput> = None;
        loop {
            // Hard wall-clock cap: Gemini can drip keep-alive SSE bytes every ~7s,
            // resetting the idle timer without making progress. Bail if the whole
            // SSE session has run too long regardless of individual byte activity.
            if sse_start.elapsed() > Duration::from_secs(STREAMING_TOTAL_SECS) {
                drop(token_tx);
                bail!(
                    "streaming_timeout: Gemini SSE session exceeded {}s total (keep-alive drip?)",
                    STREAMING_TOTAL_SECS
                );
            }
            match timeout(idle_dur, byte_stream.next()).await {
                Ok(Some(chunk_result)) => {
                    let bytes = chunk_result.context("Gemini SSE stream read error")?;
                    for &byte in bytes.iter() {
                        if byte == b'\n' {
                            let line = String::from_utf8_lossy(&line_buf).trim().to_string();
                            line_buf.clear();
                            if line.is_empty() {
                                continue;
                            }
                            if let Some(text_chunk) = Self::parse_sse_text_chunk(&line) {
                                full_text.push_str(&text_chunk);
                                if is_structured {
                                    let tokens = Self::extract_display_text_tokens(
                                        &full_text,
                                        &mut cursor,
                                        &mut _display_text_out,
                                    );
                                    for token in tokens {
                                        let _ = token_tx.send(token).await;
                                    }
                                } else {
                                    let _ = token_tx.send(text_chunk).await;
                                }
                            }
                            // Check for a function call independently — a single SSE chunk can
                            // carry both a text part and a functionCall part simultaneously.
                            // Using a separate `if` (not `else if`) ensures the function call
                            // is captured even when text was also present in the same chunk.
                            if pending_function_call.is_none() {
                                if let Some(fc_chunk) = Self::parse_sse_function_call_chunk(&line) {
                                    match Self::parse_native_function_call(task, &fc_chunk) {
                                        Ok(Some(tc)) => pending_function_call = Some(tc),
                                        Ok(None) => {}
                                        Err(e) => {
                                            warn!("Failed to parse SSE function call chunk: {}", e)
                                        }
                                    }
                                }
                            }
                        } else {
                            line_buf.push(byte);
                        }
                    }
                }
                Ok(None) => break, // stream closed normally
                Err(_elapsed) => {
                    // No bytes arrived within STREAMING_IDLE_SECS. Abort — the token
                    // "streaming_timeout" in the message is detected by classify_provider_failure
                    // in model-router so philote can escalate to the next fallback tier.
                    drop(token_tx);
                    bail!(
                        "streaming_timeout: Gemini SSE stream produced no data for {}s",
                        STREAMING_IDLE_SECS
                    );
                }
            }
        }
        // Process any remaining buffered line.
        if !line_buf.is_empty() {
            let line = String::from_utf8_lossy(&line_buf).trim().to_string();
            if !line.is_empty() {
                if let Some(text_chunk) = Self::parse_sse_text_chunk(&line) {
                    full_text.push_str(&text_chunk);
                }
                if pending_function_call.is_none() {
                    if let Some(fc_chunk) = Self::parse_sse_function_call_chunk(&line) {
                        match Self::parse_native_function_call(task, &fc_chunk) {
                            Ok(Some(tc)) => pending_function_call = Some(tc),
                            Ok(None) => {}
                            Err(e) => {
                                warn!("Failed to parse SSE function call chunk: {}", e)
                            }
                        }
                    }
                }
            }
        }
        // Close the channel — token consumer will see this as end of stream.
        drop(token_tx);

        // If a function call arrived alongside streamed text, prefer the function call.
        // The text was already forwarded to the user as partial_reply tokens; returning
        // ToolCall here ensures the tool actually executes instead of being silently dropped.
        if let Some(fc) = pending_function_call {
            return Ok(fc);
        }

        if full_text.trim().is_empty() {
            // Stream completed without delivering text content (safety block, quota, etc.).
            // Do NOT fall back to batch — that path has no timeout and caused a 27-minute hang.
            // Return a streaming_timeout error so philote escalates to the next fallback tier.
            warn!("Gemini streaming returned no content; escalating to fallback tier");
            bail!("streaming_timeout: Gemini SSE stream completed with no text content");
        }

        // Parse the accumulated full_text into a ProviderOutput the same way invoke() does.
        if is_structured {
            // full_text is the complete JSON string; wrap it in the expected Gemini body shape
            // so parse_structured_response can process it.
            let body = json!({
                "candidates": [{
                    "content": {
                        "parts": [{ "text": full_text }]
                    }
                }]
            });
            if has_tools {
                // Check for a function-call response.
                if let Some(tool_call) = Self::parse_native_function_call(task, &body)? {
                    return Ok(tool_call);
                }
            }
            let (content, spoken_text, memory_concept, memory_candidate, active_plan) =
                Self::parse_structured_response(reqwest::StatusCode::OK, body.clone());
            if let Ok(parsed) = serde_json::from_str::<Value>(&content) {
                if let Some(tool_call) = Self::parse_tool_call_candidate(task, &parsed)? {
                    return Ok(tool_call);
                }
            }
            if content.trim().is_empty() {
                bail!("Gemini streaming returned an empty structured response");
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
                model_gen: None,
            })
        } else {
            Ok(ProviderOutput::Text {
                display_text: Some(full_text.clone()),
                content: full_text,
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
            response_route: None,
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
            ..Default::default()
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
            ..Default::default()
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
                    ..Default::default()
                }],
                ..Default::default()
            },
            context_projection: Default::default(),
            affordances: Default::default(),
            routing_hints: RoutingHints::default(),
            response_route: None,
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
                    ..Default::default()
                }],
                ..Default::default()
            },
            context_projection: Default::default(),
            affordances: Default::default(),
            routing_hints: RoutingHints::default(),
            response_route: None,
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
            response_route: None,
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
            response_route: None,
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
                    ..Default::default()
                }],
                ..Default::default()
            },
            context_projection: Default::default(),
            affordances: Default::default(),
            routing_hints: RoutingHints::default(),
            response_route: None,
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
                    ..Default::default()
                }],
                ..Default::default()
            },
            context_projection: Default::default(),
            affordances: Default::default(),
            routing_hints: RoutingHints::default(),
            response_route: None,
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
            response_route: None,
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

/// Convert base64-encoded i16 LE PCM audio to a WAV-format byte vector.
///
/// Builds a minimal RIFF/WAV header followed by the raw PCM data so that
/// Gemini (and other providers) can consume it as `audio/wav`.
fn pcm_i16_b64_to_wav(pcm_b64: &str, sample_rate: u32, channels: u16) -> Result<Vec<u8>> {
    use base64::Engine;
    let pcm_bytes = BASE64_STANDARD
        .decode(pcm_b64)
        .context("failed to base64-decode inline PCM audio")?;

    // Validate byte alignment (each i16 sample = 2 bytes)
    if pcm_bytes.len() % 2 != 0 {
        anyhow::bail!("inline PCM audio has odd byte count ({})", pcm_bytes.len());
    }

    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * u32::from(channels) * u32::from(bits_per_sample) / 8;
    let block_align: u16 = channels * bits_per_sample / 8;
    let data_len = pcm_bytes.len() as u32;
    let chunk_size = 36 + data_len; // 4-byte RIFF size field = header - 8 + data

    let mut wav = Vec::with_capacity(44 + pcm_bytes.len());

    // RIFF header
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&chunk_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    // fmt sub-chunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // sub-chunk size = 16 for PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // AudioFormat = PCM
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());

    // data sub-chunk
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(&pcm_bytes);

    Ok(wav)
}
