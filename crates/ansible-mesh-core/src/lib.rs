use std::net::IpAddr;

use serde::{Deserialize, Serialize};

pub mod adapter;
pub mod agent;
pub mod agent_graph_storage;
pub mod attention_steward;
pub mod authz;
pub mod autonomy;
pub mod beacon;
pub mod capability;
pub mod catalog_rights;
pub mod cron;
pub mod cursor;
pub mod domain;
pub mod event;
pub mod graph;
pub mod graph_tools;
pub mod heal_queue;
pub mod heartbeat;
pub mod ledger;
pub mod materializer;
pub mod mcp_endpoint;
pub mod mcp_route;
pub mod membership;
pub mod meshops;
pub mod model_catalog_discovery;
pub mod model_manager;
pub mod model_routing;
pub mod provider_keys;
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

pub use philotic_primitives_mesh::beacon::{BeaconMessage, BeaconPayload, MsgType, WireEncoding};

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

/// Ordered trust levels for network exposure. Higher = more exposed = more enforcement.
/// Used as a ceiling on hotel listeners and as a required field on MCP endpoint configs.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Default,
)]
#[serde(rename_all = "snake_case")]
pub enum ExposureTier {
    /// Loopback/UDS only. IPC-equivalent trust. No auth ever required.
    #[default]
    Local = 0,
    /// RFC1918 private network. Operator-trusted environment.
    Lan = 1,
    /// Tailscale CGNAT (100.64/10). PSK-authenticated private mesh.
    /// Separate trust domain from LAN — tailnet may include devices the operator does not own.
    Mesh = 2,
    /// Public-facing. Zero implicit trust. Maximum enforcement.
    Internet = 3,
}

/// Per-socket network binding classified by exposure tier.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ListenerProfile {
    /// Logical purpose: "gateway", "beacon", "execution", "blob", "membrane-mcp", "ipc"
    pub purpose: String,
    pub bind_addr: IpAddr,
    pub port: u16,
    /// Interface name if detectable (e.g., "en0", "tailscale0")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iface: Option<String>,
    pub tier: ExposureTier,
}

/// Point-in-time snapshot of a hotel's network security posture.
/// Propagated via NodeHealthSnapshot heartbeat so the mesh can reason about placement.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct PerimeterSnapshot {
    /// Maximum tier across all active listeners — the hotel's effective exposure ceiling.
    pub ceiling: ExposureTier,
    pub listeners: Vec<ListenerProfile>,
    pub tailscale_present: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tailnet_id: Option<String>,
    #[serde(default)]
    pub public_ips: Vec<IpAddr>,
    #[serde(default)]
    pub private_ips: Vec<IpAddr>,
    /// Unix timestamp (seconds) when this snapshot was last derived.
    #[serde(default)]
    pub last_derived: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct NodeHealthSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_free_pct: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_free_pct: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_avg_1m: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub perimeter: Option<PerimeterSnapshot>,
}
