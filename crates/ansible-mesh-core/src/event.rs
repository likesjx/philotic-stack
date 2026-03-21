use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A globally unique identifier for a discrete event on the Philotic mesh.
pub type EventId = Uuid;

/// Correlation ID used to tie tasks, replies, and retries together.
pub type CorrelationId = String;

/// Node identity.
pub type NodeId = String;

/// Agent identity.
pub type AgentId = String;

/// The canonical envelope for all data-plane payload and control-plane signaling transit.
/// Replaces volatile UDP pushes with durable, sequenced, cursor-tracked events.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// The globally unique event ID.
    pub event_id: EventId,
    /// The strictly monotonic sequence number assigned by the source node's writer thread.
    pub seq: u64,
    /// The originating hotel node.
    pub source_node_id: NodeId,
    /// The destination hotel node. `None` means local-only or broadcast semantics.
    pub target_node_id: Option<NodeId>,
    /// The originating guest agent.
    pub source_agent_id: AgentId,
    /// The destination guest agent. (Optional for broadcast).
    pub target_agent_id: Option<AgentId>,
    /// The classification of the event payload.
    pub kind: EventKind,
    /// Identifier for tracing related flows (e.g. task -> reply).
    pub corr_id: CorrelationId,
    /// The number of times this event has been dispatched.
    pub attempt: u32,
    /// UTC timestamp (ms since epoch) of atomic sequence genesis.
    pub created_at: u64,
    /// UTC timestamp (ms since epoch) when this event is considered expired/stale.
    pub expires_at: Option<u64>,
    /// The payload, or a reference to a blob.
    pub payload: EventPayload,
    /// Lineage path (routing decisions + policy applied).
    pub trace: Vec<String>,
}

/// Classifications of Philotic events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EventKind {
    /// A localized task delegation (e.g. RunTool, ProcessMessage).
    TaskInvoke,
    /// A completed task result.
    TaskResult,
    /// Ephemeral WebRTC session negotiation (SDP Offer/Answer/ICE).
    WebrtcSignal,
    /// Context Graph mutations and assertions.
    MemoryOp,
    /// Capability availability advertisement.
    CapabilityPublish,
    /// Administrative control plane mutations (e.g., Session role handoffs)
    SessionControl,
}

/// The payload definition. Large files must use `BlobRef`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum EventPayload {
    /// Standard JSON data for control and data plane.
    #[serde(rename = "inline")]
    Inline { data: String }, // typically serialized JSON
    /// Pointer to a large HTTP chunked blob residing on the source node.
    #[serde(rename = "attachment")]
    BlobRef {
        blob_id: String,
        size: u64,
        mime: String,
        source_hotel_ip: String,
    },
}

/// The canonical error code taxonomy for failed terminal events.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TerminalErrorCode {
    /// The target agent identity does not exist or is not registered.
    UnknownAgent,
    /// The targeted agent lacks the required capability/tool.
    CapabilityNotSupported,
    /// The payload failed schema validation at the destination.
    MalformedPayload,
    /// The task exceeded its execution SLA or TTL before completion.
    Timeout,
    /// The request was rejected by local admission policy or governance gates.
    PolicyRejected,
    /// The guest process crashed or returned a hard error during execution.
    GuestExecutionFault,
}
