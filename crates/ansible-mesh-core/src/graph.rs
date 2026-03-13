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
}

/// A system-wide shared skill definition stored in the context graph.
///
/// Node kind: `abstract_skill`. Node key: `abstract_skill:{skill_name}`.
/// Skills provide model-facing posture hints and can imply tool grants when a
/// role or session activates them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AbstractSkillRecord {
    pub skill_name: String,
    pub description: String,
    #[serde(default)]
    pub implied_tools: Vec<String>,
}

/// Per-role runtime loop controls for a role incarnation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TurnLoopConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration_cap: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_policy: Option<String>,
}

/// A long-lived named role incarnation for an agent.
///
/// Node kind: `role_incarnation`. Node key: `role_incarnation:{agent_id}:{role_name}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoleIncarnationRecord {
    pub agent_id: String,
    pub role_name: String,
    pub guest_id: String,
    pub toolset_profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role_identity_addendum: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inactive_ttl_seconds: Option<u64>,
    #[serde(default)]
    pub turn_loop_config: TurnLoopConfig,
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
