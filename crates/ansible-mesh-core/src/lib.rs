use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod adapter;
pub mod agent;
pub mod agent_graph_storage;
pub mod authz;
pub mod beacon;
pub mod catalog_rights;
pub mod cron;
pub mod cursor;
pub mod domain;
pub mod event;
pub mod graph;
pub mod graph_tools;
pub mod heartbeat;
pub mod ledger;
pub mod materializer;
pub mod mcp_endpoint;
pub mod mcp_route;
pub mod membership;
pub mod meshops;
pub mod model_manager;
pub mod registry;
pub mod resources;
pub mod router_trace;
pub mod runtime;
pub mod sqlite_storage;
pub mod storage;
pub mod tools;
pub mod validation;
pub mod webrtc;
pub mod whisper_training;

/// Represents a unique identifier for a node in the mesh network.
pub type NodeId = String;

/// Represents a unique identifier for an agent.
pub type AgentId = String;

/// Represents a versioned tool reference (e.g., "mcp.contacts.search@1").
pub type ToolRef = String;

/// Represents a versioned model reference (e.g., "model.apple.foundation-3b@2026.1").
pub type ModelRef = String;

/// Defines the roles a node can assume on the mesh.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum NodeRole {
    PersonalDevice,
    BatteryConstrained,
    ModelNode,
    McpNode,
    StorageNode,
    ModelManager,
    AnsibleNode,
    InfraController,
    #[serde(untagged)]
    Other(String),
}

/// Node capabilities manifest (`node_capabilities.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeCapabilities {
    pub node_id: NodeId,
    pub roles: Vec<NodeRole>,
    #[serde(default)]
    pub models: Vec<ModelRef>,
    #[serde(default)]
    pub tools: Vec<ToolRef>,
    #[serde(default)]
    pub constraints: NodeConstraints,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NodeConstraints {
    pub max_concurrent_jobs: Option<u32>,
    pub latency_hint_ms: Option<u32>,
    pub trust_level: Option<String>,
}

pub use philotic_primitives_mesh::beacon::*;
