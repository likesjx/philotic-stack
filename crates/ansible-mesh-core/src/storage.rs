//! Database-agnostic storage trait abstractions for the Philotic Stack.
//!
//! These traits define the contract that any storage backend must implement
//! (SQLite, PebbleDB, RocksDB, Postgres, etc.). The Ansible daemon consumes
//! these traits via `Arc<dyn XxxStorage>`, making the storage engine a
//! pluggable deployment-time decision.

use crate::event::EventEnvelope;
use crate::NodeCapabilities;
use anyhow::Result;

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
#[derive(Debug, Clone)]
pub struct GuestRecord {
    pub hotel_name: String,
    pub guest_id: String,
    pub role: String,
    pub config_json: String,
    pub is_active: bool,
    pub active_pid: Option<String>,
}

/// A named hotel runtime record stored in the Context Graph.
#[derive(Debug, Clone)]
pub struct HotelRecord {
    pub hotel_name: String,
    pub capabilities: NodeCapabilities,
    pub mesh_port: u16,
    pub blob_port: u16,
    pub ipc_socket_path: String,
    pub active_pid: Option<String>,
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

    /// Load a named hotel record, or `None` if it has not been bootstrapped yet.
    fn get_hotel(&self, hotel_name: &str) -> Result<Option<HotelRecord>>;

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

    // ── Memory apartments ────────────────────────────────────────────

    /// Upsert a memory apartment using Last-Writer-Wins semantics.
    fn sync_apartment(
        &self,
        agent_id: &str,
        memory_type: &str,
        content_json: &serde_json::Value,
    ) -> Result<()>;
}
