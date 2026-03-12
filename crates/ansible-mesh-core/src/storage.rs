//! Database-agnostic storage trait abstractions for the Philotic Stack.
//!
//! These traits define the contract that any storage backend must implement
//! (SQLite, PebbleDB, RocksDB, Postgres, etc.). The Ansible daemon consumes
//! these traits via `Arc<dyn XxxStorage>`, making the storage engine a
//! pluggable deployment-time decision.

use crate::event::EventEnvelope;
use crate::graph::{AbstractToolRecord, GraphEdge, GraphNode, RoleIncarnationRecord};
use crate::NodeCapabilities;
use anyhow::Result;
use serde::{Deserialize, Serialize};

// ──────────────────────────────────────────────────────────────────────
// EventStorage – manages the durable mesh_events log
// ──────────────────────────────────────────────────────────────────────

/// Abstraction over the durable, append-only event ledger.
pub trait EventStorage: Send + Sync {
    /// Durably append a new event and assign it a monotonically increasing
    /// sequence number. The `seq` field of `env` is updated in place.
    fn append_event(&self, env: &mut EventEnvelope) -> Result<u64>;

    /// Delete an event by its canonical `event_id` (after terminal processing).
    fn delete_event(&self, event_id: &crate::event::EventId) -> Result<usize>;

    /// Return up to `limit` events with `seq > cursor_seq`, ordered ascending.
    fn query_unacked_events(
        &self,
        target_node_id: &str,
        cursor_seq: u64,
        limit: u32,
    ) -> Result<Vec<EventEnvelope>>;
}

// ──────────────────────────────────────────────────────────────────────
// CursorStorage – tracks per-node delivery acknowledgement cursors
// ──────────────────────────────────────────────────────────────────────

/// Abstraction over the per-node ACK cursor table.
pub trait CursorStorage: Send + Sync {
    /// Return the highest acknowledged sequence for `consumer_node_id`.
    fn get_cursor(&self, consumer_node_id: &str) -> Result<u64>;

    /// Advance (upsert) the cursor. Implementations MUST enforce
    /// `last_acked_seq = MAX(existing, acked_seq)` to prevent regression.
    fn advance_cursor(&self, consumer_node_id: &str, acked_seq: u64, ts: u64) -> Result<()>;
}

// ──────────────────────────────────────────────────────────────────────
// GraphStorage – Context Graph operations (guests, memory, config)
// ──────────────────────────────────────────────────────────────────────

/// A single guest record as loaded from the storage layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestRecord {
    pub hotel_name: String,
    pub guest_id: String,
    pub role: String,
    pub config_json: String,
    pub is_active: bool,
    pub active_pid: Option<String>,
}

/// A named hotel runtime record stored in the Context Graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotelRecord {
    pub hotel_name: String,
    pub capabilities: NodeCapabilities,
    pub mesh_port: u16,
    pub blob_port: u16,
    pub execution_port: u16,
    pub ipc_socket_path: String,
    pub active_pid: Option<String>,
}

/// A durable agent identity/profile bundle stored in the Context Graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentIdentityRecord {
    pub agent_id: String,
    pub persona_name: String,
    pub authority_hotel: String,
    #[serde(default)]
    pub bundle_json: serde_json::Value,
}

/// A generalized cross-component session record stored in the Context Graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_id: String,
    pub session_kind: String,
    pub primary_agent_id: Option<String>,
    #[serde(default)]
    pub active_incarnation_id: Option<String>,
    pub channel_kind: Option<String>,
    pub channel_session_key: Option<String>,
    pub status: String,
    pub lease_owner_component_id: Option<String>,
    pub lease_expires_at: Option<u64>,
    #[serde(default)]
    pub summary_json: serde_json::Value,
    pub created_at: u64,
    pub updated_at: u64,
}

/// A component participating in a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionParticipantRecord {
    pub session_id: String,
    pub component_id: String,
    pub role: String,
    pub joined_at: u64,
    pub last_seen_at: u64,
}

/// A turn within a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTurnRecord {
    pub turn_id: String,
    pub session_id: String,
    pub request_event_id: Option<String>,
    #[serde(default)]
    pub user_message_json: serde_json::Value,
    pub status: String,
    pub response_json: Option<serde_json::Value>,
    pub error_json: Option<serde_json::Value>,
    pub started_at: Option<u64>,
    pub completed_at: Option<u64>,
}

/// A lightweight session event, useful for deltas/progress outside of apartment snapshots.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEventRecord {
    pub event_id: String,
    pub session_id: String,
    pub turn_id: Option<String>,
    pub component_id: String,
    pub kind: String,
    #[serde(default)]
    pub payload_json: serde_json::Value,
    pub created_at: u64,
}

/// An encrypted secret owned by the hotel vault.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SecretRecord {
    pub secret_ref: String,
    pub secret_kind: String,
    pub scope: String,
    pub allowed_roles: Vec<String>,
    pub allowed_guests: Vec<String>,
    pub ciphertext_b64: String,
    pub nonce_b64: String,
    pub created_at: u64,
    pub updated_at: u64,
}

/// Abstraction over the local Context Graph database.
pub trait GraphStorage: Send + Sync {
    // ── Node configuration ───────────────────────────────────────────

    /// Load the node capability manifest, or `None` if not yet bootstrapped.
    fn load_node_capabilities(&self) -> Result<Option<NodeCapabilities>>;

    /// Persist (upsert) the node capability manifest.
    fn save_node_capabilities(&self, caps: &NodeCapabilities) -> Result<()>;

    /// Load an arbitrary JSON config value by key.
    fn get_config_value(&self, key: &str) -> Result<Option<String>>;

    /// Persist (upsert) an arbitrary JSON config value by key.
    fn set_config_value(&self, key: &str, value_json: &str) -> Result<()>;

    /// Persist (upsert) an encrypted vault secret record.
    fn upsert_secret(&self, secret: &SecretRecord) -> Result<()>;

    /// Load an encrypted vault secret record by secret ref.
    fn get_secret(&self, secret_ref: &str) -> Result<Option<SecretRecord>>;

    /// Load a named hotel record, or `None` if it has not been bootstrapped yet.
    fn get_hotel(&self, hotel_name: &str) -> Result<Option<HotelRecord>>;

    /// List all known hotel records.
    fn list_hotels(&self) -> Result<Vec<HotelRecord>>;

    /// Persist (upsert) a named hotel record.
    fn upsert_hotel(&self, hotel: &HotelRecord) -> Result<()>;

    /// Update the running PID lease for a named hotel.
    fn set_hotel_pid(&self, hotel_name: &str, pid: Option<&str>) -> Result<()>;

    // ── Guest materialization manifest ────────────────────────────────

    /// Return all guest records matching the filter.
    /// If `active_only` is true, only rows where `is_active = 1`.
    fn list_guests(&self, hotel_name: &str, active_only: bool) -> Result<Vec<GuestRecord>>;

    /// Update the `active_pid` column for a guest.
    fn set_guest_pid(&self, hotel_name: &str, guest_id: &str, pid: Option<&str>) -> Result<()>;

    /// Bulk-insert or replace guest rows (used during initial seeding).
    fn seed_guests(&self, hotel_name: &str, guests: &[GuestRecord]) -> Result<()>;

    // ── Agent identity bundles ───────────────────────────────────────

    fn upsert_agent_identity(&self, identity: &AgentIdentityRecord) -> Result<()>;
    fn get_agent_identity(&self, agent_id: &str) -> Result<Option<AgentIdentityRecord>>;

    // ── Memory apartments ────────────────────────────────────────────

    /// Upsert a memory apartment using Last-Writer-Wins semantics.
    fn sync_apartment(
        &self,
        agent_id: &str,
        memory_type: &str,
        content_json: &serde_json::Value,
    ) -> Result<()>;
    fn get_apartment(&self, agent_id: &str, memory_type: &str)
        -> Result<Option<serde_json::Value>>;

    // ── Generalized sessions ─────────────────────────────────────────

    fn upsert_session(&self, session: &SessionRecord) -> Result<()>;
    fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>>;
    fn upsert_role_incarnation(&self, role: &RoleIncarnationRecord) -> Result<()>;
    fn get_role_incarnation(
        &self,
        agent_id: &str,
        role_name: &str,
    ) -> Result<Option<RoleIncarnationRecord>>;
    fn list_role_incarnations(&self, agent_id: &str) -> Result<Vec<RoleIncarnationRecord>>;
    fn upsert_session_participant(&self, participant: &SessionParticipantRecord) -> Result<()>;
    fn list_session_participants(&self, session_id: &str) -> Result<Vec<SessionParticipantRecord>>;
    fn upsert_session_turn(&self, turn: &SessionTurnRecord) -> Result<()>;
    fn get_session_turn(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<Option<SessionTurnRecord>>;
    fn list_session_turns(&self, session_id: &str, limit: usize) -> Result<Vec<SessionTurnRecord>>;
    fn append_session_event(&self, event: &SessionEventRecord) -> Result<()>;
    fn list_session_events(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<SessionEventRecord>>;

    // ── Abstract tool catalog (shareable tool definitions) ──────────────

    /// Upsert a shared abstract tool definition into the context graph.
    fn upsert_abstract_tool(&self, tool: &AbstractToolRecord) -> Result<()>;

    /// Load a single abstract tool definition by tool name, or `None` if not cataloged.
    fn get_abstract_tool(&self, tool_name: &str) -> Result<Option<AbstractToolRecord>>;

    /// List all abstract tool definitions in the catalog.
    fn list_abstract_tools(&self) -> Result<Vec<AbstractToolRecord>>;
}

/// Generic graph persistence contract.
///
/// Backends can model nodes/edges however they want, but consumers should see
/// a graph-shaped API instead of leaking table-oriented assumptions upward.
pub trait GraphAdapter: Send + Sync {
    fn upsert_node(&self, node: &GraphNode) -> Result<()>;
    fn get_node(&self, node_key: &str) -> Result<Option<GraphNode>>;
    fn delete_node(&self, node_key: &str) -> Result<()>;
    fn list_nodes_by_kind(&self, kind: &str) -> Result<Vec<GraphNode>>;
    fn upsert_edge(&self, edge: &GraphEdge) -> Result<()>;
    fn delete_edge(&self, edge_key: &str) -> Result<()>;
    fn list_edges_from(
        &self,
        src_node_key: &str,
        edge_kind: Option<&str>,
    ) -> Result<Vec<GraphEdge>>;
}
