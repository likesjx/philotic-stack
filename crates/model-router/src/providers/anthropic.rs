//! Native Anthropic (Claude) provider — Messages API over HTTPS.
//!
//! This is a first-class provider speaking `POST /v1/messages` directly
//! (`x-api-key` + `anthropic-version` headers), NOT an OpenAI-compat shim.
//! Tool declarations map 1:1 from the philote catalog shape
//! (`tool_name`/`description`/`input_schema` → `name`/`description`/`input_schema`),
//! tool calls come back as `tool_use` content blocks, and streaming uses the
//! Messages SSE event family (`content_block_delta` etc.) with the crate's
//! standard streaming-idle / total-wall-clock timeout pattern.

use crate::controller::{
    AttemptPolicy, BackoffStrategy, ControllerTask, ModelProvider, ProviderOutput, RetryPolicy,
    RetryableErrorClass, TaskKind,
};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Map, Value, json};
use std::time::{Duration, Instant};
use tokio::time::timeout;
use tracing::{info, warn};

/// Default model for `text.generate` when the task names none.
pub const DEFAULT_TEXT_MODEL: &str = "claude-sonnet-5";
/// Heavy tier (`provider_options.model_tier = "heavy"`): most capable Opus-tier model.
pub const HEAVY_TIER_MODEL: &str = "claude-opus-4-8";
/// Fast tier (`provider_options.model_tier = "fast"`): cheapest/fastest model.
pub const FAST_TIER_MODEL: &str = "claude-haiku-4-5-20251001";

/// Messages API requires an explicit output cap.
const DEFAULT_MAX_TOKENS: u64 = 8192;
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Cap tool descriptions defensively — the catalog occasionally carries long prose.
const MAX_TOOL_DESCRIPTION_CHARS: usize = 1024;

/// How long to wait for the first response byte on the streaming path.
const STREAMING_CONNECT_SECS: u64 = 25;
/// Maximum silence gap mid-stream before treating the SSE session as stalled.
const STREAMING_IDLE_SECS: u64 = 10;
/// Hard wall-clock cap for one SSE session (ping events reset the idle timer
/// without making progress; this bounds the whole attempt).
const STREAMING_TOTAL_SECS: u64 = 32;

pub struct AnthropicProvider {
    http_client: reqwest::Client,
    api_key: Option<String>,
    base_url: String,
    default_model: String,
    heavy_model: String,
    fast_model: String,
}

/// What `parse_structured_text` pulls out of a structured model reply:
/// (text, thinking, tool_name, tool_args, raw). Named because the bare
/// five-Option tuple is unreadable at the call site and identical in both
/// providers.
type StructuredTextParts = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<Value>,
    Option<Value>,
);

impl AnthropicProvider {
    pub fn new(
        http_client: reqwest::Client,
        api_key: Option<String>,
        base_url: Option<String>,
        default_model: Option<String>,
        heavy_model: Option<String>,
        fast_model: Option<String>,
    ) -> Self {
        Self {
            http_client,
            api_key: api_key
                .map(|key| key.trim().to_string())
                .filter(|key| !key.is_empty()),
            base_url: {
                let b = base_url.unwrap_or_else(|| "https://api.anthropic.com".into());
                let b = b.trim_end_matches('/');
                let b = b.trim_end_matches("/v1");
                b.trim_end_matches('/').to_string()
            },
            default_model: default_model
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| DEFAULT_TEXT_MODEL.into()),
            heavy_model: heavy_model
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| HEAVY_TIER_MODEL.into()),
            fast_model: fast_model
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| FAST_TIER_MODEL.into()),
        }
    }

    /// Resolve the effective API key: an explicit (vault/env-ref) key wins over
    /// the bare `ANTHROPIC_API_KEY`-style fallback. Blank values are treated as
    /// absent so an empty config row cannot shadow a real fallback.
    pub fn api_key_from_config(
        api_key: Option<String>,
        bare_env_fallback: Option<String>,
    ) -> Option<String> {
        api_key
            .map(|key| key.trim().to_string())
            .filter(|key| !key.is_empty())
            .or_else(|| {
                bare_env_fallback
                    .map(|key| key.trim().to_string())
                    .filter(|key| !key.is_empty())
            })
    }

    fn messages_url(&self) -> String {
        format!("{}/v1/messages", self.base_url)
    }

    /// Model selection: explicit task model > `model_tier` provider option > default.
    fn request_model<'a>(&'a self, task: &'a ControllerTask) -> &'a str {
        if let Some(model) = task
            .model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            return model;
        }
        match task
            .provider_option_str("model_tier")
            .map(|tier| tier.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("heavy") | Some("opus") => &self.heavy_model,
            Some("fast") | Some("haiku") | Some("low") => &self.fast_model,
            _ => &self.default_model,
        }
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
                "AnthropicProvider does not support task kind [{}]",
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
        let mut parts = vec![json!({ "type": "text", "text": prompt })];
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
                    "type": "image",
                    "source": { "type": "url", "url": url }
                }));
            }
        }

        if parts.len() == 1 {
            Ok(json!({ "role": "user", "content": prompt }))
        } else {
            Ok(json!({ "role": "user", "content": parts }))
        }
    }

    /// Anthropic tools = `[{name, description, input_schema}]` — the philote
    /// catalog shape maps 1:1 (no Gemini-style schema sanitization needed).
    fn tool_definitions(tools: &[Value]) -> Vec<Value> {
        tools
            .iter()
            .filter_map(|tool| {
                let tool_name = tool.get("tool_name").and_then(Value::as_str)?.trim();
                if tool_name.is_empty() {
                    return None;
                }

                let description: String = tool
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                    .unwrap_or(tool_name)
                    .chars()
                    .take(MAX_TOOL_DESCRIPTION_CHARS)
                    .collect();
                let input_schema = tool
                    .get("input_schema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}}));

                Some(json!({
                    "name": tool_name,
                    "description": description,
                    "input_schema": input_schema,
                }))
            })
            .collect()
    }

    fn wants_structured(task: &ControllerTask) -> bool {
        task.kind == TaskKind::TextGenerate
            && (task.wants_channel("spoken_text")
                || task.wants_channel("memory_concept")
                || task.wants_channel("active_plan"))
    }

    /// System prompt carrying the structured-output contract. Tools stay native
    /// (`tool_use` blocks) — only the text-reply shape is constrained.
    fn system_text(task: &ControllerTask) -> Option<String> {
        if !Self::wants_structured(task) {
            return None;
        }

        let memory_instruction = if task.wants_channel("memory_concept") {
            " If — and only if — this exchange contains something genuinely worth remembering \
             (a user preference, a decision made, a fact learned, or a pattern worth recalling \
             later), include \"memory_candidate\" with fields: \"concept\" (short kebab-case \
             slug), \"content\" (one or two sentences distilling what is worth keeping), and \
             optional \"tags\" (array of short strings). Omit memory_candidate entirely for \
             routine exchanges, simple questions, greetings, or transient state."
        } else {
            ""
        };
        let plan_instruction = if task.wants_channel("active_plan") {
            " When working a multi-step task, also include \"active_plan\" in that JSON object: \
             {\"goal\": string, \"status\": string, \"steps\": [{\"id\": integer, \
             \"description\": string, \"tool_name\": string, \"status\": string}]}. Omit \
             active_plan for single-step exchanges."
        } else {
            ""
        };

        let tool_clause = if task.tools.is_empty() {
            ""
        } else {
            "When a tool is needed, call one of the declared tools natively — do not write a \
             JSON tool_call object by hand. Use tool input fields exactly as declared and \
             include every required field.\n"
        };

        Some(format!(
            "{}When replying with text, reply with ONLY a raw JSON object — no markdown code \
             fences, no text outside the JSON — containing \"display_text\" (your reply, \
             markdown fine) and \"spoken_text\" (conversational version for voice, no \
             markdown).{}{}",
            tool_clause, memory_instruction, plan_instruction,
        ))
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

    fn max_tokens(task: &ControllerTask) -> Value {
        Self::numeric_provider_option(task, "max_tokens")
            .and_then(|value| value.as_f64().map(|n| n as u64))
            .filter(|&n| n > 0)
            .map(|n| json!(n))
            .unwrap_or_else(|| json!(DEFAULT_MAX_TOKENS))
    }

    fn request_body(&self, task: &ControllerTask, stream: bool) -> Result<Value> {
        let mut body = json!({
            "model": self.request_model(task),
            "max_tokens": Self::max_tokens(task),
            "messages": [Self::user_message(task)?],
        });

        if stream {
            body["stream"] = Value::Bool(true);
        }

        if let Some(system) = Self::system_text(task) {
            body["system"] = Value::String(system);
        }

        if !task.tools.is_empty() {
            body["tools"] = Value::Array(Self::tool_definitions(&task.tools));
            body["tool_choice"] = json!({ "type": "auto" });
        }

        // Sampling params are pass-through only when the operator explicitly set
        // them (newer Claude models reject non-default sampling params).
        for key in ["temperature", "top_p", "top_k"] {
            if let Some(value) = Self::numeric_provider_option(task, key) {
                body[key] = value;
            }
        }

        if let Some(stop) = task.provider_options.get("stop_sequences") {
            body["stop_sequences"] = stop.clone();
        } else if let Some(stop) = task.provider_options.get("stop")
            && stop.is_array()
        {
            body["stop_sequences"] = stop.clone();
        }

        Ok(body)
    }

    fn require_api_key(&self) -> Result<&str> {
        self.api_key.as_deref().context(
            "Anthropic API key not configured — set the anthropic_api_key vault secret \
             (anthropic_api_key_ref) or export PHILOTIC_ANTHROPIC_API_KEY / ANTHROPIC_API_KEY",
        )
    }

    fn apply_headers(&self, request: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder> {
        Ok(request
            .header("x-api-key", self.require_api_key()?)
            .header("anthropic-version", ANTHROPIC_VERSION))
    }

    fn api_error_detail(body: &Value) -> String {
        body.get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| body.to_string())
    }

    fn strip_json_code_fences(raw: &str) -> &str {
        let trimmed = raw.trim();
        let Some(rest) = trimmed.strip_prefix("```") else {
            return trimmed;
        };
        let body = match rest.find('\n') {
            Some(idx) => &rest[idx + 1..],
            None => rest,
        };
        body.trim_end().strip_suffix("```").unwrap_or(body).trim()
    }

    /// Parse the structured-contract JSON out of a text reply.
    /// Returns `(display_text, spoken_text, memory_concept, memory_candidate, active_plan)`.
    fn parse_structured_text(content: &str) -> StructuredTextParts {
        let Ok(value) = serde_json::from_str::<Value>(Self::strip_json_code_fences(content)) else {
            return (None, None, None, None, None);
        };
        let Some(object) = value.as_object() else {
            return (None, None, None, None, None);
        };

        let display_text = object
            .get("display_text")
            .or_else(|| object.get("content"))
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string);
        let spoken_text = object
            .get("spoken_text")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string);
        let memory_candidate = object
            .get("memory_candidate")
            .cloned()
            .filter(|v| !v.is_null());
        let memory_concept = object
            .get("memory_concept")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .or_else(|| {
                memory_candidate
                    .as_ref()
                    .and_then(|candidate| candidate.get("concept"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string)
            });
        let active_plan = object.get("active_plan").cloned().filter(|v| !v.is_null());

        (
            display_text,
            spoken_text,
            memory_concept,
            memory_candidate,
            active_plan,
        )
    }

    fn tool_call_output(
        task: &ControllerTask,
        tool_name: &str,
        arguments: Value,
    ) -> Result<ProviderOutput> {
        let tool_name = tool_name.trim().to_string();
        if tool_name.is_empty() {
            bail!("Anthropic tool_use block missing tool name");
        }
        if !arguments.is_object() {
            bail!(
                "Anthropic tool_use.input for [{}] must be an object",
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
            bail!("Anthropic returned unsupported tool_use [{}]", tool_name);
        }
        Ok(ProviderOutput::ToolCall {
            tool_name,
            arguments,
        })
    }

    fn text_output(task: &ControllerTask, content: String, model: &str) -> Result<ProviderOutput> {
        if content.trim().is_empty() {
            bail!("Anthropic returned an empty response");
        }

        let (display_text, spoken_text, memory_concept, memory_candidate, active_plan) =
            if Self::wants_structured(task) {
                Self::parse_structured_text(&content)
            } else {
                (None, None, None, None, None)
            };

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
            model_gen: Some(model.to_string()),
        })
    }

    fn parse_message_response(
        task: &ControllerTask,
        body: &Value,
        model: &str,
    ) -> Result<ProviderOutput> {
        if body.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
            bail!(
                "Anthropic declined the request (stop_reason=refusal): {}",
                body.get("stop_details")
                    .and_then(|d| d.get("explanation"))
                    .and_then(Value::as_str)
                    .unwrap_or("no further detail")
            );
        }

        let content = body
            .get("content")
            .and_then(Value::as_array)
            .context("Anthropic response missing content array")?;

        let mut text_segments: Vec<String> = Vec::new();
        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                        let trimmed = text.trim();
                        if !trimmed.is_empty() {
                            text_segments.push(trimmed.to_string());
                        }
                    }
                }
                Some("tool_use") => {
                    // Tool call wins over any accompanying text — the tool must
                    // actually execute instead of being silently dropped.
                    let tool_name = block.get("name").and_then(Value::as_str).unwrap_or("");
                    let arguments = block
                        .get("input")
                        .cloned()
                        .unwrap_or_else(|| Value::Object(Map::new()));
                    return Self::tool_call_output(task, tool_name, arguments);
                }
                _ => {}
            }
        }

        Self::text_output(task, text_segments.join("\n"), model)
    }

    fn debug_model_requests_enabled() -> bool {
        std::env::var("PHILOTIC_DEBUG_MODEL_REQUESTS")
            .ok()
            .as_deref()
            .map(|value| matches!(value, "1" | "true" | "TRUE" | "yes" | "YES"))
            .unwrap_or(false)
    }
}

/// State accumulated while folding Messages SSE events into a final output.
#[derive(Default)]
struct SseFold {
    full_text: String,
    tool_name: Option<String>,
    tool_json: String,
    stop_reason: Option<String>,
    /// True once `message_stop` arrived — the stream ended cleanly.
    finished: bool,
    error: Option<String>,
}

impl SseFold {
    /// Fold one SSE `data:` payload. Returns text delta to forward, if any.
    fn fold_event(&mut self, event: &Value) -> Option<String> {
        match event.get("type").and_then(Value::as_str) {
            Some("content_block_start") => {
                let block = event.get("content_block")?;
                if block.get("type").and_then(Value::as_str) == Some("tool_use")
                    && self.tool_name.is_none()
                {
                    self.tool_name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    // `input` on the start event is an empty object; the real
                    // arguments arrive as input_json_delta fragments.
                    self.tool_json.clear();
                }
                None
            }
            Some("content_block_delta") => {
                let delta = event.get("delta")?;
                match delta.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        let text = delta.get("text").and_then(Value::as_str)?;
                        self.full_text.push_str(text);
                        Some(text.to_string())
                    }
                    Some("input_json_delta") => {
                        if let Some(fragment) = delta.get("partial_json").and_then(Value::as_str) {
                            self.tool_json.push_str(fragment);
                        }
                        None
                    }
                    _ => None,
                }
            }
            Some("message_delta") => {
                if let Some(stop_reason) = event
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(Value::as_str)
                {
                    self.stop_reason = Some(stop_reason.to_string());
                }
                None
            }
            Some("message_stop") => {
                self.finished = true;
                None
            }
            Some("error") => {
                self.error = Some(
                    event
                        .get("error")
                        .and_then(|error| error.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("Anthropic SSE stream returned an error")
                        .to_string(),
                );
                None
            }
            _ => None,
        }
    }

    fn into_output(self, task: &ControllerTask, model: &str) -> Result<ProviderOutput> {
        if let Some(message) = self.error {
            bail!("Anthropic API error (stream): {}", message);
        }
        if self.stop_reason.as_deref() == Some("refusal") {
            bail!("Anthropic declined the request (stop_reason=refusal)");
        }

        if let Some(tool_name) = self.tool_name {
            let arguments = if self.tool_json.trim().is_empty() {
                Value::Object(Map::new())
            } else {
                serde_json::from_str(&self.tool_json).with_context(|| {
                    format!(
                        "Anthropic returned invalid tool_call arguments for [{}]",
                        tool_name
                    )
                })?
            };
            return AnthropicProvider::tool_call_output(task, &tool_name, arguments);
        }

        AnthropicProvider::text_output(task, self.full_text, model)
    }
}

#[async_trait]
impl ModelProvider for AnthropicProvider {
    fn id(&self) -> &'static str {
        "anthropic"
    }

    fn supports(&self, task: &ControllerTask) -> bool {
        matches!(task.kind, TaskKind::TextGenerate | TaskKind::MediaAnalyze)
    }

    fn attempt_policy(&self) -> AttemptPolicy {
        // total_secs must be >= STREAMING_TOTAL_SECS (provider's internal cap).
        // Invariant: total_secs × retry_policy.max_attempts < philote watchdog (120s).
        // 35 × 2 = 70s < 120s ✓
        AttemptPolicy {
            connect_secs: STREAMING_CONNECT_SECS,
            idle_secs: STREAMING_IDLE_SECS,
            total_secs: 35,
        }
    }

    fn retry_policy(&self) -> RetryPolicy {
        RetryPolicy {
            max_attempts: 2,
            backoff: BackoffStrategy::Linear { step_ms: 800 },
            retryable: RetryableErrorClass {
                network_reset: true,
                streaming_timeout: true,
                provider_5xx: true,
                rate_limit: false,
            },
        }
    }

    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput> {
        if !self.supports(task) {
            bail!(
                "AnthropicProvider does not support task kind [{}]",
                task.kind.as_str()
            );
        }

        let model = self.request_model(task).to_string();
        let body = self.request_body(task, false)?;
        if Self::debug_model_requests_enabled() {
            info!(
                provider = ModelProvider::id(self),
                model = %model,
                request = %serde_json::to_string_pretty(&body).unwrap_or_default(),
                "Anthropic request payload"
            );
        }

        let request = self.http_client.post(self.messages_url()).json(&body);
        let response = self.apply_headers(request)?.send().await?;
        let status = response.status();
        let body = response.json::<Value>().await?;

        if !status.is_success() {
            bail!(
                "Anthropic API error ({}): {}",
                status.as_u16(),
                Self::api_error_detail(&body)
            );
        }

        Self::parse_message_response(task, &body, &model)
    }

    fn supports_streaming(&self, task: &ControllerTask) -> bool {
        // Only stream TextGenerate — MediaAnalyze is batch by nature.
        task.kind == TaskKind::TextGenerate
    }

    async fn invoke_streaming(
        &self,
        task: &ControllerTask,
        token_tx: tokio::sync::mpsc::Sender<String>,
    ) -> Result<ProviderOutput> {
        use futures::StreamExt;

        if task.kind != TaskKind::TextGenerate {
            return self.invoke(task).await;
        }

        let model = self.request_model(task).to_string();
        let body = self.request_body(task, true)?;
        // Under the structured contract the text stream is JSON syntax — don't
        // forward raw fragments as display tokens; the final parse recovers
        // display_text. Plain-text turns stream tokens straight through.
        let forward_tokens = !Self::wants_structured(task);

        let request = self.http_client.post(self.messages_url()).json(&body);
        let response = timeout(
            Duration::from_secs(STREAMING_CONNECT_SECS),
            self.apply_headers(request)?.send(),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "streaming_timeout: Anthropic did not respond within {}s",
                STREAMING_CONNECT_SECS
            )
        })??;
        let status = response.status();

        if !status.is_success() {
            let body = response.json::<Value>().await.unwrap_or_default();
            bail!(
                "Anthropic API error ({}): {}",
                status.as_u16(),
                Self::api_error_detail(&body)
            );
        }

        let mut byte_stream = response.bytes_stream();
        let mut line_buf: Vec<u8> = Vec::new();
        let mut fold = SseFold::default();
        let idle_dur = Duration::from_secs(STREAMING_IDLE_SECS);
        let sse_start = Instant::now();

        'stream: loop {
            if sse_start.elapsed() > Duration::from_secs(STREAMING_TOTAL_SECS) {
                drop(token_tx);
                bail!(
                    "streaming_timeout: Anthropic SSE session exceeded {}s total",
                    STREAMING_TOTAL_SECS
                );
            }
            match timeout(idle_dur, byte_stream.next()).await {
                Ok(Some(chunk_result)) => {
                    let bytes = chunk_result.context("Anthropic SSE stream read error")?;
                    for &byte in bytes.iter() {
                        if byte != b'\n' {
                            line_buf.push(byte);
                            continue;
                        }
                        let line = String::from_utf8_lossy(&line_buf).trim().to_string();
                        line_buf.clear();
                        let Some(payload) = line.strip_prefix("data:") else {
                            continue;
                        };
                        let payload = payload.trim();
                        if payload.is_empty() {
                            continue;
                        }
                        let Ok(event) = serde_json::from_str::<Value>(payload) else {
                            warn!("Anthropic SSE line was not valid JSON: {}", payload);
                            continue;
                        };
                        if let Some(token) = fold.fold_event(&event)
                            && forward_tokens
                            && !token.is_empty()
                        {
                            let _ = token_tx.send(token).await;
                        }
                        if fold.finished || fold.error.is_some() {
                            break 'stream;
                        }
                    }
                }
                Ok(None) => break, // stream closed
                Err(_elapsed) => {
                    drop(token_tx);
                    bail!(
                        "streaming_timeout: Anthropic SSE stream produced no data for {}s",
                        STREAMING_IDLE_SECS
                    );
                }
            }
        }
        drop(token_tx);

        fold.into_output(task, &model)
    }
}

#[cfg(test)]
mod tests {
    use super::{AnthropicProvider, DEFAULT_MAX_TOKENS};
    use crate::controller::{ControllerTask, ModelProvider, ProviderOutput};
    use axum::body::Body;
    use axum::http::HeaderMap;
    use axum::response::Response;
    use axum::{Json, Router, routing::post};
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tokio::net::TcpListener;
    use tokio::sync::Mutex;

    fn provider_with(base_url: Option<String>) -> AnthropicProvider {
        AnthropicProvider::new(
            reqwest::Client::new(),
            Some("secret".into()),
            base_url,
            None,
            None,
            None,
        )
    }

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
        let provider = provider_with(Some("https://api.anthropic.com/v1/".into()));
        assert_eq!(
            provider.messages_url(),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn api_key_resolution_prefers_explicit_key_over_bare_env() {
        assert_eq!(
            AnthropicProvider::api_key_from_config(
                Some(" vault-key ".into()),
                Some("bare-env-key".into())
            ),
            Some("vault-key".into())
        );
        assert_eq!(
            AnthropicProvider::api_key_from_config(Some("  ".into()), Some(" bare ".into())),
            Some("bare".into())
        );
        assert_eq!(AnthropicProvider::api_key_from_config(None, None), None);
    }

    #[test]
    fn request_shape_maps_messages_tools_and_system() {
        let provider = provider_with(None);
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "prompt": "Summarize the deployment status.",
            "response_contract": { "channels": ["display_text", "spoken_text"] },
            "tools_for_model": [{
                "tool_name": "workspace.read",
                "description": "Read a file",
                "input_schema": {
                    "type": "object",
                    "properties": { "path": { "type": "string" } },
                    "required": ["path"]
                }
            }]
        }))
        .unwrap();

        let body = provider.request_body(&task, false).unwrap();
        assert_eq!(body["model"], "claude-sonnet-5");
        assert_eq!(body["max_tokens"], json!(DEFAULT_MAX_TOKENS));
        assert_eq!(body["messages"][0]["role"], "user");
        // Structured channels requested → system carries the JSON contract.
        assert!(
            body["system"]
                .as_str()
                .unwrap()
                .contains("\"display_text\"")
        );
        // Tools map 1:1 into the Anthropic shape.
        assert_eq!(body["tools"][0]["name"], "workspace.read");
        assert_eq!(body["tools"][0]["description"], "Read a file");
        assert_eq!(
            body["tools"][0]["input_schema"]["required"][0],
            json!("path")
        );
        assert_eq!(body["tool_choice"]["type"], "auto");
        // Sampling params are absent unless explicitly configured.
        assert!(body.get("temperature").is_none());
        assert!(body.get("stream").is_none());
    }

    #[test]
    fn model_tier_selects_heavy_and_fast_models() {
        let provider = provider_with(None);
        let heavy = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "prompt": "think hard",
            "provider_options": { "model_tier": "heavy" }
        }))
        .unwrap();
        let fast = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "prompt": "be quick",
            "provider_options": { "model_tier": "fast" }
        }))
        .unwrap();
        let pinned = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "prompt": "explicit model",
            "model": "claude-3-9-test"
        }))
        .unwrap();

        assert_eq!(provider.request_model(&heavy), "claude-opus-4-8");
        assert_eq!(provider.request_model(&fast), "claude-haiku-4-5-20251001");
        assert_eq!(provider.request_model(&pinned), "claude-3-9-test");
    }

    #[tokio::test]
    async fn invoke_sends_anthropic_headers_and_parses_text() {
        let captured = Arc::new(Mutex::new(None::<(Value, String, String)>));
        let captured_for_handler = Arc::clone(&captured);
        let app = Router::new().route(
            "/v1/messages",
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let captured_for_handler = Arc::clone(&captured_for_handler);
                async move {
                    let api_key = headers
                        .get("x-api-key")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    let version = headers
                        .get("anthropic-version")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    *captured_for_handler.lock().await = Some((body, api_key, version));
                    Json(json!({
                        "id": "msg_test",
                        "type": "message",
                        "role": "assistant",
                        "model": "claude-sonnet-5",
                        "content": [{ "type": "text", "text": "All systems nominal." }],
                        "stop_reason": "end_turn"
                    }))
                }
            }),
        );
        let (base_url, _handle) = spawn_test_server(app).await;

        let provider = provider_with(Some(base_url));
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "prompt": "Status?"
        }))
        .unwrap();

        let output = provider.invoke(&task).await.unwrap();
        match output {
            ProviderOutput::Text {
                content, model_gen, ..
            } => {
                assert_eq!(content, "All systems nominal.");
                assert_eq!(model_gen.as_deref(), Some("claude-sonnet-5"));
            }
            other => panic!("unexpected output: {:?}", other),
        }

        let (body, api_key, version) = captured.lock().await.clone().expect("captured");
        assert_eq!(api_key, "secret");
        assert_eq!(version, "2023-06-01");
        assert_eq!(body["model"], "claude-sonnet-5");
    }

    #[tokio::test]
    async fn parses_tool_use_response() {
        let app = Router::new().route(
            "/v1/messages",
            post(|| async move {
                Json(json!({
                    "id": "msg_tool",
                    "type": "message",
                    "role": "assistant",
                    "model": "claude-sonnet-5",
                    "content": [
                        { "type": "text", "text": "Let me check that file." },
                        {
                            "type": "tool_use",
                            "id": "toolu_1",
                            "name": "workspace.read",
                            "input": { "path": "docs/task.md" }
                        }
                    ],
                    "stop_reason": "tool_use"
                }))
            }),
        );
        let (base_url, _handle) = spawn_test_server(app).await;

        let provider = provider_with(Some(base_url));
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
    async fn rejects_undeclared_tool_use() {
        let app = Router::new().route(
            "/v1/messages",
            post(|| async move {
                Json(json!({
                    "content": [{
                        "type": "tool_use",
                        "id": "toolu_1",
                        "name": "not.declared",
                        "input": {}
                    }],
                    "stop_reason": "tool_use"
                }))
            }),
        );
        let (base_url, _handle) = spawn_test_server(app).await;

        let provider = provider_with(Some(base_url));
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "prompt": "hi",
            "tools_for_model": [{
                "tool_name": "workspace.read",
                "input_schema": { "type": "object", "properties": {} }
            }]
        }))
        .unwrap();

        let err = provider.invoke(&task).await.unwrap_err().to_string();
        assert!(err.contains("unsupported tool_use"), "{err}");
    }

    #[tokio::test]
    async fn parses_structured_text_response() {
        let app = Router::new().route(
            "/v1/messages",
            post(|| async move {
                Json(json!({
                    "content": [{
                        "type": "text",
                        "text": "{\"display_text\":\"Hello\",\"spoken_text\":\"Hello there\",\"active_plan\":{\"goal\":\"test\"}}"
                    }],
                    "stop_reason": "end_turn"
                }))
            }),
        );
        let (base_url, _handle) = spawn_test_server(app).await;

        let provider = provider_with(Some(base_url));
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "prompt": "say hello",
            "response_contract": { "channels": ["display_text", "spoken_text", "active_plan"] }
        }))
        .unwrap();

        let output = provider.invoke(&task).await.unwrap();
        match output {
            ProviderOutput::Text {
                display_text,
                spoken_text,
                active_plan,
                ..
            } => {
                assert_eq!(display_text.as_deref(), Some("Hello"));
                assert_eq!(spoken_text.as_deref(), Some("Hello there"));
                assert_eq!(active_plan, Some(json!({ "goal": "test" })));
            }
            other => panic!("unexpected output: {:?}", other),
        }
    }

    #[tokio::test]
    async fn refusal_stop_reason_is_an_error() {
        let app = Router::new().route(
            "/v1/messages",
            post(|| async move {
                Json(json!({
                    "content": [],
                    "stop_reason": "refusal",
                    "stop_details": { "type": "refusal", "explanation": "declined" }
                }))
            }),
        );
        let (base_url, _handle) = spawn_test_server(app).await;

        let provider = provider_with(Some(base_url));
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "prompt": "hi"
        }))
        .unwrap();

        let err = provider.invoke(&task).await.unwrap_err().to_string();
        assert!(err.contains("refusal"), "{err}");
    }

    #[tokio::test]
    async fn missing_api_key_fails_fast_with_hint() {
        let provider = AnthropicProvider::new(
            reqwest::Client::new(),
            None,
            Some("http://127.0.0.1:1".into()),
            None,
            None,
            None,
        );
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "prompt": "hi"
        }))
        .unwrap();

        let err = provider.invoke(&task).await.unwrap_err().to_string();
        assert!(err.contains("Anthropic API key not configured"), "{err}");
    }

    fn sse_body(events: &[Value]) -> String {
        events
            .iter()
            .map(|event| {
                format!(
                    "event: {}\ndata: {}\n\n",
                    event["type"].as_str().unwrap_or("message"),
                    event
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn streaming_folds_text_deltas_and_forwards_tokens() {
        let body = sse_body(&[
            json!({"type": "message_start", "message": {"id": "msg_1", "model": "claude-sonnet-5"}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hello "}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "stream"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 4}}),
            json!({"type": "message_stop"}),
        ]);
        let app = Router::new().route(
            "/v1/messages",
            post(move || {
                let body = body.clone();
                async move {
                    Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(Body::from(body))
                        .unwrap()
                }
            }),
        );
        let (base_url, _handle) = spawn_test_server(app).await;

        let provider = provider_with(Some(base_url));
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "prompt": "stream please"
        }))
        .unwrap();

        let (token_tx, mut token_rx) = tokio::sync::mpsc::channel::<String>(16);
        let output = provider.invoke_streaming(&task, token_tx).await.unwrap();

        let mut tokens = Vec::new();
        while let Some(token) = token_rx.recv().await {
            tokens.push(token);
        }
        assert_eq!(tokens, vec!["Hello ".to_string(), "stream".to_string()]);

        match output {
            ProviderOutput::Text { content, .. } => assert_eq!(content, "Hello stream"),
            other => panic!("unexpected output: {:?}", other),
        }
    }

    #[tokio::test]
    async fn streaming_folds_tool_use_from_input_json_deltas() {
        let body = sse_body(&[
            json!({"type": "message_start", "message": {"id": "msg_2", "model": "claude-sonnet-5"}}),
            json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": "toolu_2", "name": "workspace.read", "input": {}}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"path\":"}}),
            json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "\"docs/task.md\"}"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}}),
            json!({"type": "message_stop"}),
        ]);
        let app = Router::new().route(
            "/v1/messages",
            post(move || {
                let body = body.clone();
                async move {
                    Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(Body::from(body))
                        .unwrap()
                }
            }),
        );
        let (base_url, _handle) = spawn_test_server(app).await;

        let provider = provider_with(Some(base_url));
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "context": { "active_turn": { "text": "read the task file" } },
            "tools_for_model": [{
                "tool_name": "workspace.read",
                "description": "Read a file",
                "input_schema": { "type": "object", "properties": { "path": { "type": "string" } } }
            }]
        }))
        .unwrap();

        let (token_tx, mut token_rx) = tokio::sync::mpsc::channel::<String>(16);
        let output = provider.invoke_streaming(&task, token_tx).await.unwrap();
        assert!(token_rx.recv().await.is_none());

        assert_eq!(
            output,
            ProviderOutput::ToolCall {
                tool_name: "workspace.read".into(),
                arguments: json!({ "path": "docs/task.md" }),
            }
        );
    }

    #[test]
    fn tool_descriptions_are_capped() {
        let long = "x".repeat(5000);
        let tools = vec![json!({
            "tool_name": "long.tool",
            "description": long,
            "input_schema": { "type": "object", "properties": {} }
        })];
        let declarations = AnthropicProvider::tool_definitions(&tools);
        let description = declarations[0]["description"].as_str().unwrap();
        assert_eq!(description.len(), super::MAX_TOOL_DESCRIPTION_CHARS);
    }
}
