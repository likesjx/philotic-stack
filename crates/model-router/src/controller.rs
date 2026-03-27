use ansible_mesh_core::catalog_rights::{has_right, tool_right};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use media_prep::serialize_audio_artifact_envelope;
use philotic_client::{IpcRequest, IpcResponse, PhiloticClient};
use serde_json::{Map, Value, json};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    TextGenerate,
    ResponseGenerate,
    MediaAnalyze,
    AudioTranscribe,
    VoiceDialogue,
    VoiceSynthesize,
    Embed,
}

impl TaskKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TextGenerate => "text.generate",
            Self::ResponseGenerate => "response.generate",
            Self::MediaAnalyze => "media.analyze",
            Self::AudioTranscribe => "voice.transcribe",
            Self::VoiceDialogue => "voice.dialogue",
            Self::VoiceSynthesize => "voice.synthesize",
            Self::Embed => "text.embed",
        }
    }

    pub fn is_native_live(&self) -> bool {
        matches!(self, Self::ResponseGenerate | Self::VoiceDialogue)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RequestClass {
    #[default]
    Cognitive,
    Transform,
    Synthesis,
    Embedding,
}

impl RequestClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cognitive => "cognitive",
            Self::Transform => "transform",
            Self::Synthesis => "synthesis",
            Self::Embedding => "embedding",
        }
    }

    fn infer_from_kind(kind: TaskKind) -> Self {
        match kind {
            TaskKind::TextGenerate | TaskKind::ResponseGenerate | TaskKind::VoiceDialogue => {
                Self::Cognitive
            }
            TaskKind::MediaAnalyze | TaskKind::AudioTranscribe => Self::Transform,
            TaskKind::VoiceSynthesize => Self::Synthesis,
            TaskKind::Embed => Self::Embedding,
        }
    }

    fn parse(value: Option<&Value>, kind: TaskKind) -> Result<Self> {
        let Some(raw) = value.and_then(Value::as_str) else {
            return Ok(Self::infer_from_kind(kind));
        };

        match raw {
            "cognitive" => Ok(Self::Cognitive),
            "transform" => Ok(Self::Transform),
            "synthesis" => Ok(Self::Synthesis),
            "embedding" => Ok(Self::Embedding),
            other => bail!("unsupported request_class [{}]", other),
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
pub struct ToolHistoryEntry {
    pub index: usize,
    pub tool_name: String,
    pub arguments: Value,
    pub result: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContextEnvelope {
    pub instructions: Vec<ProjectionItem>,
    pub identity: Vec<ProjectionItem>,
    pub memory: Vec<ProjectionItem>,
    pub recalled_memory: Vec<ProjectionItem>,
    pub dialogue_window: Vec<TurnInput>,
    pub active_turn: Option<TurnInput>,
    pub tool_history: Vec<ToolHistoryEntry>,
    pub active_plan: Option<Value>,
    pub attachments: Vec<AttachmentInput>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConversationTurnProjection {
    pub conversation_turn_id: Option<String>,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub source: Option<String>,
    pub active_incarnation_id: Option<String>,
    pub trigger_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContextProjectionEnvelope {
    pub conversation_turn: Option<ConversationTurnProjection>,
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
    pub model_ref: Option<String>,
    pub controller_role: Option<String>,
    pub capability: Option<String>,
    pub context_envelope: Option<String>,
    pub stage: Option<String>,
    pub streaming: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerTask {
    pub kind: TaskKind,
    pub request_class: RequestClass,
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
    pub context_projection: ContextProjectionEnvelope,
    pub affordances: Affordances,
    pub routing_hints: RoutingHints,
    pub provider_options: Map<String, Value>,
    pub effective_rights: Vec<String>,
    /// Tools the agent has available this turn. Non-empty signals tool-enabled mode.
    /// Each entry is a raw JSON object with at minimum `tool_name` and `description`.
    pub tools: Vec<Value>,
}

impl ControllerTask {
    pub fn from_value(task: &Value) -> Result<Self> {
        let response_contract = parse_response_contract(task.get("response_contract"));
        let mut context = parse_context(task.get("context"));
        let context_projection = parse_context_projection(task.get("context_projection"));
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
            Some("response.generate") | Some("response_generate") => TaskKind::ResponseGenerate,
            Some("media.analyze") | Some("media_analyze") | Some("analyze_media") => {
                TaskKind::MediaAnalyze
            }
            Some("voice.transcribe") | Some("audio.transcribe") | Some("transcribe") => {
                TaskKind::AudioTranscribe
            }
            Some("voice.dialogue") | Some("voice_dialogue") => TaskKind::VoiceDialogue,
            Some("voice.synthesize") | Some("voice_synthesize") => TaskKind::VoiceSynthesize,
            Some("text.embed") | Some("embed") => TaskKind::Embed,
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

        let request_class = RequestClass::parse(task.get("request_class"), kind)?;

        let provider_options = task
            .get("provider_options")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        let controller_task = Self {
            kind,
            request_class,
            provider: task
                .get("provider")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| routing_hints.implementation.clone()),
            model: task
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| routing_hints.model_ref.clone()),
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
            context_projection,
            affordances,
            routing_hints,
            provider_options,
            effective_rights: task
                .get("effective_rights")
                .and_then(Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            tools: task
                .get("tools_for_model")
                .and_then(Value::as_array)
                .map(|arr| arr.iter().filter(|v| v.is_object()).cloned().collect())
                .unwrap_or_default(),
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

    pub fn is_cognitive(&self) -> bool {
        self.request_class == RequestClass::Cognitive
    }

    pub fn composed_prompt_text(&self) -> Option<String> {
        let mut sections = Vec::new();

        if !self.context.identity.is_empty() {
            let text = join_projection_items(&self.context.identity);
            if !text.is_empty() {
                sections.push(format!("[Identity]\n{text}"));
            }
        }

        if let Some(active_incarnation_id) = self.active_incarnation_id() {
            sections.push(format!(
                "[Active incarnation]\nThe currently active role/incarnation is `{active_incarnation_id}`."
            ));
        }

        if !self.context.instructions.is_empty() {
            let text = join_projection_items(&self.context.instructions);
            if !text.is_empty() {
                sections.push(format!("[Instructions]\n{text}"));
            }
        }

        if !self.context.memory.is_empty() {
            let text = join_projection_items(&self.context.memory);
            if !text.is_empty() {
                sections.push(format!("[Memory]\n{text}"));
            }
        }

        if !self.context.recalled_memory.is_empty() {
            let text = join_projection_items(&self.context.recalled_memory);
            if !text.is_empty() {
                sections.push(format!("[Recalled memory]\n{text}"));
            }
        }

        if !self.context.dialogue_window.is_empty() {
            let dialogue = self
                .context
                .dialogue_window
                .iter()
                .filter_map(|turn| {
                    let role = turn.role.as_deref().unwrap_or("unknown");
                    turn.text_content().map(|text| format!("{role}: {text}"))
                })
                .collect::<Vec<_>>()
                .join("\n");
            if !dialogue.is_empty() {
                sections.push(format!("[Dialogue window]\n{dialogue}"));
            }
        }

        if let Some(active_turn) = self.context.active_turn.as_ref() {
            if let Some(text) = active_turn.text_content() {
                let role = active_turn.role.as_deref().unwrap_or("user");
                sections.push(format!("[Active turn]\n{role}: {text}"));
            }
        }

        if !self.context.tool_history.is_empty() {
            let history = self
                .context
                .tool_history
                .iter()
                .map(|entry| {
                    let args = serde_json::to_string(&entry.arguments).unwrap_or_default();
                    format!(
                        "Call {n}: {name}({args})\nResult {n}: {result}",
                        n = entry.index,
                        name = entry.tool_name,
                        result = entry.result,
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            sections.push(format!(
                "[Tool call history]\n{history}\n\nReview the above tool results and continue. \
                 Call another tool if needed, or respond to the user if you have enough information."
            ));
        }

        if let Some(plan) = self.context.active_plan.as_ref() {
            let goal = plan
                .get("goal")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let status = plan
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let steps = plan
                .get("steps")
                .and_then(Value::as_array)
                .map(|steps| {
                    steps
                        .iter()
                        .map(|step| {
                            let id = step
                                .get("id")
                                .or_else(|| step.get("index"))
                                .and_then(Value::as_u64)
                                .map(|n| n.to_string())
                                .unwrap_or_else(|| "?".into());
                            let description = step
                                .get("description")
                                .and_then(Value::as_str)
                                .unwrap_or("unnamed step");
                            let step_status = step
                                .get("status")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown");
                            format!("[{step_status}] {id}. {description}")
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .filter(|steps| !steps.is_empty())
                .unwrap_or_else(|| "No steps recorded.".into());
            sections.push(format!(
                "[Active plan]\nGoal: {goal}\nStatus: {status}\nSteps:\n{steps}"
            ));
        }

        if let Some(prompt) = self
            .prompt
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            sections.push(format!("[Prompt]\n{prompt}"));
        }

        if sections.is_empty() {
            None
        } else {
            Some(sections.join("\n\n"))
        }
    }

    pub fn media_prompt(&self) -> Option<&str> {
        self.prompt.as_deref()
    }

    pub fn active_incarnation_id(&self) -> Option<&str> {
        self.context_projection
            .conversation_turn
            .as_ref()
            .and_then(|turn| turn.active_incarnation_id.as_deref())
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
        if !self.effective_rights.is_empty() {
            for tool in &self.tools {
                let Some(tool_name) = tool.get("tool_name").and_then(Value::as_str) else {
                    bail!("tool entry missing tool_name");
                };
                if !has_right(&self.effective_rights, &tool_right(tool_name)) {
                    bail!(
                        "tool [{}] is present in tools_for_model without matching effective_rights",
                        tool_name
                    );
                }
            }
        }

        match self.kind {
            TaskKind::TextGenerate | TaskKind::ResponseGenerate => {
                let prompt = self
                    .composed_prompt_text()
                    .with_context(|| format!("{} task missing prompt", self.kind.as_str()))?;
                if prompt.trim().is_empty() {
                    bail!("{} task prompt cannot be empty", self.kind.as_str());
                }
            }
            TaskKind::MediaAnalyze | TaskKind::AudioTranscribe => {
                let kind_label = self.kind.as_str();
                let prompt = self
                    .media_prompt()
                    .with_context(|| format!("{kind_label} task missing prompt"))?;
                if prompt.trim().is_empty() {
                    bail!("{kind_label} task prompt cannot be empty");
                }
                if self.context.attachments.is_empty() {
                    bail!("{kind_label} task requires at least one attachment");
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
                    bail!("{kind_label} task requires at least one blob-backed attachment url");
                }
            }
            TaskKind::VoiceDialogue => {
                let prompt = self
                    .media_prompt()
                    .context("voice.dialogue task missing prompt")?;
                if prompt.trim().is_empty() {
                    bail!("voice.dialogue task prompt cannot be empty");
                }
                if self.context.attachments.is_empty() {
                    bail!("voice.dialogue task requires at least one attachment");
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
                    bail!("voice.dialogue task requires at least one blob-backed attachment url");
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
            TaskKind::Embed => {
                let text = self
                    .composed_prompt_text()
                    .context("text.embed task missing input text (prompt)")?;
                if text.trim().is_empty() {
                    bail!("text.embed task input text cannot be empty");
                }
            }
        }

        match self.request_class {
            RequestClass::Cognitive => {
                if !matches!(
                    self.kind,
                    TaskKind::TextGenerate | TaskKind::ResponseGenerate | TaskKind::VoiceDialogue
                ) {
                    bail!(
                        "request_class [{}] is not currently supported for [{}]",
                        self.request_class.as_str(),
                        self.kind.as_str()
                    );
                }
            }
            RequestClass::Transform => {
                if !matches!(
                    self.kind,
                    TaskKind::MediaAnalyze | TaskKind::AudioTranscribe
                ) {
                    bail!(
                        "request_class [{}] is not currently supported for [{}]",
                        self.request_class.as_str(),
                        self.kind.as_str()
                    );
                }
            }
            RequestClass::Synthesis => {
                if self.kind != TaskKind::VoiceSynthesize {
                    bail!(
                        "request_class [{}] is not currently supported for [{}]",
                        self.request_class.as_str(),
                        self.kind.as_str()
                    );
                }
            }
            RequestClass::Embedding => {
                if self.kind != TaskKind::Embed {
                    bail!(
                        "request_class [embedding] requires task kind [text.embed], got [{}]",
                        self.kind.as_str()
                    );
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

    let tool_history = object
        .get("tool_history")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    let obj = entry.as_object()?;
                    Some(ToolHistoryEntry {
                        index: obj
                            .get("index")
                            .and_then(Value::as_u64)
                            .map(|n| n as usize)
                            .unwrap_or(0),
                        tool_name: obj
                            .get("tool_name")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown")
                            .to_string(),
                        arguments: obj
                            .get("arguments")
                            .cloned()
                            .unwrap_or(Value::Object(Default::default())),
                        result: obj
                            .get("result")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    ContextEnvelope {
        instructions: parse_projection_items(object.get("instructions")),
        identity: parse_projection_items(object.get("identity")),
        memory: parse_projection_items(object.get("memory")),
        recalled_memory: parse_projection_items(object.get("recalled_memory")),
        dialogue_window: parse_turns(object.get("dialogue_window")),
        active_turn: object.get("active_turn").map(parse_turn_input),
        tool_history,
        active_plan: object.get("active_plan").cloned(),
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

fn parse_context_projection(value: Option<&Value>) -> ContextProjectionEnvelope {
    let Some(object) = value.and_then(Value::as_object) else {
        return ContextProjectionEnvelope::default();
    };

    let conversation_turn = object
        .get("conversation_turn")
        .and_then(Value::as_object)
        .map(|turn| ConversationTurnProjection {
            conversation_turn_id: turn
                .get("conversation_turn_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            session_id: turn
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            agent_id: turn
                .get("agent_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            source: turn
                .get("source")
                .and_then(Value::as_str)
                .map(str::to_string),
            active_incarnation_id: turn
                .get("active_incarnation_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            trigger_kind: turn
                .get("trigger_kind")
                .and_then(Value::as_str)
                .map(str::to_string),
        });

    ContextProjectionEnvelope { conversation_turn }
}

fn join_projection_items(items: &[ProjectionItem]) -> String {
    items
        .iter()
        .filter_map(|item| item.text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>()
        .join("\n\n")
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
        model_ref: object
            .get("model_ref")
            .and_then(Value::as_str)
            .map(str::to_string),
        controller_role: object
            .get("controller_role")
            .and_then(Value::as_str)
            .map(str::to_string),
        capability: object
            .get("capability")
            .and_then(Value::as_str)
            .map(str::to_string),
        context_envelope: object
            .get("context_envelope")
            .and_then(Value::as_str)
            .map(str::to_string),
        stage: object
            .get("stage")
            .and_then(Value::as_str)
            .map(str::to_string),
        streaming: object.get("streaming").and_then(Value::as_bool),
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
    pub partial_replies: Vec<String>,
    pub working_memory_delta: Option<String>,
    pub follow_up_questions: Vec<String>,
    pub intent_summary: Option<String>,
    pub memory_concept: Option<String>,
    pub memory_candidate: Option<serde_json::Value>,
    pub active_plan: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ProviderOutput {
    Text {
        content: String,
        display_text: Option<String>,
        spoken_text: Option<String>,
        partial_replies: Vec<String>,
        working_memory_delta: Option<String>,
        follow_up_questions: Vec<String>,
        intent_summary: Option<String>,
        memory_concept: Option<String>,
        memory_candidate: Option<serde_json::Value>,
        active_plan: Option<serde_json::Value>,
    },
    Audio(AudioArtifact),
    /// Model chose to call a tool rather than produce a text response.
    ToolCall {
        tool_name: String,
        arguments: Value,
    },
    /// A dense embedding vector produced by a local ONNX embedding model.
    ///
    /// `model_gen` carries the provenance token `"{repo}@{sha8}"` so consumers
    /// can detect embedding-space drift when the model is hot-swapped.
    Embedding {
        vector: Vec<f32>,
        model_gen: String,
    },
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
                partial_replies,
                working_memory_delta,
                follow_up_questions,
                intent_summary,
                memory_concept,
                memory_candidate,
                active_plan,
            } => {
                let text_result = TextResult {
                    display_text: display_text.or_else(|| Some(content.clone())),
                    spoken_text,
                    partial_replies,
                    working_memory_delta,
                    follow_up_questions,
                    intent_summary,
                    memory_concept,
                    memory_candidate,
                    active_plan,
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
            ProviderOutput::Embedding { vector, model_gen } => {
                let vector_value: Value = vector
                    .iter()
                    .map(|&f| serde_json::Number::from_f64(f as f64).map(Value::Number))
                    .collect::<Option<Vec<_>>>()
                    .map(Value::Array)
                    .unwrap_or(Value::Null);

                Ok(Self {
                    capability: task.kind.as_str().to_string(),
                    content: model_gen.clone(),
                    result: json!({
                        "model_gen": model_gen,
                        "dim": vector.len(),
                    }),
                    artifacts: vec![ResponseArtifact {
                        kind: "embedding".into(),
                        mime_type: Some("application/json".into()),
                        output_format: None,
                        payload: json!({
                            "vector": vector_value,
                            "model_gen": model_gen,
                            "dim": vector.len(),
                        }),
                    }],
                    trace: ResponseTrace {
                        provider: Some(provider_id.to_string()),
                        model: task.model.clone(),
                        voice: None,
                    },
                    provider_output: Value::Null,
                })
            }
            // ToolCall is intercepted in runtime.rs before reaching from_output.
            // If it somehow arrives here, treat it as an error.
            ProviderOutput::ToolCall { tool_name, .. } => {
                anyhow::bail!(
                    "ProviderOutput::ToolCall({}) reached from_output — should have been intercepted in runtime",
                    tool_name
                )
            }
        }
    }
}

fn serialize_text_result(task: &ControllerTask, result: &TextResult) -> Value {
    let channels_requested = !task.response_contract.channels.is_empty();
    let include_spoken = channels_requested && task.wants_channel("spoken_text");
    let include_partial = channels_requested && task.wants_channel("partial_replies");
    let include_memory = channels_requested && task.wants_channel("working_memory_delta");
    let include_questions = channels_requested && task.wants_channel("follow_up_questions");
    let include_intent = channels_requested && task.wants_channel("intent_summary");
    let include_concept = channels_requested && task.wants_channel("memory_concept");
    let include_memory_candidate = channels_requested && task.wants_channel("memory_candidate");
    let include_plan = channels_requested && task.wants_channel("active_plan");

    json!({
        "display_text": result.display_text,
        "spoken_text": if include_spoken { result.spoken_text.clone() } else { None::<String> },
        "partial_replies": if include_partial { result.partial_replies.clone() } else { Vec::<String>::new() },
        "working_memory_delta": if include_memory { result.working_memory_delta.clone() } else { None::<String> },
        "follow_up_questions": if include_questions { result.follow_up_questions.clone() } else { Vec::<String>::new() },
        "intent_summary": if include_intent { result.intent_summary.clone() } else { None::<String> },
        "memory_concept": if include_concept { result.memory_concept.clone() } else { None::<String> },
        "memory_candidate": if include_memory_candidate { result.memory_candidate.clone() } else { None::<Value> },
        "active_plan": if include_plan { result.active_plan.clone() } else { None::<Value> },
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

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NativeLiveSessionMarker {
    pub provider_session_id: Option<String>,
    pub resumption_handle: Option<String>,
    pub protocol: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NativeLiveTurnOutput {
    pub final_output: ProviderOutput,
    pub partial_text_deltas: Vec<String>,
    pub session_marker: Option<NativeLiveSessionMarker>,
    pub generation_complete: bool,
    pub turn_complete: bool,
}

#[async_trait]
pub trait NativeLiveProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn supports_live(&self, task: &ControllerTask) -> bool;
    async fn invoke_live(&self, task: &ControllerTask) -> Result<NativeLiveTurnOutput>;
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

#[derive(Clone)]
pub struct NativeLiveRegistry {
    providers: Vec<Arc<dyn NativeLiveProvider>>,
}

impl NativeLiveRegistry {
    pub fn new(providers: Vec<Arc<dyn NativeLiveProvider>>) -> Self {
        Self { providers }
    }

    pub fn resolve(&self, task: &ControllerTask) -> Result<Arc<dyn NativeLiveProvider>> {
        if let Some(provider_id) = task.provider_hint() {
            return self
                .providers
                .iter()
                .find(|provider| provider.id() == provider_id && provider.supports_live(task))
                .cloned()
                .with_context(|| {
                    format!(
                        "requested native-live provider [{}] does not support {}",
                        provider_id,
                        task.kind.as_str()
                    )
                });
        }

        self.providers
            .iter()
            .find(|provider| provider.supports_live(task))
            .cloned()
            .with_context(|| {
                format!(
                    "no native-live provider registered for {}",
                    task.kind.as_str()
                )
            })
    }
}

pub fn serialize_audio_artifact(artifact: &AudioArtifact) -> Result<String> {
    serialize_audio_artifact_envelope(
        Some(&artifact.provider),
        Some(&artifact.model),
        Some(&artifact.voice_id),
        &artifact.mime_type,
        Some(&artifact.output_format),
        &artifact.audio_bytes,
    )
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
        AudioArtifact, ControllerResponseEnvelope, ControllerTask, NativeLiveRegistry,
        NativeLiveTurnOutput, ProviderOutput, ProviderRegistry, RequestClass, TaskKind,
        serialize_audio_artifact,
    };
    use anyhow::Result;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::Arc;

    struct FakeProvider {
        id: &'static str,
        kind: TaskKind,
    }

    struct FakeLiveProvider {
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

    #[async_trait]
    impl super::NativeLiveProvider for FakeLiveProvider {
        fn id(&self) -> &'static str {
            self.id
        }

        fn supports_live(&self, task: &ControllerTask) -> bool {
            task.kind == self.kind
        }

        async fn invoke_live(&self, _task: &ControllerTask) -> Result<NativeLiveTurnOutput> {
            unreachable!("invoke_live is not used in registry tests")
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
        assert_eq!(task.request_class, RequestClass::Cognitive);
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
        assert_eq!(task.request_class, RequestClass::Synthesis);
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
        assert_eq!(task.request_class, RequestClass::Cognitive);
        assert_eq!(task.prompt_text(), Some("Summarize the deployment status."));
        assert_eq!(task.context.identity.len(), 1);
        let composed = task
            .composed_prompt_text()
            .expect("structured task should compose a prompt");
        assert!(composed.contains("[Identity]"));
        assert!(composed.contains("You are Jane."));
        assert!(composed.contains("[Active turn]"));
        assert!(composed.contains("Summarize the deployment status."));
    }

    #[test]
    fn composed_prompt_includes_active_incarnation_when_present() {
        let task = ControllerTask::from_value(&json!({
            "context": {
                "identity": [{"text": "You are Jane."}],
                "instructions": [{"text": "Session status: active.", "projection_kind": "session"}],
                "memory": [{"text": "User prefers direct collaboration.", "projection_kind": "relationship"}],
                "active_turn": {
                    "role": "user",
                    "text": "Status?"
                }
            },
            "context_projection": {
                "conversation_turn": {
                    "conversation_turn_id": "turn-1",
                    "agent_id": "agent-jane-01",
                    "active_incarnation_id": "agent-jane:developer"
                }
            }
        }))
        .unwrap();

        assert_eq!(task.active_incarnation_id(), Some("agent-jane:developer"));
        assert!(task.is_cognitive());
        let composed = task
            .composed_prompt_text()
            .expect("structured task should compose a prompt");
        assert!(composed.contains("[Active incarnation]"));
        assert!(composed.contains("agent-jane:developer"));
        assert!(composed.contains("[Instructions]"));
        assert!(composed.contains("[Memory]"));
    }

    #[test]
    fn parse_context_preserves_active_plan() {
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "context": {
                "active_turn": {
                    "role": "user",
                    "text": "Continue the task."
                },
                "active_plan": {
                    "goal": "finish the task",
                    "status": "in_progress",
                    "steps": [
                        {
                            "id": 1,
                            "description": "inspect repo",
                            "status": "done"
                        },
                        {
                            "id": 2,
                            "description": "apply fix",
                            "status": "in_progress"
                        }
                    ]
                }
            }
        }))
        .unwrap();

        let plan = task
            .context
            .active_plan
            .as_ref()
            .expect("active plan should survive context parsing");
        assert_eq!(plan["goal"], "finish the task");
        assert_eq!(plan["steps"][1]["description"], "apply fix");
    }

    #[test]
    fn composed_prompt_includes_active_plan() {
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "context": {
                "identity": [{"text": "You are Jane."}],
                "active_turn": {
                    "role": "user",
                    "text": "Keep going."
                },
                "tool_history": [{
                    "index": 1,
                    "tool_name": "workspace.read",
                    "arguments": {"path": "README.md"},
                    "result": "Read successfully."
                }],
                "active_plan": {
                    "goal": "summarize README and patch the issue",
                    "status": "in_progress",
                    "steps": [
                        {
                            "id": 1,
                            "description": "read README",
                            "status": "done"
                        },
                        {
                            "id": 2,
                            "description": "patch the issue",
                            "status": "in_progress"
                        }
                    ]
                }
            }
        }))
        .unwrap();

        let composed = task
            .composed_prompt_text()
            .expect("structured task should compose a prompt");
        assert!(composed.contains("[Tool call history]"));
        assert!(composed.contains("[Active plan]"));
        assert!(composed.contains("Goal: summarize README and patch the issue"));
        assert!(composed.contains("[done] 1. read README"));
        assert!(composed.contains("[in_progress] 2. patch the issue"));
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
        assert_eq!(task.request_class, RequestClass::Transform);
        assert_eq!(task.media_attachments().len(), 1);
        assert_eq!(
            task.media_attachments()[0].url.as_deref(),
            Some("http://127.0.0.1:9001/download/sha256-1")
        );
    }

    #[test]
    fn parses_audio_transcribe_task_from_voice_transcribe_kind() {
        let task = ControllerTask::from_value(&json!({
            "kind": "voice.transcribe",
            "prompt": "Transcribe this audio verbatim.",
            "attachments": [{
                "kind": "voice",
                "file_id": "voice-1",
                "mime_type": "audio/ogg",
                "blob_id": "sha256-audio-1",
                "blob_download_url": "http://127.0.0.1:9001/download/sha256-audio-1"
            }]
        }))
        .unwrap();

        assert_eq!(task.kind, TaskKind::AudioTranscribe);
        assert_eq!(task.request_class, RequestClass::Transform);
        assert_eq!(task.kind.as_str(), "voice.transcribe");
        assert_eq!(task.media_attachments().len(), 1);
    }

    #[test]
    fn parses_audio_transcribe_from_legacy_transcribe_action() {
        let task = ControllerTask::from_value(&json!({
            "action": "transcribe",
            "prompt": "Transcribe this audio verbatim.",
            "attachments": [{
                "kind": "audio",
                "file_id": "audio-1",
                "mime_type": "audio/mp4",
                "blob_download_url": "http://127.0.0.1:9001/download/sha256-audio-1"
            }]
        }))
        .unwrap();

        assert_eq!(task.kind, TaskKind::AudioTranscribe);
        assert_eq!(task.request_class, RequestClass::Transform);
    }

    #[test]
    fn parses_response_generate_as_cognitive_native_live_species() {
        let task = ControllerTask::from_value(&json!({
            "kind": "response.generate",
            "request_class": "cognitive",
            "prompt": "Respond with concise spoken and text output.",
            "response_contract": {
                "modalities": ["text", "audio"],
                "channels": ["spoken_text"]
            }
        }))
        .unwrap();

        assert_eq!(task.kind, TaskKind::ResponseGenerate);
        assert_eq!(task.request_class, RequestClass::Cognitive);
        assert_eq!(task.kind.as_str(), "response.generate");
    }

    #[test]
    fn parses_voice_dialogue_as_cognitive_attachment_task() {
        let task = ControllerTask::from_value(&json!({
            "kind": "voice.dialogue",
            "request_class": "cognitive",
            "prompt": "Continue this voice conversation naturally.",
            "attachments": [{
                "kind": "voice",
                "file_id": "voice-1",
                "mime_type": "audio/ogg",
                "blob_download_url": "http://127.0.0.1:9001/download/sha256-audio-1"
            }]
        }))
        .unwrap();

        assert_eq!(task.kind, TaskKind::VoiceDialogue);
        assert_eq!(task.request_class, RequestClass::Cognitive);
        assert_eq!(task.media_attachments().len(), 1);
    }

    #[test]
    fn rejects_voice_dialogue_without_audio_attachment() {
        let err = ControllerTask::from_value(&json!({
            "kind": "voice.dialogue",
            "request_class": "cognitive",
            "prompt": "Continue this voice conversation naturally."
        }))
        .expect_err("voice.dialogue should require audio attachment");

        assert!(err.to_string().contains("requires at least one attachment"));
    }

    #[test]
    fn respects_explicit_request_class_on_text_tasks() {
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "request_class": "cognitive",
            "context": {
                "active_turn": {"role": "user", "text": "hello"}
            }
        }))
        .unwrap();

        assert_eq!(task.request_class, RequestClass::Cognitive);
    }

    #[test]
    fn rejects_mismatched_request_class_for_task_kind() {
        let err = ControllerTask::from_value(&json!({
            "kind": "voice.synthesize",
            "request_class": "cognitive",
            "spoken_text": "hello",
            "voice": "voice-1"
        }))
        .expect_err("mismatched request class should fail");

        assert!(err.to_string().contains("request_class"));
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
    fn parses_extended_stage_routing_hints() {
        let task = ControllerTask::from_value(&json!({
            "kind": "voice.synthesize",
            "text": "hello",
            "routing_hints": {
                "implementation": "elevenlabs",
                "model_ref": "eleven_multilingual_v2",
                "controller_role": "model.elevenlabs",
                "capability": "voice.synthesize",
                "context_envelope": "egress",
                "stage": "egress",
                "streaming": true
            }
        }))
        .unwrap();

        assert_eq!(
            task.routing_hints.implementation.as_deref(),
            Some("elevenlabs")
        );
        assert_eq!(
            task.routing_hints.model_ref.as_deref(),
            Some("eleven_multilingual_v2")
        );
        assert_eq!(
            task.routing_hints.controller_role.as_deref(),
            Some("model.elevenlabs")
        );
        assert_eq!(
            task.routing_hints.capability.as_deref(),
            Some("voice.synthesize")
        );
        assert_eq!(
            task.routing_hints.context_envelope.as_deref(),
            Some("egress")
        );
        assert_eq!(task.routing_hints.stage.as_deref(), Some("egress"));
        assert_eq!(task.routing_hints.streaming, Some(true));
        assert_eq!(task.model.as_deref(), Some("eleven_multilingual_v2"));
    }

    #[test]
    fn parses_effective_rights() {
        let task = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "prompt": "hello",
            "effective_rights": ["tool.echo", "component.text.generate"]
        }))
        .expect("task should parse");

        assert_eq!(
            task.effective_rights,
            vec![
                "tool.echo".to_string(),
                "component.text.generate".to_string()
            ]
        );
    }

    #[test]
    fn rejects_tool_surface_without_matching_effective_right() {
        let err = ControllerTask::from_value(&json!({
            "kind": "text.generate",
            "prompt": "hello",
            "effective_rights": ["tool.echo"],
            "tools_for_model": [{
                "tool_name": "workspace.read",
                "description": "Read a file",
                "input_schema": { "type": "object" }
            }]
        }))
        .expect_err("task should reject tool surface without matching right");

        assert!(err.to_string().contains(
            "tool [workspace.read] is present in tools_for_model without matching effective_rights"
        ));
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
                partial_replies: vec!["Hello".into(), "Hello back".into()],
                working_memory_delta: Some("The user greeted the assistant.".into()),
                follow_up_questions: vec!["How can I help next?".into()],
                intent_summary: Some("Exchange greetings".into()),
                memory_concept: None,
                memory_candidate: None,
                active_plan: None,
            },
        )
        .unwrap();

        assert_eq!(response.content, "Hello back");
        assert_eq!(response.result["display_text"], "Hello back");
        assert!(response.result["spoken_text"].is_null());
        assert_eq!(response.result["partial_replies"], json!([]));
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
                "channels": ["spoken_text", "partial_replies", "working_memory_delta", "follow_up_questions", "intent_summary", "memory_candidate"]
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
                partial_replies: vec!["Hello".into(), "Hello back".into()],
                working_memory_delta: Some("The user greeted the assistant.".into()),
                follow_up_questions: vec!["How can I help next?".into()],
                intent_summary: Some("Exchange greetings".into()),
                memory_concept: None,
                memory_candidate: Some(json!({
                    "concept": "greeting-exchange",
                    "content": "The user greeted the assistant and reopened the collaboration thread.",
                    "tags": ["greeting", "session"]
                })),
                active_plan: None,
            },
        )
        .unwrap();

        assert_eq!(response.result["spoken_text"], "Hello back, warmly.");
        assert_eq!(
            response.result["partial_replies"],
            json!(["Hello", "Hello back"])
        );
        assert_eq!(
            response.result["working_memory_delta"],
            "The user greeted the assistant."
        );
        assert_eq!(
            response.result["memory_candidate"]["concept"],
            "greeting-exchange"
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
    fn native_live_registry_prefers_matching_provider_hint() {
        let registry = NativeLiveRegistry::new(vec![
            Arc::new(FakeLiveProvider {
                id: "gemini",
                kind: TaskKind::VoiceDialogue,
            }),
            Arc::new(FakeLiveProvider {
                id: "other-live",
                kind: TaskKind::ResponseGenerate,
            }),
        ]);

        let task = ControllerTask::from_value(&json!({
            "kind": "voice.dialogue",
            "provider": "gemini",
            "prompt": "Continue this voice conversation naturally.",
            "attachments": [{
                "kind": "voice",
                "file_id": "voice-1",
                "mime_type": "audio/ogg",
                "blob_download_url": "http://127.0.0.1:9001/download/sha256-audio-1"
            }]
        }))
        .unwrap();

        let provider = registry.resolve(&task).unwrap();
        assert_eq!(provider.id(), "gemini");
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
