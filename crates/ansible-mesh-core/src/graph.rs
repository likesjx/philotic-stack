use crate::{AgentId};
use serde::{Deserialize, Serialize};

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
    pub id: String, // e.g., "mem:2026-03-04T15:00Z"
    pub kind: String, // e.g., "conversation", "semantic", "episodic"
    pub summary: String,
    pub embedding_ref: Option<String>,
    #[serde(default)]
    pub links: Vec<GraphRelationship>,
}
