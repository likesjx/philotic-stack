use crate::AgentId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Generic graph node stored by the adapter layer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphNode {
    pub node_key: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub data: Value,
}

/// Generic graph edge stored by the adapter layer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GraphEdge {
    pub edge_key: String,
    pub src_node_key: String,
    pub edge_kind: String,
    pub dst_node_key: String,
    #[serde(default)]
    pub data: Value,
}

/// Represents an Agent identity within the Context Graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentGraphNode {
    pub id: AgentId,
    pub kind: String, // e.g., "Agent"
    pub soul: FileHashRef,
    pub identity: FileHashRef,
    pub bundle: BundleRef,
    pub relationships: Vec<GraphRelationship>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileHashRef {
    pub source: String,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleRef {
    pub spec_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphRelationship {
    pub kind: String, // e.g., "USES_TOOL", "RUNS_ON_ROLE"
    pub target: String,
}

/// A system-wide shared tool definition stored in the context graph.
///
/// Node kind: `abstract_tool`. Node key: `abstract_tool:{tool_name}`.
/// These are the canonical capability definitions visible to agents and used
/// for approval class evaluation. They are seeded at hotel startup and can be
/// extended by tool-runner guests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AbstractToolRecord {
    pub tool_name: String,
    pub description: String,
    #[serde(default)]
    pub input_schema: serde_json::Value,
    /// Approval and projection class: "session", "workspace", "utility", "capability"
    pub class: String,
    #[serde(default)]
    pub tool_markers: Vec<String>,
}

/// A system-wide shared model definition stored in the context graph.
///
/// Node kind: `abstract_model`. Node key: `abstract_model:{model_ref}`.
/// These are static model markers used to inform routing and policy projection;
/// they do not grant rights or declare live availability.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AbstractModelRecord {
    pub model_ref: String,
    pub provider_hint: String,
    pub description: String,
    #[serde(default)]
    pub capability_markers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_stem: Option<String>,
    #[serde(default)]
    pub speed_marker: i32,
    #[serde(default)]
    pub thinking_marker: i32,
    #[serde(default)]
    pub tool_use_marker: i32,
    #[serde(default)]
    pub audio_native_marker: i32,
}

/// A system-wide shared right definition stored in the context graph.
///
/// Node kind: `abstract_right`. Node key: `abstract_right:{right_name}`.
/// Rights are shared reference knowledge describing a named grant the hotel may
/// project into a session's effective key ring.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AbstractRightRecord {
    pub right_name: String,
    pub description: String,
    /// What kind of target this right refers to: "tool", "skill", or "component".
    pub target_kind: String,
    /// Stable identifier for the referenced tool, skill, or component capability.
    pub target_ref: String,
}

/// Lifecycle and validation state for a shared skill definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", content = "data")]
#[serde(rename_all = "snake_case")]
pub enum SkillValidationState {
    Draft,
    Validated,
    Registered,
    Active,
    Suspended { reason: String },
    Invalid { errors: Vec<String> },
    Deprecated,
}

impl Default for SkillValidationState {
    fn default() -> Self {
        Self::Draft
    }
}

/// Provenance snapshot describing where and when a skill was registered.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SkillSourceSnapshot {
    pub mesh_catalog_version: String,
    pub hotel_policy_version: String,
    pub registered_at: u64,
    pub registered_by: String,
}

/// A system-wide shared skill definition stored in the context graph.
///
/// Node kind: `abstract_skill`. Node key: `abstract_skill:{skill_name}`.
/// Skills provide model-facing posture hints and can imply tool grants when a
/// role or session activates them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct AbstractSkillRecord {
    pub skill_name: String,
    pub description: String,
    #[serde(default)]
    pub implied_tools: Vec<String>,
    #[serde(default)]
    pub skill_markers: Vec<String>,
    #[serde(default)]
    pub validation_state: SkillValidationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_snapshot: Option<SkillSourceSnapshot>,
    #[serde(default)]
    pub field_sources: serde_json::Value,
}

/// Append-only audit entry recorded on every accepted `skill.register`.
///
/// Node kind: `skill_registration_audit`. Node key: `skill_registration_audit:{audit_id}`.
/// Captures who (the authenticated IPC peer), what (skill + resulting validation
/// state), and when a skill entered the catalog. Because skills later project tools
/// onto agents, this trail lets operators review every registration and closes the
/// "open to any agent" gap flagged in the security audit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SkillRegistrationAuditRecord {
    /// Unique id for this audit entry (node key suffix).
    pub audit_id: String,
    /// Name of the skill that was registered.
    pub skill_name: String,
    /// `guest_id` of the registering guest (the authenticated IPC peer).
    pub registered_by: String,
    /// Role the registering guest held at registration time.
    pub registered_by_role: String,
    /// Resulting validation state (e.g. `validated`, `invalid`, `draft`).
    pub validation_state: String,
    /// Unix timestamp (seconds) when the registration was accepted.
    pub registered_at: u64,
}

/// A governed workflow record defining an advanced process like handoff or delegation.
///
/// Node kind: `workflow_skill`. Node key: `workflow_skill:{workflow_name}`.
/// Sits "above" `AbstractSkillRecord` to handle target selection, context
/// packaging, and governance for operations crossing identity/process boundaries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct WorkflowSkillRecord {
    pub workflow_name: String,
    pub workflow_kind: String, // e.g., "handoff.to_role", "delegate.to_peer"
    pub owner_scope: String,   // e.g., "orchestrator", "admin"
    pub target_class: String,  // e.g., "same_identity_role", "peer_agent"
    pub description: String,
    #[serde(default)]
    pub target_selection_policy: serde_json::Value,
    #[serde(default)]
    pub context_requirements: serde_json::Value,
    #[serde(default)]
    pub return_contract: serde_json::Value,
    #[serde(default)]
    pub governance: serde_json::Value,
    pub rollout_state: String, // e.g., "draft", "active"
}

/// A named bundle of allowed tools, tool classes, and skills granted to a role.
///
/// Node kind: `toolset_profile`. Node key: `toolset_profile:{profile_name}`.
/// `RoleIncarnationRecord.toolset_profile` holds this name as a reference.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ToolsetProfileRecord {
    pub profile_name: String,
    /// Explicit tool names granted by this profile.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    /// Tool class names granted by this profile (e.g. "session", "utility").
    #[serde(default)]
    pub allowed_classes: Vec<String>,
    /// Skill names whose `implied_tools` are transitively granted.
    #[serde(default)]
    pub allowed_skills: Vec<String>,
    /// Skills whose tools are included in the ToolAssembly but suppressed from
    /// the model's visible list unless the current turn's content activates
    /// the skill. Use for domain-specific tool groups (cron, table, graph, etc.)
    /// that should not appear in every orchestrator turn.
    #[serde(default)]
    pub on_demand_skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Remote datasource guests whose tools should be available to sessions
    /// using this profile. Each entry is an AllowedIncarnation-compatible JSON
    /// object (fields: incarnation_id, hotel_id, target_node, target_role,
    /// supported_tools, execution_mode). Merged into
    /// `allowed_tool_runner_incarnations` during session snapshot composition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remote_tool_runners: Vec<serde_json::Value>,
}

/// A single step in a LoopScript.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct LoopStep {
    pub id: String,
    #[serde(rename = "type")]
    pub step_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(default)]
    pub config: serde_json::Value,
}

/// A scripted turn loop tree — an ordered list of steps that replaces the
/// hard-coded tool re-entry loop when present on a role's TurnLoopConfig.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct LoopScript {
    pub variant: String,
    pub steps: Vec<LoopStep>,
}

/// Per-role overrides for the dialogue-window / tool-history budgets carried on
/// a session's `ContextWindowPolicy`. Every field is optional and serde-default,
/// so old `TurnLoopConfig` records (and roles that set none of these) deserialize
/// cleanly and leave the session's effective policy untouched. Applied at role
/// activation and reverted to the session baseline on handoff-return, so a terse
/// specialist and a long-form orchestrator no longer share one budget.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ContextWindowOverrides {
    /// Overrides `dialogue_window_minutes` (max age of included turns).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dialogue_window_minutes: Option<u32>,
    /// Overrides `dialogue_window_chars` (total char budget of the window).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dialogue_window_chars: Option<usize>,
    /// Overrides `max_tool_result_chars` (per-tool-result truncation budget).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_result_chars: Option<usize>,
    /// Overrides `max_tool_history_entries` (tool-call entries kept per turn).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_history_entries: Option<usize>,
}

/// Per-role runtime loop controls for a role incarnation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TurnLoopConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration_cap: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
    /// Vestigial: persisted and operator-editable (`role.configure`), but
    /// never consumed at dispatch (see `bug: philote per-agent model-name
    /// has no wired lever`, routing drill 2026-07-09). Superseded by
    /// `model_bindings` below, which IS consumed. Kept for backward
    /// compatibility with already-persisted role records; do not add new
    /// consumers of this field — extend `model_bindings` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_policy: Option<String>,
    /// Per-agent model NAME binding, keyed by provider role (a `fallback_tiers`
    /// entry, e.g. `"model.openrouter"`, `"model"`, `"model.ollama"`) mapping
    /// to the model id/name to request from that provider for this role.
    /// Consumed by philote's dispatch (`role_model_binding` in
    /// `crates/philote/src/runtime.rs`) to set `ControllerTask.model` for
    /// whichever provider role `resolve_model_execution_target` resolves —
    /// covering both the primary dispatch and every fallback tier, since each
    /// tier is looked up independently by its own role key. Precedence at
    /// dispatch: an explicit caller-set model (if any) > this binding for the
    /// resolved provider role > the provider's own global default (e.g.
    /// `openrouter_default_model`). A `BTreeMap` (not `HashMap`) for
    /// deterministic serialization — durability diffing (mesh-config
    /// preserve-or-source, `seed_orchestrator_roles`) depends on stable
    /// ordering. Empty when unset; missing entries fall through to the
    /// provider default.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub model_bindings: std::collections::BTreeMap<String, String>,
    /// When present, philote runs the scripted step tree instead of the
    /// standard hard-coded tool re-entry loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_script: Option<LoopScript>,
    /// Ordered list of model roles for tiered provider fallback.
    /// Tier 0 is attempted first; on retriable failure the loop advances to the
    /// next tier. Defaults to `model_routing::DEFAULT_FALLBACK_TIERS`
    /// (`["model", "model.openrouter", "model.ollama"]`) when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_tiers: Vec<String>,
    /// Per-role override for the maximum number of paracrine delegation hops
    /// allowed in a single turn's chain. `None` -> use the agent default (5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paracrine_hop_budget: Option<u32>,
    /// Per-role override for the cumulative wall-clock budget (seconds) of a
    /// turn's paracrine delegation chain. `None` -> use the agent default (900).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paracrine_chain_budget_secs: Option<u64>,
    /// Per-role overrides for the session's dialogue-window / tool-history
    /// budgets. `None` -> the session keeps its baseline `ContextWindowPolicy`.
    /// Serde default so older serialized `TurnLoopConfig` values load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<ContextWindowOverrides>,
    /// Per-role override for the number of automatic plan-continuation turns
    /// the plan-eval-repeat loop may synthesize for a single carried-over plan.
    /// `None` -> use the agent default (3). Serde default so older serialized
    /// `TurnLoopConfig` values load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_continuation_budget: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RoleReadinessState {
    #[default]
    Configured,
    Materializing,
    Materialized,
    Routable,
    ActiveInSession,
}

impl RoleReadinessState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::Materializing => "materializing",
            Self::Materialized => "materialized",
            Self::Routable => "routable",
            Self::ActiveInSession => "active_in_session",
        }
    }
}

/// A long-lived named role incarnation for an agent.
///
/// Node kind: `role_incarnation`. Node key: `role_incarnation:{agent_id}:{role_name}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RoleIncarnationRecord {
    pub agent_id: String,
    pub role_name: String,
    pub guest_id: String,
    pub toolset_profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_identity_addendum: Option<String>,
    /// Governance document projected into the agent's Identity context layer when this role is
    /// active. Written in natural language — describes focus, rules, delegation posture, and
    /// approval constraints so the agent can reason about its own capabilities and limits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_manifest: Option<String>,
    /// Content-filtering posture for this role incarnation. `"unrestricted"` disables
    /// provider-level safety filtering where the provider exposes a toggle (Gemini
    /// `safetySettings` at `BLOCK_NONE`) and adds a permissive system-instruction line
    /// so providers without a toggle don't second-guess it with over-cautious refusals.
    /// `"strict"` tightens filtering (Gemini `BLOCK_MEDIUM_AND_ABOVE`). `"standard"`
    /// (the default) preserves prior behavior exactly — no safetySettings are sent,
    /// providers use their own defaults. This is a private, single-operator system;
    /// the policy is set explicitly per role by the operator via role.configure.
    #[serde(default = "default_content_policy")]
    pub content_policy: String,
    /// Admin roles may update operator-owned records such as the orchestrator role manifest.
    /// Only the hotel seed or an existing admin role may create a role with is_admin: true.
    #[serde(default)]
    pub is_admin: bool,
    #[serde(default)]
    pub readiness_state: RoleReadinessState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inactive_ttl_seconds: Option<u64>,
    #[serde(default)]
    pub turn_loop_config: TurnLoopConfig,
    /// The hotel (node_id) where this role's philote guest process should run.
    /// When set to a remote hotel, `HandoffToRole` routes via the mesh instead of
    /// materializing locally. When `None`, the role runs on the authority hotel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_node: Option<String>,
}

impl Default for RoleIncarnationRecord {
    /// Manual (not derived) so `content_policy` defaults to `"standard"` — the
    /// same default `#[serde(default = "default_content_policy")]` uses — rather
    /// than the empty string a derived `Default` would give a `String` field.
    /// Test fixtures across the workspace use `..Default::default()` to pick up
    /// this field without listing it explicitly at every call site.
    fn default() -> Self {
        Self {
            agent_id: String::new(),
            role_name: String::new(),
            guest_id: String::new(),
            toolset_profile: String::new(),
            role_identity_addendum: None,
            role_manifest: None,
            content_policy: default_content_policy(),
            is_admin: false,
            readiness_state: RoleReadinessState::default(),
            inactive_ttl_seconds: None,
            turn_loop_config: TurnLoopConfig::default(),
            home_node: None,
        }
    }
}

impl RoleIncarnationRecord {
    pub fn routing_role(&self) -> String {
        format!("role:{}:{}", self.agent_id, self.role_name)
    }
}

/// Serde default for [`RoleIncarnationRecord::content_policy`] and the mirrored
/// session-side fields — `"standard"` preserves current (pre-feature) behavior
/// exactly for every role that hasn't explicitly set a content policy.
pub fn default_content_policy() -> String {
    "standard".to_string()
}

/// The three valid `content_policy` values. Anything else is rejected at the
/// `ConfigureRole` IPC boundary rather than silently stored.
pub const CONTENT_POLICY_VALUES: [&str; 3] = ["unrestricted", "standard", "strict"];

pub fn is_valid_content_policy(value: &str) -> bool {
    CONTENT_POLICY_VALUES.contains(&value)
}

/// Lifecycle state for an externally polled membrane transport home.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MembraneTransportHomeStatus {
    #[default]
    Active,
    Paused,
    Retired,
    Blocked,
}

impl MembraneTransportHomeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Retired => "retired",
            Self::Blocked => "blocked",
        }
    }
}

/// Declares the single active hotel that may poll an external membrane transport.
///
/// Node kind: `membrane_transport_home`.
/// Node key: `membrane_transport_home:{agent_id}:{transport}:{resource_ref}`.
///
/// This is deliberately distinct from [`RoleIncarnationRecord::home_node`]:
/// role homes place cognitive/runtime guests, while transport homes place
/// scarce external ingress like Telegram long polling.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MembraneTransportHomeRecord {
    pub agent_id: String,
    pub transport: String,
    pub resource_ref: String,
    pub active_home_hotel: String,
    #[serde(default)]
    pub standby_hotels: Vec<String>,
    pub managed_by_role: String,
    pub lease_type: String,
    pub failover_policy: String,
    #[serde(default)]
    pub status: MembraneTransportHomeStatus,
}

impl MembraneTransportHomeRecord {
    pub fn is_active_home(&self, hotel_name: &str) -> bool {
        self.status == MembraneTransportHomeStatus::Active && self.active_home_hotel == hotel_name
    }

    pub fn is_standby_home(&self, hotel_name: &str) -> bool {
        self.standby_hotels.iter().any(|hotel| hotel == hotel_name)
    }
}

/// A durable behavioral constraint stored in the context graph.
///
/// Node kind: `rule`. Node key: `rule:{rule_id}`.
/// Rules are injected into the `instructions` section of every cognitive call
/// and never rolled off by the dialogue window. An agent cannot self-elevate
/// beliefs into rules — all rule proposals require operator confirmation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuleRecord {
    /// Unique rule identifier (UUID).
    pub rule_id: String,
    /// Agent identity that owns this rule.
    pub agent_id: String,
    /// Human-readable behavioral constraint. Written in the imperative.
    /// E.g. "Always ask for clarification before deleting files."
    pub description: String,
    /// The reasoning or observation that led to this rule being elevated.
    pub rationale: String,
    /// Unix epoch (seconds) when this rule was created.
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutingPolicyDispositionRecord {
    pub state: String,
    pub reason: String,
    pub decided_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutingPolicyEvaluationRecord {
    pub evaluation_kind: String,
    pub decision: String,
    pub reason: String,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_tool: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutingPolicyRecord {
    pub proposal_id: String,
    pub agent_id: String,
    pub problem: String,
    pub proposed_change: String,
    pub evidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affected_stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub affected_capability: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learned_reflex_preference_key: Option<String>,
    pub operator_disposition: RoutingPolicyDispositionRecord,
    #[serde(default)]
    pub evaluations: Vec<RoutingPolicyEvaluationRecord>,
    pub created_at: u64,
}

/// A dedicated namespace for an agent's memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryApartment {
    pub id: String,
    pub kind: String, // e.g., "MemoryApartment"
    pub owner_agent: AgentId,
    pub entries: Vec<MemoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,   // e.g., "mem:2026-03-04T15:00Z"
    pub kind: String, // e.g., "conversation", "semantic", "episodic"
    pub summary: String,
    pub embedding_ref: Option<String>,
    #[serde(default)]
    pub links: Vec<GraphRelationship>,
}

// ──────────────────────────────────────────────────────────────────────────────
// CompactSessionEnvelope — canonical bounded session snapshot for handoff,
// rematerialization, and compaction. Explicitly excludes durable continuity
// (autobiographical memory, long-term topic memory, transcript archives).
// ──────────────────────────────────────────────────────────────────────────────

/// A single compacted turn entry — enough for context coherence without bulk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CompactTurnEntry {
    pub turn_id: String,
    /// One-line summary of the user request or trigger.
    pub user_summary: String,
    /// Brief labels for tool calls made (e.g. ["workspace.read", "echo"]).
    #[serde(default)]
    pub tool_calls: Vec<String>,
    /// Outcome: "completed", "approval_required", "failed", "abandoned".
    pub outcome: String,
}

/// Metadata recorded at compaction time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CompactCheckpointMeta {
    /// Unix epoch seconds when this envelope was produced.
    pub compacted_at: u64,
    /// Monotonically incremented each time the session is compacted.
    pub compaction_version: u32,
    /// How many raw turns were folded into this snapshot.
    pub prior_turn_count: u32,
    /// Component that produced this snapshot (e.g. "agent-core", "hotel").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub produced_by: Option<String>,
}

/// Canonical bounded session snapshot.
///
/// Designed to be small enough to fit in a single model context window entry
/// and large enough to reconstruct coherent working state after rematerialization
/// or role handoff. Durable continuity (autobiographical memory, relationship
/// memory, long-term topic memory, transcript archives) is explicitly excluded
/// and must be sourced from Muninn or apartment storage separately.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CompactSessionEnvelope {
    // ── Identity ──────────────────────────────────────────────────────────
    pub session_id: String,
    pub session_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_agent_id: Option<String>,

    // ── Active role / incarnation ─────────────────────────────────────────
    /// Role name currently active (e.g. "developer", "orchestrator").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_role: Option<String>,
    /// Guest ID of the active incarnation process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_incarnation_id: Option<String>,

    // ── Live status ───────────────────────────────────────────────────────
    /// One of: "active", "waiting_approval", "waiting_tool", "paused", "closed".
    pub live_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_owner_component_id: Option<String>,

    // ── Working state (role-local, not durable) ───────────────────────────
    /// Freeform JSON: active_goal, pending_items, current_plan fragment, etc.
    #[serde(default)]
    pub working_state: Value,

    // ── Bounded recent-turn window ────────────────────────────────────────
    /// Last N turns needed for coherence. Not a transcript — summaries only.
    #[serde(default)]
    pub recent_turns: Vec<CompactTurnEntry>,

    // ── Session-local facts (still live in this session) ─────────────────
    /// Short key=value or plain-text facts relevant only to this session.
    /// Cleared on session close. NOT durable memory.
    #[serde(default)]
    pub session_facts: Vec<String>,

    // ── Checkpoint metadata ───────────────────────────────────────────────
    pub checkpoint_metadata: CompactCheckpointMeta,
}

/// Operational profile for a model provider, keyed per (model_ref, node_id).
///
/// Node kind: `model_profile`. Node key: `model_profile:{model_ref}:{node_id}`.
/// Updated after every dispatch via `observe_model_outcome`. Used by
/// `GraphDomain::best_model_for` to select the healthiest available provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelProfileRecord {
    /// Canonical model identifier, e.g. "gemini", "gemini-2.5-flash", "parakeet".
    pub model_ref: String,
    /// Node (hotel) this record belongs to, e.g. "mac-jane-aiua-01".
    pub node_id: String,
    /// Provider kind: "gemini", "ollama", "mlx", "elevenlabs", etc.
    pub provider: String,
    /// Task kinds this provider handles, e.g. ["text.generate", "media.analyze"].
    #[serde(default)]
    pub task_kinds: Vec<String>,
    /// Trust tier: "remote_cloud", "local_trusted", "local_experimental".
    #[serde(default)]
    pub trust_tier: String,
    /// Maximum context tokens this provider accepts (0 = unknown).
    #[serde(default)]
    pub max_context_tokens: u32,
    /// Observed p50 latency in milliseconds over a rolling window.
    #[serde(default)]
    pub latency_p50_ms: u64,
    /// Rolling error rate 0.0–1.0 (exponential moving average, α=0.1).
    #[serde(default)]
    pub error_rate: f32,
    /// Operational status: "healthy", "degraded", "unavailable".
    #[serde(default = "default_model_status")]
    pub status: String,
    /// Unix timestamp of last successful dispatch.
    #[serde(default)]
    pub last_healthy_secs: u64,
    /// Unix timestamp of when this record was last updated.
    #[serde(default)]
    pub updated_secs: u64,
    /// Whether the provider reliably supports native tool/function calling.
    /// Defaults to `true` for records written before this field existed so the
    /// routing oracle never filters legacy records more aggressively than the
    /// pre-oracle dispatcher did.
    #[serde(default = "default_true")]
    pub supports_tools: bool,
    /// Whether the provider reliably returns structured (JSON-contract) output.
    /// Same permissive legacy default as `supports_tools`.
    #[serde(default = "default_true")]
    pub supports_structured: bool,
    /// Coarse speed/capability tier: "fast", "heavy", or "" when unknown.
    #[serde(default)]
    pub model_tier: String,
    /// Consecutive dispatch failures since the last success. Drives the
    /// degrade-after-N transition in `apply_model_outcome`.
    #[serde(default)]
    pub consecutive_failures: u32,
}

impl Default for ModelProfileRecord {
    fn default() -> Self {
        Self {
            model_ref: String::new(),
            node_id: String::new(),
            provider: String::new(),
            task_kinds: Vec::new(),
            trust_tier: String::new(),
            max_context_tokens: 0,
            latency_p50_ms: 0,
            error_rate: 0.0,
            status: default_model_status(),
            last_healthy_secs: 0,
            updated_secs: 0,
            supports_tools: true,
            supports_structured: true,
            model_tier: String::new(),
            consecutive_failures: 0,
        }
    }
}

fn default_model_status() -> String {
    "healthy".to_string()
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod content_policy_tests {
    use super::*;

    /// A `RoleIncarnationRecord` JSON blob that predates `content_policy` (as
    /// every real DB row does today) must deserialize with `content_policy ==
    /// "standard"` — the serde default that preserves current behavior for
    /// every already-configured role.
    #[test]
    fn content_policy_serde_defaults_to_standard_when_absent() {
        let json = serde_json::json!({
            "agent_id": "agent-jane",
            "role_name": "orchestrator",
            "guest_id": "agent-jane:orchestrator",
            "toolset_profile": "orchestrator",
        });
        let record: RoleIncarnationRecord =
            serde_json::from_value(json).expect("legacy record without content_policy decodes");
        assert_eq!(record.content_policy, "standard");
    }

    #[test]
    fn content_policy_round_trips_explicit_value() {
        let record = RoleIncarnationRecord {
            agent_id: "agent-jane".into(),
            role_name: "orchestrator".into(),
            guest_id: "agent-jane:orchestrator".into(),
            toolset_profile: "orchestrator".into(),
            content_policy: "unrestricted".into(),
            ..Default::default()
        };
        let json = serde_json::to_value(&record).expect("serialize");
        let decoded: RoleIncarnationRecord = serde_json::from_value(json).expect("deserialize");
        assert_eq!(decoded.content_policy, "unrestricted");
    }

    #[test]
    fn default_impl_uses_standard_content_policy() {
        assert_eq!(RoleIncarnationRecord::default().content_policy, "standard");
        assert_eq!(default_content_policy(), "standard");
    }

    #[test]
    fn content_policy_validation_accepts_only_known_values() {
        assert!(is_valid_content_policy("unrestricted"));
        assert!(is_valid_content_policy("standard"));
        assert!(is_valid_content_policy("strict"));
        assert!(!is_valid_content_policy("permissive"));
        assert!(!is_valid_content_policy(""));
        assert!(!is_valid_content_policy("UNRESTRICTED"));
    }
}
