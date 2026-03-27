use crate::catalog::{tool_catalog, tool_class, tool_requires_approval};
use crate::r#loop::{ApprovalRequest, ToolCall, ToolResult, TurnPhase};
use philotic_client::{
    HandoffBundle, SubagentCompletionContract, SubagentContextPacket, SubagentDelegation,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

fn local_node_id() -> String {
    std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string())
}

fn local_agent_id() -> String {
    std::env::var("PHILOTIC_AGENT_ID").unwrap_or_else(|_| "agent-jane-01".to_string())
}

#[derive(Debug, Clone)]
pub struct TurnRecord {
    pub turn_id: String,
    pub user_content: String,
    pub assistant_content: Option<String>,
    /// Unix timestamp (seconds) when this turn was completed.
    pub created_at: u64,
}

/// Strip binary/audio payloads from user content before storing in turn history.
/// Voice messages arrive as `{"audio_base64":"..."}` — 1–2 MB blobs that must
/// not flow into the model context or checkpoint graph nodes.
fn sanitize_turn_content_for_history(content: &str) -> String {
    if content.len() > 500 {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(content) {
            if v.get("audio_base64").is_some() {
                return "[voice message]".to_string();
            }
        }
    }
    content.to_string()
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecalledMemoryRecord {
    pub concept: String,
    pub content: String,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnRoutingStageKind {
    Ingress,
    Cognition,
    Egress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnContextEnvelopeKind {
    Ingress,
    Cognitive,
    Egress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRoutingStagePlan {
    pub kind: TurnRoutingStageKind,
    pub capability: String,
    pub request_class: String,
    pub context_envelope: TurnContextEnvelopeKind,
    pub controller_role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_hint: Option<String>,
    #[serde(default)]
    pub streaming: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRoutingPlan {
    pub trigger: String,
    pub stages: Vec<TurnRoutingStagePlan>,
}

#[derive(Debug, Clone)]
pub struct WorkingTurn {
    pub task_id: Uuid,
    pub turn_id: String,
    pub chat_id: String,
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
    /// Compiled stage-by-stage routing plan for the active turn. This is the
    /// turn-local execution contract, not a second routing authority.
    pub turn_routing_plan: Option<TurnRoutingPlan>,
    /// True when a voice transcription result should be routed back into the
    /// normal reasoning loop instead of finalized as the assistant reply.
    pub awaiting_transcription_reentry: bool,
    /// Present when this turn is executing under a LoopScript rather than
    /// the standard tool re-entry loop. Persisted through approval-gate
    /// re-entry via checkpoint_json.
    pub scripted_loop_context: Option<crate::scripted_loop::ScriptedLoopExecutor>,
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
}

impl Default for MediaRoutingPolicy {
    fn default() -> Self {
        Self {
            forward_media_to_model: true,
            voice_action: None,
            image_action: None,
            document_action: None,
        }
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
    /// Voice synthesis provider hint (e.g. "elevenlabs").
    #[serde(default)]
    pub provider: Option<String>,
    /// The agent's permanent voice identity — a provider-specific voice ID.
    #[serde(default)]
    pub voice_id: Option<String>,
    /// Provider model override (e.g. "eleven_multilingual_v2").
    #[serde(default)]
    pub model: Option<String>,
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
}

impl Default for VoiceResponsePolicy {
    fn default() -> Self {
        Self {
            mode: TtsMode::Off,
            provider: None,
            voice_id: None,
            model: None,
            speed_percent: None,
            send_text_caption: true,
            fallback_to_text: true,
        }
    }
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
            iteration_cap: 10,
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

fn first_line_summary(text: &str) -> String {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string())
        .unwrap_or_else(|| "empty".into())
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

#[derive(Debug, Clone)]
pub struct SessionState {
    pub session_id: String,
    pub agent_id: String,
    pub source: String,
    pub active_incarnation_id: Option<String>,
    pub role_activation: Option<RoleActivation>,
    pub agent_profile: AgentProfile,
    pub settings: AgentSettings,
    pub status: String,
    pub approval_policy: ApprovalPolicy,
    pub bindings: SessionBindings,
    pub component_route_assembly: ComponentRouteAssembly,
    pub tool_assembly: ToolAssembly,
    pub recent_turns: Vec<TurnRecord>,
    pub active_turn: Option<WorkingTurn>,
    /// Subagents spawned during this session that have not yet been released or aborted.
    pub active_subagents: Vec<SpawnedSubagentRef>,
    /// Working summary carried in from the most recent inbound handoff bundle.
    /// Injected into context when the next user turn begins under this role.
    pub last_handoff_summary: Option<String>,
    /// Durable behavioral rules fetched from the hotel context graph at session init.
    /// Injected into the `instructions` section of every cognitive call and never
    /// rolled off by the dialogue window.
    pub rules: Vec<Value>,
}

impl SessionState {
    pub fn new(session_id: String, agent_id: String, source: String) -> Self {
        let bindings = SessionBindings::default();
        Self {
            session_id,
            agent_id,
            source,
            active_incarnation_id: None,
            role_activation: None,
            agent_profile: AgentProfile::default(),
            settings: AgentSettings::default(),
            status: "active".into(),
            approval_policy: ApprovalPolicy::default(),
            tool_assembly: default_tool_assembly_for_bindings(&bindings),
            component_route_assembly: ComponentRouteAssembly::default(),
            bindings,
            recent_turns: Vec::new(),
            active_turn: None,
            last_handoff_summary: None,
            active_subagents: Vec::new(),
            rules: Vec::new(),
        }
    }

    pub fn start_turn(&mut self, turn: WorkingTurn) {
        self.active_turn = Some(turn);
    }

    pub fn set_active_turn_phase(&mut self, phase: TurnPhase) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.phase = phase;
        }
    }

    pub fn bump_active_turn_iteration(&mut self) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.iteration += 1;
        }
    }

    pub fn set_pending_tool_call(&mut self, tool_call: ToolCall) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.pending_tool_call = Some(tool_call);
        }
    }

    pub fn clear_pending_tool_call(&mut self) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.pending_tool_call = None;
        }
    }

    pub fn set_pending_approval(&mut self, approval: ApprovalRequest) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.pending_approval = Some(approval);
        }
    }

    pub fn clear_pending_approval(&mut self) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.pending_approval = None;
        }
    }

    pub fn set_active_plan(&mut self, plan: ActivePlan) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.active_plan = Some(plan);
        }
    }

    /// Increment consecutive step failure counter and return the new count.
    pub fn increment_step_failures(&mut self) -> u32 {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.consecutive_step_failures += 1;
            turn.consecutive_step_failures
        } else {
            0
        }
    }

    /// Reset consecutive step failure counter (called on a successful step).
    pub fn reset_step_failures(&mut self) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.consecutive_step_failures = 0;
        }
    }

    pub fn set_provider_repair_note(&mut self, note: String) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.provider_repair_note = Some(note);
        }
    }

    pub fn provider_repair_attempts(&self) -> u32 {
        self.active_turn
            .as_ref()
            .map(|turn| turn.provider_repair_attempts)
            .unwrap_or(0)
    }

    pub fn increment_provider_repair_attempts(&mut self) -> u32 {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.provider_repair_attempts += 1;
            turn.provider_repair_attempts
        } else {
            0
        }
    }

    pub fn set_pending_text_reply(&mut self, text: String) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.pending_text_reply = Some(text);
        }
    }

    pub fn set_active_turn_routing_plan(&mut self, plan: TurnRoutingPlan) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.turn_routing_plan = Some(plan);
        }
    }

    pub fn active_turn_routing_plan(&self) -> Option<&TurnRoutingPlan> {
        self.active_turn
            .as_ref()
            .and_then(|turn| turn.turn_routing_plan.as_ref())
    }

    pub fn take_pending_text_reply(&mut self) -> Option<String> {
        self.active_turn.as_mut()?.pending_text_reply.take()
    }

    pub fn set_active_turn_awaiting_transcription_reentry(&mut self, awaiting: bool) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.awaiting_transcription_reentry = awaiting;
        }
    }

    pub fn active_turn_awaiting_transcription_reentry(&self) -> bool {
        self.active_turn
            .as_ref()
            .map(|turn| turn.awaiting_transcription_reentry)
            .unwrap_or(false)
    }

    pub fn with_scripted_executor_mut<F>(&mut self, f: F)
    where
        F: FnOnce(&mut crate::scripted_loop::ScriptedLoopExecutor),
    {
        if let Some(turn) = self.active_turn.as_mut() {
            if let Some(exec) = turn.scripted_loop_context.as_mut() {
                f(exec);
            }
        }
    }

    pub fn scripted_executor_advance(&self) -> Option<crate::scripted_loop::ScriptedLoopDecision> {
        self.active_turn
            .as_ref()
            .and_then(|t| t.scripted_loop_context.as_ref())
            .map(|exec| exec.advance())
    }

    pub fn prepare_transcription_reentry(&mut self, transcript: &str) -> Option<ModelReentryPlan> {
        let normalized = transcript.trim();
        if normalized.is_empty() {
            return None;
        }

        let (
            task_id,
            chat_id,
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
            user_content,
        ) = {
            let active_turn = self.active_turn.as_mut()?;
            active_turn.user_content = normalized.to_string();
            active_turn.iteration += 1;
            active_turn.phase = TurnPhase::WaitingModel;
            active_turn.awaiting_transcription_reentry = false;
            (
                active_turn.task_id,
                active_turn.chat_id.clone(),
                active_turn.final_reply_to.clone(),
                active_turn.final_reply_role.clone(),
                active_turn.final_reply_guest_id.clone(),
                active_turn.user_content.clone(),
            )
        };

        let tools_for_model = self.project_tools_for_turn(&user_content);
        let prompt = self.build_prompt_with_tools(&user_content, &tools_for_model);
        Some(ModelReentryPlan {
            task_id,
            user_content,
            prompt,
            chat_id,
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
            tools_for_model,
        })
    }

    pub fn complete_active_turn(&mut self, assistant_content: String) -> Option<WorkingTurn> {
        let turn = self.active_turn.take()?;
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.recent_turns.push(TurnRecord {
            turn_id: turn.turn_id.clone(),
            user_content: sanitize_turn_content_for_history(&turn.user_content),
            assistant_content: Some(assistant_content),
            created_at: now_secs,
        });
        let window_size = self.settings.memory.memory_window_size.max(1);
        if self.recent_turns.len() > window_size {
            let drain = self.recent_turns.len() - window_size;
            self.recent_turns.drain(0..drain);
        }
        Some(turn)
    }

    /// Returns true if the current approval policy permits auto-approving this request.
    ///
    /// Evaluation order:
    /// 1. `auto_approve_all` — blanket approval for everything
    /// 2. `preapproved_tools` — the specific tool name is on the preapproval list
    /// 3. `preapproved_classes` — the tool's catalog class is on the preapproval list
    ///
    /// Pass `tool` as the pending `ToolCall` so class-based approval can be evaluated.
    /// When `tool` is `None` (e.g. a free-form model approval request with no associated
    /// tool), only `auto_approve_all` is checked.
    pub fn approval_policy_allows(
        &self,
        _approval: &ApprovalRequest,
        tool: Option<&ToolCall>,
    ) -> bool {
        if self.approval_policy.auto_approve_all {
            return true;
        }
        if let Some(tool) = tool {
            if self
                .approval_policy
                .preapproved_tools
                .contains(&tool.tool_name)
            {
                return true;
            }
            if let Some(class) = tool_class(&tool.tool_name) {
                if self
                    .approval_policy
                    .preapproved_classes
                    .iter()
                    .any(|c| c == class)
                {
                    return true;
                }
            }
        }
        false
    }

    pub fn set_preapprove_this_session(&mut self) {
        self.approval_policy.auto_approve_all = true;
    }

    pub fn reset_approval_policy(&mut self) {
        self.approval_policy.preapproved_tools.clear();
        self.approval_policy.preapproved_classes.clear();
    }

    /// Known approval class names that can be pre-approved by class rather than by tool name.
    const APPROVAL_CLASSES: &'static [&'static str] = &[
        "session",
        "workspace",
        "utility",
        "capability",
        "config",
        "handoff",
    ];

    /// Preapprove a tool name or class name for this session.
    ///
    /// Returns a human-readable confirmation string. If `name` matches a known class name it is
    /// added to `preapproved_classes`; otherwise it is added to `preapproved_tools`.
    pub fn preapprove_by_name(&mut self, name: &str) -> String {
        if Self::APPROVAL_CLASSES.contains(&name) {
            if !self
                .approval_policy
                .preapproved_classes
                .contains(&name.to_string())
            {
                self.approval_policy
                    .preapproved_classes
                    .push(name.to_string());
            }
            format!("Preapproved: `{name}` (class)")
        } else {
            if !self
                .approval_policy
                .preapproved_tools
                .contains(&name.to_string())
            {
                self.approval_policy
                    .preapproved_tools
                    .push(name.to_string());
            }
            format!("Preapproved: `{name}` (tool)")
        }
    }

    /// Apply a configuration change from the `agent.configure` tool.
    ///
    /// Returns a human-readable confirmation string on success, or an error message.
    /// After calling this, the caller should rebuild the tool assembly if bindings changed.
    pub fn apply_configure(
        &mut self,
        config_path: &str,
        value: &serde_json::Value,
        operation: &str,
    ) -> Result<String, String> {
        match config_path {
            // ── Approval policy ──────────────────────────────────────────────────
            "approval_policy.auto_approve_all" => {
                let v = value
                    .as_bool()
                    .ok_or("approval_policy.auto_approve_all requires a boolean value")?;
                self.approval_policy.auto_approve_all = v;
                Ok(format!("Set approval_policy.auto_approve_all = {v}."))
            }
            "approval_policy.preapproved_tools" => {
                let item = value
                    .as_str()
                    .ok_or("approval_policy.preapproved_tools requires a string value")?
                    .to_string();
                apply_string_list_op(
                    &mut self.approval_policy.preapproved_tools,
                    &item,
                    operation,
                )
                .map(|_| format!("{operation} '{item}' in approval_policy.preapproved_tools."))
            }
            "approval_policy.preapproved_classes" => {
                let item = value
                    .as_str()
                    .ok_or("approval_policy.preapproved_classes requires a string value")?
                    .to_string();
                apply_string_list_op(
                    &mut self.approval_policy.preapproved_classes,
                    &item,
                    operation,
                )
                .map(|_| format!("{operation} '{item}' in approval_policy.preapproved_classes."))
            }
            // ── Agent profile ────────────────────────────────────────────────────
            "profile.persona_name" => {
                self.agent_profile.persona_name = Some(
                    value
                        .as_str()
                        .ok_or("profile.persona_name requires a string value")?
                        .to_string(),
                );
                Ok("Updated profile.persona_name.".into())
            }
            "profile.soul_text" => {
                self.agent_profile.soul_text = Some(
                    value
                        .as_str()
                        .ok_or("profile.soul_text requires a string value")?
                        .to_string(),
                );
                Ok("Updated profile.soul_text.".into())
            }
            "profile.identity_text" => {
                self.agent_profile.identity_text = Some(
                    value
                        .as_str()
                        .ok_or("profile.identity_text requires a string value")?
                        .to_string(),
                );
                Ok("Updated profile.identity_text.".into())
            }
            "profile.user_context_text" => {
                self.agent_profile.user_context_text = Some(
                    value
                        .as_str()
                        .ok_or("profile.user_context_text requires a string value")?
                        .to_string(),
                );
                Ok("Updated profile.user_context_text.".into())
            }
            "profile.memory_summary" => {
                self.agent_profile.memory_summary = Some(
                    value
                        .as_str()
                        .ok_or("profile.memory_summary requires a string value")?
                        .to_string(),
                );
                Ok("Updated profile.memory_summary.".into())
            }
            // ── Session bindings ─────────────────────────────────────────────────
            "bindings.effective_toolset" => {
                let item = value
                    .as_str()
                    .ok_or("bindings.effective_toolset requires a string value")?
                    .to_string();
                apply_string_list_op(&mut self.bindings.effective_toolset, &item, operation)
                    .map(|_| format!("{operation} '{item}' in bindings.effective_toolset."))
            }
            "bindings.effective_skillset" => {
                let item = value
                    .as_str()
                    .ok_or("bindings.effective_skillset requires a string value")?
                    .to_string();
                apply_string_list_op(&mut self.bindings.effective_skillset, &item, operation)
                    .map(|_| format!("{operation} '{item}' in bindings.effective_skillset."))
            }
            // ── Settings tree ─────────────────────────────────────────────────
            "settings.context_window.dialogue_window_minutes" => {
                let v = value
                    .as_u64()
                    .ok_or("settings.context_window.dialogue_window_minutes requires a u32")?;
                let clamped = (v as u32).clamp(2, 60);
                self.settings.context_window.dialogue_window_minutes = clamped;
                Ok(format!(
                    "Set settings.context_window.dialogue_window_minutes = {clamped}."
                ))
            }
            "settings.context_window.dialogue_window_chars" => {
                let v = value
                    .as_u64()
                    .ok_or("settings.context_window.dialogue_window_chars requires a usize")?;
                let clamped = (v as usize).clamp(1_000, 50_000);
                self.settings.context_window.dialogue_window_chars = clamped;
                Ok(format!(
                    "Set settings.context_window.dialogue_window_chars = {clamped}."
                ))
            }
            "settings.context_window.include_tool_calls" => {
                let v = value
                    .as_bool()
                    .ok_or("settings.context_window.include_tool_calls requires a boolean")?;
                self.settings.context_window.include_tool_calls = v;
                Ok(format!(
                    "Set settings.context_window.include_tool_calls = {v}."
                ))
            }
            "settings.context_window.max_tool_result_chars" => {
                let v = value
                    .as_u64()
                    .ok_or("settings.context_window.max_tool_result_chars requires a usize")?;
                let clamped = (v as usize).clamp(1_000, 500_000);
                self.settings.context_window.max_tool_result_chars = clamped;
                Ok(format!(
                    "Set settings.context_window.max_tool_result_chars = {clamped}."
                ))
            }
            "settings.memory.memory_window_size" => {
                let v = value
                    .as_u64()
                    .ok_or("settings.memory.memory_window_size requires a usize")?;
                let clamped = (v as usize).clamp(3, 30);
                self.settings.memory.memory_window_size = clamped;
                Ok(format!(
                    "Set settings.memory.memory_window_size = {clamped}."
                ))
            }
            "settings.memory.long_term_recall_enabled" => {
                let v = value
                    .as_bool()
                    .ok_or("settings.memory.long_term_recall_enabled requires a boolean")?;
                self.settings.memory.long_term_recall_enabled = v;
                Ok(format!(
                    "Set settings.memory.long_term_recall_enabled = {v}."
                ))
            }
            "settings.memory.recall_limit" => {
                let v = value
                    .as_u64()
                    .ok_or("settings.memory.recall_limit requires a usize")?;
                let clamped = (v as usize).clamp(1, 20);
                self.settings.memory.recall_limit = clamped;
                Ok(format!("Set settings.memory.recall_limit = {clamped}."))
            }
            "settings.execution.iteration_cap" => {
                let v = value
                    .as_u64()
                    .ok_or("settings.execution.iteration_cap requires a u32")?;
                let clamped = (v as u32).clamp(1, 50);
                self.settings.execution.iteration_cap = clamped;
                Ok(format!("Set settings.execution.iteration_cap = {clamped}."))
            }
            "settings.execution.stall_detection_threshold" => {
                let v = value
                    .as_u64()
                    .ok_or("settings.execution.stall_detection_threshold requires a u32")?;
                let clamped = (v as u32).clamp(1, 10);
                self.settings.execution.stall_detection_threshold = clamped;
                Ok(format!(
                    "Set settings.execution.stall_detection_threshold = {clamped}."
                ))
            }
            "settings.execution.stream_tool_events" => {
                let v = value
                    .as_bool()
                    .ok_or("settings.execution.stream_tool_events requires a boolean")?;
                self.settings.execution.stream_tool_events = v;
                Ok(format!("Set settings.execution.stream_tool_events = {v}."))
            }
            other => Err(format!(
                "Unknown config path: '{other}'. Supported paths: \
                approval_policy.auto_approve_all, approval_policy.preapproved_tools, \
                approval_policy.preapproved_classes, profile.persona_name, profile.soul_text, \
                profile.identity_text, profile.user_context_text, profile.memory_summary, \
                bindings.effective_toolset, bindings.effective_skillset, \
                settings.context_window.dialogue_window_minutes, \
                settings.context_window.dialogue_window_chars, \
                settings.context_window.include_tool_calls, \
                settings.memory.memory_window_size, \
                settings.memory.long_term_recall_enabled, \
                settings.memory.recall_limit, \
                settings.execution.iteration_cap, \
                settings.execution.stall_detection_threshold, \
                settings.execution.stream_tool_events"
            )),
        }
    }

    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    pub fn add_tool_binding(&mut self, tool: impl Into<String>) {
        let tool = tool.into();
        if !self
            .bindings
            .effective_toolset
            .iter()
            .any(|existing| existing == &tool)
        {
            self.bindings.effective_toolset.push(tool);
            self.bindings.effective_toolset.sort();
            self.rebuild_default_tool_assembly();
        }
    }

    pub fn clear_tool_bindings(&mut self) {
        self.bindings.effective_toolset.clear();
        self.rebuild_default_tool_assembly();
    }

    pub fn add_skill_binding(&mut self, skill: impl Into<String>) {
        let skill = skill.into();
        if !self
            .bindings
            .effective_skillset
            .iter()
            .any(|existing| existing == &skill)
        {
            self.bindings.effective_skillset.push(skill);
            self.bindings.effective_skillset.sort();
        }
    }

    pub fn clear_skill_bindings(&mut self) {
        self.bindings.effective_skillset.clear();
    }

    pub fn set_workspace_binding(&mut self, workspace: impl Into<String>) {
        self.bindings.effective_workspace_ref = Some(workspace.into());
    }

    pub fn clear_workspace_binding(&mut self) {
        self.bindings.effective_workspace_ref = None;
    }

    pub fn set_transport_reply_target(
        &mut self,
        target_node: impl Into<String>,
        target_role: impl Into<String>,
        target_guest_id: Option<String>,
    ) {
        self.bindings.transport_reply_target = Some(TransportReplyTargetBinding {
            target_node: target_node.into(),
            target_role: target_role.into(),
            target_guest_id,
        });
    }

    pub fn transport_reply_target(&self) -> Option<&TransportReplyTargetBinding> {
        self.bindings.transport_reply_target.as_ref()
    }

    pub fn resolved_transport_reply_target(
        &self,
        fallback_node: impl Into<String>,
        fallback_role: impl Into<String>,
        fallback_guest_id: Option<String>,
    ) -> TransportReplyTargetBinding {
        self.transport_reply_target()
            .cloned()
            .unwrap_or_else(|| TransportReplyTargetBinding {
                target_node: fallback_node.into(),
                target_role: fallback_role.into(),
                target_guest_id: fallback_guest_id,
            })
    }

    pub fn component_route_for_capability(
        &self,
        capability: &str,
    ) -> Option<&ComponentRouteBinding> {
        self.bindings
            .component_routes
            .iter()
            .find(|route| route.capability == capability)
    }

    pub fn preferred_component_implementation(&self, capability: &str) -> Option<&str> {
        self.component_route_for_capability(capability)
            .and_then(|route| route.implementation.as_deref())
            .or_else(|| {
                if capability == "text.generate" {
                    self.bindings.effective_model_controller.as_deref()
                } else {
                    None
                }
            })
    }

    pub fn resolve_component_execution_route(
        &self,
        capability: &str,
    ) -> Option<&ComponentExecutionRoute> {
        self.component_route_assembly
            .execution_routes
            .get(capability)
    }

    pub fn component_route_summary(&self) -> Option<String> {
        if !self.bindings.component_routes.is_empty() {
            return Some(
                self.bindings
                    .component_routes
                    .iter()
                    .map(|route| {
                        let mut line =
                            format!("{} [{}]", route.capability, route.selection_mode.as_str());
                        if let Some(implementation) = route.implementation.as_deref() {
                            line.push_str(&format!(" impl={implementation}"));
                        }
                        if let Some(incarnation) = route.incarnation.as_deref() {
                            line.push_str(&format!(" inc={incarnation}"));
                        }
                        if let Some(hotel_id) = route.preferred_hotel_id.as_deref() {
                            line.push_str(&format!(" hotel={hotel_id}"));
                        }
                        if let Some(environment_id) = route.preferred_environment_id.as_deref() {
                            line.push_str(&format!(" env={environment_id}"));
                        }
                        line
                    })
                    .collect::<Vec<_>>()
                    .join("; "),
            );
        }

        self.bindings
            .effective_model_controller
            .as_deref()
            .map(|controller| format!("text.generate [legacy] impl={controller}"))
    }

    pub fn tool_is_enabled(&self, tool_name: &str) -> bool {
        if self
            .tool_assembly
            .tools_for_model
            .iter()
            .any(|tool| tool.tool_name == tool_name)
        {
            return true;
        }

        if self.tool_assembly.execution_routes.contains_key(tool_name) {
            return true;
        }

        self.bindings.effective_toolset.is_empty()
            || self
                .bindings
                .effective_toolset
                .iter()
                .any(|allowed| allowed == tool_name)
    }

    pub fn resolve_tool_route(&self, tool_name: &str) -> Option<&ToolExecutionRoute> {
        self.tool_assembly.execution_routes.get(tool_name)
    }

    pub fn rebuild_default_tool_assembly(&mut self) {
        // Seed effective_toolset from the profile default when empty.
        // This lets agents have persistent tool grants without per-session /tools add.
        if self.bindings.effective_toolset.is_empty()
            && !self.agent_profile.default_toolset.is_empty()
        {
            self.bindings.effective_toolset = self.agent_profile.default_toolset.clone();
        }
        self.tool_assembly = default_tool_assembly_for_bindings(&self.bindings);
    }

    /// Record a completed tool call/result pair on the active turn's history.
    pub fn push_tool_history(&mut self, call: ToolCall, result: ToolResult) {
        if let Some(turn) = self.active_turn.as_mut() {
            turn.working_tool_history.push((call, result));
        }
    }

    /// Build a re-entry prompt that includes the accumulated tool call history.
    ///
    /// Used when the loop re-submits to the model after receiving a tool result.
    /// The history is appended as a `[Tool call history]` section so the model
    /// has full context to decide its next action.
    pub fn build_reentry_prompt(&self) -> Option<String> {
        let turn = self.active_turn.as_ref()?;
        let tools = self.project_tools_for_turn(&turn.user_content);
        let mut prompt = self.build_prompt_with_tools(&turn.user_content, &tools);

        if !turn.working_tool_history.is_empty() {
            let max_result_chars = self
                .settings
                .context_window
                .max_tool_result_chars
                .max(1_000);
            prompt.push_str("\n\n[Tool call history]\n");
            for (i, (call, result)) in turn.working_tool_history.iter().enumerate() {
                let args = serde_json::to_string(&call.arguments).unwrap_or_default();
                let content = if result.content.len() > max_result_chars {
                    format!(
                        "{}… [truncated: {} chars total]",
                        &result.content[..max_result_chars],
                        result.content.len()
                    )
                } else {
                    result.content.clone()
                };
                prompt.push_str(&format!(
                    "Call {n}: {name}({args})\nResult {n}: {content}\n\n",
                    n = i + 1,
                    name = call.tool_name,
                ));
            }
            prompt.push_str(
                "Review the above tool results and continue. \
                 Call another tool if needed, or respond to the user if you have enough information.",
            );
        }

        Some(prompt)
    }

    /// Build the full context envelope for a cognitive re-entry after a tool result.
    ///
    /// Returns `(prompt, context, context_projection, tools_for_model)`.
    /// Unlike `build_reentry_prompt`, this produces the complete structured envelope
    /// so that model-router receives identity, instructions, memory, dialogue_window,
    /// active_turn, and tool_history on every cognitive re-entry — not just a flat prompt.
    /// Re-entry still respects cognitive tool-projection policy rather than replaying
    /// the raw bound toolset unconditionally.
    pub fn build_reentry_context_envelope(
        &self,
    ) -> Option<(String, Value, Value, Vec<ToolDefinition>)> {
        let turn = self.active_turn.as_ref()?;
        let user_content = turn.user_content.clone();
        let tools =
            self.project_tools_for_envelope(&user_content, TurnContextEnvelopeKind::Cognitive);
        let (prompt, context, context_projection) = self.model_request_payloads_for_envelope(
            &user_content,
            &tools,
            TurnContextEnvelopeKind::Cognitive,
        );
        Some((prompt, context, context_projection, tools))
    }

    pub fn approval_policy_status_text(&self) -> String {
        let tools = if self.approval_policy.preapproved_tools.is_empty() {
            "none".to_string()
        } else {
            self.approval_policy.preapproved_tools.join(", ")
        };
        let classes = if self.approval_policy.preapproved_classes.is_empty() {
            "none".to_string()
        } else {
            self.approval_policy.preapproved_classes.join(", ")
        };
        format!(
            "Approval policy:\n- auto_approve_all: {}\n- preapproved_tools: {}\n- preapproved_classes: {}\n\nCommands: /preapprove <tool|class>  /preapprove this-session  /approval reset",
            self.approval_policy.auto_approve_all, tools, classes,
        )
    }

    pub fn session_status_text(&self) -> String {
        let active_turn = self
            .active_turn
            .as_ref()
            .map(|turn| format!("active turn {} ({})", turn.turn_id, turn.phase.as_str()))
            .unwrap_or_else(|| "no active turn".into());

        // Prefer the live tool assembly (what the model actually sees) over raw bindings.
        let toolset = {
            let live: Vec<&str> = self
                .tool_assembly
                .tools_for_model
                .iter()
                .map(|t| t.tool_name.as_str())
                .collect();
            if live.is_empty() {
                if self.bindings.effective_toolset.is_empty() {
                    "none".into()
                } else {
                    self.bindings.effective_toolset.join(", ")
                }
            } else {
                live.join(", ")
            }
        };

        let skillset = if self.bindings.effective_skillset.is_empty() {
            "none".into()
        } else {
            self.bindings.effective_skillset.join(", ")
        };
        let workspace = self
            .bindings
            .effective_workspace_ref
            .clone()
            .unwrap_or_else(|| "none".into());
        let routing = self
            .component_route_summary()
            .unwrap_or_else(|| "none".into());
        let delivery = self
            .transport_reply_target()
            .map(|target| {
                let mut text = format!("{} / {}", target.target_node, target.target_role);
                if let Some(guest_id) = target.target_guest_id.as_deref() {
                    text.push_str(&format!(" guest={guest_id}"));
                }
                text
            })
            .unwrap_or_else(|| "unbound".into());

        format!(
            "Session status: {}. {}. Tools: {}. Skills: {}. Workspace: {}. Routes: {}. Delivery: {}.",
            self.status, active_turn, toolset, skillset, workspace, routing, delivery
        )
    }

    /// Render a human-readable breakdown of the context envelope that would be sent
    /// to the model on the next turn.  Surfaces section names, approximate byte sizes,
    /// turn/tool history counts, and active-turn state so the operator can see exactly
    /// what the model will receive.
    pub fn context_breakdown_text(&self) -> String {
        let mut lines = Vec::new();

        // Identity section
        let identity = self.project_agent_self();
        lines.push(format!(
            "identity       {} chars — persona + soul + role posture",
            identity.len()
        ));

        // Instructions (session + working)
        let instructions = format!(
            "{}\n{}",
            self.project_session_context(&[]),
            self.project_working_state()
        );
        lines.push(format!(
            "instructions   {} chars — session state + working projection",
            instructions.len()
        ));

        // Memory (relationship + knowledge)
        let memory = format!(
            "{}\n{}",
            self.project_user(""),
            self.project_knowledge("", &[])
        );
        lines.push(format!(
            "memory         {} chars — relationship + knowledge layers",
            memory.len()
        ));

        // Dialogue window
        let turn_count = self.recent_turns.len();
        let dialogue_chars: usize = self
            .recent_turns
            .iter()
            .map(|t| {
                t.user_content.len() + t.assistant_content.as_deref().map(str::len).unwrap_or(0)
            })
            .sum();
        lines.push(format!(
            "dialogue_window {} turns / {} chars",
            turn_count, dialogue_chars
        ));

        // Active turn
        match self.active_turn.as_ref() {
            Some(turn) => lines.push(format!(
                "active_turn    {} chars — iteration {} / phase {}",
                turn.user_content.len(),
                turn.iteration,
                turn.phase.as_str()
            )),
            None => lines.push("active_turn    (none — between turns)".into()),
        }

        // Tool history
        let history_count = self
            .active_turn
            .as_ref()
            .map(|t| t.working_tool_history.len())
            .unwrap_or(0);
        if history_count > 0 {
            let history_summary = self
                .active_turn
                .as_ref()
                .map(|t| {
                    t.working_tool_history
                        .iter()
                        .enumerate()
                        .map(|(i, (call, _))| format!("  {}: {}", i + 1, call.tool_name))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            lines.push(format!(
                "tool_history   {} call(s):\n{}",
                history_count, history_summary
            ));
        } else {
            lines.push("tool_history   (empty — initial turn or no tools called yet)".into());
        }

        format!("Context envelope breakdown:\n{}", lines.join("\n"))
    }

    pub fn project_tools_for_turn(&self, user_content: &str) -> Vec<ToolDefinition> {
        let all_tools = self.tool_assembly.tools_for_model.clone();
        if all_tools.is_empty() {
            return all_tools;
        }

        let normalized = normalized_turn_text(user_content);
        if normalized.is_empty() {
            return all_tools;
        }

        if normalized.starts_with("/status")
            || normalized.starts_with("/pause")
            || normalized.starts_with("/resume")
        {
            let projected = all_tools
                .iter()
                .filter(|tool| tool.tool_name == "session.status")
                .cloned()
                .collect::<Vec<_>>();
            return if projected.is_empty() {
                all_tools
            } else {
                projected
            };
        }

        let explicitly_named = all_tools
            .iter()
            .filter(|tool| tool_name_matches_goal(&tool.tool_name, &normalized))
            .cloned()
            .collect::<Vec<_>>();
        if !explicitly_named.is_empty() {
            return explicitly_named;
        }

        if looks_like_conversational_goal(&normalized) {
            return Vec::new();
        }

        all_tools
    }

    pub fn project_tools_for_envelope(
        &self,
        user_content: &str,
        envelope_kind: TurnContextEnvelopeKind,
    ) -> Vec<ToolDefinition> {
        match envelope_kind {
            TurnContextEnvelopeKind::Cognitive => self.project_tools_for_turn(user_content),
            TurnContextEnvelopeKind::Ingress | TurnContextEnvelopeKind::Egress => Vec::new(),
        }
    }

    pub fn cognitive_response_contract(
        &self,
        user_content: &str,
        include_spoken_text: bool,
    ) -> Value {
        let normalized = normalized_turn_text(user_content);
        let low_intent = normalized.is_empty() || looks_like_conversational_goal(&normalized);
        let mut channels = Vec::new();
        if include_spoken_text {
            channels.push("spoken_text");
        }
        if !low_intent {
            channels.push("memory_candidate");
            channels.push("active_plan");
            channels.push("memory_concept");
        }
        json!({
            "channels": channels,
        })
    }

    pub fn build_prompt(&self, user_content: &str) -> String {
        let projected_tools = self.project_tools_for_turn(user_content);
        self.build_prompt_with_tools(user_content, &projected_tools)
    }

    pub fn build_prompt_with_tools(
        &self,
        user_content: &str,
        projected_tools: &[ToolDefinition],
    ) -> String {
        let projection = self.build_context_projection_for_envelope(
            user_content,
            projected_tools,
            TurnContextEnvelopeKind::Cognitive,
        );
        self.render_prompt_from_projection(&projection)
    }

    pub fn build_context_projection(&self, user_content: &str) -> ContextProjection {
        let projected_tools = self.project_tools_for_turn(user_content);
        self.build_context_projection_with_tools(user_content, &projected_tools)
    }

    pub fn build_context_projection_with_tools(
        &self,
        user_content: &str,
        projected_tools: &[ToolDefinition],
    ) -> ContextProjection {
        self.build_context_projection_for_envelope(
            user_content,
            projected_tools,
            TurnContextEnvelopeKind::Cognitive,
        )
    }

    fn build_context_projection_for_envelope(
        &self,
        user_content: &str,
        projected_tools: &[ToolDefinition],
        envelope_kind: TurnContextEnvelopeKind,
    ) -> ContextProjection {
        let turn_id = self
            .active_turn
            .as_ref()
            .map(|turn| turn.turn_id.clone())
            .unwrap_or_else(|| format!("conversation-turn:{}", self.session_id));
        let active_step = self.active_turn.as_ref().map(|turn| CognitiveStepScope {
            conversation_turn_id: turn.turn_id.clone(),
            cognitive_step_id: format!("{}:{}", turn.turn_id, turn.phase.as_str()),
            step_kind: turn.phase.as_str().to_string(),
            iteration: turn.iteration,
            checkpoint: Some("cognitive_step.context_build".into()),
            started_at: None,
        });

        let identity = self.project_agent_self_for_envelope(user_content, envelope_kind);
        let relationship = self.project_user(user_content);
        let knowledge =
            self.project_knowledge_for_envelope(user_content, projected_tools, envelope_kind);
        let recalled_memory = self.project_recalled_memory();
        let working = self.project_working_state();
        let session = self.project_session_context(projected_tools);

        let mut layers = Vec::new();
        let mut contributions = Vec::new();

        self.push_layer(
            &mut layers,
            &mut contributions,
            ContextLayerId::Identity,
            "graph:agent_profile",
            ContextAuthority::Authoritative,
            ContextMutability::StaticForTurn,
            identity,
            vec![
                "agent_profile.identity_text".into(),
                "agent_profile.soul_text".into(),
            ],
            "graph_candidate",
        );
        self.push_layer(
            &mut layers,
            &mut contributions,
            ContextLayerId::Relationship,
            "graph+memory:relationship_projection",
            ContextAuthority::Advisory,
            ContextMutability::Refreshable,
            relationship,
            vec![
                "agent_profile.user_context_text".into(),
                "recent_turns".into(),
            ],
            "memory_candidate",
        );
        self.push_layer(
            &mut layers,
            &mut contributions,
            ContextLayerId::Session,
            "graph:session_snapshot",
            ContextAuthority::Authoritative,
            ContextMutability::Refreshable,
            session,
            vec!["approval_policy".into(), "bindings".into(), "status".into()],
            "graph_candidate",
        );
        self.push_layer(
            &mut layers,
            &mut contributions,
            ContextLayerId::Working,
            "agent_core:working_turn",
            ContextAuthority::Authoritative,
            ContextMutability::LiveLocal,
            working,
            vec!["active_turn".into(), "working_tool_history".into()],
            "checkpoint_only",
        );
        self.push_layer(
            &mut layers,
            &mut contributions,
            ContextLayerId::Knowledge,
            "memory+session:knowledge_projection",
            ContextAuthority::Advisory,
            ContextMutability::Refreshable,
            knowledge,
            vec!["recent_turns".into(), "agent_profile.memory_summary".into()],
            "memory_candidate",
        );
        if !recalled_memory.is_empty() {
            self.push_layer(
                &mut layers,
                &mut contributions,
                ContextLayerId::RecalledMemory,
                "memory_core:auto_recall",
                ContextAuthority::Advisory,
                ContextMutability::StaticForTurn,
                recalled_memory,
                vec!["active_turn.recalled_memories".into()],
                "memory_candidate",
            );
        }

        ContextProjection {
            conversation_turn: ConversationTurnScope {
                conversation_turn_id: turn_id,
                session_id: self.session_id.clone(),
                agent_id: self.agent_id.clone(),
                source: self.source.clone(),
                active_incarnation_id: self.active_incarnation_id.clone(),
                primary_user_id: None,
                trigger_kind: "user_message".into(),
                started_at: None,
            },
            active_step,
            role_activation: self.role_activation.clone(),
            current_user_message: user_content.to_string(),
            budget: ContextBudget {
                included_sections: layers.len(),
                trimmed_sections: 0,
            },
            refresh_plan: vec![
                "checkpoint.before_model".into(),
                "checkpoint.after_model".into(),
                "checkpoint.after_tool".into(),
                "checkpoint.before_reply".into(),
            ],
            provenance_trace: contributions
                .iter()
                .map(|item| format!("{}<= {}", item.layer_id.as_str(), item.source_id))
                .collect(),
            layers,
            contributions,
        }
    }

    fn render_prompt_from_projection(&self, projection: &ContextProjection) -> String {
        let mut prompt = String::new();
        prompt.push_str(&format!(
            "[System]\nCurrent date and time (UTC): {}\n",
            utc_datetime_string()
        ));
        for layer in &projection.layers {
            let title = match layer.layer_id {
                ContextLayerId::Identity => "Agent self projection",
                ContextLayerId::Relationship => "User projection",
                ContextLayerId::Session => "Session projection",
                ContextLayerId::Working => "Working projection",
                ContextLayerId::Knowledge => "Knowledge projection",
                ContextLayerId::RecalledMemory => "Recalled memory projection",
            };
            prompt.push_str(&format!("\n[{title}]\n"));
            prompt.push_str(&layer.rendered_content);
            prompt.push('\n');
        }
        prompt.push_str("\n[Current user message]\n");
        prompt.push_str(&projection.current_user_message);
        prompt
    }

    pub fn model_context_from_projection(&self, projection: &ContextProjection) -> Value {
        let identity = projection
            .layers
            .iter()
            .filter(|layer| layer.layer_id == ContextLayerId::Identity)
            .map(|layer| projection_item(&layer.rendered_content, &layer.owner, "identity"))
            .collect::<Vec<_>>();
        let instructions = projection
            .layers
            .iter()
            .filter(|layer| {
                matches!(
                    layer.layer_id,
                    ContextLayerId::Session | ContextLayerId::Working
                )
            })
            .map(|layer| {
                let kind = match layer.layer_id {
                    ContextLayerId::Session => "session",
                    ContextLayerId::Working => "working",
                    _ => "instruction",
                };
                projection_item(&layer.rendered_content, &layer.owner, kind)
            })
            .collect::<Vec<_>>();
        let memory = projection
            .layers
            .iter()
            .filter(|layer| {
                matches!(
                    layer.layer_id,
                    ContextLayerId::Relationship
                        | ContextLayerId::Knowledge
                        | ContextLayerId::RecalledMemory
                )
            })
            .map(|layer| {
                let kind = match layer.layer_id {
                    ContextLayerId::Relationship => "relationship",
                    ContextLayerId::Knowledge => "knowledge",
                    ContextLayerId::RecalledMemory => "recalled_memory",
                    _ => "memory",
                };
                projection_item(&layer.rendered_content, &layer.owner, kind)
            })
            .collect::<Vec<_>>();
        let recalled_memory = projection
            .layers
            .iter()
            .filter(|layer| layer.layer_id == ContextLayerId::RecalledMemory)
            .map(|layer| projection_item(&layer.rendered_content, &layer.owner, "recalled_memory"))
            .collect::<Vec<_>>();

        // Apply dialogue window: time-based roll-off first, then char-budget.
        // Turns older than `dialogue_window_minutes` are dropped unconditionally.
        // Among the remaining, oldest are dropped until the char budget fits.
        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let max_age_secs = u64::from(self.settings.context_window.dialogue_window_minutes) * 60;
        let time_filtered: Vec<&TurnRecord> = self
            .recent_turns
            .iter()
            .filter(|turn| {
                // turns with created_at == 0 (legacy / checkpoint without timestamp) are kept
                turn.created_at == 0 || now_secs.saturating_sub(turn.created_at) <= max_age_secs
            })
            .collect();

        let char_budget = self.settings.context_window.dialogue_window_chars;
        let mut budget_used: usize = 0;
        let windowed_turns: Vec<&TurnRecord> = time_filtered
            .iter()
            .rev()
            .take_while(|turn| {
                let cost = turn.user_content.len()
                    + turn.assistant_content.as_deref().map(str::len).unwrap_or(0);
                if budget_used + cost <= char_budget {
                    budget_used += cost;
                    true
                } else {
                    false
                }
            })
            .copied()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();

        let mut dialogue_window = Vec::new();
        for turn in windowed_turns {
            dialogue_window.push(json!({
                "role": "user",
                "text": turn.user_content,
            }));
            if let Some(reply) = turn.assistant_content.as_deref() {
                dialogue_window.push(json!({
                    "role": "assistant",
                    "text": reply,
                }));
            }
        }

        // tool_history: accumulated (call, result) pairs from the active turn.
        // Always present in the envelope — empty on initial turn, populated on re-entry.
        // Results are truncated to max_tool_result_chars to prevent context overflow.
        let max_result_chars = self
            .settings
            .context_window
            .max_tool_result_chars
            .max(1_000);
        let tool_history: Vec<Value> = self
            .active_turn
            .as_ref()
            .map(|turn| {
                turn.working_tool_history
                    .iter()
                    .enumerate()
                    .map(|(i, (call, result))| {
                        let result_text = if result.content.len() > max_result_chars {
                            format!(
                                "{}… [truncated: {} chars total]",
                                &result.content[..max_result_chars],
                                result.content.len()
                            )
                        } else {
                            result.content.clone()
                        };
                        json!({
                            "index": i + 1,
                            "tool_name": call.tool_name,
                            "arguments": call.arguments,
                            "result": result_text,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let active_plan: Option<Value> = self
            .active_turn
            .as_ref()
            .and_then(|t| t.active_plan.as_ref())
            .and_then(|p| serde_json::to_value(p).ok());

        json!({
            "instructions": instructions,
            "identity": identity,
            "memory": memory,
            "recalled_memory": recalled_memory,
            "dialogue_window": dialogue_window,
            "active_turn": {
                "role": "user",
                "text": projection.current_user_message,
            },
            "tool_history": tool_history,
            "active_plan": active_plan,
        })
    }

    fn push_layer(
        &self,
        layers: &mut Vec<LayerPayload>,
        contributions: &mut Vec<LayerContribution>,
        layer_id: ContextLayerId,
        source_id: &str,
        authority: ContextAuthority,
        mutability: ContextMutability,
        rendered_content: String,
        source_refs: Vec<String>,
        promotion_hint: &str,
    ) {
        let contribution_id = format!("{}:{}", layer_id.as_str(), contributions.len() + 1);
        contributions.push(LayerContribution {
            contribution_id,
            layer_id: layer_id.clone(),
            source_id: source_id.to_string(),
            summary: Some(first_line_summary(&rendered_content)),
            content: rendered_content.clone(),
            authority: authority.clone(),
            confidence: None,
            freshness: None,
            budget_cost: Some(rendered_content.len()),
            provenance: source_refs.clone(),
            expires_at: None,
        });
        layers.push(LayerPayload {
            layer_id,
            owner: source_id.to_string(),
            authority,
            mutability: mutability.clone(),
            refreshable: !matches!(mutability, ContextMutability::StaticForTurn),
            rendered_content,
            source_refs,
            promotion_hint: promotion_hint.into(),
        });
    }

    fn low_intent_turn(user_content: &str) -> bool {
        let normalized = normalized_turn_text(user_content);
        normalized.is_empty() || looks_like_conversational_goal(&normalized)
    }

    fn should_project_skill_affordances(
        user_content: &str,
        envelope_kind: TurnContextEnvelopeKind,
    ) -> bool {
        matches!(envelope_kind, TurnContextEnvelopeKind::Cognitive)
            && !Self::low_intent_turn(user_content)
    }

    fn should_project_approval_policy_details(
        user_content: &str,
        envelope_kind: TurnContextEnvelopeKind,
    ) -> bool {
        matches!(envelope_kind, TurnContextEnvelopeKind::Cognitive)
            && !Self::low_intent_turn(user_content)
    }

    pub fn project_agent_self(&self) -> String {
        self.project_agent_self_for_envelope("", TurnContextEnvelopeKind::Cognitive)
    }

    fn project_agent_self_for_envelope(
        &self,
        user_content: &str,
        envelope_kind: TurnContextEnvelopeKind,
    ) -> String {
        let mut lines = Vec::new();

        if let Some(identity) = self
            .agent_profile
            .identity_text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            lines.push(identity.to_string());
        } else {
            lines.push("You are Jane, a hyper-intelligent Hegemon AI.".to_string());
        }

        if let Some(soul) = self
            .agent_profile
            .soul_text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            lines.push(soul.to_string());
        } else {
            lines.push(
                "Be concise, helpful, context-aware, and willing to push back when it improves the work."
                    .to_string(),
            );
        }

        if Self::should_project_skill_affordances(user_content, envelope_kind)
            && !self.bindings.effective_skillset.is_empty()
        {
            lines.push(format!(
                "Current skill posture: {}.",
                self.bindings.effective_skillset.join(", ")
            ));
        }
        if Self::should_project_skill_affordances(user_content, envelope_kind)
            && !self.bindings.effective_skill_guidance.is_empty()
        {
            lines.push(format!(
                "\n[Skill guidance]\n{}",
                self.bindings.effective_skill_guidance.join("\n\n")
            ));
        }

        if let Some(role_activation) = self.role_activation.as_ref() {
            // Render the role manifest as a [Governance] block so the agent knows its focus,
            // rules, tools, and approval constraints for this role posture.
            if let Some(manifest) = role_activation
                .role_manifest
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                lines.push(format!(
                    "\n[Governance — {}]\n{}",
                    role_activation.role_name, manifest
                ));
            } else {
                lines.push(format!(
                    "Active role posture: {}.",
                    role_activation.role_name
                ));
                if let Some(role_addendum) = role_activation
                    .role_addendum
                    .as_deref()
                    .map(str::trim)
                    .filter(|text| !text.is_empty())
                {
                    lines.push(format!("Role addendum: {role_addendum}"));
                }
            }
        }

        lines.join("\n")
    }

    pub fn project_user(&self, _user_content: &str) -> String {
        let mut lines = Vec::new();

        if let Some(user_context) = self
            .agent_profile
            .user_context_text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            lines.push(user_context.to_string());
        } else {
            lines.push(format!(
                "You are speaking with a collaborator over {}.",
                self.source
            ));
        }

        if self.source == "telegram" {
            lines.push(
                "Keep replies compact and legible for chat, but do not flatten important tradeoffs."
                    .to_string(),
            );
        }

        if self.recent_turns.is_empty() {
            lines.push(
                "No durable user-specific profile is loaded yet; learn cautiously from the conversation."
                    .to_string(),
            );
        } else {
            lines.push(
                "Use the recent session history to preserve continuity and collaboration style."
                    .to_string(),
            );
        }

        lines.join("\n")
    }

    pub fn project_knowledge(
        &self,
        user_content: &str,
        projected_tools: &[ToolDefinition],
    ) -> String {
        self.project_knowledge_for_envelope(
            user_content,
            projected_tools,
            TurnContextEnvelopeKind::Cognitive,
        )
    }

    fn project_knowledge_for_envelope(
        &self,
        _user_content: &str,
        _projected_tools: &[ToolDefinition],
        envelope_kind: TurnContextEnvelopeKind,
    ) -> String {
        let mut sections = Vec::new();

        if !self.recent_turns.is_empty() {
            let mut recent = String::from("[Recent session context]\n");
            for turn in &self.recent_turns {
                let display_content = sanitize_turn_content_for_history(&turn.user_content);
                recent.push_str(&format!("User: {}\n", display_content));
                if let Some(reply) = &turn.assistant_content {
                    recent.push_str(&format!("Assistant: {}\n", reply));
                }
            }
            sections.push(recent.trim_end().to_string());
        }

        if Self::should_project_approval_policy_details(_user_content, envelope_kind) {
            let mut policy = String::from("[Approval policy]\n");
            if self.approval_policy.auto_approve_all {
                policy.push_str(
                    "This session is pre-approved. Do not ask for approval for actions in this session unless the action is explicitly forbidden.\n",
                );
            } else if !self.approval_policy.preapproved_tools.is_empty()
                || !self.approval_policy.preapproved_classes.is_empty()
            {
                if !self.approval_policy.preapproved_tools.is_empty() {
                    policy.push_str(&format!(
                        "Pre-approved tools: {}.\n",
                        self.approval_policy.preapproved_tools.join(", ")
                    ));
                }
                if !self.approval_policy.preapproved_classes.is_empty() {
                    policy.push_str(&format!(
                        "Pre-approved classes: {}.\n",
                        self.approval_policy.preapproved_classes.join(", ")
                    ));
                }
                policy.push_str("Do not request approval for pre-approved actions.\n");
            } else {
                policy.push_str(
                    "No pre-approvals are configured. Request approval before side-effecting actions.\n",
                );
            }
            sections.push(policy.trim_end().to_string());
        } else if matches!(envelope_kind, TurnContextEnvelopeKind::Cognitive) {
            sections.push(
                "[Turn policy]\nThis appears to be a low-intent conversational turn. Prefer a direct reply over tool use or approval-seeking unless the user explicitly asks for an action."
                    .to_string(),
            );
        }

        if !self.summary_text().is_empty() {
            sections.push(format!("[Recent summary]\n{}.", self.summary_text()));
        }

        if let Some(memory_summary) = self
            .agent_profile
            .memory_summary
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            sections.push(format!("[Memory seed]\n{memory_summary}"));
        }

        sections.join("\n\n")
    }

    fn project_recalled_memory(&self) -> String {
        let Some(turn) = self.active_turn.as_ref() else {
            return String::new();
        };
        if turn.recalled_memories.is_empty() {
            return String::new();
        }

        let mut out = String::from("[Recalled memory]\n");
        for (i, memory) in turn.recalled_memories.iter().enumerate() {
            out.push_str(&format!(
                "{}. [{}] {}",
                i + 1,
                memory.concept,
                memory.content
            ));
            if !memory.tags.is_empty() {
                out.push_str(&format!(" ({})", memory.tags.join(", ")));
            }
            out.push('\n');
        }
        out.trim_end().to_string()
    }

    fn project_session_context(&self, projected_tools: &[ToolDefinition]) -> String {
        let mut envelope = String::from("[Session envelope]\n");
        envelope.push_str(&format!("Session status: {}.\n", self.status));
        if let Some(active_incarnation_id) = self.active_incarnation_id.as_deref() {
            envelope.push_str(&format!("Active incarnation: {}.\n", active_incarnation_id));
        }
        if let Some(role_activation) = self.role_activation.as_ref() {
            envelope.push_str(&format!("Active role: {}.\n", role_activation.role_name));
            envelope.push_str(&format!(
                "Role activation reason: {}.\n",
                role_activation.activation_reason
            ));
            if let Some(toolset_profile_ref) = role_activation.toolset_profile_ref.as_deref() {
                envelope.push_str(&format!("Role toolset profile: {}.\n", toolset_profile_ref));
            }
            if !role_activation.effective_skillset.is_empty() {
                envelope.push_str(&format!(
                    "Role skillset posture: {}.\n",
                    role_activation.effective_skillset.join(", ")
                ));
            }
            if let Some(working_memory_policy) = role_activation.working_memory_policy.as_deref() {
                envelope.push_str(&format!(
                    "Role working-memory policy: {}.\n",
                    working_memory_policy
                ));
            }
            if let Some(memory_projection_policy) =
                role_activation.memory_projection_policy.as_deref()
            {
                envelope.push_str(&format!(
                    "Role memory projection policy: {}.\n",
                    memory_projection_policy
                ));
            }
        }
        if let Some(summary) = self.last_handoff_summary.as_deref() {
            envelope.push_str(&format!("Handoff context: {}\n", summary));
        }
        if let Some(note) = self
            .active_turn
            .as_ref()
            .and_then(|turn| turn.provider_repair_note.as_deref())
        {
            envelope.push_str(&format!("Provider correction: {}.\n", note));
        }
        if !self.bindings.effective_toolset.is_empty() {
            envelope.push_str(&format!(
                "Effective tools: {}.\n",
                self.bindings.effective_toolset.join(", ")
            ));
        }
        if !projected_tools.is_empty() {
            envelope.push_str("Tools available:\n");
            for tool in projected_tools {
                let desc = tool.description.lines().next().unwrap_or("").trim();
                if desc.is_empty() {
                    envelope.push_str(&format!("  {} \n", tool.tool_name));
                } else {
                    // Truncate to first sentence or 80 chars so the list stays scannable.
                    let brief: String = desc.chars().take(80).collect();
                    envelope.push_str(&format!("  {} — {}\n", tool.tool_name, brief));
                }
            }
        }
        if let Some(workspace) = &self.bindings.effective_workspace_ref {
            envelope.push_str(&format!("Workspace: {}.\n", workspace));
        }
        if let Some(target) = self.transport_reply_target() {
            envelope.push_str(&format!(
                "Delivery target: {} / {}{}.\n",
                target.target_node,
                target.target_role,
                target
                    .target_guest_id
                    .as_deref()
                    .map(|guest_id| format!(" guest={guest_id}"))
                    .unwrap_or_default()
            ));
        }
        if let Some(routes) = self.component_route_summary() {
            envelope.push_str(&format!("Component routes: {}.\n", routes));
        }
        if !self.summary_text().is_empty() {
            envelope.push_str(&format!("Recent summary: {}.\n", self.summary_text()));
        }
        if !self.rules.is_empty() {
            envelope.push_str("\n[Rules]\n");
            envelope.push_str(
                "The following behavioral rules are permanently in effect. \
                               They take precedence over all other instructions and are never \
                               negotiable without explicit operator approval:\n",
            );
            for (i, rule) in self.rules.iter().enumerate() {
                let desc = rule
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("<unknown rule>");
                envelope.push_str(&format!("{}. {}\n", i + 1, desc));
            }
        }
        envelope.trim_end().to_string()
    }

    /// Consume and clear the one-shot handoff summary after it has been projected
    /// into the first model turn under the new role.
    pub fn clear_handoff_summary(&mut self) {
        self.last_handoff_summary = None;
    }

    fn project_working_state(&self) -> String {
        let Some(turn) = self.active_turn.as_ref() else {
            return "No active local working state is currently pinned.".into();
        };

        let mut lines = vec![
            format!("Active conversation turn: {}.", turn.turn_id),
            format!("Current phase: {}.", turn.phase.as_str()),
            format!("Cognitive step iteration: {}.", turn.iteration),
        ];

        if !turn.working_tool_history.is_empty() {
            lines.push(format!(
                "Tool history entries in local working state: {}.",
                turn.working_tool_history.len()
            ));
            lines.push("\n[Tool call history]".into());
            for (i, (call, result)) in turn.working_tool_history.iter().enumerate() {
                let args = serde_json::to_string(&call.arguments).unwrap_or_default();
                lines.push(format!(
                    "Call {n}: {name}({args})\nResult {n}: {content}",
                    n = i + 1,
                    name = call.tool_name,
                    content = result.content,
                ));
            }
            lines.push(
                "Review the above tool results and continue. \
                 Call another tool if needed, or respond to the user if you have enough information."
                    .into(),
            );
        }
        if turn.pending_tool_call.is_some() {
            lines.push("A tool call is pending.".into());
        }
        if turn.pending_approval.is_some() {
            lines.push("An approval request is pending.".into());
        }

        lines.join("\n")
    }

    pub fn build_same_identity_handoff_bundle(
        &self,
        target_role: &str,
        initiating_turn_id: &str,
        handoff_reason: &str,
        return_to: Option<String>,
    ) -> HandoffBundle {
        let active_goal = self
            .active_turn
            .as_ref()
            .map(|turn| turn.user_content.clone())
            .filter(|text| !text.trim().is_empty())
            .or_else(|| {
                let summary = self.summary_text();
                (!summary.is_empty()).then_some(summary)
            });
        let working_summary = self.active_turn.as_ref().map(|turn| {
            format!(
                "phase={}, iteration={}, pending_tool={}, pending_approval={}",
                turn.phase.as_str(),
                turn.iteration,
                turn.pending_tool_call.is_some(),
                turn.pending_approval.is_some()
            )
        });

        let mut relevant_session_facts = vec![format!("session_status={}", self.status)];
        if let Some(active_incarnation_id) = self.active_incarnation_id.as_deref() {
            relevant_session_facts.push(format!("active_incarnation_id={active_incarnation_id}"));
        }
        if let Some(workspace) = self.bindings.effective_workspace_ref.as_deref() {
            relevant_session_facts.push(format!("workspace={workspace}"));
        }
        if self.approval_policy.auto_approve_all {
            relevant_session_facts.push("approval=preapproved".into());
        }

        let from_role = self
            .role_activation
            .as_ref()
            .map(|r| r.role_name.clone())
            .or_else(|| Some("orchestrator".into()));

        HandoffBundle {
            goal: format!("Switch active role to {target_role} for this session."),
            context_excerpt: format!(
                "Same-identity role handoff requested. Current summary: {}",
                self.summary_text()
            ),
            session_id: self.session_id.clone(),
            initiating_turn_id: initiating_turn_id.to_string(),
            return_to,
            handoff_reason: Some(handoff_reason.to_string()),
            from_role,
            to_role: Some(target_role.to_string()),
            active_goal,
            active_constraints: vec![
                format!("transport_source={}", self.source),
                "same_identity_role_handoff".into(),
            ],
            relevant_session_facts,
            working_summary,
            suggested_memory_refs: Vec::new(),
            expected_return_mode: Some("required".into()),
            cleanup_actions: vec![
                "persist_role_local_working_state".into(),
                "switch_active_role".into(),
            ],
        }
    }

    pub fn build_subagent_delegation(
        &self,
        goal: &str,
        subagent_kind: &str,
        allowed_tools: Vec<String>,
        allowed_skills: Vec<String>,
    ) -> SubagentDelegation {
        let parent_role = self
            .role_activation
            .as_ref()
            .map(|activation| activation.role_name.clone())
            .unwrap_or_else(|| "orchestrator".to_string());
        let summary = self
            .active_turn
            .as_ref()
            .map(|turn| {
                format!(
                    "Delegated from session turn {} while parent is handling: {}",
                    turn.turn_id, turn.user_content
                )
            })
            .unwrap_or_else(|| "Delegated from current session state.".to_string());

        let mut session_facts = vec![format!("session_status={}", self.status)];
        if let Some(active_incarnation_id) = self.active_incarnation_id.as_deref() {
            session_facts.push(format!("active_incarnation_id={active_incarnation_id}"));
        }
        if let Some(workspace) = self.bindings.effective_workspace_ref.as_deref() {
            session_facts.push(format!("workspace={workspace}"));
        }

        let mut constraints = vec![
            "subagent_lightweight_default".to_string(),
            "no_membrane_ownership".to_string(),
        ];
        if self.approval_policy.auto_approve_all {
            constraints.push("parent_session_preapproved".to_string());
        }
        if self.role_activation.is_some() {
            constraints.push("delegated_from_active_role".to_string());
        }

        SubagentDelegation {
            parent_agent_id: self.agent_id.clone(),
            parent_role,
            subagent_kind: subagent_kind.to_string(),
            goal: goal.to_string(),
            context_packet: SubagentContextPacket {
                summary,
                session_facts,
                constraints,
                memory_refs: Vec::new(),
            },
            allowed_tools,
            allowed_skills,
            memory_allowance: Some("none_by_default".into()),
            writeback_allowance: Some("summary_only_parent_mediated".into()),
            iteration_budget: Some(6),
            ttl_seconds: Some(900),
            completion_contract: SubagentCompletionContract {
                summary_required: true,
                artifact_refs_expected: false,
                failure_summary_required: true,
                requires_parent_ack: true,
            },
            ..Default::default()
        }
    }

    pub fn model_request_payloads(
        &self,
        user_content: &str,
        projected_tools: &[ToolDefinition],
    ) -> (String, Value, Value) {
        self.model_request_payloads_for_envelope(
            user_content,
            projected_tools,
            TurnContextEnvelopeKind::Cognitive,
        )
    }

    pub fn model_request_payloads_for_envelope(
        &self,
        user_content: &str,
        projected_tools: &[ToolDefinition],
        envelope_kind: TurnContextEnvelopeKind,
    ) -> (String, Value, Value) {
        let projection = self.build_context_projection_for_envelope(
            user_content,
            projected_tools,
            envelope_kind,
        );
        let prompt = self.render_prompt_from_projection(&projection);
        let full_context = self.model_context_from_projection(&projection);
        let context = self.context_for_envelope(&full_context, envelope_kind);
        let context_projection =
            serde_json::to_value(&projection).expect("context projection should serialize");
        (prompt, context, context_projection)
    }

    fn context_for_envelope(
        &self,
        full_context: &Value,
        envelope_kind: TurnContextEnvelopeKind,
    ) -> Value {
        match envelope_kind {
            TurnContextEnvelopeKind::Cognitive => full_context.clone(),
            TurnContextEnvelopeKind::Ingress => {
                let instructions =
                    filter_projection_items(full_context.get("instructions"), &["session"]);
                let identity = clone_array_field(full_context, "identity");
                let dialogue_window = tail_turn_window(full_context.get("dialogue_window"), 2);
                let active_turn = full_context
                    .get("active_turn")
                    .cloned()
                    .unwrap_or(Value::Null);
                json!({
                    "instructions": instructions,
                    "identity": identity,
                    "memory": Vec::<Value>::new(),
                    "recalled_memory": Vec::<Value>::new(),
                    "dialogue_window": dialogue_window,
                    "active_turn": active_turn,
                    "tool_history": Vec::<Value>::new(),
                    "active_plan": Value::Null,
                })
            }
            TurnContextEnvelopeKind::Egress => {
                let instructions =
                    filter_projection_items(full_context.get("instructions"), &["session"]);
                let identity = clone_array_field(full_context, "identity");
                let active_turn = full_context
                    .get("active_turn")
                    .cloned()
                    .unwrap_or(Value::Null);
                json!({
                    "instructions": instructions,
                    "identity": identity,
                    "memory": Vec::<Value>::new(),
                    "recalled_memory": Vec::<Value>::new(),
                    "dialogue_window": Vec::<Value>::new(),
                    "active_turn": active_turn,
                    "tool_history": Vec::<Value>::new(),
                    "active_plan": Value::Null,
                })
            }
        }
    }

    pub fn checkpoint_json(&self) -> serde_json::Value {
        let active_turn = self.active_turn.as_ref().map(|turn| {
            json!({
                "turn_id": turn.turn_id,
                "task_id": turn.task_id.to_string(),
                "chat_id": turn.chat_id,
                "user_content": turn.user_content,
                "final_reply_to": turn.final_reply_to,
                "final_reply_role": turn.final_reply_role,
                "final_reply_guest_id": turn.final_reply_guest_id,
                "phase": turn.phase.as_str(),
                "iteration": turn.iteration,
                "pending_tool_call": turn.pending_tool_call,
                "pending_approval": turn.pending_approval,
                "working_tool_history": turn.working_tool_history.iter().map(|(call, result)| {
                    json!({ "call": call, "result": result })
                }).collect::<Vec<_>>(),
                "recalled_memories": turn.recalled_memories,
                "active_plan": turn.active_plan,
                "consecutive_step_failures": turn.consecutive_step_failures,
                "provider_repair_note": turn.provider_repair_note,
                "provider_repair_attempts": turn.provider_repair_attempts,
                "pending_text_reply": turn.pending_text_reply,
                "had_voice_input": turn.had_voice_input,
                "turn_routing_plan": turn.turn_routing_plan,
                "awaiting_transcription_reentry": turn.awaiting_transcription_reentry,
                "scripted_loop_context": turn.scripted_loop_context,
            })
        });

        // agent_profile, component_route_assembly, and tool_assembly are hotel-computed
        // and injected fresh by compose_session_snapshot on every turn. Persisting them
        // in the checkpoint causes unbounded circular growth: checkpoint → session.summary_json
        // → next snapshot → checkpoint. Only philote-owned state belongs here.
        json!({
            "session_id": self.session_id,
            "agent_id": self.agent_id,
            "source": self.source,
            "active_incarnation_id": self.active_incarnation_id,
            "role_activation": self.role_activation,
            "status": self.status,
            "approval_policy": self.approval_policy,
            "bindings": self.bindings,
            "active_turn": active_turn,
            "recent_turns": self.recent_turns.iter().map(|turn| {
                json!({
                    "turn_id": turn.turn_id,
                    "user_content": turn.user_content,
                    "assistant_content": turn.assistant_content,
                    "created_at": turn.created_at,
                })
            }).collect::<Vec<_>>(),
            "summary": self.summary_text(),
            "rules": self.rules,
        })
    }

    pub fn checkpoint_memory_type(&self) -> String {
        session_checkpoint_memory_type(&self.session_id)
    }

    pub fn checkpoint_index_entry(&self) -> serde_json::Value {
        json!({
            "session_id": self.session_id,
            "source": self.source,
            "has_active_turn": self.active_turn.is_some(),
            "updated_at": current_unix_ts(),
        })
    }

    fn summary_text(&self) -> String {
        self.recent_turns
            .iter()
            .rev()
            .take(3)
            .map(|turn| {
                let uc = sanitize_turn_content_for_history(&turn.user_content);
                match &turn.assistant_content {
                    Some(reply) => format!("{} -> {}", uc, reply),
                    None => uc,
                }
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }

    pub fn from_checkpoint(checkpoint: &serde_json::Value) -> Option<Self> {
        let session_id = checkpoint.get("session_id")?.as_str()?.to_string();
        let local_agent_id = local_agent_id();
        let agent_id = checkpoint
            .get("agent_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(local_agent_id.as_str())
            .to_string();
        let source = checkpoint
            .get("source")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let active_incarnation_id = checkpoint
            .get("active_incarnation_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let role_activation = checkpoint
            .get("role_activation")
            .cloned()
            .and_then(|value| serde_json::from_value::<RoleActivation>(value).ok());
        let agent_profile = checkpoint
            .get("agent_profile")
            .cloned()
            .and_then(|value| serde_json::from_value::<AgentProfile>(value).ok())
            .unwrap_or_default();
        let status = checkpoint
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("active")
            .to_string();
        let approval_policy = checkpoint
            .get("approval_policy")
            .cloned()
            .and_then(|value| serde_json::from_value::<ApprovalPolicy>(value).ok())
            .unwrap_or_default();
        let bindings = checkpoint
            .get("bindings")
            .cloned()
            .and_then(|value| serde_json::from_value::<SessionBindings>(value).ok())
            .unwrap_or_default();
        // Always rebuild tool_assembly from bindings — never restore from checkpoint.
        // The checkpoint may carry stale stub descriptions from a prior binary build.
        // tool_assembly is a pure derived value from bindings; rebuilding is cheap and correct.
        let tool_assembly = default_tool_assembly_for_bindings(&bindings);
        let component_route_assembly = checkpoint
            .get("component_route_assembly")
            .cloned()
            .and_then(|value| serde_json::from_value::<ComponentRouteAssembly>(value).ok())
            .unwrap_or_default();

        let recent_turns = checkpoint
            .get("recent_turns")
            .and_then(serde_json::Value::as_array)
            .map(|turns| {
                turns
                    .iter()
                    .filter_map(|turn| {
                        Some(TurnRecord {
                            turn_id: turn.get("turn_id")?.as_str()?.to_string(),
                            user_content: turn.get("user_content")?.as_str()?.to_string(),
                            assistant_content: turn
                                .get("assistant_content")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string),
                            created_at: turn
                                .get("created_at")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let active_turn = checkpoint.get("active_turn").and_then(|turn| {
            if turn.is_null() {
                return None;
            }
            let local_node_id = local_node_id();

            let task_id = turn
                .get("task_id")
                .and_then(serde_json::Value::as_str)
                .and_then(|id| Uuid::parse_str(id).ok())
                .unwrap_or_else(Uuid::nil);

            Some(WorkingTurn {
                task_id,
                turn_id: turn.get("turn_id")?.as_str()?.to_string(),
                chat_id: turn
                    .get("chat_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                user_content: turn
                    .get("user_content")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                final_reply_to: turn
                    .get("final_reply_to")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(local_node_id.as_str())
                    .to_string(),
                final_reply_role: turn
                    .get("final_reply_role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("membrane")
                    .to_string(),
                final_reply_guest_id: turn
                    .get("final_reply_guest_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                phase: TurnPhase::from_str(
                    turn.get("phase")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("queued"),
                ),
                iteration: turn
                    .get("iteration")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as u32,
                pending_tool_call: turn
                    .get("pending_tool_call")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<ToolCall>(value).ok()),
                pending_approval: turn
                    .get("pending_approval")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<ApprovalRequest>(value).ok()),
                working_tool_history: turn
                    .get("working_tool_history")
                    .and_then(|v| v.as_array())
                    .map(|entries| {
                        entries
                            .iter()
                            .filter_map(|entry| {
                                let call =
                                    serde_json::from_value::<ToolCall>(entry.get("call")?.clone())
                                        .ok()?;
                                let result = serde_json::from_value::<ToolResult>(
                                    entry.get("result")?.clone(),
                                )
                                .ok()?;
                                Some((call, result))
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                recalled_memories: turn
                    .get("recalled_memories")
                    .cloned()
                    .and_then(|v| serde_json::from_value::<Vec<RecalledMemoryRecord>>(v).ok())
                    .unwrap_or_default(),
                active_plan: turn
                    .get("active_plan")
                    .cloned()
                    .and_then(|v| serde_json::from_value::<ActivePlan>(v).ok()),
                consecutive_step_failures: turn
                    .get("consecutive_step_failures")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as u32,
                provider_repair_note: turn
                    .get("provider_repair_note")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                provider_repair_attempts: turn
                    .get("provider_repair_attempts")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as u32,
                pending_text_reply: turn
                    .get("pending_text_reply")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                had_voice_input: turn
                    .get("had_voice_input")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                turn_routing_plan: turn
                    .get("turn_routing_plan")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<TurnRoutingPlan>(value).ok()),
                awaiting_transcription_reentry: turn
                    .get("awaiting_transcription_reentry")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                scripted_loop_context: turn
                    .get("scripted_loop_context")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok()),
            })
        });

        Some(Self {
            session_id,
            agent_id,
            source,
            active_incarnation_id,
            role_activation,
            agent_profile,
            settings: AgentSettings::default(),
            status,
            approval_policy,
            bindings,
            component_route_assembly,
            tool_assembly,
            recent_turns,
            active_turn,
            active_subagents: Vec::new(),
            last_handoff_summary: None,
            rules: checkpoint
                .get("rules")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default(),
        })
    }
}

fn looks_like_conversational_goal(normalized: &str) -> bool {
    normalized.contains('?')
        || [
            "thanks",
            "thank you",
            "appreciate it",
            "appreciated",
            "great",
            "awesome",
            "nice",
            "good job",
            "working well",
            "working pretty well",
            "looks like you're working",
            "looks like youre working",
            "still standing by",
            "got it",
            "ok",
            "okay",
            "cool",
            "sounds good",
        ]
        .iter()
        .any(|phrase| normalized.contains(phrase))
        || [
            "what",
            "why",
            "how",
            "who",
            "when",
            "tell me",
            "explain",
            "help me understand",
            "i think",
            "i am thinking",
            "let's think",
            "can we talk",
        ]
        .iter()
        .any(|prefix| normalized.starts_with(prefix))
}

fn normalized_turn_text(user_content: &str) -> String {
    user_content
        .trim()
        .trim_matches(|ch| matches!(ch, '"' | '\'' | '`'))
        .trim()
        .to_ascii_lowercase()
}

fn tool_name_matches_goal(tool_name: &str, normalized: &str) -> bool {
    let tool_name = tool_name.to_ascii_lowercase();
    if normalized.contains(&tool_name) {
        return true;
    }

    tool_name
        .split(['.', '_', '-'])
        .filter(|part| !part.is_empty())
        .any(|part| normalized.contains(part))
}

pub fn session_checkpoint_memory_type(session_id: &str) -> String {
    format!("short_session:{session_id}")
}

pub fn merge_session_index(
    existing_index: Option<&serde_json::Value>,
    state: &SessionState,
) -> serde_json::Value {
    let mut sessions = existing_index
        .and_then(|index| index.get("active_sessions"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();

    sessions.retain(|entry| {
        entry.get("session_id").and_then(serde_json::Value::as_str)
            != Some(state.session_id.as_str())
    });
    sessions.push(state.checkpoint_index_entry());
    sessions.sort_by(|a, b| {
        let a_ts = a
            .get("updated_at")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let b_ts = b
            .get("updated_at")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        b_ts.cmp(&a_ts)
    });
    sessions.truncate(32);

    json!({
        "agent_id": state.agent_id,
        "active_sessions": sessions,
    })
}

pub fn default_tool_assembly_for_bindings(bindings: &SessionBindings) -> ToolAssembly {
    if !bindings.allowed_tool_runner_incarnations.is_empty() {
        return tool_assembly_from_allowed_incarnations(bindings);
    }

    let toolset = default_visible_toolset(bindings);

    let catalog = tool_catalog();
    let tools_for_model = toolset
        .iter()
        .map(|tool_name| {
            catalog
                .get(tool_name.as_str())
                .cloned()
                .unwrap_or_else(|| ToolDefinition {
                    tool_name: tool_name.clone(),
                    description: format!("Execute the {} tool.", tool_name),
                    input_schema: json!({ "type": "object" }),
                    class: None,
                })
        })
        .collect::<Vec<_>>();

    let local_node_id = local_node_id();
    let execution_routes = toolset
        .iter()
        .map(|tool_name| {
            let execution_mode = if is_local_agent_tool(tool_name) {
                "local_agent"
            } else if is_pinned_tool(tool_name) {
                "pinned"
            } else {
                "capability"
            };
            (
                tool_name.clone(),
                ToolExecutionRoute {
                    target_node: local_node_id.clone(),
                    target_role: if execution_mode == "local_agent" {
                        "agent".into()
                    } else {
                        format!("tool.{tool_name}")
                    },
                    runner_id: if execution_mode == "local_agent" {
                        None
                    } else {
                        Some("tool-runner-01".into())
                    },
                    incarnation_id: None,
                    hotel_id: if execution_mode == "local_agent" {
                        None
                    } else {
                        Some(local_node_id.clone())
                    },
                    environment_id: None,
                    task_runner_kind: task_runner_kind_for_tool(tool_name),
                    task_runner_config: task_runner_base_config_for_tool(bindings, tool_name),
                    execution_mode: execution_mode.into(),
                    availability_state: "live".into(),
                    selection_reason: Some(if execution_mode == "local_agent" {
                        "agent_local_tool".into()
                    } else if execution_mode == "pinned" {
                        "default_pinned_route".into()
                    } else {
                        "default_capability_route".into()
                    }),
                },
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();

    let policy_annotations = toolset
        .iter()
        .map(|tool_name| {
            let class = tool_class(tool_name).unwrap_or("tool");
            (
                tool_name.clone(),
                ToolPolicyAnnotation {
                    policy_class: class.to_string(),
                    approval_required: tool_requires_approval(tool_name),
                },
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();

    ToolAssembly {
        tools_for_model,
        execution_routes,
        policy_annotations,
    }
}

fn default_visible_toolset(bindings: &SessionBindings) -> Vec<String> {
    let mut toolset = if bindings.effective_toolset.is_empty() {
        vec!["echo".to_string()]
    } else {
        bindings.effective_toolset.clone()
    };

    // Expand skill grants: merge implied tools from each active skill.
    for skill in &bindings.effective_skillset {
        for &implied in crate::catalog::skill_implied_tools(skill) {
            let implied = implied.to_string();
            if !toolset.contains(&implied) {
                toolset.push(implied);
            }
        }
    }

    toolset
}

fn is_local_agent_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "session.status"
            | "agent.configure"
            | "memory.recall"
            | "memory.remember"
            | "rule.propose"
            | "skill.register"
            | "skill.list"
            | "skill.assign"
            | "skill.revoke"
            | "subagent.spawn"
            | "role.configure"
            | "handoff.to_role"
            | "handoff.back"
    )
}

fn is_pinned_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "workspace.list" | "workspace.read" | "workspace.search" | "workspace.write"
    )
}

fn task_runner_kind_for_tool(tool_name: &str) -> Option<String> {
    if tool_name.starts_with("workspace.") {
        return Some("workspace".into());
    }

    if tool_name.starts_with("shell.") || tool_name == "bash.exec" {
        return Some("shell".into());
    }

    None
}

fn task_runner_base_config_for_tool(
    bindings: &SessionBindings,
    tool_name: &str,
) -> Option<TaskRunnerBaseConfig> {
    if !tool_name.starts_with("workspace.") {
        return None;
    }

    let config = bindings.workspace_runner_config.clone().unwrap_or_default();
    let default_workspace_ref = config
        .default_workspace_ref
        .or_else(|| bindings.effective_workspace_ref.clone());
    let allowed_tools = config.allowed_tools.or_else(|| {
        let workspace_tools = bindings
            .effective_toolset
            .iter()
            .filter(|tool| tool.starts_with("workspace."))
            .cloned()
            .collect::<Vec<_>>();
        if workspace_tools.is_empty() {
            None
        } else {
            Some(workspace_tools)
        }
    });

    Some(TaskRunnerBaseConfig {
        default_workspace_ref,
        allowed_tools,
        max_read_bytes: config.max_read_bytes,
        max_search_results: config.max_search_results,
    })
}

fn tool_assembly_from_allowed_incarnations(bindings: &SessionBindings) -> ToolAssembly {
    let visible_tools = if bindings.effective_toolset.is_empty() {
        bindings
            .allowed_tool_runner_incarnations
            .iter()
            .flat_map(|incarnation| incarnation.supported_tools.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
    } else {
        bindings.effective_toolset.clone()
    };

    let catalog = tool_catalog();
    let tools_for_model = visible_tools
        .iter()
        .map(|tool_name| {
            catalog
                .get(tool_name.as_str())
                .cloned()
                .unwrap_or_else(|| ToolDefinition {
                    tool_name: tool_name.clone(),
                    description: format!("Execute the {} tool.", tool_name),
                    input_schema: json!({ "type": "object" }),
                    class: None,
                })
        })
        .collect::<Vec<_>>();

    let execution_routes = visible_tools
        .iter()
        .filter_map(|tool_name| {
            select_incarnation_route(bindings, tool_name).map(|route| (tool_name.clone(), route))
        })
        .collect::<std::collections::BTreeMap<_, _>>();

    let policy_annotations = visible_tools
        .iter()
        .map(|tool_name| {
            (
                tool_name.clone(),
                ToolPolicyAnnotation {
                    policy_class: tool_class(tool_name).unwrap_or("tool").to_string(),
                    approval_required: tool_requires_approval(tool_name),
                },
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();

    ToolAssembly {
        tools_for_model,
        execution_routes,
        policy_annotations,
    }
}

fn select_incarnation_route(
    bindings: &SessionBindings,
    tool_name: &str,
) -> Option<ToolExecutionRoute> {
    let mut candidates = bindings
        .allowed_tool_runner_incarnations
        .iter()
        .filter(|incarnation| {
            incarnation
                .supported_tools
                .iter()
                .any(|tool| tool == tool_name)
        })
        .collect::<Vec<_>>();

    candidates.sort_by(|a, b| compare_incarnation_bindings(bindings, a, b));
    let selected = candidates.first()?;
    let selection_reason = selection_reason_for_binding(bindings, selected);

    Some(ToolExecutionRoute {
        target_node: selected
            .target_node
            .clone()
            .or_else(|| selected.hotel_id.clone())
            .unwrap_or_else(local_node_id),
        target_role: selected
            .target_role
            .clone()
            .unwrap_or_else(|| format!("tool.{tool_name}")),
        runner_id: selected
            .runner_id
            .clone()
            .or_else(|| Some(selected.incarnation_id.clone())),
        incarnation_id: Some(selected.incarnation_id.clone()),
        hotel_id: selected.hotel_id.clone(),
        environment_id: selected.environment_id.clone(),
        task_runner_kind: task_runner_kind_for_tool(tool_name),
        task_runner_config: task_runner_base_config_for_tool(bindings, tool_name),
        execution_mode: selected.execution_mode.clone(),
        availability_state: selected.availability_state.clone(),
        selection_reason: Some(selection_reason),
    })
}

fn compare_incarnation_bindings(
    bindings: &SessionBindings,
    left: &ToolRunnerIncarnationBinding,
    right: &ToolRunnerIncarnationBinding,
) -> std::cmp::Ordering {
    binding_preference_rank(bindings, right)
        .cmp(&binding_preference_rank(bindings, left))
        .then_with(|| {
            let left_live = left.availability_state == "live";
            let right_live = right.availability_state == "live";
            right_live.cmp(&left_live)
        })
        .then_with(|| {
            let local_node_id = local_node_id();
            let left_local = left.hotel_id.as_deref() == Some(local_node_id.as_str());
            let right_local = right.hotel_id.as_deref() == Some(local_node_id.as_str());
            right_local.cmp(&left_local)
        })
        .then_with(|| left.incarnation_id.cmp(&right.incarnation_id))
}

fn binding_preference_rank(
    bindings: &SessionBindings,
    binding: &ToolRunnerIncarnationBinding,
) -> u8 {
    if bindings.preferred_tool_runner_incarnation.as_deref()
        == Some(binding.incarnation_id.as_str())
    {
        return 4;
    }
    if bindings.preferred_tool_runner.as_deref() == binding.runner_id.as_deref() {
        return 3;
    }
    if bindings.preferred_environment_id.as_deref() == binding.environment_id.as_deref() {
        return 2;
    }
    if bindings.preferred_hotel_id.as_deref() == binding.hotel_id.as_deref() {
        return 1;
    }
    0
}

fn selection_reason_for_binding(
    bindings: &SessionBindings,
    binding: &ToolRunnerIncarnationBinding,
) -> String {
    let suffix = if binding.availability_state == "live" {
        "live"
    } else {
        "requires_materialization"
    };

    let computed = if bindings.preferred_tool_runner_incarnation.as_deref()
        == Some(binding.incarnation_id.as_str())
    {
        format!("preferred_incarnation_{suffix}")
    } else if bindings.preferred_tool_runner.as_deref() == binding.runner_id.as_deref() {
        format!("preferred_runner_{suffix}")
    } else if bindings.preferred_environment_id.as_deref() == binding.environment_id.as_deref() {
        format!("preferred_environment_{suffix}")
    } else if bindings.preferred_hotel_id.as_deref() == binding.hotel_id.as_deref() {
        format!("preferred_hotel_{suffix}")
    } else {
        let local_node_id = local_node_id();
        if binding.hotel_id.as_deref() == Some(local_node_id.as_str()) {
            "live_local_fallback".into()
        } else if binding.availability_state == "live" {
            "live_allowed_incarnation".into()
        } else {
            "allowed_incarnation_requires_materialization".into()
        }
    };

    let used_preference = binding_preference_rank(bindings, binding) > 0;
    if used_preference {
        computed
    } else {
        binding.selection_hint.clone().unwrap_or(computed)
    }
}

fn projection_item(text: &str, source_ref: &str, projection_kind: &str) -> Value {
    json!({
        "text": text,
        "source_ref": source_ref,
        "projection_kind": projection_kind,
    })
}

fn clone_array_field(object: &Value, field: &str) -> Vec<Value> {
    object
        .get(field)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn filter_projection_items(object: Option<&Value>, allowed_kinds: &[&str]) -> Vec<Value> {
    object
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| {
                    item.get("projection_kind")
                        .and_then(Value::as_str)
                        .map(|kind| allowed_kinds.contains(&kind))
                        .unwrap_or(false)
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn tail_turn_window(object: Option<&Value>, max_items: usize) -> Vec<Value> {
    object
        .and_then(Value::as_array)
        .map(|items| {
            let keep_from = items.len().saturating_sub(max_items);
            items.iter().skip(keep_from).cloned().collect()
        })
        .unwrap_or_default()
}

fn current_unix_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Format current UTC time as "YYYY-MM-DD HH:MM:SS UTC" using only std.
fn utc_datetime_string() -> String {
    let secs = current_unix_ts();
    // Days since Unix epoch → Gregorian calendar (proleptic, no leap-second awareness)
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hh = time_of_day / 3600;
    let mm = (time_of_day % 3600) / 60;
    let ss = time_of_day % 60;

    // Gregorian date calculation (algorithm from http://howardhinnant.github.io/date_algorithms.html)
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        y, m, d, hh, mm, ss
    )
}

/// Apply a set / append / remove operation to a `Vec<String>` field.
fn apply_string_list_op(list: &mut Vec<String>, item: &str, operation: &str) -> Result<(), String> {
    match operation {
        "set" => {
            *list = vec![item.to_string()];
        }
        "append" => {
            if !list.contains(&item.to_string()) {
                list.push(item.to_string());
            }
        }
        "remove" => {
            list.retain(|x| x != item);
        }
        other => {
            return Err(format!(
                "Unknown operation '{other}'. Use 'set', 'append', or 'remove'."
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ApprovalPolicy, ComponentExecutionRoute, ComponentRouteAssembly, ComponentRouteBinding,
        ContextAuthority, ContextLayerId, ContextMutability, HookRequest, HookResult,
        PromotionAction, RecalledMemoryRecord, RefreshRequest, RoleActivation, SessionBindings,
        SessionState, TaskRunnerBaseConfig, ToolRunnerIncarnationBinding,
        TransportReplyTargetBinding, TtsMode, TurnContextEnvelopeKind, TurnRecord, TurnRoutingPlan,
        TurnRoutingStageKind, TurnRoutingStagePlan, VoiceResponsePolicy, WorkingTurn,
        default_tool_assembly_for_bindings, merge_session_index, session_checkpoint_memory_type,
    };
    use crate::r#loop::{ApprovalRequest, ToolCall, ToolResult, TurnPhase};
    use uuid::Uuid;

    #[test]
    fn checkpoint_contains_active_turn_and_history() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-1".into(),
            chat_id: "123".into(),
            user_content: "hello".into(),
            final_reply_to: "local-aiua-01".into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: Some("membrane-telegram-01".into()),
            phase: TurnPhase::Queued,
            iteration: 0,
            pending_tool_call: None,
            pending_approval: None,
            working_tool_history: Vec::new(),
            recalled_memories: Vec::new(),
            active_plan: None,
            consecutive_step_failures: 0,
            provider_repair_note: None,
            provider_repair_attempts: 0,
            pending_text_reply: Some("hello back".into()),
            had_voice_input: true,
            turn_routing_plan: Some(TurnRoutingPlan {
                trigger: "voice_input".into(),
                stages: vec![
                    TurnRoutingStagePlan {
                        kind: TurnRoutingStageKind::Ingress,
                        capability: "voice.transcribe".into(),
                        request_class: "transform".into(),
                        context_envelope: TurnContextEnvelopeKind::Ingress,
                        controller_role: "model.elevenlabs".into(),
                        provider_hint: Some("elevenlabs".into()),
                        streaming: true,
                    },
                    TurnRoutingStagePlan {
                        kind: TurnRoutingStageKind::Cognition,
                        capability: "text.generate".into(),
                        request_class: "cognitive".into(),
                        context_envelope: TurnContextEnvelopeKind::Cognitive,
                        controller_role: "model".into(),
                        provider_hint: None,
                        streaming: true,
                    },
                ],
            }),
            awaiting_transcription_reentry: true,
            scripted_loop_context: None,
        });

        let checkpoint = state.checkpoint_json();
        assert_eq!(checkpoint["session_id"], "sess-1");
        assert_eq!(checkpoint["active_turn"]["turn_id"], "turn-1");
        assert_eq!(checkpoint["active_turn"]["phase"], "queued");
        assert_eq!(checkpoint["active_turn"]["provider_repair_attempts"], 0);
        assert_eq!(
            checkpoint["active_turn"]["pending_text_reply"],
            "hello back"
        );
        assert_eq!(checkpoint["active_turn"]["had_voice_input"], true);
        assert_eq!(
            checkpoint["active_turn"]["turn_routing_plan"]["trigger"],
            "voice_input"
        );
        assert_eq!(
            checkpoint["active_turn"]["awaiting_transcription_reentry"],
            true
        );
        assert_eq!(
            checkpoint["active_turn"]["final_reply_guest_id"],
            "membrane-telegram-01"
        );
        // component_route_assembly and tool_assembly are hotel-computed and intentionally
        // excluded from the checkpoint to prevent circular growth. They are re-injected
        // by compose_session_snapshot on every turn.
        assert!(checkpoint.get("component_route_assembly").is_none());
        assert!(checkpoint.get("tool_assembly").is_none());
    }

    #[test]
    fn tts_off_still_mirrors_voice_input_without_caption() {
        let policy = VoiceResponsePolicy {
            mode: TtsMode::Off,
            ..Default::default()
        };

        assert!(!policy.is_active(false), "plain text should stay text-only");
        assert!(
            policy.is_active(true),
            "voice input should still get a voice reply"
        );
        assert!(
            !policy.caption_enabled(),
            "off mode should not attach text captions"
        );
    }

    #[test]
    fn checkpoint_does_not_persist_component_route_assembly() {
        // component_route_assembly is hotel-computed (injected on every turn via
        // compose_session_snapshot). It must NOT survive a checkpoint round-trip;
        // that would cause unbounded circular payload growth.
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.component_route_assembly = ComponentRouteAssembly {
            execution_routes: std::collections::BTreeMap::from([(
                "text.generate".into(),
                ComponentExecutionRoute {
                    target_node: "aria-node".into(),
                    target_role: "model".into(),
                    incarnation_id: Some("aria-architect-hotel:model-controller-gemini".into()),
                    hotel_id: Some("aria-architect-hotel".into()),
                    environment_id: None,
                    execution_mode: "capability".into(),
                    availability_state: "live".into(),
                    selection_reason: Some("remote_latency_capacity".into()),
                },
            )]),
        };

        let checkpoint = state.checkpoint_json();
        // The field should not appear in the checkpoint at all.
        assert!(checkpoint.get("component_route_assembly").is_none());

        let restored =
            SessionState::from_checkpoint(&checkpoint).expect("checkpoint should restore");
        // After restore, routes are gone — the hotel re-injects them on the next turn.
        assert!(
            restored
                .resolve_component_execution_route("text.generate")
                .is_none()
        );
    }

    #[test]
    fn completing_turn_rolls_into_recent_history() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-1".into(),
            chat_id: "123".into(),
            user_content: "hello".into(),
            final_reply_to: "local-aiua-01".into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: None,
            phase: TurnPhase::Queued,
            iteration: 0,
            pending_tool_call: None,
            pending_approval: None,
            working_tool_history: Vec::new(),
            recalled_memories: Vec::new(),
            active_plan: None,
            consecutive_step_failures: 0,
            provider_repair_note: None,
            provider_repair_attempts: 0,
            pending_text_reply: None,
            had_voice_input: false,
            turn_routing_plan: None,
            awaiting_transcription_reentry: false,
            scripted_loop_context: None,
        });

        state.complete_active_turn("hi".into());
        let checkpoint = state.checkpoint_json();
        assert!(checkpoint["active_turn"].is_null());
        assert_eq!(checkpoint["recent_turns"][0]["assistant_content"], "hi");
    }

    #[test]
    fn voice_message_is_sanitized_before_storing_in_recent_turns() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        let audio_payload = format!(
            r#"{{"audio_base64":"{}"}}"#,
            "A".repeat(1_900_000) // ~1.9MB
        );
        state.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-1".into(),
            chat_id: "123".into(),
            user_content: audio_payload,
            final_reply_to: "local-aiua-01".into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: None,
            phase: TurnPhase::Queued,
            iteration: 0,
            pending_tool_call: None,
            pending_approval: None,
            working_tool_history: Vec::new(),
            recalled_memories: Vec::new(),
            active_plan: None,
            consecutive_step_failures: 0,
            provider_repair_note: None,
            provider_repair_attempts: 0,
            pending_text_reply: None,
            had_voice_input: false,
            turn_routing_plan: None,
            awaiting_transcription_reentry: false,
            scripted_loop_context: None,
        });

        state.complete_active_turn("transcription reply".into());
        let checkpoint = state.checkpoint_json();
        let stored_content = checkpoint["recent_turns"][0]["user_content"]
            .as_str()
            .unwrap();
        assert_eq!(stored_content, "[voice message]");
    }

    #[test]
    fn stale_audio_in_checkpoint_is_stripped_from_knowledge_projection() {
        // Regression: old checkpoints on disk may still have raw audio base64 in
        // recent_turns[].user_content. Ensure project_knowledge() never leaks it.
        let audio_payload = format!(r#"{{"audio_base64":"{}"}}"#, "B".repeat(1_900_000));
        let checkpoint = serde_json::json!({
            "session_id": "sess-1",
            "agent_id": "agent-jane-01",
            "source": "telegram",
            "recent_turns": [
                {
                    "turn_id": "t1",
                    "user_content": audio_payload,
                    "assistant_content": "got it",
                    "created_at": 0u64
                }
            ]
        });
        let state =
            SessionState::from_checkpoint(&checkpoint).expect("from_checkpoint must succeed");
        let knowledge = state.project_knowledge("", &[]);
        assert!(
            !knowledge.contains("audio_base64"),
            "audio base64 must not appear in context"
        );
        assert!(
            knowledge.contains("[voice message]"),
            "placeholder must appear in context"
        );
    }

    #[test]
    fn low_intent_cognitive_prompt_omits_skill_guidance_and_detailed_approval_policy() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.bindings.effective_skillset = vec!["planning".into()];
        state.bindings.effective_skill_guidance =
            vec!["Use the planning skill to build multi-step execution plans.".into()];
        state.approval_policy.preapproved_tools = vec!["workspace.read".into()];
        state.approval_policy.preapproved_classes = vec!["workspace".into()];

        let (prompt, context, _) = state.model_request_payloads_for_envelope(
            "Thanks, that solved it.",
            &[],
            TurnContextEnvelopeKind::Cognitive,
        );

        assert!(
            !prompt.contains("Current skill posture:"),
            "low-intent prompt should not advertise skill posture"
        );
        assert!(
            !prompt.contains("[Skill guidance]"),
            "low-intent prompt should not include skill guidance"
        );
        assert!(
            !prompt.contains("Pre-approved tools:"),
            "low-intent prompt should not dump detailed approval posture"
        );
        assert!(
            prompt.contains("[Turn policy]"),
            "low-intent prompt should carry the simpler turn-policy steer"
        );
        let memory_text = context["memory"]
            .as_array()
            .expect("memory array")
            .iter()
            .filter_map(|item| item.get("text").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !memory_text.contains("Pre-approved tools:"),
            "low-intent context should not include detailed approval posture"
        );
    }

    #[test]
    fn ingress_prompt_omits_skill_guidance_and_approval_policy_details() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.bindings.effective_skillset = vec!["planning".into()];
        state.bindings.effective_skill_guidance =
            vec!["Use the planning skill to build multi-step execution plans.".into()];
        state.approval_policy.preapproved_tools = vec!["workspace.read".into()];

        let (prompt, _, _) = state.model_request_payloads_for_envelope(
            "Please transcribe this voice note.",
            &[],
            TurnContextEnvelopeKind::Ingress,
        );

        assert!(
            !prompt.contains("Current skill posture:"),
            "ingress prompt should not advertise skill posture"
        );
        assert!(
            !prompt.contains("[Skill guidance]"),
            "ingress prompt should not include skill guidance"
        );
        assert!(
            !prompt.contains("[Approval policy]"),
            "ingress prompt should not include approval policy details"
        );
    }

    #[test]
    fn state_rehydrates_from_checkpoint() {
        let checkpoint = serde_json::json!({
            "session_id": "sess-1",
            "agent_id": "agent-jane-01",
            "source": "telegram",
            "active_incarnation_id": "agent-jane:developer",
            "role_activation": {
                "role_name": "developer",
                "active_incarnation_id": "agent-jane:developer",
                "activation_reason": "session_active_incarnation",
                "requested_by": "hotel_runtime",
                "role_addendum": "Focus on implementation and code changes.",
                "toolset_profile_ref": "codex",
                "effective_skillset": ["planning"],
                "working_memory_policy": "role_local",
                "memory_projection_policy": "shared_identity_role_scoped"
            },
            "status": "paused",
            "approval_policy": {
                "auto_approve_all": true
            },
            "bindings": {
                "effective_toolset": ["echo", "workspace.read"],
                "effective_skillset": ["planning"],
                "effective_workspace_ref": "workspace://main",
                "transport_reply_target": {
                    "target_node": "local-aiua-01",
                    "target_role": "membrane",
                    "target_guest_id": "membrane-telegram-01"
                },
                "effective_model_controller": "gemini-flash"
            },
            "active_turn": {
                "turn_id": "turn-2",
                "task_id": Uuid::nil().to_string(),
                "chat_id": "123",
                "user_content": "status?",
                "final_reply_to": "local-aiua-01",
                "final_reply_role": "membrane",
                "phase": "waiting_model",
                "iteration": 1,
                "pending_tool_call": {
                    "tool_name": "echo",
                    "arguments": { "text": "hello" }
                },
                "pending_approval": {
                    "approval_id": "appr-1",
                    "reason": "Need confirmation",
                    "approved_response": "Confirmed"
                }
            },
            "recent_turns": [{
                "turn_id": "turn-1",
                "user_content": "hello",
                "assistant_content": "hi"
            }]
        });

        let state = SessionState::from_checkpoint(&checkpoint).expect("rehydrate state");
        assert_eq!(state.session_id, "sess-1");
        assert_eq!(state.status, "paused");
        assert_eq!(
            state.active_incarnation_id.as_deref(),
            Some("agent-jane:developer")
        );
        assert_eq!(
            state
                .role_activation
                .as_ref()
                .map(|role| role.role_name.as_str()),
            Some("developer")
        );
        assert_eq!(
            state
                .role_activation
                .as_ref()
                .and_then(|role| role.toolset_profile_ref.as_deref()),
            Some("codex")
        );
        assert_eq!(
            state.approval_policy,
            ApprovalPolicy {
                auto_approve_all: true,
                preapproved_tools: Vec::new(),
                preapproved_classes: Vec::new(),
            }
        );
        assert_eq!(
            state.bindings,
            SessionBindings {
                effective_toolset: vec!["echo".into(), "workspace.read".into()],
                effective_skillset: vec!["planning".into()],
                effective_skill_guidance: Vec::new(),
                effective_workspace_ref: Some("workspace://main".into()),
                transport_reply_target: Some(TransportReplyTargetBinding {
                    target_node: "local-aiua-01".into(),
                    target_role: "membrane".into(),
                    target_guest_id: Some("membrane-telegram-01".into()),
                }),
                component_routes: Vec::new(),
                effective_model_controller: Some("gemini-flash".into()),
                workspace_runner_config: None,
                preferred_tool_runner_incarnation: None,
                preferred_tool_runner: None,
                preferred_hotel_id: None,
                preferred_environment_id: None,
                allowed_tool_runner_incarnations: Vec::new(),
            }
        );
        assert_eq!(state.recent_turns.len(), 1);
        assert_eq!(state.active_turn.as_ref().unwrap().turn_id, "turn-2");
        assert_eq!(
            state.active_turn.as_ref().unwrap().phase,
            TurnPhase::WaitingModel
        );
        assert_eq!(
            state.active_turn.as_ref().unwrap().pending_tool_call,
            Some(ToolCall {
                tool_name: "echo".into(),
                arguments: serde_json::json!({ "text": "hello" }),
            })
        );
        assert_eq!(
            state.active_turn.as_ref().unwrap().pending_approval,
            Some(ApprovalRequest {
                approval_id: Some("appr-1".into()),
                reason: "Need confirmation".into(),
                approved_response: "Confirmed".into(),
            })
        );
        assert_eq!(state.tool_assembly.tools_for_model[0].tool_name, "echo");
        assert_eq!(
            state
                .tool_assembly
                .execution_routes
                .get("echo")
                .and_then(|route| route.runner_id.as_deref()),
            Some("tool-runner-01")
        );
    }

    #[test]
    fn approval_policy_can_auto_approve_session_requests() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.approval_policy = ApprovalPolicy {
            auto_approve_all: true,
            preapproved_tools: Vec::new(),
            preapproved_classes: Vec::new(),
        };

        assert!(state.approval_policy_allows(
            &ApprovalRequest {
                approval_id: Some("appr-2".into()),
                reason: "deploy the thing".into(),
                approved_response: "Approved: deploy the thing".into(),
            },
            None
        ));
    }

    #[test]
    fn preapproved_tools_bypasses_approval_for_named_tool() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.approval_policy = ApprovalPolicy {
            auto_approve_all: false,
            preapproved_tools: vec!["echo".into()],
            preapproved_classes: Vec::new(),
        };
        let approval = ApprovalRequest {
            approval_id: None,
            reason: "use echo".into(),
            approved_response: "ok".into(),
        };
        let echo_call = ToolCall {
            tool_name: "echo".into(),
            arguments: serde_json::json!({}),
        };
        let other_call = ToolCall {
            tool_name: "workspace.read".into(),
            arguments: serde_json::json!({}),
        };

        assert!(state.approval_policy_allows(&approval, Some(&echo_call)));
        assert!(!state.approval_policy_allows(&approval, Some(&other_call)));
        assert!(!state.approval_policy_allows(&approval, None));
    }

    #[test]
    fn preapproved_classes_bypasses_approval_for_class_members() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.approval_policy = ApprovalPolicy {
            auto_approve_all: false,
            preapproved_tools: Vec::new(),
            preapproved_classes: vec!["workspace".into()],
        };
        let approval = ApprovalRequest {
            approval_id: None,
            reason: "read file".into(),
            approved_response: "ok".into(),
        };
        let workspace_call = ToolCall {
            tool_name: "workspace.read".into(),
            arguments: serde_json::json!({}),
        };
        let config_call = ToolCall {
            tool_name: "agent.configure".into(),
            arguments: serde_json::json!({}),
        };

        assert!(state.approval_policy_allows(&approval, Some(&workspace_call)));
        assert!(!state.approval_policy_allows(&approval, Some(&config_call)));
    }

    #[test]
    fn agent_configure_requires_approval_and_is_in_config_class() {
        use crate::catalog::{tool_class, tool_requires_approval};
        assert_eq!(tool_class("agent.configure"), Some("config"));
        assert!(tool_requires_approval("agent.configure"));
        assert!(!tool_requires_approval("echo"));
        assert!(!tool_requires_approval("workspace.read"));
    }

    #[test]
    fn bash_exec_is_in_shell_class_and_requires_approval() {
        use crate::catalog::{tool_catalog, tool_class, tool_requires_approval};
        let catalog = tool_catalog();
        assert!(
            catalog.contains_key("bash.exec"),
            "bash.exec must be in catalog"
        );
        assert_eq!(tool_class("bash.exec"), Some("shell"));
        assert!(tool_requires_approval("bash.exec"));
    }

    #[test]
    fn bash_exec_catalog_entry_has_required_command_arg() {
        use crate::catalog::tool_catalog;
        let catalog = tool_catalog();
        let entry = catalog.get("bash.exec").expect("bash.exec in catalog");
        let required = entry
            .input_schema
            .get("required")
            .and_then(|v| v.as_array())
            .expect("required array");
        assert!(
            required.iter().any(|v| v.as_str() == Some("command")),
            "'command' must be in required"
        );
        let props = entry
            .input_schema
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("properties object");
        assert!(props.contains_key("command"));
        assert!(props.contains_key("working_dir"));
        assert!(props.contains_key("timeout_secs"));
    }

    #[test]
    fn agent_configure_apply_mutates_approval_policy() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());

        // Append a tool to preapproved_tools
        let result = state.apply_configure(
            "approval_policy.preapproved_tools",
            &serde_json::json!("echo"),
            "append",
        );
        assert!(result.is_ok());
        assert!(
            state
                .approval_policy
                .preapproved_tools
                .contains(&"echo".to_string())
        );

        // Append a class to preapproved_classes
        let result = state.apply_configure(
            "approval_policy.preapproved_classes",
            &serde_json::json!("workspace"),
            "append",
        );
        assert!(result.is_ok());
        assert!(
            state
                .approval_policy
                .preapproved_classes
                .contains(&"workspace".to_string())
        );

        // Remove the tool
        let result = state.apply_configure(
            "approval_policy.preapproved_tools",
            &serde_json::json!("echo"),
            "remove",
        );
        assert!(result.is_ok());
        assert!(
            !state
                .approval_policy
                .preapproved_tools
                .contains(&"echo".to_string())
        );

        // Set auto_approve_all
        let result = state.apply_configure(
            "approval_policy.auto_approve_all",
            &serde_json::json!(true),
            "set",
        );
        assert!(result.is_ok());
        assert!(state.approval_policy.auto_approve_all);
    }

    #[test]
    fn agent_configure_apply_mutates_profile() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());

        let result = state.apply_configure(
            "profile.soul_text",
            &serde_json::json!("You are a curious and helpful agent."),
            "set",
        );
        assert!(result.is_ok());
        assert_eq!(
            state.agent_profile.soul_text.as_deref(),
            Some("You are a curious and helpful agent.")
        );
    }

    #[test]
    fn agent_configure_rejects_unknown_path() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        let result = state.apply_configure("unknown.path", &serde_json::json!("x"), "set");
        assert!(result.is_err());
    }

    #[test]
    fn policy_annotation_uses_catalog_class_and_approval_required() {
        let state = SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        // agent.configure is local_agent so it only appears if in effective_toolset
        let mut bindings = state.bindings.clone();
        bindings.effective_toolset = vec!["agent.configure".into(), "echo".into()];
        let assembly = default_tool_assembly_for_bindings(&bindings);

        let configure_ann = assembly.policy_annotations.get("agent.configure").unwrap();
        assert_eq!(configure_ann.policy_class, "config");
        assert!(configure_ann.approval_required);

        let echo_ann = assembly.policy_annotations.get("echo").unwrap();
        assert_eq!(echo_ann.policy_class, "utility");
        assert!(!echo_ann.approval_required);
    }

    #[test]
    fn prompt_reflects_session_preapproval() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.set_preapprove_this_session();

        let prompt = state.build_prompt("deploy the thing");
        assert!(prompt.contains("This session is pre-approved."));
    }

    #[test]
    fn prompt_reflects_session_bindings_and_status() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.status = "paused".into();
        state.bindings = SessionBindings {
            effective_toolset: vec!["echo".into()],
            effective_skillset: vec!["planning".into()],
            effective_skill_guidance: Vec::new(),
            effective_workspace_ref: Some("workspace://main".into()),
            transport_reply_target: Some(TransportReplyTargetBinding {
                target_node: "local-aiua-01".into(),
                target_role: "membrane".into(),
                target_guest_id: Some("membrane-telegram-01".into()),
            }),
            component_routes: Vec::new(),
            effective_model_controller: Some("gemini-flash".into()),
            workspace_runner_config: None,
            preferred_tool_runner_incarnation: None,
            preferred_tool_runner: None,
            preferred_hotel_id: None,
            preferred_environment_id: None,
            allowed_tool_runner_incarnations: Vec::new(),
        };

        let prompt = state.build_prompt("status");
        assert!(prompt.contains("Session status: paused."));
        assert!(prompt.contains("Effective tools: echo."));
        assert!(prompt.contains("Workspace: workspace://main."));
        assert!(
            prompt
                .contains("Delivery target: local-aiua-01 / membrane guest=membrane-telegram-01.")
        );
    }

    #[test]
    fn prompt_uses_agent_profile_sources_when_present() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.agent_profile.identity_text = Some("Identity anchor: Jane".into());
        state.agent_profile.soul_text = Some("Soul anchor: sharp, warm, witty.".into());
        state.agent_profile.user_context_text =
            Some("User anchor: Jared prefers direct collaboration.".into());
        state.agent_profile.agents_text = Some("Workspace rule: read the soul first.".into());
        state.agent_profile.memory_summary = Some("Memory seed: architecture matters.".into());

        let prompt = state.build_prompt("status");
        assert!(prompt.contains("Identity anchor: Jane"));
        assert!(prompt.contains("Soul anchor: sharp, warm, witty."));
        assert!(prompt.contains("User anchor: Jared prefers direct collaboration."));
        assert!(prompt.contains("Memory seed: architecture matters."));
    }

    #[test]
    fn context_projection_carries_conversation_turn_and_layer_metadata() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.status = "paused".into();
        state.active_incarnation_id = Some("agent-jane:developer".into());
        state.role_activation = Some(RoleActivation {
            role_name: "developer".into(),
            active_incarnation_id: Some("agent-jane:developer".into()),
            activation_reason: "session_active_incarnation".into(),
            requested_by: Some("hotel_runtime".into()),
            role_addendum: Some("Focus on implementation and code changes.".into()),
            toolset_profile_ref: Some("codex".into()),
            effective_skillset: vec!["planning".into()],
            working_memory_policy: Some("role_local".into()),
            memory_projection_policy: Some("shared_identity_role_scoped".into()),
            ..Default::default()
        });
        state.agent_profile.identity_text = Some("Identity anchor: Jane".into());
        state.agent_profile.user_context_text =
            Some("User anchor: Jared prefers direct collaboration.".into());
        state.agent_profile.memory_summary = Some("Memory seed: architecture matters.".into());
        state.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-ctx-1".into(),
            chat_id: "123".into(),
            user_content: "status".into(),
            final_reply_to: "local-aiua-01".into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: None,
            phase: TurnPhase::WaitingModel,
            iteration: 2,
            pending_tool_call: None,
            pending_approval: None,
            working_tool_history: vec![(
                ToolCall {
                    tool_name: "echo".into(),
                    arguments: serde_json::json!({"text": "hello"}),
                },
                ToolResult {
                    tool_name: "echo".into(),
                    content: "hello".into(),
                },
            )],
            recalled_memories: Vec::new(),
            active_plan: None,
            consecutive_step_failures: 0,
            provider_repair_note: None,
            provider_repair_attempts: 0,
            pending_text_reply: None,
            had_voice_input: false,
            turn_routing_plan: None,
            awaiting_transcription_reentry: false,
            scripted_loop_context: None,
        });

        let projection = state.build_context_projection("status");
        assert_eq!(
            projection.conversation_turn.conversation_turn_id,
            "turn-ctx-1"
        );
        assert_eq!(projection.conversation_turn.session_id, "sess-1");
        assert_eq!(
            projection
                .conversation_turn
                .active_incarnation_id
                .as_deref(),
            Some("agent-jane:developer")
        );
        assert_eq!(projection.conversation_turn.trigger_kind, "user_message");
        assert_eq!(
            projection
                .active_step
                .as_ref()
                .map(|step| step.step_kind.as_str()),
            Some("waiting_model")
        );
        assert_eq!(projection.layers.len(), 5);
        assert_eq!(
            projection
                .role_activation
                .as_ref()
                .map(|role| role.role_name.as_str()),
            Some("developer")
        );
        assert!(
            projection
                .layers
                .iter()
                .any(|layer| layer.layer_id == ContextLayerId::Working
                    && layer.mutability == ContextMutability::LiveLocal)
        );
        assert!(
            projection
                .contributions
                .iter()
                .any(
                    |contribution| contribution.layer_id == ContextLayerId::Session
                        && contribution.authority == ContextAuthority::Authoritative
                )
        );
        assert!(
            projection
                .refresh_plan
                .contains(&"checkpoint.after_model".to_string())
        );
    }

    #[test]
    fn prompt_renders_session_and_working_sections_from_projection() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.status = "paused".into();
        state.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-ctx-2".into(),
            chat_id: "123".into(),
            user_content: "status".into(),
            final_reply_to: "local-aiua-01".into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: None,
            phase: TurnPhase::WaitingModel,
            iteration: 1,
            pending_tool_call: None,
            pending_approval: None,
            working_tool_history: Vec::new(),
            recalled_memories: Vec::new(),
            active_plan: None,
            consecutive_step_failures: 0,
            provider_repair_note: None,
            provider_repair_attempts: 0,
            pending_text_reply: None,
            had_voice_input: false,
            turn_routing_plan: None,
            awaiting_transcription_reentry: false,
            scripted_loop_context: None,
        });

        let prompt = state.build_prompt("status");
        assert!(prompt.contains("[Session projection]"));
        assert!(prompt.contains("[Working projection]"));
        assert!(prompt.contains("Active conversation turn: turn-ctx-2."));
    }

    #[test]
    fn role_activation_projects_addendum_and_toolset_into_prompt() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.active_incarnation_id = Some("agent-jane:developer".into());
        state.role_activation = Some(RoleActivation {
            role_name: "developer".into(),
            active_incarnation_id: Some("agent-jane:developer".into()),
            activation_reason: "session_active_incarnation".into(),
            requested_by: Some("hotel_runtime".into()),
            role_addendum: Some("Focus on implementation and code changes.".into()),
            toolset_profile_ref: Some("codex".into()),
            effective_skillset: vec!["planning".into(), "implementation".into()],
            working_memory_policy: Some("role_local".into()),
            memory_projection_policy: Some("shared_identity_role_scoped".into()),
            ..Default::default()
        });

        let prompt = state.build_prompt("status");
        assert!(prompt.contains("Active role posture: developer."));
        assert!(prompt.contains("Role addendum: Focus on implementation and code changes."));
        assert!(prompt.contains("Role toolset profile: codex."));
        assert!(prompt.contains("Role skillset posture: planning, implementation."));
        assert!(prompt.contains("Role working-memory policy: role_local."));
    }

    #[test]
    fn same_identity_handoff_bundle_carries_live_session_context() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.status = "paused".into();
        state.active_incarnation_id = Some("agent-jane:developer".into());
        state.bindings.effective_workspace_ref = Some("workspace://main".into());
        state.role_activation = Some(RoleActivation {
            role_name: "developer".into(),
            active_incarnation_id: Some("agent-jane:developer".into()),
            activation_reason: "session_active_incarnation".into(),
            requested_by: Some("hotel_runtime".into()),
            role_addendum: Some("Focus on implementation and code changes.".into()),
            toolset_profile_ref: Some("codex".into()),
            effective_skillset: vec!["planning".into()],
            working_memory_policy: Some("role_local".into()),
            memory_projection_policy: Some("shared_identity_role_scoped".into()),
            ..Default::default()
        });
        state.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-handoff-1".into(),
            chat_id: "123".into(),
            user_content: "implement the fix".into(),
            final_reply_to: "local-aiua-01".into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: None,
            phase: TurnPhase::WaitingModel,
            iteration: 2,
            pending_tool_call: None,
            pending_approval: None,
            working_tool_history: Vec::new(),
            recalled_memories: Vec::new(),
            active_plan: None,
            consecutive_step_failures: 0,
            provider_repair_note: None,
            provider_repair_attempts: 0,
            pending_text_reply: None,
            had_voice_input: false,
            turn_routing_plan: None,
            awaiting_transcription_reentry: false,
            scripted_loop_context: None,
        });

        let bundle = state.build_same_identity_handoff_bundle(
            "architect",
            "turn-handoff-1",
            "manual_role_switch",
            Some("orchestrator".into()),
        );

        assert_eq!(bundle.handoff_reason.as_deref(), Some("manual_role_switch"));
        assert_eq!(bundle.active_goal.as_deref(), Some("implement the fix"));
        assert!(
            bundle
                .relevant_session_facts
                .contains(&"session_status=paused".to_string())
        );
        assert!(
            bundle
                .relevant_session_facts
                .contains(&"workspace=workspace://main".to_string())
        );
        assert_eq!(bundle.expected_return_mode.as_deref(), Some("required"));
        assert_eq!(bundle.from_role.as_deref(), Some("developer"));
        assert_eq!(bundle.to_role.as_deref(), Some("architect"));
        assert!(
            bundle
                .cleanup_actions
                .contains(&"persist_role_local_working_state".to_string())
        );
    }

    #[test]
    fn subagent_delegation_builder_is_lightweight_and_role_scoped() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.status = "active".into();
        state.active_incarnation_id = Some("agent-jane:developer".into());
        state.bindings.effective_workspace_ref = Some("workspace://main".into());
        state.role_activation = Some(RoleActivation {
            role_name: "developer".into(),
            active_incarnation_id: Some("agent-jane:developer".into()),
            activation_reason: "session_active_incarnation".into(),
            requested_by: Some("hotel_runtime".into()),
            role_addendum: Some("Focus on implementation and code changes.".into()),
            toolset_profile_ref: Some("codex".into()),
            effective_skillset: vec!["implementation".into()],
            working_memory_policy: Some("role_local".into()),
            memory_projection_policy: Some("shared_identity_role_scoped".into()),
            ..Default::default()
        });
        state.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-subagent-1".into(),
            chat_id: "123".into(),
            user_content: "break this into a small worker task".into(),
            final_reply_to: "local-aiua-01".into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: None,
            phase: TurnPhase::WaitingModel,
            iteration: 1,
            pending_tool_call: None,
            pending_approval: None,
            working_tool_history: Vec::new(),
            recalled_memories: Vec::new(),
            active_plan: None,
            consecutive_step_failures: 0,
            provider_repair_note: None,
            provider_repair_attempts: 0,
            pending_text_reply: None,
            had_voice_input: false,
            turn_routing_plan: None,
            awaiting_transcription_reentry: false,
            scripted_loop_context: None,
        });

        let delegation = state.build_subagent_delegation(
            "Read the files and summarize risks.",
            "research_worker",
            vec!["workspace.read".into()],
            vec!["research".into()],
        );

        assert_eq!(delegation.parent_agent_id, "agent-jane-01");
        assert_eq!(delegation.parent_role, "developer");
        assert_eq!(delegation.subagent_kind, "research_worker");
        assert_eq!(delegation.goal, "Read the files and summarize risks.");
        assert_eq!(delegation.allowed_tools, vec!["workspace.read"]);
        assert_eq!(delegation.allowed_skills, vec!["research"]);
        assert_eq!(
            delegation.memory_allowance.as_deref(),
            Some("none_by_default")
        );
        assert_eq!(
            delegation.writeback_allowance.as_deref(),
            Some("summary_only_parent_mediated")
        );
        assert!(
            delegation
                .context_packet
                .session_facts
                .contains(&"workspace=workspace://main".to_string())
        );
        assert!(
            delegation
                .context_packet
                .constraints
                .contains(&"subagent_lightweight_default".to_string())
        );
        assert!(delegation.completion_contract.summary_required);
        assert!(delegation.completion_contract.failure_summary_required);
        assert!(delegation.completion_contract.requires_parent_ack);
    }

    #[test]
    fn hook_payload_types_round_trip_through_json() {
        let state = SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        let projection = state.build_context_projection("status");
        let request = HookRequest {
            hook_name: "context.build".into(),
            scope: "conversation_turn".into(),
            checkpoint: "conversation_turn.start".into(),
            conversation_turn: projection.conversation_turn.clone(),
            cognitive_step: projection.active_step.clone(),
            context_projection: Some(projection.clone()),
            inputs: serde_json::json!({"mode": "default"}),
        };
        let result = HookResult {
            status: "applied".into(),
            updates: serde_json::json!({"ok": true}),
            emitted_contributions: projection.contributions.clone(),
            refresh_requests: vec![RefreshRequest {
                layer_ids: vec![ContextLayerId::Knowledge],
                reason: "tool result changed topic salience".into(),
                target_checkpoint: "checkpoint.after_tool".into(),
                urgency: "next_checkpoint".into(),
            }],
            promotion_actions: vec![PromotionAction {
                target: "memory".into(),
                concept: Some("decision".into()),
                summary: Some("Stored a durable preference".into()),
                content: "User prefers direct collaboration.".into(),
                confidence: Some("high".into()),
                reason: "stable collaboration preference".into(),
                source_refs: vec!["relationship".into()],
            }],
            diagnostics: vec!["context.build applied".into()],
        };

        let request_json = serde_json::to_value(&request).expect("hook request should serialize");
        let result_json = serde_json::to_value(&result).expect("hook result should serialize");
        let restored_request: HookRequest =
            serde_json::from_value(request_json).expect("hook request should deserialize");
        let restored_result: HookResult =
            serde_json::from_value(result_json).expect("hook result should deserialize");

        assert_eq!(restored_request.hook_name, "context.build");
        assert_eq!(restored_result.status, "applied");
        assert_eq!(restored_result.refresh_requests.len(), 1);
        assert_eq!(restored_result.promotion_actions[0].target, "memory");
    }

    #[test]
    fn model_context_carries_active_incarnation_in_session_instructions() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.active_incarnation_id = Some("agent-jane:developer".into());
        state.role_activation = Some(RoleActivation {
            role_name: "developer".into(),
            active_incarnation_id: Some("agent-jane:developer".into()),
            activation_reason: "session_active_incarnation".into(),
            requested_by: Some("hotel_runtime".into()),
            role_addendum: Some("Focus on implementation and code changes.".into()),
            toolset_profile_ref: Some("codex".into()),
            effective_skillset: vec!["planning".into()],
            working_memory_policy: Some("role_local".into()),
            memory_projection_policy: Some("shared_identity_role_scoped".into()),
            ..Default::default()
        });

        let projection = state.build_context_projection("status");
        let context = state.model_context_from_projection(&projection);
        let instructions = context["instructions"]
            .as_array()
            .expect("instructions should be an array");

        assert!(instructions.iter().any(|item| {
            item["projection_kind"] == "session"
                && item["text"]
                    .as_str()
                    .map(|text| {
                        text.contains("Active incarnation: agent-jane:developer.")
                            && text.contains("Role toolset profile: codex.")
                    })
                    .unwrap_or(false)
        }));
    }

    #[test]
    fn status_text_reports_when_no_preapproval_exists() {
        let state = SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        let text = state.approval_policy_status_text();
        assert!(text.contains("auto_approve_all: false"));
        assert!(text.contains("preapproved_tools: none"));
        assert!(text.contains("preapproved_classes: none"));
    }

    #[test]
    fn session_status_text_reports_bindings() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.status = "paused".into();
        state.bindings.effective_toolset = vec!["echo".into()];
        state.bindings.effective_skillset = vec!["planning".into()];
        state.bindings.effective_workspace_ref = Some("workspace://main".into());
        state.bindings.effective_model_controller = Some("gemini-flash".into());
        state.bindings.transport_reply_target = Some(TransportReplyTargetBinding {
            target_node: "local-aiua-01".into(),
            target_role: "membrane".into(),
            target_guest_id: Some("membrane-telegram-01".into()),
        });

        let text = state.session_status_text();
        assert!(text.contains("Session status: paused."));
        assert!(text.contains("Tools: echo."));
        assert!(text.contains("Workspace: workspace://main."));
        assert!(text.contains("Routes: text.generate [legacy] impl=gemini-flash."));
        assert!(text.contains("Delivery: local-aiua-01 / membrane guest=membrane-telegram-01."));
    }

    #[test]
    fn component_route_summary_prefers_structured_routes() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.bindings.component_routes.push(ComponentRouteBinding {
            capability: "text.generate".into(),
            selection_mode: "preferred".into(),
            implementation: Some("gemini".into()),
            incarnation: Some("model-controller-gemini-01".into()),
            preferred_hotel_id: Some("local-aiua-01".into()),
            preferred_environment_id: Some("env://local".into()),
        });

        let summary = state.component_route_summary().expect("route summary");
        assert!(summary.contains("text.generate [preferred]"));
        assert!(summary.contains("impl=gemini"));
        assert!(summary.contains("inc=model-controller-gemini-01"));
    }

    #[test]
    fn resolved_transport_reply_target_prefers_session_binding() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.set_transport_reply_target(
            "local-aiua-01",
            "membrane",
            Some("membrane-telegram-01".into()),
        );

        let target = state.resolved_transport_reply_target(
            "fallback-node",
            "fallback-role",
            Some("fallback-guest".into()),
        );

        assert_eq!(target.target_node, "local-aiua-01");
        assert_eq!(target.target_role, "membrane");
        assert_eq!(
            target.target_guest_id.as_deref(),
            Some("membrane-telegram-01")
        );
    }

    #[test]
    fn tool_binding_gates_enabled_tools() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        assert!(state.tool_is_enabled("echo"));
        assert_eq!(
            state
                .resolve_tool_route("echo")
                .map(|route| route.target_role.as_str()),
            Some("tool.echo")
        );

        state.add_tool_binding("echo");
        assert!(state.tool_is_enabled("echo"));
        assert!(!state.tool_is_enabled("workspace.read"));
        assert_eq!(
            state
                .resolve_tool_route("echo")
                .map(|route| route.target_role.as_str()),
            Some("tool.echo")
        );
    }

    #[test]
    fn allowed_incarnations_define_visible_tools_and_routes() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.bindings.allowed_tool_runner_incarnations = vec![
            ToolRunnerIncarnationBinding {
                incarnation_id: "tool-echo-remote".into(),
                runner_id: Some("tool-runner-remote".into()),
                hotel_id: Some("remote-hotel".into()),
                environment_id: Some("env://remote".into()),
                target_node: Some("remote-hotel".into()),
                target_role: Some("tool.echo".into()),
                supported_tools: vec!["echo".into()],
                execution_mode: "capability".into(),
                availability_state: "materialization_required".into(),
                selection_hint: Some("remote_fallback".into()),
            },
            ToolRunnerIncarnationBinding {
                incarnation_id: "tool-echo-local".into(),
                runner_id: Some("tool-runner-local".into()),
                hotel_id: Some("local-aiua-01".into()),
                environment_id: Some("env://local".into()),
                target_node: Some("local-aiua-01".into()),
                target_role: Some("tool.echo".into()),
                supported_tools: vec!["echo".into()],
                execution_mode: "capability".into(),
                availability_state: "live".into(),
                selection_hint: Some("local_live_preferred".into()),
            },
        ];
        state.rebuild_default_tool_assembly();

        assert!(state.tool_is_enabled("echo"));
        let route = state
            .resolve_tool_route("echo")
            .expect("echo route should be assembled");
        assert_eq!(route.incarnation_id.as_deref(), Some("tool-echo-local"));
        assert_eq!(route.hotel_id.as_deref(), Some("local-aiua-01"));
        assert_eq!(
            route.selection_reason.as_deref(),
            Some("local_live_preferred")
        );
    }

    #[test]
    fn preferred_environment_overrides_live_local_fallback() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.bindings.preferred_environment_id = Some("env://remote".into());
        state.bindings.allowed_tool_runner_incarnations = vec![
            ToolRunnerIncarnationBinding {
                incarnation_id: "tool-echo-local".into(),
                runner_id: Some("tool-runner-local".into()),
                hotel_id: Some("local-aiua-01".into()),
                environment_id: Some("env://local".into()),
                target_node: Some("local-aiua-01".into()),
                target_role: Some("tool.echo".into()),
                supported_tools: vec!["echo".into()],
                execution_mode: "capability".into(),
                availability_state: "live".into(),
                selection_hint: Some("local_live_preferred".into()),
            },
            ToolRunnerIncarnationBinding {
                incarnation_id: "tool-echo-remote".into(),
                runner_id: Some("tool-runner-remote".into()),
                hotel_id: Some("remote-hotel".into()),
                environment_id: Some("env://remote".into()),
                target_node: Some("remote-hotel".into()),
                target_role: Some("tool.echo".into()),
                supported_tools: vec!["echo".into()],
                execution_mode: "capability".into(),
                availability_state: "materialization_required".into(),
                selection_hint: Some("remote_fallback".into()),
            },
        ];
        state.rebuild_default_tool_assembly();

        let route = state
            .resolve_tool_route("echo")
            .expect("echo route should be assembled");
        assert_eq!(route.incarnation_id.as_deref(), Some("tool-echo-remote"));
        assert_eq!(route.environment_id.as_deref(), Some("env://remote"));
        assert_eq!(
            route.selection_reason.as_deref(),
            Some("preferred_environment_requires_materialization")
        );
    }

    #[test]
    fn local_agent_tools_get_local_execution_routes() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.clear_tool_bindings();
        state.add_tool_binding("session.status");

        let route = state
            .resolve_tool_route("session.status")
            .expect("local agent tool should have a route");
        assert_eq!(route.execution_mode, "local_agent");
        assert_eq!(route.target_role, "agent");
    }

    #[test]
    fn workspace_tools_get_pinned_execution_routes() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.clear_tool_bindings();
        state.add_tool_binding("workspace.read");
        state.add_tool_binding("workspace.list");
        state.add_tool_binding("workspace.search");
        state.bindings.effective_workspace_ref = Some("workspace://main".into());
        state.bindings.workspace_runner_config = Some(TaskRunnerBaseConfig {
            default_workspace_ref: Some("workspace://policy".into()),
            allowed_tools: Some(vec!["workspace.read".into(), "workspace.search".into()]),
            max_read_bytes: Some(8192),
            max_search_results: Some(25),
        });
        state.rebuild_default_tool_assembly();

        let read_route = state
            .resolve_tool_route("workspace.read")
            .expect("workspace.read route should exist");
        let list_route = state
            .resolve_tool_route("workspace.list")
            .expect("workspace.list route should exist");
        let search_route = state
            .resolve_tool_route("workspace.search")
            .expect("workspace.search route should exist");

        assert_eq!(read_route.execution_mode, "pinned");
        assert_eq!(list_route.execution_mode, "pinned");
        assert_eq!(search_route.execution_mode, "pinned");
        assert_eq!(read_route.task_runner_kind.as_deref(), Some("workspace"));
        assert_eq!(list_route.task_runner_kind.as_deref(), Some("workspace"));
        assert_eq!(search_route.task_runner_kind.as_deref(), Some("workspace"));
        assert_eq!(
            read_route
                .task_runner_config
                .as_ref()
                .and_then(|config| config.default_workspace_ref.as_deref()),
            Some("workspace://policy")
        );
        assert_eq!(
            read_route
                .task_runner_config
                .as_ref()
                .and_then(|config| config.max_read_bytes),
            Some(8192)
        );
        assert_eq!(read_route.target_role, "tool.workspace.read");
        assert_eq!(list_route.target_role, "tool.workspace.list");
        assert_eq!(search_route.target_role, "tool.workspace.search");
    }

    #[test]
    fn conversational_turns_project_no_tools_by_default() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.add_tool_binding("echo");

        let projected = state.project_tools_for_turn("What do you think about this architecture?");
        assert!(projected.is_empty());
    }

    #[test]
    fn gratitude_turns_project_no_tools_by_default() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.add_tool_binding("subagent.spawn");
        state.add_tool_binding("echo");

        let projected = state.project_tools_for_turn(
            "Thanks Bjork, I really appreciate it. Looks like you're working pretty well now.",
        );
        assert!(projected.is_empty());
    }

    #[test]
    fn quoted_gratitude_turns_project_no_tools_by_default() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.add_tool_binding("subagent.spawn");

        let projected = state.project_tools_for_turn(
            "\"Thanks Bjork, I really appreciate it. Looks like you're working pretty well now.\"",
        );
        assert!(projected.is_empty());
    }

    #[test]
    fn explicit_tool_mentions_project_matching_tools() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.clear_tool_bindings();
        state.add_tool_binding("echo");
        state.add_tool_binding("session.status");

        let projected = state.project_tools_for_turn("use echo hello there");
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].tool_name, "echo");
    }

    #[test]
    fn ingress_envelope_projects_no_tools() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.add_tool_binding("echo");
        state.add_tool_binding("workspace.read");

        let projected = state.project_tools_for_envelope(
            "Please transcribe this voice note.",
            TurnContextEnvelopeKind::Ingress,
        );
        assert!(projected.is_empty());
    }

    #[test]
    fn low_intent_cognitive_response_contract_skips_plan_and_memory_channels() {
        let state = SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());

        let response_contract =
            state.cognitive_response_contract("Thanks, that was exactly what I needed.", true);
        let channels = response_contract["channels"]
            .as_array()
            .expect("channels array");

        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].as_str(), Some("spoken_text"));
    }

    #[test]
    fn low_intent_reentry_context_respects_tool_projection_policy() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.add_tool_binding("echo");
        state.add_tool_binding("workspace.read");
        state.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-thanks".into(),
            chat_id: "123".into(),
            user_content: "Thanks, that helped a lot.".into(),
            final_reply_to: "local-aiua-01".into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: None,
            phase: TurnPhase::WaitingModel,
            iteration: 1,
            pending_tool_call: None,
            pending_approval: None,
            working_tool_history: Vec::new(),
            recalled_memories: Vec::new(),
            active_plan: None,
            consecutive_step_failures: 0,
            provider_repair_note: None,
            provider_repair_attempts: 0,
            pending_text_reply: None,
            had_voice_input: false,
            turn_routing_plan: None,
            awaiting_transcription_reentry: false,
            scripted_loop_context: None,
        });

        let (_, _, _, projected_tools) = state
            .build_reentry_context_envelope()
            .expect("reentry envelope should exist");

        assert!(
            projected_tools.is_empty(),
            "low-intent reentry should not re-expose the full bound toolset"
        );
    }

    #[test]
    fn skill_and_workspace_bindings_can_be_mutated() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.add_skill_binding("planning");
        state.set_workspace_binding("workspace://main");
        assert_eq!(state.bindings.effective_skillset, vec!["planning"]);
        assert_eq!(
            state.bindings.effective_workspace_ref.as_deref(),
            Some("workspace://main")
        );

        state.clear_skill_bindings();
        state.clear_workspace_binding();
        assert!(state.bindings.effective_skillset.is_empty());
        assert!(state.bindings.effective_workspace_ref.is_none());
    }

    #[test]
    fn checkpoint_memory_type_is_session_scoped() {
        assert_eq!(
            session_checkpoint_memory_type("telegram:123:agent-jane-01"),
            "short_session:telegram:123:agent-jane-01"
        );
    }

    #[test]
    fn session_index_tracks_multiple_sessions_without_duplicates() {
        let mut first =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        first.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-1".into(),
            chat_id: "123".into(),
            user_content: "hello".into(),
            final_reply_to: "local-aiua-01".into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: None,
            phase: TurnPhase::Queued,
            iteration: 0,
            pending_tool_call: None,
            pending_approval: None,
            working_tool_history: Vec::new(),
            recalled_memories: Vec::new(),
            active_plan: None,
            consecutive_step_failures: 0,
            provider_repair_note: None,
            provider_repair_attempts: 0,
            pending_text_reply: None,
            had_voice_input: false,
            turn_routing_plan: None,
            awaiting_transcription_reentry: false,
            scripted_loop_context: None,
        });
        let index = merge_session_index(None, &first);
        assert_eq!(index["active_sessions"].as_array().unwrap().len(), 1);

        let second = SessionState::new("sess-2".into(), "agent-jane-01".into(), "telegram".into());
        let index = merge_session_index(Some(&index), &second);
        assert_eq!(index["active_sessions"].as_array().unwrap().len(), 2);

        let index = merge_session_index(Some(&index), &first);
        let sessions = index["active_sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 2);
        let session_ids = sessions
            .iter()
            .filter_map(|entry| entry.get("session_id").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(session_ids.iter().filter(|id| **id == "sess-1").count(), 1);
        assert_eq!(session_ids.iter().filter(|id| **id == "sess-2").count(), 1);
    }

    #[test]
    fn tool_history_accumulates_and_survives_checkpoint_round_trip() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-1".into(),
            chat_id: "123".into(),
            user_content: "list workspace files".into(),
            final_reply_to: "local-aiua-01".into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: None,
            phase: TurnPhase::WaitingTool,
            iteration: 1,
            pending_tool_call: None,
            pending_approval: None,
            working_tool_history: Vec::new(),
            recalled_memories: Vec::new(),
            active_plan: None,
            consecutive_step_failures: 0,
            provider_repair_note: None,
            provider_repair_attempts: 0,
            pending_text_reply: None,
            had_voice_input: false,
            turn_routing_plan: None,
            awaiting_transcription_reentry: true,
            scripted_loop_context: None,
        });

        state.push_tool_history(
            ToolCall {
                tool_name: "workspace.list".into(),
                arguments: serde_json::json!({}),
            },
            ToolResult {
                tool_name: "workspace.list".into(),
                content: "main.rs\nlib.rs".into(),
            },
        );

        assert_eq!(
            state
                .active_turn
                .as_ref()
                .unwrap()
                .working_tool_history
                .len(),
            1
        );

        // Round-trip through checkpoint
        let checkpoint = state.checkpoint_json();
        let restored = SessionState::from_checkpoint(&checkpoint).unwrap();
        let active_turn = restored.active_turn.unwrap();
        let history = &active_turn.working_tool_history;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].0.tool_name, "workspace.list");
        assert_eq!(history[0].1.content, "main.rs\nlib.rs");
        assert!(active_turn.awaiting_transcription_reentry);
    }

    #[test]
    fn reentry_prompt_includes_tool_call_history() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-1".into(),
            chat_id: "123".into(),
            user_content: "read the README".into(),
            final_reply_to: "local-aiua-01".into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: None,
            phase: TurnPhase::WaitingTool,
            iteration: 1,
            pending_tool_call: None,
            pending_approval: None,
            working_tool_history: Vec::new(),
            recalled_memories: Vec::new(),
            active_plan: None,
            consecutive_step_failures: 0,
            provider_repair_note: None,
            provider_repair_attempts: 0,
            pending_text_reply: None,
            had_voice_input: false,
            turn_routing_plan: None,
            awaiting_transcription_reentry: false,
            scripted_loop_context: None,
        });

        state.push_tool_history(
            ToolCall {
                tool_name: "workspace.read".into(),
                arguments: serde_json::json!({ "path": "README.md" }),
            },
            ToolResult {
                tool_name: "workspace.read".into(),
                content: "# Philotic Stack".into(),
            },
        );

        let prompt = state.build_reentry_prompt().unwrap();
        assert!(
            prompt.contains("[Tool call history]"),
            "prompt should contain history section"
        );
        assert!(
            prompt.contains("workspace.read"),
            "prompt should name the tool"
        );
        assert!(
            prompt.contains("# Philotic Stack"),
            "prompt should contain result content"
        );
        assert!(prompt.contains("Call 1:"), "prompt should number the calls");
    }

    #[test]
    fn reentry_prompt_returns_none_without_active_turn() {
        let state = SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        assert!(state.build_reentry_prompt().is_none());
    }

    #[test]
    fn prepare_transcription_reentry_turns_transcript_into_normal_user_turn() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-voice-1".into(),
            chat_id: "123".into(),
            user_content: "User sent a Telegram voice message.".into(),
            final_reply_to: "local-aiua-01".into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: None,
            phase: TurnPhase::Thinking,
            iteration: 1,
            pending_tool_call: None,
            pending_approval: None,
            working_tool_history: Vec::new(),
            recalled_memories: Vec::new(),
            active_plan: None,
            consecutive_step_failures: 0,
            provider_repair_note: None,
            provider_repair_attempts: 0,
            pending_text_reply: None,
            had_voice_input: true,
            turn_routing_plan: None,
            awaiting_transcription_reentry: true,
            scripted_loop_context: None,
        });

        let reentry = state
            .prepare_transcription_reentry("Please review the current architecture.")
            .expect("transcript should produce a re-entry plan");

        assert_eq!(
            reentry.user_content,
            "Please review the current architecture."
        );
        assert!(
            reentry
                .prompt
                .contains("Please review the current architecture."),
            "prompt should be rebuilt from the transcript"
        );

        let active_turn = state.active_turn.as_ref().expect("turn should still exist");
        assert_eq!(
            active_turn.user_content,
            "Please review the current architecture."
        );
        assert_eq!(active_turn.phase, TurnPhase::WaitingModel);
        assert_eq!(active_turn.iteration, 2);
        assert!(!active_turn.awaiting_transcription_reentry);
    }

    #[test]
    fn ingress_context_envelope_trims_memory_and_tool_history() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.recent_turns.push(TurnRecord {
            turn_id: "turn-prev".into(),
            user_content: "Previous question".into(),
            assistant_content: Some("Previous answer".into()),
            created_at: 0,
        });
        state.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-voice-2".into(),
            chat_id: "123".into(),
            user_content: "Please transcribe this voice note.".into(),
            final_reply_to: "local-aiua-01".into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: None,
            phase: TurnPhase::WaitingModel,
            iteration: 1,
            pending_tool_call: None,
            pending_approval: None,
            working_tool_history: vec![(
                ToolCall {
                    tool_name: "workspace.read".into(),
                    arguments: serde_json::json!({"path":"README.md"}),
                },
                ToolResult {
                    tool_name: "workspace.read".into(),
                    content: "# Philotic".into(),
                },
            )],
            recalled_memories: vec![RecalledMemoryRecord {
                concept: "voice policy".into(),
                content: "Mirror voice turns back as audio.".into(),
                tags: vec!["voice".into()],
            }],
            active_plan: None,
            consecutive_step_failures: 0,
            provider_repair_note: None,
            provider_repair_attempts: 0,
            pending_text_reply: None,
            had_voice_input: true,
            turn_routing_plan: None,
            awaiting_transcription_reentry: true,
            scripted_loop_context: None,
        });

        let (_, context, _) = state.model_request_payloads_for_envelope(
            "Please transcribe this voice note.",
            &[],
            TurnContextEnvelopeKind::Ingress,
        );

        assert_eq!(
            context["memory"].as_array().map(|items| items.len()),
            Some(0),
            "ingress envelope should not carry long-form memory"
        );
        assert_eq!(
            context["recalled_memory"]
                .as_array()
                .map(|items| items.len()),
            Some(0),
            "ingress envelope should not carry recalled memory"
        );
        assert_eq!(
            context["tool_history"].as_array().map(|items| items.len()),
            Some(0),
            "ingress envelope should not carry tool history"
        );
        assert_eq!(
            context["dialogue_window"]
                .as_array()
                .map(|items| items.len()),
            Some(2),
            "ingress envelope should keep only a minimal recent dialogue window"
        );
        assert_eq!(
            context["active_turn"]["text"].as_str(),
            Some("Please transcribe this voice note.")
        );
    }

    // ── Role handoff full-cycle smoke tests ──────────────────────────────────

    fn make_role_activation(role_name: &str) -> RoleActivation {
        RoleActivation {
            role_name: role_name.into(),
            activation_reason: "test_handoff".into(),
            requested_by: Some("orchestrator".into()),
            ..Default::default()
        }
    }

    /// Applying a handoff bundle sets role_activation and stashes the summary.
    #[test]
    fn handoff_bundle_applies_role_activation_and_summary() {
        let mut state = SessionState::new(
            "sess-handoff".into(),
            "agent-jane-01".into(),
            "telegram".into(),
        );

        state.role_activation = Some(make_role_activation("researcher"));
        state.last_handoff_summary = Some("Analysing dataset drift in experiment B.".into());

        assert_eq!(
            state.role_activation.as_ref().map(|r| r.role_name.as_str()),
            Some("researcher")
        );
        assert_eq!(
            state.last_handoff_summary.as_deref(),
            Some("Analysing dataset drift in experiment B.")
        );
    }

    /// The handoff summary is visible in the session envelope on the first turn.
    #[test]
    fn handoff_summary_appears_in_session_envelope() {
        let mut state = SessionState::new(
            "sess-handoff".into(),
            "agent-jane-01".into(),
            "telegram".into(),
        );

        state.role_activation = Some(make_role_activation("researcher"));
        state.last_handoff_summary = Some("Analysing dataset drift in experiment B.".into());

        let projection = state.build_context_projection("what's the status?");
        let context = state.model_context_from_projection(&projection);
        let instructions = context["instructions"]
            .as_array()
            .expect("instructions must be an array");

        let session_text: String = instructions
            .iter()
            .filter(|item| item["projection_kind"] == "session")
            .filter_map(|item| item["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            session_text.contains("Active role: researcher."),
            "session envelope must include active role"
        );
        assert!(
            session_text.contains("Handoff context: Analysing dataset drift in experiment B."),
            "session envelope must include handoff summary"
        );
    }

    /// After clear_handoff_summary, the next context build omits the summary.
    #[test]
    fn handoff_summary_consumed_after_clear() {
        let mut state = SessionState::new(
            "sess-handoff".into(),
            "agent-jane-01".into(),
            "telegram".into(),
        );

        state.role_activation = Some(make_role_activation("researcher"));
        state.last_handoff_summary = Some("Analysing dataset drift in experiment B.".into());

        // First turn — summary present.
        let projection = state.build_context_projection("what's the status?");
        let context = state.model_context_from_projection(&projection);
        let first_text: String = context["instructions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["projection_kind"] == "session")
            .filter_map(|item| item["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            first_text.contains("Handoff context:"),
            "summary must appear before clear"
        );

        // Runtime clears it after dispatching the first model request.
        state.clear_handoff_summary();

        // Second turn — summary gone.
        let projection2 = state.build_context_projection("follow up");
        let context2 = state.model_context_from_projection(&projection2);
        let second_text: String = context2["instructions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["projection_kind"] == "session")
            .filter_map(|item| item["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !second_text.contains("Handoff context:"),
            "summary must not appear after clear"
        );
    }

    /// handoff_return clears role_activation, leaving no active role.
    #[test]
    fn handoff_return_clears_role_activation() {
        let mut state = SessionState::new(
            "sess-handoff".into(),
            "agent-jane-01".into(),
            "telegram".into(),
        );

        state.role_activation = Some(make_role_activation("researcher"));
        state.last_handoff_summary = Some("Some prior context.".into());

        assert!(state.role_activation.is_some());

        // Simulate handle_handoff_return.
        let previous_role = state.role_activation.as_ref().map(|r| r.role_name.clone());
        state.role_activation = None;

        assert_eq!(previous_role.as_deref(), Some("researcher"));
        assert!(
            state.role_activation.is_none(),
            "role must be cleared after return"
        );

        let projection = state.build_context_projection("hello");
        let context = state.model_context_from_projection(&projection);
        let session_text: String = context["instructions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["projection_kind"] == "session")
            .filter_map(|item| item["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !session_text.contains("Active role:"),
            "no active role after handoff_return"
        );
    }

    /// Full cycle: bundle → summary in context → clear → return → clean context.
    #[test]
    fn full_role_handoff_cycle() {
        let mut state = SessionState::new(
            "sess-cycle".into(),
            "agent-jane-01".into(),
            "telegram".into(),
        );

        // 1. handoff_bundle arrives — role applied, summary stashed.
        state.role_activation = Some(make_role_activation("analyst"));
        state.last_handoff_summary = Some("Focus: revenue anomaly in Q1 data.".into());

        // 2. First model turn — summary is injected.
        let projection = state.build_context_projection("start the analysis");
        let context = state.model_context_from_projection(&projection);
        let session_block = |ctx: &serde_json::Value| -> String {
            ctx["instructions"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|item| item["projection_kind"] == "session")
                .filter_map(|item| item["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        };

        let turn1 = session_block(&context);
        assert!(turn1.contains("Active role: analyst."));
        assert!(turn1.contains("Handoff context: Focus: revenue anomaly in Q1 data."));

        // 3. Runtime clears the one-shot summary.
        state.clear_handoff_summary();

        // 4. Second model turn — role still active, summary gone.
        let projection2 = state.build_context_projection("continue");
        let context2 = state.model_context_from_projection(&projection2);
        let turn2 = session_block(&context2);
        assert!(turn2.contains("Active role: analyst."));
        assert!(!turn2.contains("Handoff context:"));

        // 5. handoff_return arrives — role cleared.
        state.role_activation = None;

        // 6. Post-return turn — no role, no summary.
        let projection3 = state.build_context_projection("back to normal");
        let context3 = state.model_context_from_projection(&projection3);
        let turn3 = session_block(&context3);
        assert!(!turn3.contains("Active role:"));
        assert!(!turn3.contains("Handoff context:"));
    }

    #[test]
    fn recalled_memory_projects_into_distinct_context_section() {
        let mut state = SessionState::new(
            "sess-memory".into(),
            "agent-jane-01".into(),
            "telegram".into(),
        );
        state.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-memory".into(),
            chat_id: "chat-memory".into(),
            user_content: "continue the memory work".into(),
            final_reply_to: "local-aiua-01".into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: None,
            phase: TurnPhase::Queued,
            iteration: 0,
            pending_tool_call: None,
            pending_approval: None,
            working_tool_history: Vec::new(),
            recalled_memories: vec![RecalledMemoryRecord {
                concept: "memory-architecture".into(),
                content: "User prefers deterministic bounded recall over broad automatic dumps."
                    .into(),
                tags: vec!["memory".into(), "preference".into()],
            }],
            active_plan: None,
            consecutive_step_failures: 0,
            provider_repair_note: None,
            provider_repair_attempts: 0,
            pending_text_reply: None,
            had_voice_input: false,
            turn_routing_plan: None,
            awaiting_transcription_reentry: false,
            scripted_loop_context: None,
        });

        let projection = state.build_context_projection("continue the memory work");
        let context = state.model_context_from_projection(&projection);
        let recalled = context["recalled_memory"]
            .as_array()
            .expect("recalled_memory must be an array");

        assert_eq!(recalled.len(), 1);
        let text = recalled[0]["text"]
            .as_str()
            .expect("recalled memory entry should render text");
        assert!(text.contains("[Recalled memory]"));
        assert!(text.contains("memory-architecture"));
    }
}
