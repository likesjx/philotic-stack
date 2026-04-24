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
    /// The mesh node that should receive this signal envelope.
    pub target_node_id: NodeId,
    /// The target application or agent on that node, when the caller knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_guest_id: Option<String>,
    /// The NodeID of the peer who initiated this signal
    pub sender_node: NodeId,
    /// The guest on the sender node that should receive follow-up signaling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_guest_id: Option<String>,
    /// The actual signaling payload
    pub signal: SignalPayload,
}

#[cfg(test)]
mod tests {
    use super::{SignalPayload, WebRtcSignalMessage};

    #[test]
    fn webrtc_signal_message_round_trips_node_and_guest_routing() {
        let msg = WebRtcSignalMessage {
            session_id: "session-123".into(),
            target_node_id: "mbp-jane-aiua-01".into(),
            target_guest_id: Some("agent-aria:architect".into()),
            sender_node: "default-aiua-01".into(),
            sender_guest_id: Some("membrane-telegram-01".into()),
            signal: SignalPayload::Offer("v=0".into()),
        };

        let encoded = serde_json::to_vec(&msg).expect("encode signal");
        let decoded: WebRtcSignalMessage =
            serde_json::from_slice(&encoded).expect("decode signal");

        assert_eq!(decoded.session_id, "session-123");
        assert_eq!(decoded.target_node_id, "mbp-jane-aiua-01");
        assert_eq!(
            decoded.target_guest_id.as_deref(),
            Some("agent-aria:architect")
        );
        assert_eq!(decoded.sender_node, "default-aiua-01");
        assert_eq!(
            decoded.sender_guest_id.as_deref(),
            Some("membrane-telegram-01")
        );
        assert!(matches!(decoded.signal, SignalPayload::Offer(ref sdp) if sdp == "v=0"));
    }
}
