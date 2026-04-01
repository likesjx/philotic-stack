use crate::controller::{AttachmentInput, ControllerTask, ModelProvider, ProviderOutput, TaskKind};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde_json::{Value, json};
use std::borrow::Cow;
use tracing::info;

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
            TaskKind::VoiceSynthesize => bail!("Gemini does not support voice synthesis"),
            TaskKind::Embed => bail!("Gemini does not support local embedding (use OnnxProvider)"),
        };

        if Self::debug_model_requests_enabled() && task.kind == TaskKind::TextGenerate {
            let prompt = task
                .composed_prompt_text()
                .unwrap_or_else(|| "<missing prompt>".into());
            info!(
                "PHILOTIC_DEBUG_MODEL_REQUESTS gemini composed prompt provider={} model={:?}:\n{}",
                self.id(),
                task.model,
                prompt
            );
            match serde_json::to_string_pretty(&payload) {
                Ok(json) => info!(
                    "PHILOTIC_DEBUG_MODEL_REQUESTS gemini provider payload provider={} model={:?}:\n{}",
                    self.id(),
                    task.model,
                    json
                ),
                Err(err) => info!(
                    "PHILOTIC_DEBUG_MODEL_REQUESTS gemini payload serialization failed: {}",
                    err
                ),
            }
        }

        let url = self.endpoint_url(task.model.as_deref())?;
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
        AttachmentInput, ContextEnvelope, ControllerTask, RequestClass, RoutingHints, TaskKind,
    };

    fn minimal_text_task_with_tools(tools: Vec<serde_json::Value>) -> ControllerTask {
        ControllerTask {
            kind: TaskKind::TextGenerate,
            request_class: RequestClass::Cognitive,
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
            provider_options: Default::default(),
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
            provider_options: Default::default(),
            tools: vec![],
        };

        assert!(crate::controller::ModelProvider::supports(&provider, &task));
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
fn pcm_i16_b64_to_wav(
    pcm_b64: &str,
    sample_rate: u32,
    channels: u16,
) -> Result<Vec<u8>> {
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
    wav.extend_from_slice(&16u32.to_le_bytes());       // sub-chunk size = 16 for PCM
    wav.extend_from_slice(&1u16.to_le_bytes());         // AudioFormat = PCM
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
