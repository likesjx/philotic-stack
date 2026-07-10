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
    /// Per-source injection budget usage for this assembly. See `InjectionBudget`.
    #[serde(default)]
    pub budget_ledger: BudgetLedger,
    /// `total_used / InjectionBudget.total_envelope_chars`, clamped to 100.
    /// Fires `ReflexEvent::ContextPressure { used_pct: context_pressure_pct }`
    /// at the live turn-assembly call site (see `runtime.rs`).
    #[serde(default)]
    pub context_pressure_pct: u8,
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
    /// Content-filtering posture for this role — `"unrestricted"` | `"standard"`
    /// | `"strict"`. `None` here means "not loaded" (e.g. a test fixture or a
    /// role fetched before this field existed); [`SessionState::effective_content_policy`]
    /// treats that the same as `"standard"`. Populated from the role incarnation
    /// record's `content_policy` at activation time (see `roles.rs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_policy: Option<String>,
}

/// A single step inside an `ActivePlan`. Tracks the description, optional bound
/// tool, and lifecycle status of the step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// A plan carried across turns by the plan-eval-repeat loop.
///
/// Created when a turn completes (final Respond) with an `ActivePlan` whose
/// steps are not all done. Persisted on `SessionState` (checkpointed with
/// backward-compatible serde defaults) and consumed to synthesize budgeted
/// auto-continuation turns until the plan completes, blocks, or the budget
/// is exhausted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CarryoverPlan {
    /// The plan as last seen on the completed turn.
    pub plan: ActivePlan,
    /// Index-aligned per-step completion mirror from the last plan eval.
    pub steps_done: Vec<bool>,
    /// Number of continuation turns already synthesized for this plan.
    #[serde(default)]
    pub continuations_used: u32,
    /// The turn that originally produced this carryover.
    pub created_turn_id: String,
}

impl CarryoverPlan {
    pub fn steps_done_count(&self) -> usize {
        self.steps_done.iter().filter(|d| **d).count()
    }
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
pub struct MemorySpacetimeFrame {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_from: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_until: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_verified_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temporal_kind: Option<MemoryTemporalKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spatial_scope: Option<MemorySpatialScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotel_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<MemoryAuthority>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_level: Option<MemoryValidationLevel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTemporalKind {
    Event,
    State,
    Preference,
    Rule,
    Decision,
    Hypothesis,
    Gap,
    Checkpoint,
}

impl MemoryTemporalKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::State => "state",
            Self::Preference => "preference",
            Self::Rule => "rule",
            Self::Decision => "decision",
            Self::Hypothesis => "hypothesis",
            Self::Gap => "gap",
            Self::Checkpoint => "checkpoint",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySpatialScope {
    #[serde(rename = "self")]
    SelfScope,
    Agent,
    User,
    Session,
    Workspace,
    Hotel,
    Mesh,
    Global,
}

impl MemorySpatialScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SelfScope => "self",
            Self::Agent => "agent",
            Self::User => "user",
            Self::Session => "session",
            Self::Workspace => "workspace",
            Self::Hotel => "hotel",
            Self::Mesh => "mesh",
            Self::Global => "global",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAuthority {
    ObservedRuntime,
    ObservedRepo,
    GraphStructured,
    UserStated,
    VerifiedMemory,
    InferredMemory,
    External,
    Untrusted,
}

impl MemoryAuthority {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ObservedRuntime => "observed_runtime",
            Self::ObservedRepo => "observed_repo",
            Self::GraphStructured => "graph_structured",
            Self::UserStated => "user_stated",
            Self::VerifiedMemory => "verified_memory",
            Self::InferredMemory => "inferred_memory",
            Self::External => "external",
            Self::Untrusted => "untrusted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MemoryValidationLevel {
    Unverified,
    CheckGreen,
    TestGreen,
    SmokeGreen,
    WatchedLiveGreen,
}

impl MemoryValidationLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::CheckGreen => "check-green",
            Self::TestGreen => "test-green",
            Self::SmokeGreen => "smoke-green",
            Self::WatchedLiveGreen => "watched-live-green",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct GraphAnchors {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seam_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_doc: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected_nodes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MemoryShapingContext {
    pub frame: MemorySpacetimeFrame,
    #[serde(default)]
    pub graph_anchors: GraphAnchors,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recall_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promotion_policy: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spacetime_frame: Option<MemorySpacetimeFrame>,
}

/// One cached LifeGraph prefetch result, keyed by named recall strategy.
///
/// Populated out-of-band by the fire-and-forget `life.recall` prefetch lane
/// (session load + after each completed turn) and injected into
/// `WorkingTurn::recalled_memories` at turn start when fresh. Persisted in the
/// session checkpoint so a restart keeps the last known graph context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LifeRecallCacheEntry {
    /// Named recall strategy that produced this packet
    /// (e.g. `re_entry_context`, `open_loops_by_context`).
    pub strategy: String,
    /// Unix timestamp (seconds) when the packet arrived from the runner.
    pub fetched_at: u64,
    /// The query text the prefetch was issued with (for observability).
    #[serde(default)]
    pub query_text: String,
    /// Already-mapped recalled-memory records ready for turn injection.
    #[serde(default)]
    pub records: Vec<RecalledMemoryRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParacrineThreadStatus {
    Open,
    Completed,
    Superseded,
    Cancelled,
    Expired,
    Escalated,
    /// Terminal: the delegation chain was closed because it exhausted its
    /// cross-hop budget (hop count or cumulative wall-clock).
    BudgetExhausted,
}

impl ParacrineThreadStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Completed => "completed",
            Self::Superseded => "superseded",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
            Self::Escalated => "escalated",
            Self::BudgetExhausted => "budget_exhausted",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParacrineThread {
    pub id: String,
    pub origin_turn_id: String,
    pub role: String,
    pub goal: String,
    pub status: ParacrineThreadStatus,
    pub routing: philotic_client::ParacrineRouting,
    pub authority: String,
    pub tool_policy: String,
    pub approval_scope: String,
    pub opened_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_signal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<String>,
}

/// Where a turn's model tier/provider selection originated. Drives whether a
/// no-response failure is allowed to escalate to the next fallback tier or
/// must fail fast instead: an operator-pinned session (`/model <tier>`) means
/// silent auto-fallback would violate the operator's explicit intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SelectionSource {
    /// The operator explicitly pinned this turn to a tier via `/model <tier>`.
    /// Fallback escalation is disabled — a no-response failure fails the turn.
    OperatorExplicit,
    /// No pin or special routing in effect; the session's configured default
    /// tier/provider was used.
    #[default]
    ConfiguredDefault,
    /// The turn was triggered by a cron job (`InboundTaskPayload.cron_job_id`).
    CronPrimary,
    /// The turn's provider was chosen by automatic fallback escalation after
    /// a prior tier failed.
    AutoFallback,
}

/// Narrow persisted fallback override (Slice 2 of Model Failover Layers).
///
/// Today a fallback tier lives only on the active turn (`WorkingTurn::fallback_tier`)
/// and resets on the next turn, so every new user message re-probes a known-bad
/// primary. `FallbackOverride` sticks a session to the tier that last worked and
/// tracks when to health-check the origin tier again — see
/// `advance_turn_to_next_fallback_tier` (writes/updates it on a successful
/// escalation), `resolve_model_execution_target` (routes new turns to
/// `active_tier_role` while it is set), and `turn_loop::probe_degraded_sessions`
/// (clears it once the origin tier answers again).
///
/// Owns ONLY these fields — clearing it (on `/role`, `/model` pin changes, and
/// session reset paths) must never touch other session state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackOverride {
    /// The tier role the session would use with no override in effect (the
    /// primary/tier-0 resolution at the time the override was first created).
    pub origin_tier_role: String,
    /// The tier role fallback escalation landed on most recently.
    pub active_tier_role: String,
    /// Short cause string for the failure that (most recently) drove escalation
    /// (e.g. `"provider_failure"`, `"provider_contract_failure"`, `"model_timeout"`).
    pub reason: String,
    /// Epoch millis when this override was first created. Stable across
    /// subsequent escalations on the same degraded session.
    pub since_epoch_ms: u64,
    /// Epoch millis of the last origin-tier probe attempt (or override
    /// creation, before any probe has fired). Advances every eligible tick
    /// regardless of probe outcome so the 300s cadence is honored.
    pub last_probe_epoch_ms: u64,
    /// Latch reserved for Slice 3's membrane recovery/degradation notices.
    /// This slice only persists it — no rendering happens here.
    pub notice_sent: bool,
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
    /// How the originating philote should handle the specialist's response.
    /// Captured from the inbound exosome and echoed back on `paracrine_response`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paracrine_response_routing: Option<philotic_client::ParacrineRouting>,
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
    /// True once a configured role ladder tier has actually been *dispatched*
    /// as a fallback escalation (as opposed to `fallback_tier == 0` meaning
    /// "primary dispatch, ladder never consulted"). `fallback_tier == 0` is
    /// ambiguous on its own — it means both "virgin primary" and "ladder
    /// tier 0 was just (re-)dispatched after the primary bypassed the
    /// ladder" (see `resolve_model_execution_target`'s precedence). This flag
    /// disambiguates the two so `advance_turn_to_next_fallback_tier`'s walk
    /// advances past tier 0 instead of re-dispatching it forever. Always
    /// `false` for a fresh turn; never restored across a restart (only
    /// `WaitingTool` turns survive checkpoint restore, and this only matters
    /// mid-`WaitingModel`).
    #[serde(default)]
    pub ladder_tier0_dispatched: bool,
    /// Where this turn's tier/provider selection came from. `OperatorExplicit`
    /// (session pinned via `/model <tier>`) disables automatic fallback
    /// escalation on a no-response failure — see `advance_turn_to_next_fallback_tier`.
    #[serde(default)]
    pub selection_source: SelectionSource,
    /// Number of same-tier retries attempted for streaming_timeout errors.
    /// Allows one automatic retry before escalating to the next fallback tier.
    #[serde(default)]
    pub streaming_retry_attempts: u8,
    /// Running accumulation of streaming tokens for this turn. The model-router
    /// emits `streaming_token` tasks carrying individual SSE deltas; membrane's
    /// draft edit expects the *cumulative* text (it replaces the draft message),
    /// so tokens are appended here and the running total is what gets emitted as
    /// `partial_reply`. Reset implicitly per turn (new `WorkingTurn` starts empty).
    #[serde(default)]
    pub streamed_content: String,
    /// Number of paracrine delegation hops this turn's chain has traversed.
    /// A hop is one `delegate.whisper` -> specialist -> `EnrichedToolResult`
    /// re-entry. The per-hop re-entry resets `iteration` to 0 (the turn was
    /// blocked, not thinking), so this is the only cross-hop bound on runaway
    /// orchestrator<->specialist ping-pong. `#[serde(default)]` so checkpoints
    /// written before this field deserialize to 0.
    #[serde(default)]
    pub paracrine_hop_count: u32,
    /// Unix-seconds timestamp of the first paracrine hop in this turn's
    /// delegation chain. `None` until the first hop is charged; used to enforce
    /// the cumulative wall-clock budget across hops. `#[serde(default)]` so old
    /// checkpoints deserialize to `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paracrine_chain_started_at: Option<u64>,
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
    /// Maximum number of tool call entries included in the history for a single
    /// turn. Oldest entries are dropped first; a compact note is prepended when
    /// any are dropped. Prevents unbounded context growth on long agentic turns.
    /// Default: 15, min: 3, max: 100.
    #[serde(default = "default_max_tool_history_entries")]
    pub max_tool_history_entries: usize,
}

fn default_max_tool_result_chars() -> usize {
    8_192
}

fn default_max_tool_history_entries() -> usize {
    15
}

impl Default for ContextWindowPolicy {
    fn default() -> Self {
        Self {
            dialogue_window_minutes: 10,
            dialogue_window_chars: 10_000,
            include_tool_calls: true,
            max_tool_result_chars: 8_192,
            max_tool_history_entries: 15,
        }
    }
}

impl ContextWindowPolicy {
    /// Apply any per-role context-window overrides carried on a role's
    /// [`TurnLoopConfig`](ansible_mesh_core::graph::TurnLoopConfig). Absent
    /// overrides leave the current values untouched; present values are clamped
    /// to the documented safe ranges, mirroring the `iteration_cap.clamp(1, 50)`
    /// precedent so a role record cannot push the session outside sane bounds.
    ///
    /// This mutates in place, so callers snapshot the session baseline first and
    /// revert to it on handoff-return (see `SessionState::apply_role_context_window`).
    pub fn apply_overrides(&mut self, ov: &ansible_mesh_core::graph::ContextWindowOverrides) {
        if let Some(minutes) = ov.dialogue_window_minutes {
            self.dialogue_window_minutes = minutes.clamp(2, 60);
        }
        if let Some(chars) = ov.dialogue_window_chars {
            self.dialogue_window_chars = chars.clamp(1_000, 50_000);
        }
        if let Some(chars) = ov.max_tool_result_chars {
            self.max_tool_result_chars = chars.clamp(1_000, 500_000);
        }
        if let Some(entries) = ov.max_tool_history_entries {
            self.max_tool_history_entries = entries.clamp(3, 100);
        }
    }
}

/// Hard per-source character caps for prompt-injected content, with visible
/// usage. Model-independent (chars, not tokens), same shape as Hermes'
/// `memory_char_limit` / `user_char_limit`. `dialogue_window_chars` stays
/// owned by `ContextWindowPolicy` — it is not duplicated here.
///
/// Slice 0 applies `persona_chars`, `rules_chars`, and `recalled_memory_chars`
/// to the layers that exist today (`project_agent_self`, `project_rules`,
/// `project_recalled_memory`). `memory_snapshot_chars`, `skills_chars`, and
/// `reflex_snapshot_chars` are reserved for the frozen `MemorySnapshot` and
/// tiered skill/reflex projections landing in later slices — no renderer
/// consumes them yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InjectionBudget {
    /// Cap for the Identity layer (identity_text + soul_text + role governance).
    pub persona_chars: usize,
    /// Reserved for the slice-2 frozen `MemorySnapshot` block. Unused today.
    pub memory_snapshot_chars: usize,
    /// Cap for the per-turn RecalledMemory layer (`project_recalled_memory`).
    pub recalled_memory_chars: usize,
    /// Reserved for a future skills/role-manifest index projection. Unused today.
    pub skills_chars: usize,
    /// Reserved for a future reflex-state projection. Unused today.
    pub reflex_snapshot_chars: usize,
    /// Cap for the Rules layer (`project_rules`).
    pub rules_chars: usize,
    /// Whole-envelope ceiling (sum of all rendered layer chars). Drives
    /// `ReflexEvent::ContextPressure` emission — see `BudgetLedger`.
    pub total_envelope_chars: usize,
}

impl Default for InjectionBudget {
    fn default() -> Self {
        Self {
            persona_chars: 6_000,
            memory_snapshot_chars: 4_000,
            recalled_memory_chars: 3_000,
            skills_chars: 2_500,
            reflex_snapshot_chars: 1_000,
            rules_chars: 2_000,
            total_envelope_chars: 60_000,
        }
    }
}

/// One budgeted source's usage against its cap for a single assembled turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetEntry {
    pub source: String,
    pub used_chars: usize,
    pub cap_chars: usize,
    pub truncated: bool,
}

impl BudgetEntry {
    /// Usage percentage against the cap. Deliberately not clamped to 100 —
    /// this is the diagnostic value shown in `/context`, where an honest
    /// over-100% reading is more useful than a capped one. Uses u128
    /// intermediate math so pathological caps/usage cannot overflow.
    pub fn pct(&self) -> u32 {
        if self.cap_chars == 0 {
            return 100;
        }
        ((self.used_chars as u128 * 100) / self.cap_chars as u128).min(u32::MAX as u128) as u32
    }
}

/// Per-turn usage ledger for every budgeted context source, in assembly order.
/// Surfaced by `/context` (`context_breakdown_text`) so the agent and operator
/// can see how close any section is to its cap before it silently truncates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct BudgetLedger {
    pub entries: Vec<BudgetEntry>,
}

impl BudgetLedger {
    pub fn any_truncated(&self) -> bool {
        self.entries.iter().any(|e| e.truncated)
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
    /// Maximum age (seconds) of a cached LifeGraph prefetch packet before it is
    /// considered stale and skipped at turn-start injection.
    /// Default: 1800 (30 minutes). Staleness-by-one-turn is intended: the cache
    /// is refreshed out-of-band, never fetched synchronously at turn start.
    #[serde(default = "default_life_recall_max_age_secs")]
    pub life_recall_max_age_secs: u64,
}

pub(crate) fn default_life_recall_max_age_secs() -> u64 {
    1800
}

impl Default for MemoryPolicy {
    fn default() -> Self {
        Self {
            memory_window_size: 10,
            long_term_recall_enabled: true,
            recall_limit: 5,
            life_recall_max_age_secs: default_life_recall_max_age_secs(),
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
    /// Maximum number of paracrine delegation hops (whisper -> specialist ->
    /// re-entry) allowed in a single turn's delegation chain before the turn is
    /// failed. Bounds runaway orchestrator<->specialist ping-pong that the
    /// per-hop iteration reset would otherwise leave unchecked. Default: 5.
    /// Serde default so older serialized `ExecutionPolicy` values load.
    #[serde(default = "default_paracrine_hop_budget")]
    pub paracrine_hop_budget: u32,
    /// Cumulative wall-clock budget (seconds) for a turn's paracrine delegation
    /// chain, measured from the first hop. Default: 900 (15 minutes).
    /// Serde default so older serialized `ExecutionPolicy` values load.
    #[serde(default = "default_paracrine_chain_budget_secs")]
    pub paracrine_chain_budget_secs: u64,
}

fn default_paracrine_hop_budget() -> u32 {
    5
}

fn default_paracrine_chain_budget_secs() -> u64 {
    900
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            iteration_cap: 10,
            plan_required_on_skill: true,
            stream_tool_events: true,
            stall_detection_threshold: 3,
            paracrine_hop_budget: default_paracrine_hop_budget(),
            paracrine_chain_budget_secs: default_paracrine_chain_budget_secs(),
        }
    }
}

impl ExecutionPolicy {
    /// Apply any paracrine-budget overrides carried on a role's
    /// [`TurnLoopConfig`](ansible_mesh_core::graph::TurnLoopConfig). Absent
    /// overrides leave the current (default) values untouched. Called at the
    /// same sites that apply `iteration_cap`, since settings are re-derived from
    /// the role activation rather than persisted in the checkpoint.
    pub fn apply_paracrine_overrides(&mut self, tlc: &ansible_mesh_core::graph::TurnLoopConfig) {
        if let Some(budget) = tlc.paracrine_hop_budget {
            self.paracrine_hop_budget = budget;
        }
        if let Some(secs) = tlc.paracrine_chain_budget_secs {
            self.paracrine_chain_budget_secs = secs;
        }
    }
}

/// Outcome of charging one paracrine delegation hop against a turn's budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParacrineBudgetOutcome {
    /// The hop was charged and the chain is still within both budgets.
    WithinBudget,
    /// The chain has exceeded its hop-count budget.
    HopsExhausted { hops: u32, budget: u32 },
    /// The chain has exceeded its cumulative wall-clock budget.
    TimeExhausted { elapsed_secs: u64, budget_secs: u64 },
}

/// Charge one paracrine delegation hop against `turn`, mutating its hop count
/// and (on the first hop) its chain start timestamp, then report whether either
/// budget in `exec` is now exhausted.
///
/// Semantics: `paracrine_hop_budget` hops are permitted; the `budget + 1`-th
/// hop breaches. Time breaches when elapsed since the first hop reaches the
/// budget. Hop breaches take precedence in reporting when both trip.
pub fn charge_paracrine_hop(
    turn: &mut WorkingTurn,
    exec: &ExecutionPolicy,
    now: u64,
) -> ParacrineBudgetOutcome {
    if turn.paracrine_chain_started_at.is_none() {
        turn.paracrine_chain_started_at = Some(now);
    }
    turn.paracrine_hop_count = turn.paracrine_hop_count.saturating_add(1);
    let started = turn.paracrine_chain_started_at.unwrap_or(now);
    let elapsed = now.saturating_sub(started);

    if turn.paracrine_hop_count > exec.paracrine_hop_budget {
        ParacrineBudgetOutcome::HopsExhausted {
            hops: turn.paracrine_hop_count,
            budget: exec.paracrine_hop_budget,
        }
    } else if elapsed >= exec.paracrine_chain_budget_secs {
        ParacrineBudgetOutcome::TimeExhausted {
            elapsed_secs: elapsed,
            budget_secs: exec.paracrine_chain_budget_secs,
        }
    } else {
        ParacrineBudgetOutcome::WithinBudget
    }
}

// ── Paracrine wire-payload succinctness budgets ─────────────────────────────
//
// Whisper prompts, merge summaries, and handoff context excerpts are all
// model-authored free text that crosses the IPC/mesh wire and lands verbatim
// in another philote's context window. None of them need to be long: a
// whisper is a brief (goal / context / constraints / expected return), a
// merge is a distilled answer, and a handoff excerpt is orientation — the
// durable truth stays in the session checkpoint and memory. These caps stop
// a verbose model from dumping its whole context across the wire.

/// Maximum characters for a `delegate.whisper` prompt. Longer prompts are
/// truncated with an explicit marker before dispatch.
pub const PARACRINE_WHISPER_PROMPT_MAX_CHARS: usize = 4_000;

/// Maximum characters for a `delegate.merge` content payload (the specialist's
/// distilled answer back to the orchestrator or user).
pub const PARACRINE_MERGE_CONTENT_MAX_CHARS: usize = 6_000;

/// Maximum characters for a `HandoffBundle.context_excerpt` (role handoffs).
pub const HANDOFF_CONTEXT_EXCERPT_MAX_CHARS: usize = 1_200;

/// Truncate `text` to at most `max_chars` characters (char-boundary safe),
/// appending an explicit omission marker when anything was cut. Returns the
/// input unchanged (no reallocation of content) when it already fits.
pub fn truncate_for_wire(text: &str, max_chars: usize) -> String {
    let total_chars = text.chars().count();
    if total_chars <= max_chars {
        return text.to_string();
    }
    let keep: String = text.chars().take(max_chars).collect();
    let omitted = total_chars - max_chars;
    format!("{keep}\n…[truncated: {omitted} chars omitted]")
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
    #[serde(default)]
    pub injection_budget: InjectionBudget,
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
    /// Agent-level content-filtering posture fallback — `"unrestricted"` |
    /// `"standard"` | `"strict"`. Consulted by
    /// [`SessionState::effective_content_policy`] when the active role hasn't
    /// set an explicit (non-`"standard"`) policy of its own, so a single-role
    /// agent (the common case) only needs one place to set this. `None`
    /// (the default) is equivalent to `"standard"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_policy: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SessionBindings {
    #[serde(default)]
    pub effective_toolset: Vec<String>,
    #[serde(default)]
    pub effective_skillset: Vec<String>,
    #[serde(default)]
    pub effective_skill_guidance: Vec<String>,
    /// Skills whose tools are in the ToolAssembly but suppressed per-turn unless
    /// the turn content signals the skill is needed. Populated from the role's
    /// toolset profile `on_demand_skills` list at session snapshot time.
    #[serde(default)]
    pub on_demand_skills: Vec<String>,
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
    /// Tool classes whose catalog members are eligible for this session.
    /// Populated from the active toolset profile; expanded into concrete tool names
    /// by `default_visible_toolset` using the local catalog's class annotations.
    #[serde(default)]
    pub allowed_classes: Vec<String>,
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

#[cfg(test)]
mod paracrine_budget_tests {
    use super::*;
    use crate::r#loop::TurnPhase;
    use uuid::Uuid;

    #[test]
    fn truncate_for_wire_leaves_short_text_unchanged() {
        assert_eq!(truncate_for_wire("hello", 10), "hello");
        assert_eq!(truncate_for_wire("", 10), "");
        // Exactly at the budget: unchanged, no marker.
        assert_eq!(truncate_for_wire("abcde", 5), "abcde");
    }

    #[test]
    fn truncate_for_wire_cuts_long_text_with_marker() {
        let long = "x".repeat(100);
        let out = truncate_for_wire(&long, 40);
        assert!(out.starts_with(&"x".repeat(40)));
        assert!(out.ends_with("…[truncated: 60 chars omitted]"), "{out}");
    }

    #[test]
    fn truncate_for_wire_is_char_boundary_safe_for_multibyte() {
        // 10 multibyte chars; cutting at 4 chars must not panic or split a char.
        let text = "héllö wörld…™✓✗";
        let total = text.chars().count();
        let out = truncate_for_wire(text, 4);
        assert!(out.starts_with("héll"));
        assert!(
            out.ends_with(&format!("…[truncated: {} chars omitted]", total - 4)),
            "{out}"
        );
    }

    /// Minimal fresh working turn with the paracrine budget counters at their
    /// starting values (0 hops, no chain start).
    fn sample_turn() -> WorkingTurn {
        WorkingTurn {
            task_id: Uuid::new_v4(),
            turn_id: "turn-test".into(),
            chat_id: "chat-1".into(),
            primary_user_id: None,
            user_content: "hi".into(),
            final_reply_to: "node-1".into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: None,
            phase: TurnPhase::WaitingTool,
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
            awaiting_transcription_reentry: false,
            scripted_loop_context: None,
            associated_paracrine_ids: Vec::new(),
            paracrine_origin: None,
            paracrine_reply_session_id: None,
            paracrine_reply_chat_id: None,
            paracrine_response_routing: None,
            paracrine_merge_completed: false,
            plan_confirmed: false,
            plan_confirm_note: None,
            fallback_tier: 0,
            ladder_tier0_dispatched: false,
            streaming_retry_attempts: 0,
            streamed_content: String::new(),
            paracrine_hop_count: 0,
            paracrine_chain_started_at: None,
            selection_source: SelectionSource::default(),
        }
    }

    #[test]
    fn default_execution_policy_carries_paracrine_budget() {
        let exec = ExecutionPolicy::default();
        assert_eq!(exec.paracrine_hop_budget, 5);
        assert_eq!(exec.paracrine_chain_budget_secs, 900);
    }

    #[test]
    fn charge_increments_hop_count_and_sets_chain_start() {
        let mut turn = sample_turn();
        let exec = ExecutionPolicy::default();
        assert_eq!(turn.paracrine_hop_count, 0);
        assert_eq!(turn.paracrine_chain_started_at, None);

        let outcome = charge_paracrine_hop(&mut turn, &exec, 1_000);
        assert_eq!(outcome, ParacrineBudgetOutcome::WithinBudget);
        assert_eq!(turn.paracrine_hop_count, 1);
        assert_eq!(turn.paracrine_chain_started_at, Some(1_000));

        // A later hop must not move the chain start (it anchors the wall clock).
        let _ = charge_paracrine_hop(&mut turn, &exec, 1_050);
        assert_eq!(turn.paracrine_hop_count, 2);
        assert_eq!(turn.paracrine_chain_started_at, Some(1_000));
    }

    #[test]
    fn hop_budget_permits_exactly_budget_hops_then_breaches() {
        let mut turn = sample_turn();
        let exec = ExecutionPolicy::default(); // budget 5 hops
        // Hops 1..=5 stay within budget.
        for expected in 1..=5u32 {
            let outcome = charge_paracrine_hop(&mut turn, &exec, 1_000);
            assert_eq!(
                outcome,
                ParacrineBudgetOutcome::WithinBudget,
                "hop {expected} should be within budget"
            );
            assert_eq!(turn.paracrine_hop_count, expected);
        }
        // The 6th hop breaches.
        let outcome = charge_paracrine_hop(&mut turn, &exec, 1_000);
        assert_eq!(
            outcome,
            ParacrineBudgetOutcome::HopsExhausted { hops: 6, budget: 5 }
        );
    }

    #[test]
    fn cumulative_wall_clock_budget_breaches_on_elapsed() {
        let mut turn = sample_turn();
        let exec = ExecutionPolicy::default(); // 900s budget
        // First hop anchors the clock at t=1000.
        assert_eq!(
            charge_paracrine_hop(&mut turn, &exec, 1_000),
            ParacrineBudgetOutcome::WithinBudget
        );
        // A hop 899s later is still within budget.
        assert_eq!(
            charge_paracrine_hop(&mut turn, &exec, 1_899),
            ParacrineBudgetOutcome::WithinBudget
        );
        // A hop exactly 900s later breaches on time (well under the 5-hop cap).
        assert_eq!(
            charge_paracrine_hop(&mut turn, &exec, 1_900),
            ParacrineBudgetOutcome::TimeExhausted {
                elapsed_secs: 900,
                budget_secs: 900,
            }
        );
    }

    #[test]
    fn role_override_tightens_hop_budget() {
        let mut exec = ExecutionPolicy::default();
        let tlc = ansible_mesh_core::graph::TurnLoopConfig {
            paracrine_hop_budget: Some(1),
            paracrine_chain_budget_secs: Some(60),
            ..Default::default()
        };
        exec.apply_paracrine_overrides(&tlc);
        assert_eq!(exec.paracrine_hop_budget, 1);
        assert_eq!(exec.paracrine_chain_budget_secs, 60);

        let mut turn = sample_turn();
        // With the tightened budget, hop 1 is allowed and hop 2 breaches.
        assert_eq!(
            charge_paracrine_hop(&mut turn, &exec, 0),
            ParacrineBudgetOutcome::WithinBudget
        );
        assert_eq!(
            charge_paracrine_hop(&mut turn, &exec, 0),
            ParacrineBudgetOutcome::HopsExhausted { hops: 2, budget: 1 }
        );
    }

    #[test]
    fn role_override_absent_leaves_defaults() {
        let mut exec = ExecutionPolicy::default();
        exec.apply_paracrine_overrides(&ansible_mesh_core::graph::TurnLoopConfig::default());
        assert_eq!(exec.paracrine_hop_budget, 5);
        assert_eq!(exec.paracrine_chain_budget_secs, 900);
    }

    #[test]
    fn context_window_override_applies_and_clamps() {
        let mut policy = ContextWindowPolicy::default();
        // Values within range apply verbatim; out-of-range values clamp to the
        // documented bounds rather than pushing the session outside sane limits.
        policy.apply_overrides(&ansible_mesh_core::graph::ContextWindowOverrides {
            dialogue_window_minutes: Some(120), // clamps to 60
            dialogue_window_chars: Some(2_000), // within range
            max_tool_result_chars: Some(1),     // clamps to 1_000
            max_tool_history_entries: Some(5),  // within range
        });
        assert_eq!(policy.dialogue_window_minutes, 60);
        assert_eq!(policy.dialogue_window_chars, 2_000);
        assert_eq!(policy.max_tool_result_chars, 1_000);
        assert_eq!(policy.max_tool_history_entries, 5);
    }

    #[test]
    fn context_window_override_absent_leaves_defaults() {
        let mut policy = ContextWindowPolicy::default();
        // A serde-default (all-None) override struct — e.g. from an old role
        // record — is a no-op and preserves the session's effective policy.
        policy.apply_overrides(&ansible_mesh_core::graph::ContextWindowOverrides::default());
        assert_eq!(policy, ContextWindowPolicy::default());
    }

    #[test]
    fn old_turn_loop_config_without_context_window_deserializes() {
        // A TurnLoopConfig persisted before this feature has no `context_window`
        // key; it must deserialize with the field defaulting to None.
        let json = serde_json::json!({ "iteration_cap": 7 });
        let tlc: ansible_mesh_core::graph::TurnLoopConfig =
            serde_json::from_value(json).expect("deserialize legacy TurnLoopConfig");
        assert_eq!(tlc.iteration_cap, Some(7));
        assert!(tlc.context_window.is_none());
    }

    #[test]
    fn old_checkpoint_without_budget_fields_deserializes_to_defaults() {
        // Serialize a turn that has non-default budget counters, then strip the
        // new fields to simulate a checkpoint written before this feature.
        let mut turn = sample_turn();
        turn.paracrine_hop_count = 4;
        turn.paracrine_chain_started_at = Some(12_345);
        let mut json = serde_json::to_value(&turn).expect("serialize");
        let obj = json.as_object_mut().expect("object");
        obj.remove("paracrine_hop_count");
        obj.remove("paracrine_chain_started_at");
        assert!(!obj.contains_key("paracrine_hop_count"));

        let restored: WorkingTurn =
            serde_json::from_value(json).expect("deserialize old checkpoint");
        assert_eq!(restored.paracrine_hop_count, 0);
        assert_eq!(restored.paracrine_chain_started_at, None);
    }

    #[test]
    fn old_checkpoint_without_selection_source_deserializes_to_configured_default() {
        // A checkpoint written before Slice 1 has no `selection_source` key at
        // all; it must deserialize to `ConfiguredDefault`, not fail or panic.
        let turn = sample_turn();
        let mut json = serde_json::to_value(&turn).expect("serialize");
        let obj = json.as_object_mut().expect("object");
        obj.remove("selection_source");
        assert!(!obj.contains_key("selection_source"));

        let restored: WorkingTurn =
            serde_json::from_value(json).expect("deserialize old checkpoint");
        assert_eq!(
            restored.selection_source,
            SelectionSource::ConfiguredDefault
        );
    }

    #[test]
    fn old_execution_policy_without_budget_fields_deserializes_to_defaults() {
        // An ExecutionPolicy serialized before the paracrine budget fields existed.
        let json = serde_json::json!({
            "iteration_cap": 10,
            "plan_required_on_skill": true,
            "stream_tool_events": true,
            "stall_detection_threshold": 3
        });
        let exec: ExecutionPolicy = serde_json::from_value(json).expect("deserialize old policy");
        assert_eq!(exec.paracrine_hop_budget, 5);
        assert_eq!(exec.paracrine_chain_budget_secs, 900);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_entry_pct_reports_true_usage_uncapped() {
        let entry = BudgetEntry {
            source: "x".into(),
            used_chars: 150,
            cap_chars: 100,
            truncated: true,
        };
        assert_eq!(entry.pct(), 150);
    }

    #[test]
    fn budget_entry_pct_zero_cap_reports_100() {
        let entry = BudgetEntry {
            source: "x".into(),
            used_chars: 5,
            cap_chars: 0,
            truncated: false,
        };
        assert_eq!(entry.pct(), 100);
    }

    #[test]
    fn injection_budget_default_matches_proposal_defaults() {
        let budget = InjectionBudget::default();
        assert_eq!(budget.persona_chars, 6_000);
        assert_eq!(budget.memory_snapshot_chars, 4_000);
        assert_eq!(budget.recalled_memory_chars, 3_000);
        assert_eq!(budget.skills_chars, 2_500);
        assert_eq!(budget.reflex_snapshot_chars, 1_000);
        assert_eq!(budget.rules_chars, 2_000);
        assert_eq!(budget.total_envelope_chars, 60_000);
    }

    #[test]
    fn budget_ledger_any_truncated_reflects_entries() {
        let mut ledger = BudgetLedger::default();
        assert!(!ledger.any_truncated());
        ledger.entries.push(BudgetEntry {
            source: "x".into(),
            used_chars: 1,
            cap_chars: 1,
            truncated: false,
        });
        assert!(!ledger.any_truncated());
        ledger.entries.push(BudgetEntry {
            source: "y".into(),
            used_chars: 5,
            cap_chars: 1,
            truncated: true,
        });
        assert!(ledger.any_truncated());
    }
}
