use crate::r#loop::{ApprovalRequest, ToolCall, ToolResult, TurnPhase};
use crate::reflex::MaterializationContext;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct TurnRecord {
    pub turn_id: String,
    pub user_content: String,
    pub assistant_content: Option<String>,
    /// Unix timestamp (seconds) when this turn was completed.
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextLayerId {
    Identity,
    Relationship,
    Session,
    Working,
    Knowledge,
    RecalledMemory,
    /// Authoritative operational rules derived from the agent graph at runtime.
    /// Distinct from identity prose — treated as hard constraints, not suggestions.
    Rules,
    /// Structured knowledge the agent has stored in its own graph partition.
    /// Entities, relationships, and facts retrieved via GraphRAG at session load.
    AgentGraph,
}

impl ContextLayerId {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Relationship => "relationship",
            Self::Session => "session",
            Self::Working => "working",
            Self::Knowledge => "knowledge",
            Self::RecalledMemory => "recalled_memory",
            Self::Rules => "rules",
            Self::AgentGraph => "agent_graph",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextAuthority {
    Authoritative,
    Advisory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMutability {
    StaticForTurn,
    Refreshable,
    LiveLocal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationTurnScope {
    pub conversation_turn_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_incarnation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_user_id: Option<String>,
    pub trigger_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CognitiveStepScope {
    pub conversation_turn_id: String,
    pub cognitive_step_id: String,
    pub step_kind: String,
    pub iteration: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerContribution {
    pub contribution_id: String,
    pub layer_id: ContextLayerId,
    pub source_id: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub authority: ContextAuthority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_cost: Option<usize>,
    #[serde(default)]
    pub provenance: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerPayload {
    pub layer_id: ContextLayerId,
    pub owner: String,
    pub authority: ContextAuthority,
    pub mutability: ContextMutability,
    pub rendered_content: String,
    #[serde(default)]
    pub source_refs: Vec<String>,
    pub refreshable: bool,
    pub promotion_hint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ContextBudget {
    #[serde(default)]
    pub included_sections: usize,
    #[serde(default)]
    pub trimmed_sections: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextProjection {
    pub conversation_turn: ConversationTurnScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_step: Option<CognitiveStepScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_activation: Option<RoleActivation>,
    pub current_user_message: String,
    #[serde(default)]
    pub layers: Vec<LayerPayload>,
    #[serde(default)]
    pub contributions: Vec<LayerContribution>,
    #[serde(default)]
    pub budget: ContextBudget,
    #[serde(default)]
    pub refresh_plan: Vec<String>,
    #[serde(default)]
    pub provenance_trace: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshRequest {
    #[serde(default)]
    pub layer_ids: Vec<ContextLayerId>,
    pub reason: String,
    pub target_checkpoint: String,
    pub urgency: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionAction {
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concept: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    pub reason: String,
    #[serde(default)]
    pub source_refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookRequest {
    pub hook_name: String,
    pub scope: String,
    pub checkpoint: String,
    pub conversation_turn: ConversationTurnScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cognitive_step: Option<CognitiveStepScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_projection: Option<ContextProjection>,
    #[serde(default)]
    pub inputs: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookResult {
    pub status: String,
    #[serde(default)]
    pub updates: Value,
    #[serde(default)]
    pub emitted_contributions: Vec<LayerContribution>,
    #[serde(default)]
    pub refresh_requests: Vec<RefreshRequest>,
    #[serde(default)]
    pub promotion_actions: Vec<PromotionAction>,
    #[serde(default)]
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RoleActivation {
    pub role_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_incarnation_id: Option<String>,
    pub activation_reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_addendum: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_identity_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_requester_class: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_policy_owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolset_profile_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skillset_profile_ref: Option<String>,
    #[serde(default)]
    pub effective_skillset: Vec<String>,
    #[serde(default)]
    pub effective_skill_guidance: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_memory_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_projection_policy: Option<String>,
    /// Governance document for this role — projected into the Identity layer so the agent
    /// knows its focus, rules, tools, delegation posture, and approval constraints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_manifest: Option<String>,
    /// Turn loop configuration for this role. When loop_script is present,
    /// philote runs the scripted step tree instead of the standard loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_loop_config: Option<ansible_mesh_core::graph::TurnLoopConfig>,
}

/// A single step inside an `ActivePlan`. Tracks the description, optional bound
/// tool, and lifecycle status of the step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: u32,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// One of: "pending", "in_progress", "done", "failed"
    pub status: String,
}

/// The model's declared execution plan for the current turn.
/// Captured from the model response and threaded through re-entry turns so the
/// model can update step status as it executes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivePlan {
    pub goal: String,
    pub steps: Vec<PlanStep>,
    /// One of: "planning", "executing", "done", "failed"
    pub status: String,
    /// Optional context-1 advisory captured alongside the plan on long planning turns.
    /// This is advisory only; approval policy still decides whether a tool may run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_1_advisory: Option<Context1Advisory>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalRiskHint {
    /// Low-risk, read-mostly actions on a planning turn.
    Low,
    /// Some caution is warranted, but the turn is still largely planning-oriented.
    #[default]
    Medium,
    /// The model does not have enough confidence to widen preapproval.
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Context1Advisory {
    pub approval_risk_hint: ApprovalRiskHint,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recommended_preapproved_classes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RecalledMemoryRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_id: Option<String>,
    pub concept: String,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relationships: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recall_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingTurn {
    pub task_id: Uuid,
    pub turn_id: String,
    pub chat_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_user_id: Option<String>,
    pub user_content: String,
    pub final_reply_to: String,
    pub final_reply_role: String,
    pub final_reply_guest_id: Option<String>,
    pub phase: TurnPhase,
    /// Number of model round-trips completed for this turn. Used as the loop counter
    /// against `MAX_TOOL_ITERATIONS` to bound the re-entry loop.
    pub iteration: u32,
    pub pending_tool_call: Option<ToolCall>,
    pub pending_approval: Option<ApprovalRequest>,
    /// Accumulated (tool_call, tool_result) pairs for the current in-flight turn.
    /// Fed back into the model prompt on each re-entry so the model has full context.
    pub working_tool_history: Vec<(ToolCall, ToolResult)>,
    /// Long-term memories auto-recalled for this turn before the first model request.
    pub recalled_memories: Vec<RecalledMemoryRecord>,
    /// Current execution plan if the model has declared one. Updated from model
    /// responses; threaded into context on re-entry.
    pub active_plan: Option<ActivePlan>,
    /// Consecutive tool step failures this turn. Reset to 0 on any successful step.
    /// When this reaches `settings.execution.stall_detection_threshold`, the loop
    /// surfaces to the user instead of re-entering.
    pub consecutive_step_failures: u32,
    /// One-shot corrective note injected into the next model request after a
    /// retryable provider failure. Cleared after projection.
    pub provider_repair_note: Option<String>,
    /// Number of corrective provider retries attempted for this turn.
    pub provider_repair_attempts: u32,
    /// Stashed text content while waiting for voice synthesis to complete.
    pub pending_text_reply: Option<String>,
    pub had_voice_input: bool,
    /// True when a voice transcription result should be routed back into the
    /// normal reasoning loop instead of finalized as the assistant reply.
    pub awaiting_transcription_reentry: bool,
    /// Present when this turn is executing under a LoopScript rather than
    /// the standard tool re-entry loop. Persisted through approval-gate
    /// re-entry via checkpoint_json.
    pub scripted_loop_context: Option<crate::scripted_loop::ScriptedLoopExecutor>,
    /// IDs of every [`Exosome`] dispatched from this turn via `delegate.whisper`.
    /// Used to correlate incoming `paracrine_response` tasks back to this turn
    /// and to reconstruct the full thought graph across the mesh.
    pub associated_paracrine_ids: Vec<String>,
    /// Set when this turn was started by an incoming `paracrine_request`.
    /// Holds the `paracrine_id` from the originating exosome.
    /// When present, `deliver_text_reply` emits `action: "paracrine_response"`
    /// (instead of `"send_reply"`) so A's routing reflex can handle it correctly.
    pub paracrine_origin: Option<String>,
    /// The session_id of the conversation that originated the paracrine request.
    /// Overrides the specialist's own ephemeral session_id in the `paracrine_response`
    /// payload so the orchestrator can route the reply back to the correct channel.
    pub paracrine_reply_session_id: Option<String>,
    /// The chat_id (Telegram / membrane channel) of the originating conversation.
    /// Included in the `paracrine_response` so the routing reflex knows where to deliver.
    pub paracrine_reply_chat_id: Option<String>,
    /// Set to true when the specialist explicitly calls `delegate.merge` during a turn.
    /// Suppresses the auto-emit of `paracrine_response` in deliver_text_reply so there
    /// is no duplicate delivery after the explicit merge already fired.
    pub paracrine_merge_completed: bool,
    /// Set to true when the operator has confirmed a plan_proposal and the parked
    /// plan turn is restored. Injected into the working-state projection so the model
    /// knows it is cleared to execute its declared plan.
    #[serde(default)]
    pub plan_confirmed: bool,
    /// Optional operator steering note provided when confirming a plan. Threaded
    /// into the working-state projection alongside `plan_confirmed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_confirm_note: Option<String>,
    /// Current position in the provider fallback ladder for this turn (0 = primary cloud).
    /// Incremented each time the loop escalates to a lower-tier provider.
    #[serde(default)]
    pub fallback_tier: u8,
    /// Number of same-tier retries attempted for streaming_timeout errors.
    /// Allows one automatic retry before escalating to the next fallback tier.
    #[serde(default)]
    pub streaming_retry_attempts: u8,
}

#[derive(Debug, Clone)]
pub struct ModelReentryPlan {
    pub task_id: Uuid,
    pub user_content: String,
    pub prompt: String,
    pub chat_id: String,
    pub final_reply_to: String,
    pub final_reply_role: String,
    pub final_reply_guest_id: Option<String>,
    pub tools_for_model: Vec<ToolDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ApprovalPolicy {
    #[serde(default)]
    pub auto_approve_all: bool,
    #[serde(default)]
    pub preapproved_tools: Vec<String>,
    #[serde(default)]
    pub preapproved_classes: Vec<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TtsMode {
    /// Never synthesize voice. Default.
    #[default]
    Off,
    /// Synthesize iff the inbound turn had voice/audio input (mirrors modality).
    Auto,
    /// Always synthesize regardless of input type.
    On,
}

/// Per-agent policy controlling how inbound media attachments are routed to model components.
///
/// Each `*_action` field accepts either a well-known action name or a custom string:
/// - `"analyze_media"` (default) — routes to the `media.analyze` capability (e.g. Gemini vision)
/// - `"transcribe"` — routes to the `voice.transcribe` capability (dedicated STT model)
/// - `"describe"` — routes to the `image.describe` capability
/// - `"summarize"` — routes to the `document.summarize` capability
/// - any other string — used verbatim as the action name; capability defaults to `media.analyze`
///
/// The capability string is then resolved against the session's `component_route_assembly` so
/// each kind can be pointed at a different model guest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaRoutingPolicy {
    /// When false, all attachments are stripped and the turn is treated as text-only.
    #[serde(default = "default_true")]
    pub forward_media_to_model: bool,
    /// Action to use for voice/audio attachments. None = "analyze_media".
    #[serde(default)]
    pub voice_action: Option<String>,
    /// Action to use for photo/image attachments. None = "analyze_media".
    #[serde(default)]
    pub image_action: Option<String>,
    /// Action to use for document attachments. None = "analyze_media".
    #[serde(default)]
    pub document_action: Option<String>,
    /// When true (default), tools are stripped from the model request on media turns.
    #[serde(default = "default_true")]
    pub strip_tools_on_media: bool,
    /// Which model implementation handles `voice.transcribe` for this agent.
    /// Accepts the same values as `voice_response_policy.provider`: `"onnx"`, `"gemini"`, etc.
    /// `None` falls back to the hotel-resolved route or the `model` role default.
    #[serde(default)]
    pub transcription_provider: Option<String>,
}

impl Default for MediaRoutingPolicy {
    fn default() -> Self {
        Self {
            forward_media_to_model: true,
            voice_action: None,
            image_action: None,
            document_action: None,
            strip_tools_on_media: true,
            transcription_provider: None,
        }
    }
}

/// Controls whether voice replies are delivered through provider-native audio or
/// the classic TTS follow-up path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceDeliveryMode {
    Synthesized,
    NativeAudio,
}

impl VoiceDeliveryMode {
    pub fn is_native_audio(&self) -> bool {
        matches!(self, Self::NativeAudio)
    }
}

impl Default for VoiceDeliveryMode {
    fn default() -> Self {
        Self::Synthesized
    }
}

/// Controls whether the agent synthesizes speech for its text responses and, if so, how.
///
/// The agent's `voice_id` is the permanent voice identity for this persona — it doesn't change
/// per message. `provider` and `model` select the synthesis engine. When `mode` is `Off`
/// (the default) the agent replies with text only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceResponsePolicy {
    /// Controls when the agent synthesizes speech for its responses.
    #[serde(default)]
    pub mode: TtsMode,
    /// Voice synthesis provider hint (e.g. "elevenlabs", "onnx").
    #[serde(default)]
    pub provider: Option<String>,
    /// The agent's default voice identity — a provider-specific voice ID.
    /// Prefer `effective_voice_id()` over reading this directly; it checks
    /// `voice_ids` first so per-provider IDs are returned when the right
    /// provider is active.
    #[serde(default)]
    pub voice_id: Option<String>,
    /// Per-provider voice IDs. Populated when `/voice <provider> <id>` is used
    /// at runtime, and seeded from `voice_id` on the initial provider at load time.
    /// Allows lossless switching between providers.
    #[serde(default)]
    pub voice_ids: HashMap<String, String>,
    /// Provider model override (e.g. "eleven_multilingual_v2").
    #[serde(default)]
    pub model: Option<String>,
    /// How the response should be delivered when voice synthesis is active.
    #[serde(default)]
    pub delivery_mode: VoiceDeliveryMode,
    /// Speech speed as a percentage of normal rate. `100` means provider default speed.
    /// Lower values slow the voice down; higher values speed it up.
    #[serde(default)]
    pub speed_percent: Option<u16>,
    /// When mode is `On`, also deliver the text alongside the audio as a caption.
    /// Ignored for `Auto` (no caption when mirroring voice input).
    #[serde(default = "default_true")]
    pub send_text_caption: bool,
    /// Fall back to text-only delivery if synthesis fails. Default: true.
    #[serde(default = "default_true")]
    pub fallback_to_text: bool,
}

impl VoiceResponsePolicy {
    /// Returns true if voice synthesis should fire for this turn.
    pub fn is_active(&self, had_voice_input: bool) -> bool {
        match self.mode {
            TtsMode::Off => had_voice_input,
            TtsMode::On => true,
            TtsMode::Auto => had_voice_input,
        }
    }

    /// Returns true if the text caption should be sent alongside the audio.
    pub fn caption_enabled(&self) -> bool {
        match self.mode {
            TtsMode::Off => false,
            TtsMode::On => self.send_text_caption,
            TtsMode::Auto => false,
        }
    }

    /// Returns the voice ID to use for the current provider.
    /// Checks the per-provider `voice_ids` map first, then falls back to the
    /// legacy `voice_id` field. Always use this instead of reading `voice_id` directly.
    pub fn effective_voice_id(&self) -> Option<&str> {
        if let Some(provider) = self.provider.as_deref() {
            if let Some(id) = self.voice_ids.get(provider) {
                return Some(id.as_str());
            }
        }
        self.voice_id.as_deref()
    }

    /// Seeds `voice_ids` from the initial `voice_id` + `provider` so that
    /// switching away and back recovers the original ID automatically.
    pub fn seed_voice_ids(&mut self) {
        if let (Some(provider), Some(voice_id)) =
            (self.provider.as_deref(), self.voice_id.as_deref())
        {
            self.voice_ids
                .entry(provider.to_string())
                .or_insert_with(|| voice_id.to_string());
        }
    }

    /// Switches provider, updating `voice_ids` and returning the resolved voice ID.
    /// When `new_voice_id` is given it is persisted for this provider.
    /// When omitted the previously stored ID for the provider is used if available.
    pub fn switch_provider(
        &mut self,
        new_provider: &str,
        new_voice_id: Option<&str>,
    ) -> Option<String> {
        self.provider = Some(new_provider.to_string());
        if let Some(vid) = new_voice_id {
            self.voice_ids
                .insert(new_provider.to_string(), vid.to_string());
        }
        self.voice_ids.get(new_provider).cloned()
    }
}

impl Default for VoiceResponsePolicy {
    fn default() -> Self {
        Self {
            mode: TtsMode::Off,
            provider: None,
            voice_id: None,
            voice_ids: HashMap::new(),
            model: None,
            delivery_mode: VoiceDeliveryMode::Synthesized,
            speed_percent: None,
            send_text_caption: true,
            fallback_to_text: true,
        }
    }
}

/// Configures the preferred default route for model responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseRouteMode {
    Auto,
    TextOnly,
    ImageMultimodal,
    AudioMultimodal,
    RealtimeWebsocket,
}

impl ResponseRouteMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::TextOnly => "text_only",
            Self::ImageMultimodal => "image_multimodal",
            Self::AudioMultimodal => "audio_multimodal",
            Self::RealtimeWebsocket => "realtime_websocket",
        }
    }

    pub fn is_auto(self) -> bool {
        matches!(self, Self::Auto)
    }
}

impl Default for ResponseRouteMode {
    fn default() -> Self {
        Self::Auto
    }
}

/// Agent-level preference for the default model response route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ResponseRoutePolicy {
    /// Preferred default route when the current turn does not already force a
    /// route via explicit provider options or multimodal content.
    #[serde(default)]
    pub default_route: ResponseRouteMode,
}

/// Configures how the rolling `dialogue_window` is assembled and bounded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextWindowPolicy {
    /// Maximum age of turns included in the dialogue window, in minutes.
    /// Turns older than this are dropped before the context is built.
    /// Default: 10, min: 2, max: 60.
    pub dialogue_window_minutes: u32,
    /// Maximum total character budget for the dialogue window.
    /// Oldest turns are dropped first when the budget is exceeded.
    /// Default: 10_000, min: 1_000, max: 50_000.
    pub dialogue_window_chars: usize,
    /// When true (default), assistant turns in the dialogue window include
    /// tool call names and args alongside the response text.
    pub include_tool_calls: bool,
    /// Maximum characters included per tool result in the tool call history sent
    /// to the model. Results exceeding this are truncated with a note.
    /// Default: 32_768, min: 1_000, max: 500_000.
    #[serde(default = "default_max_tool_result_chars")]
    pub max_tool_result_chars: usize,
}

fn default_max_tool_result_chars() -> usize {
    32_768
}

impl Default for ContextWindowPolicy {
    fn default() -> Self {
        Self {
            dialogue_window_minutes: 10,
            dialogue_window_chars: 10_000,
            include_tool_calls: true,
            max_tool_result_chars: 32_768,
        }
    }
}

/// Configures the two-axis memory strategy: local rolling window + on-demand recall.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryPolicy {
    /// Number of recent memory entries kept in the rolling local window.
    /// Default: 10, min: 3, max: 30.
    pub memory_window_size: usize,
    /// When true (default), the `memory.recall` local agent tool is available
    /// for on-demand Muninn retrieval.
    pub long_term_recall_enabled: bool,
    /// Default result limit passed to `engine.activate()` when the model calls
    /// `memory.recall` without an explicit limit.
    /// Default: 5, min: 1, max: 20.
    pub recall_limit: usize,
}

impl Default for MemoryPolicy {
    fn default() -> Self {
        Self {
            memory_window_size: 10,
            long_term_recall_enabled: true,
            recall_limit: 5,
        }
    }
}

/// Configures the cognitive execution loop: iteration cap, plan behaviour, stall detection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionPolicy {
    /// Maximum model round-trips per turn before the turn is failed.
    /// Default: 10, min: 1, max: 50.
    pub iteration_cap: u32,
    /// When true (default), a structured plan is required as the first model
    /// output whenever a skill is activated.
    pub plan_required_on_skill: bool,
    /// When true (default), intermediate turn events (step_started, step_completed,
    /// step_failed) are emitted to membrane during execution.
    pub stream_tool_events: bool,
    /// Number of consecutive step failures before the loop surfaces to the user
    /// instead of continuing. Default: 3.
    pub stall_detection_threshold: u32,
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            iteration_cap: 20,
            plan_required_on_skill: true,
            stream_tool_events: true,
            stall_detection_threshold: 3,
        }
    }
}

/// Top-level settings tree for a philote session.
/// Stored in the context graph keyed by agent_id; fetched at session init.
/// Configurable via `agent.configure` with `settings.*` config path prefix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AgentSettings {
    #[serde(default)]
    pub context_window: ContextWindowPolicy,
    #[serde(default)]
    pub memory: MemoryPolicy,
    #[serde(default)]
    pub execution: ExecutionPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentProfile {
    #[serde(default)]
    pub persona_name: Option<String>,
    #[serde(default)]
    pub soul_text: Option<String>,
    #[serde(default)]
    pub identity_text: Option<String>,
    #[serde(default)]
    pub user_context_text: Option<String>,
    #[serde(default)]
    pub agents_text: Option<String>,
    #[serde(default)]
    pub memory_summary: Option<String>,
    #[serde(default)]
    pub response_route_policy: ResponseRoutePolicy,
    #[serde(default)]
    pub media_routing_policy: MediaRoutingPolicy,
    #[serde(default)]
    pub voice_response_policy: VoiceResponsePolicy,
    /// Optional filesystem path used as the default working directory for shell tools.
    /// Populated from `import_workspace` in the agent's hotel configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_workspace: Option<String>,
    /// Tools available in every new session for this agent, before any per-session
    /// `/tools add` commands. Falls back to `["echo"]` when empty.
    #[serde(default)]
    pub default_toolset: Vec<String>,
    /// Materialization context pushed by the hotel at guest spawn time.
    ///
    /// Contains the membrane bindings and capabilities known at spawn. The reflex
    /// engine evaluates this at session open to derive the initial routing policy,
    /// ensuring turn zero is correct without agent self-discovery.
    #[serde(default)]
    pub reflex_context: MaterializationContext,
    /// Role incarnation name to activate automatically on every fresh session.
    /// When set, `ensure_session_loaded` applies this role before the first turn
    /// so the agent always starts with the correct manifest, toolset, and skills
    /// without requiring an explicit `handoff.to_role` call.
    ///
    /// Example: `"orchestrator"` — ensures the orchestrator posture (including
    /// `delegate.whisper` skill guidance) is present from turn zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_role_name: Option<String>,
    /// IANA timezone name for the human user (e.g. `"America/New_York"`).
    /// Injected into the cognitive header so the model can interpret relative
    /// time references correctly. Optional; UTC is assumed when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_timezone: Option<String>,
    /// Stable mesh-facing principal for the operator when the hotel has linked
    /// the human to a projected user identity. Bounded projection only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_principal_id: Option<String>,
    /// Human-friendly preferred name for the operator when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_preferred_name: Option<String>,
    /// Non-secret primary email alias for the operator when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_primary_email: Option<String>,
    /// Linked provider labels (e.g. `google`, `github`) projected by the hotel.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub user_linked_providers: Vec<String>,
    /// Authoritative list of role names registered for this agent, fetched from
    /// the hotel graph at startup. Injected into the system prompt so the model
    /// always uses exact, DB-sourced names in delegate.whisper / handoff.to_role.
    /// Not serialized — refreshed each time the philote process starts.
    #[serde(default, skip_serializing)]
    pub agent_role_names: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SessionBindings {
    #[serde(default)]
    pub effective_toolset: Vec<String>,
    #[serde(default)]
    pub effective_skillset: Vec<String>,
    #[serde(default)]
    pub effective_skill_guidance: Vec<String>,
    #[serde(default)]
    pub effective_workspace_ref: Option<String>,
    #[serde(default)]
    pub workspace_runner_config: Option<TaskRunnerBaseConfig>,
    #[serde(default)]
    pub transport_reply_target: Option<TransportReplyTargetBinding>,
    #[serde(default)]
    pub component_routes: Vec<ComponentRouteBinding>,
    #[serde(default)]
    pub effective_model_controller: Option<String>,
    #[serde(default)]
    pub preferred_tool_runner_incarnation: Option<String>,
    #[serde(default)]
    pub preferred_tool_runner: Option<String>,
    #[serde(default)]
    pub preferred_hotel_id: Option<String>,
    #[serde(default)]
    pub preferred_environment_id: Option<String>,
    #[serde(default)]
    pub allowed_tool_runner_incarnations: Vec<ToolRunnerIncarnationBinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TaskRunnerBaseConfig {
    #[serde(default)]
    pub default_workspace_ref: Option<String>,
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub max_read_bytes: Option<usize>,
    #[serde(default)]
    pub max_search_results: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TransportReplyTargetBinding {
    pub target_node: String,
    pub target_role: String,
    #[serde(default)]
    pub target_guest_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ComponentRouteBinding {
    pub capability: String,
    #[serde(default = "default_selection_mode")]
    pub selection_mode: String,
    #[serde(default)]
    pub implementation: Option<String>,
    #[serde(default)]
    pub incarnation: Option<String>,
    #[serde(default)]
    pub preferred_hotel_id: Option<String>,
    #[serde(default)]
    pub preferred_environment_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolDefinition {
    pub tool_name: String,
    pub description: String,
    #[serde(default)]
    pub input_schema: serde_json::Value,
    /// Approval and projection class, e.g. "session", "workspace", "utility", "capability".
    /// Drives class-based approval policy and tool projection filtering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolExecutionRoute {
    pub target_node: String,
    pub target_role: String,
    #[serde(default)]
    pub runner_id: Option<String>,
    #[serde(default)]
    pub incarnation_id: Option<String>,
    #[serde(default)]
    pub hotel_id: Option<String>,
    #[serde(default)]
    pub environment_id: Option<String>,
    #[serde(default)]
    pub task_runner_kind: Option<String>,
    #[serde(default)]
    pub task_runner_config: Option<TaskRunnerBaseConfig>,
    pub execution_mode: String,
    #[serde(default = "default_route_availability")]
    pub availability_state: String,
    #[serde(default)]
    pub selection_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolRunnerIncarnationBinding {
    pub incarnation_id: String,
    #[serde(default)]
    pub runner_id: Option<String>,
    #[serde(default)]
    pub hotel_id: Option<String>,
    #[serde(default)]
    pub environment_id: Option<String>,
    #[serde(default)]
    pub target_node: Option<String>,
    #[serde(default)]
    pub target_role: Option<String>,
    #[serde(default)]
    pub supported_tools: Vec<String>,
    #[serde(default = "default_capability_execution_mode")]
    pub execution_mode: String,
    #[serde(default = "default_route_availability")]
    pub availability_state: String,
    #[serde(default)]
    pub selection_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolPolicyAnnotation {
    pub policy_class: String,
    pub approval_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ToolAssembly {
    #[serde(default)]
    pub tools_for_model: Vec<ToolDefinition>,
    #[serde(default)]
    pub execution_routes: std::collections::BTreeMap<String, ToolExecutionRoute>,
    #[serde(default)]
    pub policy_annotations: std::collections::BTreeMap<String, ToolPolicyAnnotation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ComponentExecutionRoute {
    pub target_node: String,
    pub target_role: String,
    #[serde(default)]
    pub incarnation_id: Option<String>,
    #[serde(default)]
    pub hotel_id: Option<String>,
    #[serde(default)]
    pub environment_id: Option<String>,
    pub execution_mode: String,
    #[serde(default = "default_route_availability")]
    pub availability_state: String,
    #[serde(default)]
    pub selection_reason: Option<String>,
    #[serde(default)]
    pub target_capability: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ComponentRouteAssembly {
    #[serde(default)]
    pub execution_routes: std::collections::BTreeMap<String, ComponentExecutionRoute>,
}

fn default_route_availability() -> String {
    "live".into()
}

fn default_capability_execution_mode() -> String {
    "capability".into()
}

fn default_selection_mode() -> String {
    "preferred".into()
}

/// Lightweight tracking record for a subagent spawned during this session.
///
/// Persisted in [`SessionState::active_subagents`] on every successful
/// `subagent.spawn` so subsequent tools (`subagent.release`, `subagent.abort`,
/// `subagent.list`) can reference the guest by ID without re-querying the hotel.
#[derive(Debug, Clone)]
pub struct SpawnedSubagentRef {
    pub guest_id: String,
    pub kind: String,
    pub lease_epoch: u64,
    pub lease_expires_at: u64,
}

// ── User Task Engine types ─────────────────────────────────────────────────────

/// Risk classification for a task step or the task as a whole.
/// `Destructive > Moderate > Safe` — operators approve a ceiling once and the
/// agent runs autonomously within it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Safe,
    Moderate,
    Destructive,
}

impl Default for RiskLevel {
    fn default() -> Self {
        RiskLevel::Safe
    }
}

/// Lifecycle state of a `UserTask`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Planning,
    AwaitingApproval,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

/// Execution state of one step within a `UserTask`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskStepStatus {
    #[default]
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
}

/// A single step in a `UserTask` plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStep {
    pub idx: usize,
    pub description: String,
    pub risk: RiskLevel,
    pub status: TaskStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<u64>,
}

/// A durable, user-visible multi-step task owned by the agent.
///
/// Stored in the hotel context graph as `kind = "user_task"` so it survives
/// hotel restarts and can be queried at any time via `/tasks`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTask {
    pub task_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub chat_id: String,
    pub goal: String,
    pub steps: Vec<TaskStep>,
    pub status: TaskStatus,
    pub approved_risk_ceiling: RiskLevel,
    /// `0` = cloud model (full autonomy within ceiling); `1+` = local model
    /// (always requires explicit approval for `Destructive` steps).
    pub planning_model_tier: u8,
    pub quiet: bool,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<u64>,
    pub next_step_idx: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_note: Option<String>,
}

