use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use philotic_client::{IpcRequest, IpcResponse, PhiloticClient};
use serde_json::{Map, Value, json};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    TextGenerate,
    MediaAnalyze,
    VoiceSynthesize,
}

impl TaskKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TextGenerate => "text.generate",
            Self::MediaAnalyze => "media.analyze",
            Self::VoiceSynthesize => "voice.synthesize",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResponseContract {
    pub modalities: Vec<String>,
    pub style: Option<String>,
    pub channels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectionItem {
    pub text: Option<String>,
    pub source_ref: Option<String>,
    pub projection_kind: Option<String>,
    pub priority: Option<i64>,
    pub token_estimate: Option<u64>,
    pub cache_key: Option<String>,
    pub truncation_policy: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContentPart {
    pub part_type: String,
    pub text: Option<String>,
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TurnInput {
    pub role: Option<String>,
    pub text: Option<String>,
    pub parts: Vec<ContentPart>,
}

impl TurnInput {
    pub fn text_content(&self) -> Option<String> {
        if let Some(text) = self
            .text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            return Some(text.to_string());
        }

        let texts = self
            .parts
            .iter()
            .filter(|part| part.part_type == "text")
            .filter_map(|part| part.text.as_deref())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>();

        if texts.is_empty() {
            None
        } else {
            Some(texts.join("\n"))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AttachmentInput {
    pub kind: Option<String>,
    pub file_id: Option<String>,
    pub mime_type: Option<String>,
    pub url: Option<String>,
    pub blob_ref: Option<String>,
    pub transport_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContextEnvelope {
    pub instructions: Vec<ProjectionItem>,
    pub identity: Vec<ProjectionItem>,
    pub memory: Vec<ProjectionItem>,
    pub dialogue_window: Vec<TurnInput>,
    pub active_turn: Option<TurnInput>,
    pub attachments: Vec<AttachmentInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AffordanceItem {
    pub id: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub text: Option<String>,
    pub source_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Affordances {
    pub skills: Vec<AffordanceItem>,
    pub tools: Vec<AffordanceItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RoutingHints {
    pub implementation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerTask {
    pub kind: TaskKind,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub prompt: Option<String>,
    pub text: Option<String>,
    pub spoken_text: Option<String>,
    pub display_text: Option<String>,
    pub voice: Option<String>,
    pub voice_id: Option<String>,
    pub output_format: Option<String>,
    pub language_code: Option<String>,
    pub response_contract: ResponseContract,
    pub context: ContextEnvelope,
    pub affordances: Affordances,
    pub routing_hints: RoutingHints,
    pub provider_options: Map<String, Value>,
}

impl ControllerTask {
    pub fn from_value(task: &Value) -> Result<Self> {
        let response_contract = parse_response_contract(task.get("response_contract"));
        let mut context = parse_context(task.get("context"));
        let affordances = parse_affordances(task.get("affordances"));
        let routing_hints = parse_routing_hints(task.get("routing_hints"));
        let top_level_attachments = parse_attachments(task.get("attachments"));
        if !top_level_attachments.is_empty() {
            context.attachments.extend(top_level_attachments);
        }

        let kind = match task
            .get("kind")
            .or_else(|| task.get("action"))
            .and_then(Value::as_str)
        {
            Some("generate_text") | Some("text.generate") | Some("text_generate") => {
                TaskKind::TextGenerate
            }
            Some("media.analyze") | Some("media_analyze") | Some("analyze_media") => {
                TaskKind::MediaAnalyze
            }
            Some("voice.synthesize") | Some("voice_synthesize") => TaskKind::VoiceSynthesize,
            Some(other) => bail!("unsupported task kind [{}]", other),
            None if task.get("prompt").and_then(Value::as_str).is_some() => TaskKind::TextGenerate,
            None if !context.attachments.is_empty() => TaskKind::MediaAnalyze,
            None if active_turn_text(task.get("context")).is_some() => TaskKind::TextGenerate,
            None if task.get("text").and_then(Value::as_str).is_some() => TaskKind::VoiceSynthesize,
            None if task.get("spoken_text").and_then(Value::as_str).is_some() => {
                TaskKind::VoiceSynthesize
            }
            None => bail!("task is missing a recognized kind/prompt/text payload"),
        };

        let provider_options = task
            .get("provider_options")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        let controller_task = Self {
            kind,
            provider: task
                .get("provider")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| routing_hints.implementation.clone()),
            model: task
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string),
            prompt: task
                .get("prompt")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    context
                        .active_turn
                        .as_ref()
                        .and_then(TurnInput::text_content)
                }),
            text: task.get("text").and_then(Value::as_str).map(str::to_string),
            spoken_text: task
                .get("spoken_text")
                .and_then(Value::as_str)
                .map(str::to_string),
            display_text: task
                .get("display_text")
                .and_then(Value::as_str)
                .map(str::to_string),
            voice: task
                .get("voice")
                .and_then(Value::as_str)
                .map(str::to_string),
            voice_id: task
                .get("voice_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            output_format: task
                .get("output_format")
                .and_then(Value::as_str)
                .map(str::to_string),
            language_code: task
                .get("language_code")
                .and_then(Value::as_str)
                .map(str::to_string),
            response_contract,
            context,
            affordances,
            routing_hints,
            provider_options,
        };

        controller_task.validate()?;
        Ok(controller_task)
    }

    pub fn provider_hint(&self) -> Option<&str> {
        self.provider
            .as_deref()
            .or(self.routing_hints.implementation.as_deref())
    }

    pub fn prompt_text(&self) -> Option<&str> {
        self.prompt.as_deref()
    }

    pub fn media_prompt(&self) -> Option<&str> {
        self.prompt.as_deref()
    }

    pub fn voice_text(&self) -> Option<&str> {
        self.spoken_text
            .as_deref()
            .or(self.text.as_deref())
            .or(self.display_text.as_deref())
    }

    pub fn display_text(&self) -> Option<&str> {
        self.display_text.as_deref().or(self.text.as_deref())
    }

    pub fn requested_voice(&self) -> Option<&str> {
        self.voice.as_deref().or(self.voice_id.as_deref())
    }

    pub fn provider_option_str(&self, key: &str) -> Option<&str> {
        self.provider_options.get(key).and_then(Value::as_str)
    }

    pub fn media_attachments(&self) -> &[AttachmentInput] {
        &self.context.attachments
    }

    pub fn wants_channel(&self, channel: &str) -> bool {
        self.response_contract
            .channels
            .iter()
            .any(|item| item == channel)
    }

    fn validate(&self) -> Result<()> {
        match self.kind {
            TaskKind::TextGenerate => {
                let prompt = self
                    .prompt_text()
                    .context("text.generate task missing prompt")?;
                if prompt.trim().is_empty() {
                    bail!("text.generate task prompt cannot be empty");
                }
            }
            TaskKind::MediaAnalyze => {
                let prompt = self
                    .media_prompt()
                    .context("media.analyze task missing prompt")?;
                if prompt.trim().is_empty() {
                    bail!("media.analyze task prompt cannot be empty");
                }
                if self.context.attachments.is_empty() {
                    bail!("media.analyze task requires at least one attachment");
                }
                if !self.context.attachments.iter().any(|attachment| {
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
                    bail!("media.analyze task requires at least one blob-backed attachment url");
                }
            }
            TaskKind::VoiceSynthesize => {
                let text = self
                    .voice_text()
                    .context("voice.synthesize task missing text/spoken_text/display_text")?;
                if text.trim().is_empty() {
                    bail!("voice.synthesize task text cannot be empty");
                }
            }
        }

        Ok(())
    }
}

fn parse_response_contract(value: Option<&Value>) -> ResponseContract {
    let Some(object) = value.and_then(Value::as_object) else {
        return ResponseContract::default();
    };

    ResponseContract {
        modalities: object
            .get("modalities")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        style: object
            .get("style")
            .and_then(Value::as_str)
            .map(str::to_string),
        channels: object
            .get("channels")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    }
}

fn parse_projection_items(value: Option<&Value>) -> Vec<ProjectionItem> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().map(parse_projection_item).collect())
        .unwrap_or_default()
}

fn parse_projection_item(value: &Value) -> ProjectionItem {
    let object = value.as_object();
    ProjectionItem {
        text: value.as_str().map(str::to_string).or_else(|| {
            object
                .and_then(|obj| obj.get("text"))
                .and_then(Value::as_str)
                .map(str::to_string)
        }),
        source_ref: object
            .and_then(|obj| obj.get("source_ref"))
            .and_then(Value::as_str)
            .map(str::to_string),
        projection_kind: object
            .and_then(|obj| obj.get("projection_kind"))
            .and_then(Value::as_str)
            .map(str::to_string),
        priority: object
            .and_then(|obj| obj.get("priority"))
            .and_then(Value::as_i64),
        token_estimate: object
            .and_then(|obj| obj.get("token_estimate"))
            .and_then(Value::as_u64),
        cache_key: object
            .and_then(|obj| obj.get("cache_key"))
            .and_then(Value::as_str)
            .map(str::to_string),
        truncation_policy: object
            .and_then(|obj| obj.get("truncation_policy"))
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

fn parse_turns(value: Option<&Value>) -> Vec<TurnInput> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().map(parse_turn_input).collect())
        .unwrap_or_default()
}

fn parse_turn_input(value: &Value) -> TurnInput {
    let object = value.as_object();
    TurnInput {
        role: object
            .and_then(|obj| obj.get("role"))
            .and_then(Value::as_str)
            .map(str::to_string),
        text: object
            .and_then(|obj| obj.get("text"))
            .and_then(Value::as_str)
            .map(str::to_string),
        parts: object
            .and_then(|obj| obj.get("parts"))
            .and_then(Value::as_array)
            .map(|parts| parts.iter().map(parse_content_part).collect())
            .unwrap_or_default(),
    }
}

fn parse_content_part(value: &Value) -> ContentPart {
    let object = value.as_object();
    ContentPart {
        part_type: object
            .and_then(|obj| obj.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("text")
            .to_string(),
        text: object
            .and_then(|obj| obj.get("text"))
            .and_then(Value::as_str)
            .map(str::to_string),
        mime_type: object
            .and_then(|obj| obj.get("mime_type"))
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

fn parse_attachments(value: Option<&Value>) -> Vec<AttachmentInput> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    let object = item.as_object();
                    AttachmentInput {
                        kind: object
                            .and_then(|obj| obj.get("kind"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        file_id: object
                            .and_then(|obj| obj.get("file_id"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        mime_type: object
                            .and_then(|obj| obj.get("mime_type"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        url: object
                            .and_then(|obj| obj.get("url"))
                            .or_else(|| object.and_then(|obj| obj.get("blob_download_url")))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        blob_ref: object
                            .and_then(|obj| obj.get("blob_ref"))
                            .or_else(|| object.and_then(|obj| obj.get("blob_id")))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        transport_error: object
                            .and_then(|obj| obj.get("transport_error"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_context(value: Option<&Value>) -> ContextEnvelope {
    let Some(object) = value.and_then(Value::as_object) else {
        return ContextEnvelope::default();
    };

    ContextEnvelope {
        instructions: parse_projection_items(object.get("instructions")),
        identity: parse_projection_items(object.get("identity")),
        memory: parse_projection_items(object.get("memory")),
        dialogue_window: parse_turns(object.get("dialogue_window")),
        active_turn: object.get("active_turn").map(parse_turn_input),
        attachments: parse_attachments(object.get("attachments")),
    }
}

fn parse_affordance_items(value: Option<&Value>) -> Vec<AffordanceItem> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    let object = item.as_object();
                    AffordanceItem {
                        id: object
                            .and_then(|obj| obj.get("id"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        name: object
                            .and_then(|obj| obj.get("name"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        description: object
                            .and_then(|obj| obj.get("description"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        text: item.as_str().map(str::to_string).or_else(|| {
                            object
                                .and_then(|obj| obj.get("text"))
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        }),
                        source_ref: object
                            .and_then(|obj| obj.get("source_ref"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_affordances(value: Option<&Value>) -> Affordances {
    let Some(object) = value.and_then(Value::as_object) else {
        return Affordances::default();
    };

    Affordances {
        skills: parse_affordance_items(object.get("skills")),
        tools: parse_affordance_items(object.get("tools")),
    }
}

fn parse_routing_hints(value: Option<&Value>) -> RoutingHints {
    let Some(object) = value.and_then(Value::as_object) else {
        return RoutingHints::default();
    };

    RoutingHints {
        implementation: object
            .get("implementation")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

fn active_turn_text(context: Option<&Value>) -> Option<String> {
    context
        .and_then(Value::as_object)
        .and_then(|object| object.get("active_turn"))
        .map(parse_turn_input)
        .and_then(|turn| turn.text_content())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioArtifact {
    pub provider: String,
    pub model: String,
    pub voice_id: String,
    pub mime_type: String,
    pub output_format: String,
    pub audio_bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextResult {
    pub display_text: Option<String>,
    pub spoken_text: Option<String>,
    pub working_memory_delta: Option<String>,
    pub follow_up_questions: Vec<String>,
    pub intent_summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderOutput {
    Text {
        content: String,
        display_text: Option<String>,
        spoken_text: Option<String>,
        working_memory_delta: Option<String>,
        follow_up_questions: Vec<String>,
        intent_summary: Option<String>,
    },
    Audio(AudioArtifact),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseArtifact {
    pub kind: String,
    pub mime_type: Option<String>,
    pub output_format: Option<String>,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResponseTrace {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub voice: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerResponseEnvelope {
    pub capability: String,
    pub content: String,
    pub result: Value,
    pub artifacts: Vec<ResponseArtifact>,
    pub trace: ResponseTrace,
    pub provider_output: Value,
}

impl ControllerResponseEnvelope {
    pub fn from_output(
        task: &ControllerTask,
        provider_id: &str,
        output: ProviderOutput,
    ) -> Result<Self> {
        match output {
            ProviderOutput::Text {
                content,
                display_text,
                spoken_text,
                working_memory_delta,
                follow_up_questions,
                intent_summary,
            } => {
                let text_result = TextResult {
                    display_text: display_text.or_else(|| Some(content.clone())),
                    spoken_text,
                    working_memory_delta,
                    follow_up_questions,
                    intent_summary,
                };
                let result = serialize_text_result(task, &text_result);
                let content = result
                    .get("display_text")
                    .and_then(Value::as_str)
                    .unwrap_or(&content)
                    .to_string();

                Ok(Self {
                    capability: task.kind.as_str().to_string(),
                    content,
                    result,
                    artifacts: Vec::new(),
                    trace: ResponseTrace {
                        provider: Some(provider_id.to_string()),
                        model: task.model.clone(),
                        voice: None,
                    },
                    provider_output: Value::Null,
                })
            }
            ProviderOutput::Audio(audio) => {
                let serialized_audio = serialize_audio_artifact(&audio)?;
                Ok(Self {
                    capability: task.kind.as_str().to_string(),
                    content: serialized_audio.clone(),
                    result: json!({
                        "display_text": task.display_text(),
                        "spoken_text": task.voice_text(),
                    }),
                    artifacts: vec![ResponseArtifact {
                        kind: "audio".into(),
                        mime_type: Some(audio.mime_type.clone()),
                        output_format: Some(audio.output_format.clone()),
                        payload: serde_json::from_str(&serialized_audio)?,
                    }],
                    trace: ResponseTrace {
                        provider: Some(provider_id.to_string()),
                        model: Some(audio.model.clone()),
                        voice: Some(audio.voice_id.clone()),
                    },
                    provider_output: Value::Null,
                })
            }
        }
    }
}

fn serialize_text_result(task: &ControllerTask, result: &TextResult) -> Value {
    let channels_requested = !task.response_contract.channels.is_empty();
    let include_spoken = channels_requested && task.wants_channel("spoken_text");
    let include_memory = channels_requested && task.wants_channel("working_memory_delta");
    let include_questions = channels_requested && task.wants_channel("follow_up_questions");
    let include_intent = channels_requested && task.wants_channel("intent_summary");

    json!({
        "display_text": result.display_text,
        "spoken_text": if include_spoken { result.spoken_text.clone() } else { None::<String> },
        "working_memory_delta": if include_memory { result.working_memory_delta.clone() } else { None::<String> },
        "follow_up_questions": if include_questions { result.follow_up_questions.clone() } else { Vec::<String>::new() },
        "intent_summary": if include_intent { result.intent_summary.clone() } else { None::<String> },
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProviderConfigs {
    pub gemini_api_key: Option<String>,
    pub gemini_oauth_access_token: Option<String>,
    pub gemini_oauth_project_id: Option<String>,
    pub gemini_base_url: Option<String>,
    pub elevenlabs_api_key: Option<String>,
    pub elevenlabs_default_voice_id: Option<String>,
}

impl ProviderConfigs {
    pub async fn load(ipc_client: &mut PhiloticClient) -> Result<Self> {
        Ok(Self {
            gemini_api_key: env_override("PHILOTIC_GEMINI_API_KEY").or(
                fetch_config_or_secret_string(ipc_client, "gemini_api_key", "gemini_api_key_ref")
                    .await?,
            ),
            gemini_oauth_access_token: load_env_or_config_secret_string(
                ipc_client,
                "PHILOTIC_GEMINI_OAUTH_ACCESS_TOKEN",
                "PHILOTIC_GEMINI_OAUTH_ACCESS_TOKEN_REF",
                "gemini_oauth_access_token",
                "gemini_oauth_access_token_ref",
            )
            .await?,
            gemini_oauth_project_id: env_override("PHILOTIC_GEMINI_OAUTH_PROJECT_ID")
                .or(fetch_config_string(ipc_client, "gemini_oauth_project_id").await?),
            gemini_base_url: env_override("PHILOTIC_GEMINI_BASE_URL").or(fetch_config_string(
                ipc_client,
                "gemini_base_url",
            )
            .await?),
            elevenlabs_api_key: fetch_config_or_secret_string(
                ipc_client,
                "elevenlabs_api_key",
                "elevenlabs_api_key_ref",
            )
            .await?,
            elevenlabs_default_voice_id: fetch_config_string(ipc_client, "elevenlabs_voice_id")
                .await?,
        })
    }
}

#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn supports(&self, task: &ControllerTask) -> bool;
    async fn invoke(&self, task: &ControllerTask) -> Result<ProviderOutput>;
}

#[derive(Clone)]
pub struct ProviderRegistry {
    providers: Vec<Arc<dyn ModelProvider>>,
}

impl ProviderRegistry {
    pub fn new(providers: Vec<Arc<dyn ModelProvider>>) -> Self {
        Self { providers }
    }

    pub fn resolve(&self, task: &ControllerTask) -> Result<Arc<dyn ModelProvider>> {
        if let Some(provider_id) = task.provider_hint() {
            return self
                .providers
                .iter()
                .find(|provider| provider.id() == provider_id && provider.supports(task))
                .cloned()
                .with_context(|| {
                    format!(
                        "requested provider [{}] does not support {}",
                        provider_id,
                        task.kind.as_str()
                    )
                });
        }

        self.providers
            .iter()
            .find(|provider| provider.supports(task))
            .cloned()
            .with_context(|| format!("no provider registered for {}", task.kind.as_str()))
    }
}

pub fn serialize_audio_artifact(artifact: &AudioArtifact) -> Result<String> {
    Ok(json!({
        "kind": "audio_artifact",
        "provider": artifact.provider,
        "model": artifact.model,
        "voice_id": artifact.voice_id,
        "mime_type": artifact.mime_type,
        "output_format": artifact.output_format,
        "audio_base64": BASE64_STANDARD.encode(&artifact.audio_bytes),
    })
    .to_string())
}

async fn fetch_config_string(ipc_client: &mut PhiloticClient, key: &str) -> Result<Option<String>> {
    let response = ipc_client
        .send_request(IpcRequest::GetConfig { key: key.into() })
        .await?;

    let value = match response {
        IpcResponse::ConfigData {
            key: _,
            value_json: Some(value_json),
        } => {
            if let Ok(val) = serde_json::from_str::<Value>(&value_json) {
                val.as_str().map(str::to_string).or(Some(value_json))
            } else {
                Some(value_json)
            }
        }
        _ => None,
    };

    Ok(value.filter(|value| !value.trim().is_empty()))
}

fn env_override(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn load_env_or_config_secret_string(
    ipc_client: &mut PhiloticClient,
    env_value_key: &str,
    env_ref_key: &str,
    value_key: &str,
    ref_key: &str,
) -> Result<Option<String>> {
    if let Some(value) = env_override(env_value_key) {
        return Ok(Some(value));
    }

    if let Some(secret_ref) = env_override(env_ref_key) {
        return fetch_secret_string(ipc_client, &secret_ref).await;
    }

    fetch_config_or_secret_string(ipc_client, value_key, ref_key).await
}

async fn fetch_config_or_secret_string(
    ipc_client: &mut PhiloticClient,
    value_key: &str,
    ref_key: &str,
) -> Result<Option<String>> {
    if let Some(value) = fetch_config_string(ipc_client, value_key).await? {
        return Ok(Some(value));
    }

    let Some(secret_ref) = fetch_config_string(ipc_client, ref_key).await? else {
        return Ok(None);
    };

    fetch_secret_string(ipc_client, &secret_ref).await
}

async fn fetch_secret_string(
    ipc_client: &mut PhiloticClient,
    secret_ref: &str,
) -> Result<Option<String>> {
    let response = ipc_client
        .send_request(IpcRequest::GetSecret {
            secret_ref: secret_ref.into(),
        })
        .await?;

    let value = match response {
        IpcResponse::SecretData {
            secret_ref: _,
            value_json: Some(value_json),
        } => {
            if let Ok(val) = serde_json::from_str::<Value>(&value_json) {
                val.as_str().map(str::to_string).or(Some(value_json))
            } else {
                Some(value_json)
            }
        }
        IpcResponse::SecretData {
            secret_ref: _,
            value_json: None,
        } => None,
        IpcResponse::Standard {
            ok: false, message, ..
        } => bail!("secret fetch failed: {}", message),
        other => bail!("unexpected GetSecret response: {:?}", other),
    };

    Ok(value.filter(|value| !value.trim().is_empty()))
}

#[cfg(test)]
mod tests {
    use super::{
        AudioArtifact, ControllerResponseEnvelope, ControllerTask, ProviderOutput,
        ProviderRegistry, TaskKind, serialize_audio_artifact,
    };
    use anyhow::Result;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::Arc;

    struct FakeProvider {
        id: &'static str,
        kind: TaskKind,
    }

    #[async_trait]
    impl super::ModelProvider for FakeProvider {
        fn id(&self) -> &'static str {
            self.id
        }

        fn supports(&self, task: &ControllerTask) -> bool {
            task.kind == self.kind
        }

        async fn invoke(&self, _task: &ControllerTask) -> Result<super::ProviderOutput> {
            unreachable!("invoke is not used in registry tests")
        }
    }

    #[test]
    fn infers_legacy_text_task_from_prompt() {
        let task = ControllerTask::from_value(&json!({
            "prompt": "hello",
            "user_content": "hello"
        }))
        .unwrap();

        assert_eq!(task.kind, TaskKind::TextGenerate);
        assert_eq!(task.prompt_text(), Some("hello"));
    }

    #[test]
    fn infers_voice_task_from_spoken_text_payload() {
        let task = ControllerTask::from_value(&json!({
            "kind": "voice.synthesize",
            "provider": "elevenlabs",
            "spoken_text": "Speak now",
            "display_text": "Hi there",
            "voice": "voice-123"
        }))
        .unwrap();

        assert_eq!(task.kind, TaskKind::VoiceSynthesize);
        assert_eq!(task.voice_text(), Some("Speak now"));
        assert_eq!(task.display_text(), Some("Hi there"));
        assert_eq!(task.requested_voice(), Some("voice-123"));
    }

    #[test]
    fn infers_text_task_from_structured_active_turn() {
        let task = ControllerTask::from_value(&json!({
            "context": {
                "identity": [{"text": "You are Jane."}],
                "active_turn": {
                    "role": "user",
                    "parts": [
                        {"type": "text", "text": "Summarize the deployment status."}
                    ]
                }
            }
        }))
        .unwrap();

        assert_eq!(task.kind, TaskKind::TextGenerate);
        assert_eq!(task.prompt_text(), Some("Summarize the deployment status."));
        assert_eq!(task.context.identity.len(), 1);
    }

    #[test]
    fn infers_media_task_from_flat_blob_backed_attachments() {
        let task = ControllerTask::from_value(&json!({
            "action": "analyze_media",
            "prompt": "Describe the media",
            "attachments": [{
                "kind": "photo",
                "file_id": "photo-1",
                "mime_type": "image/jpeg",
                "blob_id": "sha256-1",
                "blob_download_url": "http://127.0.0.1:9001/download/sha256-1"
            }]
        }))
        .unwrap();

        assert_eq!(task.kind, TaskKind::MediaAnalyze);
        assert_eq!(task.media_attachments().len(), 1);
        assert_eq!(
            task.media_attachments()[0].url.as_deref(),
            Some("http://127.0.0.1:9001/download/sha256-1")
        );
    }

    #[test]
    fn uses_routing_hint_as_provider_hint_when_provider_missing() {
        let task = ControllerTask::from_value(&json!({
            "kind": "voice.synthesize",
            "text": "hello",
            "routing_hints": {
                "implementation": "elevenlabs"
            }
        }))
        .unwrap();

        assert_eq!(task.provider_hint(), Some("elevenlabs"));
    }

    #[test]
    fn parses_affordances_separately_from_context() {
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "context": {
                "active_turn": {
                    "role": "user",
                    "text": "What should we do next?"
                }
            },
            "affordances": {
                "skills": [{"id": "ops.checklist", "text": "Use the ops checklist."}],
                "tools": [{"name": "workspace.read", "description": "Read workspace files."}]
            }
        }))
        .unwrap();

        assert_eq!(task.affordances.skills.len(), 1);
        assert_eq!(task.affordances.tools.len(), 1);
        assert_eq!(
            task.affordances.skills[0].id.as_deref(),
            Some("ops.checklist")
        );
        assert_eq!(
            task.affordances.tools[0].name.as_deref(),
            Some("workspace.read")
        );
    }

    #[test]
    fn parses_response_contract_channels() {
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "context": {
                "active_turn": {
                    "text": "hello"
                }
            },
            "response_contract": {
                "channels": ["display_text", "spoken_text", "working_memory_delta"]
            }
        }))
        .unwrap();

        assert!(task.wants_channel("spoken_text"));
        assert!(task.wants_channel("working_memory_delta"));
        assert!(!task.wants_channel("follow_up_questions"));
    }

    #[test]
    fn structured_text_response_preserves_minimal_content_path() {
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "context": {
                "active_turn": {
                    "text": "hello"
                }
            }
        }))
        .unwrap();

        let response = ControllerResponseEnvelope::from_output(
            &task,
            "gemini",
            ProviderOutput::Text {
                content: "Hello back".into(),
                display_text: None,
                spoken_text: Some("Hello back, warmly.".into()),
                working_memory_delta: Some("The user greeted the assistant.".into()),
                follow_up_questions: vec!["How can I help next?".into()],
                intent_summary: Some("Exchange greetings".into()),
            },
        )
        .unwrap();

        assert_eq!(response.content, "Hello back");
        assert_eq!(response.result["display_text"], "Hello back");
        assert!(response.result["spoken_text"].is_null());
        assert_eq!(response.result["follow_up_questions"], json!([]));
    }

    #[test]
    fn structured_text_response_includes_requested_channels() {
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "context": {
                "active_turn": {
                    "text": "hello"
                }
            },
            "response_contract": {
                "channels": ["spoken_text", "working_memory_delta", "follow_up_questions", "intent_summary"]
            }
        }))
        .unwrap();

        let response = ControllerResponseEnvelope::from_output(
            &task,
            "gemini",
            ProviderOutput::Text {
                content: "Hello back".into(),
                display_text: Some("Hello back".into()),
                spoken_text: Some("Hello back, warmly.".into()),
                working_memory_delta: Some("The user greeted the assistant.".into()),
                follow_up_questions: vec!["How can I help next?".into()],
                intent_summary: Some("Exchange greetings".into()),
            },
        )
        .unwrap();

        assert_eq!(response.result["spoken_text"], "Hello back, warmly.");
        assert_eq!(
            response.result["working_memory_delta"],
            "The user greeted the assistant."
        );
        assert_eq!(
            response.result["follow_up_questions"],
            json!(["How can I help next?"])
        );
        assert_eq!(response.result["intent_summary"], "Exchange greetings");
    }

    #[test]
    fn registry_prefers_matching_provider_hint() {
        let registry = ProviderRegistry::new(vec![
            Arc::new(FakeProvider {
                id: "gemini",
                kind: TaskKind::TextGenerate,
            }),
            Arc::new(FakeProvider {
                id: "elevenlabs",
                kind: TaskKind::VoiceSynthesize,
            }),
        ]);

        let task = ControllerTask::from_value(&json!({
            "kind": "voice.synthesize",
            "provider": "elevenlabs",
            "text": "Speak now"
        }))
        .unwrap();

        let provider = registry.resolve(&task).unwrap();
        assert_eq!(provider.id(), "elevenlabs");
    }

    #[test]
    fn audio_artifact_serializes_to_inline_json() {
        let payload = serialize_audio_artifact(&AudioArtifact {
            provider: "elevenlabs".into(),
            model: "eleven_multilingual_v2".into(),
            voice_id: "voice-123".into(),
            mime_type: "audio/mpeg".into(),
            output_format: "mp3_44100_128".into(),
            audio_bytes: b"hello".to_vec(),
        })
        .unwrap();

        assert!(payload.contains("\"kind\":\"audio_artifact\""));
        assert!(payload.contains("\"audio_base64\":\"aGVsbG8=\""));
    }
}
