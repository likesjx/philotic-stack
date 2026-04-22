use crate::event::NodeId;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Point-in-time environment snapshot self-reported by a hotel in each heartbeat.
/// All fields are optional so older nodes remain wire-compatible.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NodeHealthSnapshot {
    /// Active guest process count on this hotel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_count: Option<u32>,
    /// Percentage of disk space still free on the primary volume (0.0–100.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_free_pct: Option<f32>,
    /// Percentage of system memory still free (0.0–100.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_free_pct: Option<f32>,
    /// 1-minute load average (Unix `uptime` style).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_avg_1m: Option<f32>,
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
    /// A chunk of capability advertisements and reachability metadata.
    CapabilitySync,
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
    /// A membership acceptance packet emitted after an invite is accepted
    MeshMembershipAccept,
    /// WebRTC Session Description Protocol (SDP) and ICE candidate signaling
    WebRtcSignal,
}
