use crate::NodeId;
use serde::{Deserialize, Serialize};

/// Identifies the type of WebRTC signaling payload
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "kebab-case")]
pub enum SignalPayload {
    /// An SDP offer from the initiating peer
    Offer(String),
    /// An SDP answer from the receiving peer
    Answer(String),
    /// An ICE candidate for NAT traversal and connectivity
    IceCandidate(String),
    /// A signal that the WebRTC stream session has formally terminated
    SessionEnded,
}

/// The outer envelope for a WebRTC signal transmitted over the durable mesh
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebRtcSignalMessage {
    /// The unique identifier for this specific streaming session
    pub session_id: String,
    /// The target application or agent the stream is intended for (e.g., "model-router-gemini")
    pub target_guest_id: String,
    /// The NodeID of the peer who initiated this signal
    pub sender_node: NodeId,
    /// The actual signaling payload
    pub signal: SignalPayload,
}
