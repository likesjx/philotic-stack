// ── Kind constants ────────────────────────────────────────────────────────────
//
// These are the shared data vocabulary for all graph stores in the system.
// When a new entity type is added, add its kind constant here first.

// Slice 1
pub const NODE_KIND_HOTEL: &str = "hotel";
pub const NODE_KIND_ABSTRACT_TOOL: &str = "abstract_tool";
pub const NODE_KIND_ABSTRACT_MODEL: &str = "abstract_model";
pub const NODE_KIND_ABSTRACT_RIGHT: &str = "abstract_right";
pub const NODE_KIND_RULE: &str = "rule";
pub const NODE_KIND_ROUTING_POLICY: &str = "routing_policy";

// Slice 2
pub const NODE_KIND_GUEST: &str = "guest";
pub const NODE_KIND_AGENT_IDENTITY: &str = "agent_identity";
pub const NODE_KIND_SESSION: &str = "session";
pub const NODE_KIND_SESSION_PARTICIPANT: &str = "session_participant";
pub const NODE_KIND_SESSION_TURN: &str = "session_turn";
pub const NODE_KIND_SESSION_EVENT: &str = "session_event";
pub const NODE_KIND_ROLE_INCARNATION: &str = "role_incarnation";
pub const NODE_KIND_MEMBRANE_TRANSPORT_HOME: &str = "membrane_transport_home";
pub const NODE_KIND_SECRET: &str = "secret";
pub const NODE_KIND_ABSTRACT_SKILL: &str = "abstract_skill";
pub const NODE_KIND_WORKFLOW_SKILL: &str = "workflow_skill";
/// Append-only audit trail for accepted `skill.register` calls (who / what / when).
pub const NODE_KIND_SKILL_REGISTRATION_AUDIT: &str = "skill_registration_audit";
pub const NODE_KIND_TOOLSET_PROFILE: &str = "toolset_profile";
/// Per-hotel tool grant registry and tool policy (`proposal:data-driven-tool-grants-skilldag`).
pub const NODE_KIND_TOOL_GRANT_REGISTRY: &str = "tool_grant_registry";
pub const NODE_KIND_NODE_CAPABILITIES: &str = "node_capabilities";
pub const NODE_KIND_CONFIG: &str = "config";
pub const NODE_KIND_APARTMENT: &str = "apartment";
pub const NODE_KIND_CRON_JOB: &str = "cron_job";
pub const NODE_KIND_USER_PROFILE: &str = "user_profile";
pub const NODE_KIND_PROJECTED_USER_IDENTITY: &str = "projected_user_identity";
pub const NODE_KIND_USER_TASK: &str = "user_task";

// Model graph (doc:model-graph-flywheel / doc:hardened-model-dispatch)
pub const NODE_KIND_MODEL_PROFILE: &str = "model_profile";

// Autopoiesis Slice A1 (doc:AUTOPOIESIS_PROPOSAL)
/// Per-lane earned-autonomy grant (posture, budget, earned counters).
pub const NODE_KIND_AUTONOMY_GRANT: &str = "autonomy_grant";
/// Append-only decision record for autonomous actions (what/why/evidence/undo).
pub const NODE_KIND_AUTONOMY_AUDIT: &str = "autonomy_audit";

// Autopoiesis Slice A3 (doc:AUTOPOIESIS_PROPOSAL)
/// Fix request filed by the heal-dispatcher for a recurring failure pattern.
pub const NODE_KIND_HEAL_WORK_ITEM: &str = "heal_work_item";
