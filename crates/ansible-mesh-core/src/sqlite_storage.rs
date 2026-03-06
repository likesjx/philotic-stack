//! SQLite-backed implementations of the storage traits.
//!
//! These wrap the existing `EventLedger`, `CursorTracker`, and `ContextGraph`
//! behind the abstract `storage::*` traits so the Ansible daemon can
//! consume them as `Arc<dyn EventStorage>`, etc.

use crate::event::{EventEnvelope, EventId, EventKind, EventPayload};
use crate::storage::{CursorStorage, EventStorage, GraphStorage, GuestRecord, HotelRecord};
use crate::NodeCapabilities;
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tracing::{debug, info};

// ══════════════════════════════════════════════════════════════════════
// SqliteEventStorage
// ══════════════════════════════════════════════════════════════════════

#[derive(Clone)]
pub struct SqliteEventStorage {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteEventStorage {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        let s = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        s.init_schema()?;
        Ok(s)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "CREATE TABLE IF NOT EXISTS mesh_events (
                seq INTEGER PRIMARY KEY AUTOINCREMENT,
                event_id TEXT NOT NULL UNIQUE,
                source_node_id TEXT NOT NULL,
                source_agent_id TEXT NOT NULL,
                target_agent_id TEXT,
                kind TEXT NOT NULL,
                corr_id TEXT NOT NULL,
                attempt INTEGER DEFAULT 0,
                created_at INTEGER NOT NULL,
                expires_at INTEGER,
                payload_type TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                trace_json TEXT NOT NULL
            )",
            [],
        )?;
        Ok(())
    }
}

impl EventStorage for SqliteEventStorage {
    fn append_event(&self, env: &mut EventEnvelope) -> Result<u64> {
        let (payload_type, payload_json) = match &env.payload {
            EventPayload::Inline { data } => ("inline", data.clone()),
            EventPayload::BlobRef {
                blob_id,
                size,
                mime,
                source_hotel_ip,
            } => (
                "attachment",
                serde_json::json!({
                    "blob_id": blob_id,
                    "size": size,
                    "mime": mime,
                    "source_hotel_ip": source_hotel_ip
                })
                .to_string(),
            ),
        };

        let trace_json = serde_json::to_string(&env.trace).unwrap_or_else(|_| "[]".into());

        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO mesh_events (
                event_id, source_node_id, source_agent_id, target_agent_id,
                kind, corr_id, attempt, created_at, expires_at,
                payload_type, payload_json, trace_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                env.event_id.to_string(),
                env.source_node_id,
                env.source_agent_id,
                env.target_agent_id,
                serde_json::to_string(&env.kind).unwrap().trim_matches('"'),
                env.corr_id,
                env.attempt,
                env.created_at,
                env.expires_at,
                payload_type,
                payload_json,
                trace_json
            ],
        )?;

        let seq = conn.last_insert_rowid() as u64;
        env.seq = seq;
        Ok(seq)
    }

    fn delete_event(&self, event_id: &EventId) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "DELETE FROM mesh_events WHERE event_id = ?1",
            params![event_id.to_string()],
        )?;
        Ok(n)
    }

    fn query_unacked_events(
        &self,
        _target_node_id: &str,
        cursor_seq: u64,
        limit: u32,
    ) -> Result<Vec<EventEnvelope>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT
                seq, event_id, source_node_id, source_agent_id, target_agent_id,
                kind, corr_id, attempt, created_at, expires_at,
                payload_type, payload_json, trace_json
             FROM mesh_events
             WHERE seq > ?1
             ORDER BY seq ASC
             LIMIT ?2",
        )?;

        let mut rows = stmt.query(params![cursor_seq, limit])?;
        let mut events = Vec::new();

        while let Some(row) = rows.next()? {
            let payload_type: String = row.get(10)?;
            let payload_json: String = row.get(11)?;

            let payload = match payload_type.as_str() {
                "inline" => EventPayload::Inline { data: payload_json },
                "attachment" => {
                    let v: serde_json::Value =
                        serde_json::from_str(&payload_json).unwrap_or(serde_json::json!({}));
                    EventPayload::BlobRef {
                        blob_id: v["blob_id"].as_str().unwrap_or("").to_string(),
                        size: v["size"].as_u64().unwrap_or(0),
                        mime: v["mime"].as_str().unwrap_or("").to_string(),
                        source_hotel_ip: v["source_hotel_ip"].as_str().unwrap_or("").to_string(),
                    }
                }
                _ => EventPayload::Inline { data: payload_json },
            };

            let trace_json: String = row.get(12)?;
            let trace: Vec<String> =
                serde_json::from_str(&trace_json).unwrap_or_else(|_| vec![]);

            let kind_str: String = row.get(5)?;
            let kind_json = format!("\"{}\"", kind_str);
            let kind: EventKind =
                serde_json::from_str(&kind_json).unwrap_or(EventKind::TaskInvoke);

            events.push(EventEnvelope {
                seq: row.get(0)?,
                event_id: uuid::Uuid::parse_str(&row.get::<_, String>(1)?).unwrap_or_default(),
                source_node_id: row.get(2)?,
                source_agent_id: row.get(3)?,
                target_agent_id: row.get(4)?,
                kind,
                corr_id: row.get(6)?,
                attempt: row.get(7)?,
                created_at: row.get(8)?,
                expires_at: row.get(9)?,
                payload,
                trace,
            });
        }

        Ok(events)
    }
}

// ══════════════════════════════════════════════════════════════════════
// SqliteCursorStorage
// ══════════════════════════════════════════════════════════════════════

#[derive(Clone)]
pub struct SqliteCursorStorage {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteCursorStorage {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        let s = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        s.init_schema()?;
        Ok(s)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "CREATE TABLE IF NOT EXISTS mesh_cursors (
                consumer_node_id TEXT PRIMARY KEY,
                last_acked_seq INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL
            )",
            [],
        )?;
        Ok(())
    }
}

impl CursorStorage for SqliteCursorStorage {
    fn get_cursor(&self, consumer_node_id: &str) -> Result<u64> {
        let conn = self.conn.lock().unwrap();
        let seq = conn
            .query_row(
                "SELECT last_acked_seq FROM mesh_cursors WHERE consumer_node_id = ?1",
                params![consumer_node_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(seq)
    }

    fn advance_cursor(&self, consumer_node_id: &str, acked_seq: u64, ts: u64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO mesh_cursors (consumer_node_id, last_acked_seq, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(consumer_node_id) DO UPDATE SET
             last_acked_seq = MAX(last_acked_seq, excluded.last_acked_seq),
             updated_at = excluded.updated_at",
            params![consumer_node_id, acked_seq, ts],
        )?;
        Ok(())
    }
}

// ══════════════════════════════════════════════════════════════════════
// SqliteGraphStorage
// ══════════════════════════════════════════════════════════════════════

#[derive(Clone)]
pub struct SqliteGraphStorage {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteGraphStorage {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(db_path).context("Failed to open GraphStorage SQLite DB")?;
        let s = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        s.init_schema()?;
        Ok(s)
    }

    fn init_schema(&self) -> Result<()> {
        debug!("Initializing GraphStorage schema");
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "
            BEGIN;
            CREATE TABLE IF NOT EXISTS node_config (
                key TEXT PRIMARY KEY,
                value_json TEXT NOT NULL,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS hotels (
                hotel_name TEXT PRIMARY KEY,
                capabilities_json TEXT NOT NULL,
                mesh_port INTEGER NOT NULL,
                blob_port INTEGER NOT NULL,
                ipc_socket_path TEXT NOT NULL,
                active_pid TEXT,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS materialized_guests (
                hotel_name TEXT NOT NULL DEFAULT 'default',
                guest_id TEXT PRIMARY KEY,
                role TEXT NOT NULL,
                config_json TEXT NOT NULL,
                is_active BOOLEAN DEFAULT 1,
                last_seen DATETIME DEFAULT CURRENT_TIMESTAMP,
                active_pid TEXT
            );

            CREATE TABLE IF NOT EXISTS agent_identities (
                agent_id TEXT PRIMARY KEY,
                persona_name TEXT NOT NULL,
                bundle_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS memory_apartments (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                agent_id TEXT NOT NULL,
                memory_type TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY(agent_id) REFERENCES agent_identities(agent_id)
            );
            COMMIT;
            ",
        )
        .context("Failed to initialize GraphStorage schema")?;

        // Migration for legacy databases
        let _ = conn.execute(
            "ALTER TABLE materialized_guests ADD COLUMN active_pid TEXT",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE materialized_guests ADD COLUMN hotel_name TEXT NOT NULL DEFAULT 'default'",
            [],
        );

        info!("GraphStorage schema initialized successfully.");
        Ok(())
    }

    /// Expose a raw lock for edge-case operations that don't fit the trait
    /// (e.g., ad-hoc config seeding in `main.rs`).
    pub fn raw_conn(&self) -> &Arc<Mutex<Connection>> {
        &self.conn
    }
}

impl GraphStorage for SqliteGraphStorage {
    fn load_node_capabilities(&self) -> Result<Option<NodeCapabilities>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT value_json FROM node_config WHERE key = 'capabilities'")?;
        let mut rows = stmt.query([])?;

        if let Some(row) = rows.next()? {
            let json: String = row.get(0)?;
            let caps: NodeCapabilities = serde_json::from_str(&json)?;
            Ok(Some(caps))
        } else {
            Ok(None)
        }
    }

    fn save_node_capabilities(&self, caps: &NodeCapabilities) -> Result<()> {
        let json = serde_json::to_string(caps)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO node_config (key, value_json, updated_at)
             VALUES ('capabilities', ?1, CURRENT_TIMESTAMP)
             ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json, updated_at=CURRENT_TIMESTAMP",
            [&json],
        )?;
        Ok(())
    }

    fn get_config_value(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT value_json FROM node_config WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;

        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    fn get_hotel(&self, hotel_name: &str) -> Result<Option<HotelRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT capabilities_json, mesh_port, blob_port, ipc_socket_path, active_pid
             FROM hotels
             WHERE hotel_name = ?1",
        )?;
        let mut rows = stmt.query(params![hotel_name])?;

        if let Some(row) = rows.next()? {
            let capabilities_json: String = row.get(0)?;
            let capabilities: NodeCapabilities = serde_json::from_str(&capabilities_json)?;
            Ok(Some(HotelRecord {
                hotel_name: hotel_name.to_string(),
                capabilities,
                mesh_port: row.get(1)?,
                blob_port: row.get(2)?,
                ipc_socket_path: row.get(3)?,
                active_pid: row.get(4).unwrap_or(None),
            }))
        } else {
            Ok(None)
        }
    }

    fn upsert_hotel(&self, hotel: &HotelRecord) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let capabilities_json = serde_json::to_string(&hotel.capabilities)?;
        conn.execute(
            "INSERT INTO hotels (hotel_name, capabilities_json, mesh_port, blob_port, ipc_socket_path, active_pid, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, CURRENT_TIMESTAMP)
             ON CONFLICT(hotel_name) DO UPDATE SET
             capabilities_json = excluded.capabilities_json,
             mesh_port = excluded.mesh_port,
             blob_port = excluded.blob_port,
             ipc_socket_path = excluded.ipc_socket_path,
             active_pid = excluded.active_pid,
             updated_at = CURRENT_TIMESTAMP",
            params![
                hotel.hotel_name,
                capabilities_json,
                hotel.mesh_port,
                hotel.blob_port,
                hotel.ipc_socket_path,
                hotel.active_pid
            ],
        )?;
        Ok(())
    }

    fn set_hotel_pid(&self, hotel_name: &str, pid: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE hotels SET active_pid = ?1, updated_at = CURRENT_TIMESTAMP WHERE hotel_name = ?2",
            params![pid, hotel_name],
        )?;
        Ok(())
    }

    fn list_guests(&self, hotel_name: &str, active_only: bool) -> Result<Vec<GuestRecord>> {
        let conn = self.conn.lock().unwrap();
        let sql = if active_only {
            "SELECT hotel_name, guest_id, role, config_json, is_active, active_pid FROM materialized_guests WHERE hotel_name = ?1 AND is_active = 1"
        } else {
            "SELECT hotel_name, guest_id, role, config_json, is_active, active_pid FROM materialized_guests WHERE hotel_name = ?1"
        };
        let mut stmt = conn.prepare(sql)?;
        let mut rows = stmt.query(params![hotel_name])?;
        let mut out = Vec::new();

        while let Some(row) = rows.next()? {
            out.push(GuestRecord {
                hotel_name: row.get(0)?,
                guest_id: row.get(1)?,
                role: row.get(2)?,
                config_json: row.get(3)?,
                is_active: row.get(4)?,
                active_pid: row.get(5).unwrap_or(None),
            });
        }
        Ok(out)
    }

    fn set_guest_pid(&self, hotel_name: &str, guest_id: &str, pid: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE materialized_guests SET active_pid = ?1 WHERE hotel_name = ?2 AND guest_id = ?3",
            params![pid, hotel_name, guest_id],
        )?;
        Ok(())
    }

    fn seed_guests(&self, hotel_name: &str, guests: &[GuestRecord]) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        for g in guests {
            conn.execute(
                "INSERT OR REPLACE INTO materialized_guests (hotel_name, guest_id, role, config_json, is_active)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![hotel_name, g.guest_id, g.role, g.config_json, g.is_active],
            )?;
        }
        Ok(())
    }

    fn sync_apartment(
        &self,
        agent_id: &str,
        memory_type: &str,
        content_json: &serde_json::Value,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let content_str = serde_json::to_string(content_json)?;

        conn.execute(
            "DELETE FROM memory_apartments WHERE agent_id = ?1 AND memory_type = ?2",
            [agent_id, memory_type],
        )?;

        conn.execute(
            "INSERT INTO memory_apartments (agent_id, memory_type, content) VALUES (?1, ?2, ?3)",
            [agent_id, memory_type, &content_str],
        )?;

        debug!(
            "Synchronized Memory Apartment for {} ({})",
            agent_id, memory_type
        );
        Ok(())
    }
}
