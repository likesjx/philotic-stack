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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
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

/// Per-role runtime loop controls for a role incarnation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TurnLoopConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration_cap: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_policy: Option<String>,
    /// When present, philote runs the scripted step tree instead of the
    /// standard hard-coded tool re-entry loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_script: Option<LoopScript>,
    /// Ordered list of model roles for tiered provider fallback.
    /// Tier 0 is attempted first; on retriable failure the loop advances to the
    /// next tier. Defaults to `["model", "model.local"]` when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_tiers: Vec<String>,
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

impl RoleIncarnationRecord {
    pub fn routing_role(&self) -> String {
        format!("role:{}:{}", self.agent_id, self.role_name)
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
