//! Database-agnostic storage trait abstractions for the Philotic Stack.
//!
//! These traits define the contract that any storage backend must implement
//! (SQLite, PebbleDB, RocksDB, Postgres, etc.). The Ansible daemon consumes
//! these traits via `Arc<dyn XxxStorage>`, making the storage engine a
//! pluggable deployment-time decision.

use crate::event::EventEnvelope;
use crate::graph::{GraphEdge, GraphNode};
use crate::NodeCapabilities;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
// GraphStorage record types (used by GraphDomain and callers)
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
    /// Unix epoch (seconds) of the last time this guest was activated or spawned.
    /// Used by the supervisor TTL check for role incarnation guests.
    pub last_active_at: Option<u64>,
}

/// A declarative description of a component for registration with the hotel.
/// Written by `philotic-web component add`; read by aiua to upsert a GuestRecord.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentManifest {
    /// Unique guest identifier (e.g. "model-mlx-01").
    pub guest_id: String,
    /// Role announced to the hotel (e.g. "model.mlx").
    pub role: String,
    /// Hotel name to register with (e.g. "default").
    pub hotel: String,
    /// Spawn command for the component binary (resolved via PHILOTIC_BIN_DIR or PATH).
    pub command: String,
    /// Command-line arguments passed to the binary.
    #[serde(default)]
    pub args: Vec<String>,
    /// Environment variables injected into the spawned process.
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Component-specific configuration blob stored in node_config under
    /// `component:{guest_id}` and fetched by the binary via `IpcRequest::GetConfig`.
    #[serde(default)]
    pub component_config: serde_json::Value,
    /// If true, the hotel materializes (spawns) the guest immediately after upsert.
    #[serde(default = "default_auto_start")]
    pub auto_start: bool,
}

fn default_auto_start() -> bool {
    true
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

/// Hotel-scoped profile for the human operator/user.
///
/// Singleton per hotel — stored under the key `user_profile:<hotel_name>`.
/// Agents read this at session open to inherit operator context (timezone, etc.)
/// without needing per-agent configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserProfile {
    /// IANA timezone name (e.g. `"America/New_York"`). Injected into every
    /// agent's cognitive header so the model interprets relative time correctly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// Human-readable display name for the operator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
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

// ── Graph runner registry key constant ───────────────────────────────────────

/// Context Graph config key: graph runner instance registry.
/// Value is a JSON array of [`GraphRunnerInstanceRecord`].
pub const CONFIG_GRAPH_RUNNER_REGISTRY: &str = "graph_runner_registry";

/// Maps a `graph_id` to the `instance_id` of the graph-runner that owns it.
/// Stored in the ODS under [`CONFIG_GRAPH_RUNNER_REGISTRY`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphRunnerInstanceRecord {
    pub graph_id: String,
    pub instance_id: String,
    pub registered_at: u64,
}

// ── MuninnDB config key constants ────────────────────────────────────────────

/// Context Graph config key: MuninnDB REST endpoint URL.
/// Value is a JSON string, e.g. `"http://127.0.0.1:8475"`.
pub const CONFIG_MUNINN_ENDPOINT: &str = "muninn_endpoint";

/// Context Graph config key: vault registry.
/// Value is a JSON array of [`VaultRegistryEntry`].
pub const CONFIG_VAULT_REGISTRY: &str = "vault_registry";

/// Maps a MuninnDB vault name to the `secret_ref` of its API token in the
/// hotel key vault. Stored in the Context Graph under [`CONFIG_VAULT_REGISTRY`].
///
/// Vault naming convention: `[a-z0-9_-]`, `_` as scope prefix separator,
/// `-` for internal id separators — e.g. `self_philote-1`, `user_jared`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultRegistryEntry {
    /// Vault name, e.g. `self_philote-1`. Must match `[a-z0-9_-]`, max 64 chars.
    pub vault_name: String,
    /// Secret ref for this vault's API token, e.g.
    /// `secret://hotel/default/muninn/self-philote-1`.
    pub secret_ref: String,
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
