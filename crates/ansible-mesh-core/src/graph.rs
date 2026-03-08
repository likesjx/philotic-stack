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
