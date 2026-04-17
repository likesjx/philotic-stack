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
pub const NODE_KIND_SECRET: &str = "secret";
pub const NODE_KIND_ABSTRACT_SKILL: &str = "abstract_skill";
pub const NODE_KIND_WORKFLOW_SKILL: &str = "workflow_skill";
pub const NODE_KIND_TOOLSET_PROFILE: &str = "toolset_profile";
pub const NODE_KIND_NODE_CAPABILITIES: &str = "node_capabilities";
pub const NODE_KIND_CONFIG: &str = "config";
pub const NODE_KIND_APARTMENT: &str = "apartment";
pub const NODE_KIND_CRON_JOB: &str = "cron_job";

