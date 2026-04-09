use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod adapter;
pub mod agent;
pub mod agent_graph_storage;
pub mod authz;
pub mod beacon;
pub mod cron;
pub mod cursor;
pub mod domain;
pub mod event;
pub mod graph;
pub mod graph_tools;
pub mod heartbeat;
pub mod ledger;
pub mod materializer;
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

/// The core envelope for UDP mesh communication.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeaconMessage {
    /// Protocol version (e.g., 1)
    pub version: u8,
    /// Unique identifier for this message
    pub msg_id: Uuid,
    /// Originating node
    pub src_node: NodeId,
    /// Destination node (or "broadcast" / "group:X")
    pub dest_node: String,
    /// Message type identifier
    pub msg_type: MsgType,
    /// Sequence number for fragmented messages (0 for unfragmented)
    pub seq: u32,
    /// Total fragments in this sequence (1 for unfragmented)
    pub total: u32,
    /// Encoded payload (JSON, MsgPack, or CBOR)
    pub payload: Vec<u8>,
    /// Creation timestamp (Unix epoch secs)
    pub timestamp: u64,
    /// HMAC signature for integrity and authenticity
    pub hmac: Vec<u8>,
}

/// Identifiers for the types of messages sent over the beacon.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MsgType {
    /// Deploy an agent bundle to a remote node
    DeployAgent,
    /// Execute a deployed agent with an input
    RunAgent,
    /// Invoke a specific tool returning results
    ToolCall,
    /// Pull secrets from the authority node
    SecretPull,
    /// Mesh presence and health update
    Heartbeat,
    /// Asynchronous result delivery
    Result,
    /// Streaming execution logs
    Log,
    /// Model Manager routing requests
    ModelManager,
    /// Reading/writing from the Graph memory apartments
    MemoryOp,
    /// A batch of EventEnvelopes dispatched over the durable mesh
    MeshEventBatch,
    /// An acknowledgment of durably received mesh events
    MeshEventAck,
    /// WebRTC Session Description Protocol (SDP) and ICE candidate signaling
    WebRtcSignal,
}
