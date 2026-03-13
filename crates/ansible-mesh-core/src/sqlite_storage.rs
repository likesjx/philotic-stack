//! SQLite-backed implementations of the storage traits.
//!
//! These wrap the existing `EventLedger`, `CursorTracker`, and `ContextGraph`
//! behind the abstract `storage::*` traits so the Ansible daemon can
//! consume them as `Arc<dyn EventStorage>`, etc.

use crate::event::{EventEnvelope, EventId, EventKind, EventPayload};
use crate::graph::{AbstractSkillRecord, AbstractToolRecord, RoleIncarnationRecord};
use crate::graph::{GraphEdge, GraphNode};
use crate::graph::AbstractToolRecord;
use crate::storage::{
    CursorStorage, EventStorage, GraphAdapter, GraphStorage, GuestRecord, HotelRecord,
    SecretRecord, SessionEventRecord, SessionParticipantRecord, SessionRecord, SessionTurnRecord,
};
use crate::NodeCapabilities;
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::Deserialize;
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
                target_node_id TEXT,
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
        let _ = conn.execute("ALTER TABLE mesh_events ADD COLUMN target_node_id TEXT", []);
        Ok(())
    }

    /// Expose raw connection handle for tests and low-level queries.
    pub fn raw_conn(&self) -> &Arc<Mutex<Connection>> {
        &self.conn
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
        match conn.execute(
            "INSERT INTO mesh_events (
                event_id, source_node_id, target_node_id, source_agent_id, target_agent_id,
                kind, corr_id, attempt, created_at, expires_at,
                payload_type, payload_json, trace_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                env.event_id.to_string(),
                env.source_node_id,
                env.target_node_id,
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
        ) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ffi::ErrorCode::ConstraintViolation =>
            {
                let seq = conn.query_row(
                    "SELECT seq FROM mesh_events WHERE event_id = ?1",
                    params![env.event_id.to_string()],
                    |row| row.get::<_, u64>(0),
                )?;
                env.seq = seq;
                return Ok(seq);
            }
            Err(err) => return Err(err.into()),
        }

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
        target_node_id: &str,
        cursor_seq: u64,
        limit: u32,
    ) -> Result<Vec<EventEnvelope>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT
                seq, event_id, source_node_id, target_node_id, source_agent_id, target_agent_id,
                kind, corr_id, attempt, created_at, expires_at,
                payload_type, payload_json, trace_json
             FROM mesh_events
             WHERE seq > ?1
               AND (target_node_id IS NULL OR target_node_id = ?2)
             ORDER BY seq ASC
             LIMIT ?3",
        )?;

        let mut rows = stmt.query(params![cursor_seq, target_node_id, limit])?;
        let mut events = Vec::new();

        while let Some(row) = rows.next()? {
            let payload_type: String = row.get(11)?;
            let payload_json: String = row.get(12)?;

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

            let trace_json: String = row.get(13)?;
            let trace: Vec<String> = serde_json::from_str(&trace_json).unwrap_or_else(|_| vec![]);

            let kind_str: String = row.get(6)?;
            let kind_json = format!("\"{}\"", kind_str);
            let kind: EventKind = serde_json::from_str(&kind_json).unwrap_or(EventKind::TaskInvoke);

            events.push(EventEnvelope {
                seq: row.get(0)?,
                event_id: uuid::Uuid::parse_str(&row.get::<_, String>(1)?).unwrap_or_default(),
                source_node_id: row.get(2)?,
                target_node_id: row.get(3)?,
                source_agent_id: row.get(4)?,
                target_agent_id: row.get(5)?,
                kind,
                corr_id: row.get(7)?,
                attempt: row.get(8)?,
                created_at: row.get(9)?,
                expires_at: row.get(10)?,
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

    /// Expose raw connection handle for tests and low-level queries.
    pub fn raw_conn(&self) -> &Arc<Mutex<Connection>> {
        &self.conn
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
pub struct SqliteGraphAdapter {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteGraphAdapter {
    fn new(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "
            BEGIN;
            CREATE TABLE IF NOT EXISTS graph_nodes (
                node_key TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                label TEXT,
                data_json TEXT NOT NULL,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS graph_edges (
                edge_key TEXT PRIMARY KEY,
                src_node_key TEXT NOT NULL,
                edge_kind TEXT NOT NULL,
                dst_node_key TEXT NOT NULL,
                data_json TEXT NOT NULL,
                updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
            );

            CREATE INDEX IF NOT EXISTS idx_graph_nodes_kind
                ON graph_nodes(kind);
            CREATE INDEX IF NOT EXISTS idx_graph_edges_src_kind
                ON graph_edges(src_node_key, edge_kind);
            CREATE INDEX IF NOT EXISTS idx_graph_edges_dst
                ON graph_edges(dst_node_key);
            COMMIT;
            ",
        )?;
        Ok(())
    }
}

impl GraphAdapter for SqliteGraphAdapter {
    fn upsert_node(&self, node: &GraphNode) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO graph_nodes (node_key, kind, label, data_json, updated_at)
             VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP)
             ON CONFLICT(node_key) DO UPDATE SET
             kind = excluded.kind,
             label = excluded.label,
             data_json = excluded.data_json,
             updated_at = CURRENT_TIMESTAMP",
            params![
                node.node_key,
                node.kind,
                node.label,
                serde_json::to_string(&node.data)?
            ],
        )?;
        Ok(())
    }

    fn get_node(&self, node_key: &str) -> Result<Option<GraphNode>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT node_key, kind, label, data_json
             FROM graph_nodes
             WHERE node_key = ?1",
        )?;
        let mut rows = stmt.query(params![node_key])?;

        if let Some(row) = rows.next()? {
            let data_json: String = row.get(3)?;
            Ok(Some(GraphNode {
                node_key: row.get(0)?,
                kind: row.get(1)?,
                label: row.get(2)?,
                data: serde_json::from_str(&data_json)?,
            }))
        } else {
            Ok(None)
        }
    }

    fn delete_node(&self, node_key: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM graph_nodes WHERE node_key = ?1",
            params![node_key],
        )?;
        conn.execute(
            "DELETE FROM graph_edges WHERE src_node_key = ?1 OR dst_node_key = ?1",
            params![node_key],
        )?;
        Ok(())
    }

    fn list_nodes_by_kind(&self, kind: &str) -> Result<Vec<GraphNode>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT node_key, kind, label, data_json
             FROM graph_nodes
             WHERE kind = ?1
             ORDER BY node_key ASC",
        )?;
        let rows = stmt.query_map(params![kind], |row| {
            let data_json: String = row.get(3)?;
            Ok(GraphNode {
                node_key: row.get(0)?,
                kind: row.get(1)?,
                label: row.get(2)?,
                data: serde_json::from_str(&data_json).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
            })
        })?;

        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn upsert_edge(&self, edge: &GraphEdge) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO graph_edges (edge_key, src_node_key, edge_kind, dst_node_key, data_json, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, CURRENT_TIMESTAMP)
             ON CONFLICT(edge_key) DO UPDATE SET
             src_node_key = excluded.src_node_key,
             edge_kind = excluded.edge_kind,
             dst_node_key = excluded.dst_node_key,
             data_json = excluded.data_json,
             updated_at = CURRENT_TIMESTAMP",
            params![
                edge.edge_key,
                edge.src_node_key,
                edge.edge_kind,
                edge.dst_node_key,
                serde_json::to_string(&edge.data)?
            ],
        )?;
        Ok(())
    }

    fn delete_edge(&self, edge_key: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM graph_edges WHERE edge_key = ?1",
            params![edge_key],
        )?;
        Ok(())
    }

    fn list_edges_from(
        &self,
        src_node_key: &str,
        edge_kind: Option<&str>,
    ) -> Result<Vec<GraphEdge>> {
        let conn = self.conn.lock().unwrap();
        let decode_row = |row: &rusqlite::Row<'_>| {
            let data_json: String = row.get(4)?;
            Ok(GraphEdge {
                edge_key: row.get(0)?,
                src_node_key: row.get(1)?,
                edge_kind: row.get(2)?,
                dst_node_key: row.get(3)?,
                data: serde_json::from_str(&data_json).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?,
            })
        };

        let mut out = Vec::new();
        if let Some(kind) = edge_kind {
            let mut stmt = conn.prepare(
                "SELECT edge_key, src_node_key, edge_kind, dst_node_key, data_json
                 FROM graph_edges
                 WHERE src_node_key = ?1 AND edge_kind = ?2
                 ORDER BY edge_key ASC",
            )?;
            let rows = stmt.query_map(params![src_node_key, kind], decode_row)?;
            for row in rows {
                out.push(row?);
            }
        } else {
            let mut stmt = conn.prepare(
                "SELECT edge_key, src_node_key, edge_kind, dst_node_key, data_json
                 FROM graph_edges
                 WHERE src_node_key = ?1
                 ORDER BY edge_key ASC",
            )?;
            let rows = stmt.query_map(params![src_node_key], decode_row)?;
            for row in rows {
                out.push(row?);
            }
        }

        Ok(out)
    }
}

#[derive(Clone)]
pub struct SqliteGraphStorage {
    conn: Arc<Mutex<Connection>>,
    adapter: SqliteGraphAdapter,
}

impl SqliteGraphStorage {
    pub fn open(db_path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(db_path).context("Failed to open GraphStorage SQLite DB")?;
        let conn = Arc::new(Mutex::new(conn));
        let adapter = SqliteGraphAdapter::new(conn.clone());
        let s = Self { conn, adapter };
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

            CREATE TABLE IF NOT EXISTS vault_secrets (
                secret_ref TEXT PRIMARY KEY,
                secret_kind TEXT NOT NULL,
                scope TEXT NOT NULL,
                allowed_roles_json TEXT NOT NULL,
                allowed_guests_json TEXT NOT NULL,
                ciphertext_b64 TEXT NOT NULL,
                nonce_b64 TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS hotels (
                hotel_name TEXT PRIMARY KEY,
                capabilities_json TEXT NOT NULL,
                mesh_port INTEGER NOT NULL,
                blob_port INTEGER NOT NULL,
                execution_port INTEGER NOT NULL DEFAULT 0,
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
                authority_hotel TEXT NOT NULL DEFAULT '',
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
        let _ = conn.execute(
            "ALTER TABLE hotels ADD COLUMN execution_port INTEGER NOT NULL DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE agent_identities ADD COLUMN authority_hotel TEXT NOT NULL DEFAULT ''",
            [],
        );
        drop(conn);
        self.adapter.init_schema()?;

        info!("GraphStorage schema initialized successfully.");
        Ok(())
    }

    /// Expose a raw lock for edge-case operations that don't fit the trait
    /// (e.g., ad-hoc config seeding in `main.rs`).
    pub fn raw_conn(&self) -> &Arc<Mutex<Connection>> {
        &self.conn
    }

    fn config_node_key(key: &str) -> String {
        format!("config:{key}")
    }

    fn hotel_node_key(hotel_name: &str) -> String {
        format!("hotel:{hotel_name}")
    }

    fn guest_node_key(hotel_name: &str, guest_id: &str) -> String {
        format!("hotel:{hotel_name}:guest:{guest_id}")
    }

    fn hotel_guest_edge_key(hotel_name: &str, guest_id: &str) -> String {
        format!("edge:hotel:{hotel_name}:has_guest:{guest_id}")
    }

    fn agent_node_key(agent_id: &str) -> String {
        format!("agent:{agent_id}")
    }

    fn agent_identity_node_key(agent_id: &str) -> String {
        format!("agent:{agent_id}:identity")
    }

    fn apartment_node_key(agent_id: &str, memory_type: &str) -> String {
        format!("agent:{agent_id}:apartment:{memory_type}")
    }

    fn agent_apartment_edge_key(agent_id: &str, memory_type: &str) -> String {
        format!("edge:agent:{agent_id}:owns_apartment:{memory_type}")
    }

    fn session_node_key(session_id: &str) -> String {
        format!("session:{session_id}")
    }

    fn session_participant_node_key(session_id: &str, component_id: &str) -> String {
        format!("session:{session_id}:participant:{component_id}")
    }

    fn session_turn_node_key(session_id: &str, turn_id: &str) -> String {
        format!("session:{session_id}:turn:{turn_id}")
    }

    fn session_event_node_key(session_id: &str, event_id: &str) -> String {
        format!("session:{session_id}:event:{event_id}")
    }

    fn session_participant_edge_key(session_id: &str, component_id: &str) -> String {
        format!("edge:session:{session_id}:has_participant:{component_id}")
    }

    fn session_turn_edge_key(session_id: &str, turn_id: &str) -> String {
        format!("edge:session:{session_id}:has_turn:{turn_id}")
    }

    fn session_event_edge_key(session_id: &str, event_id: &str) -> String {
        format!("edge:session:{session_id}:has_event:{event_id}")
    }

    fn turn_event_edge_key(session_id: &str, turn_id: &str, event_id: &str) -> String {
        format!("edge:session:{session_id}:turn:{turn_id}:has_event:{event_id}")
    }
}

impl GraphStorage for SqliteGraphStorage {
    fn load_node_capabilities(&self) -> Result<Option<NodeCapabilities>> {
        match self.get_config_value("capabilities")? {
            Some(json) => Ok(Some(serde_json::from_str(&json)?)),
            None => Ok(None),
        }
    }

    fn save_node_capabilities(&self, caps: &NodeCapabilities) -> Result<()> {
        self.set_config_value("capabilities", &serde_json::to_string(caps)?)
    }

    fn get_config_value(&self, key: &str) -> Result<Option<String>> {
        if let Some(node) = self.adapter.get_node(&Self::config_node_key(key))? {
            if let Some(value) = node.data.get("value_json").and_then(|v| v.as_str()) {
                return Ok(Some(value.to_string()));
            }
        }

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT value_json FROM node_config WHERE key = ?1")?;
        let mut rows = stmt.query(params![key])?;

        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    fn set_config_value(&self, key: &str, value_json: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO node_config (key, value_json, updated_at)
             VALUES (?1, ?2, CURRENT_TIMESTAMP)
             ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json, updated_at=CURRENT_TIMESTAMP",
            params![key, value_json],
        )?;
        drop(conn);

        self.adapter.upsert_node(&GraphNode {
            node_key: Self::config_node_key(key),
            kind: "config".into(),
            label: Some(key.to_string()),
            data: serde_json::json!({
                "key": key,
                "value_json": value_json,
            }),
        })
    }

    fn upsert_secret(&self, secret: &SecretRecord) -> Result<()> {
        let allowed_roles_json = serde_json::to_string(&secret.allowed_roles)?;
        let allowed_guests_json = serde_json::to_string(&secret.allowed_guests)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO vault_secrets (
                secret_ref, secret_kind, scope, allowed_roles_json, allowed_guests_json,
                ciphertext_b64, nonce_b64, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(secret_ref) DO UPDATE SET
                secret_kind = excluded.secret_kind,
                scope = excluded.scope,
                allowed_roles_json = excluded.allowed_roles_json,
                allowed_guests_json = excluded.allowed_guests_json,
                ciphertext_b64 = excluded.ciphertext_b64,
                nonce_b64 = excluded.nonce_b64,
                updated_at = excluded.updated_at",
            params![
                secret.secret_ref,
                secret.secret_kind,
                secret.scope,
                allowed_roles_json,
                allowed_guests_json,
                secret.ciphertext_b64,
                secret.nonce_b64,
                secret.created_at,
                secret.updated_at,
            ],
        )?;
        Ok(())
    }

    fn get_secret(&self, secret_ref: &str) -> Result<Option<SecretRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT
                secret_kind, scope, allowed_roles_json, allowed_guests_json,
                ciphertext_b64, nonce_b64, created_at, updated_at
             FROM vault_secrets
             WHERE secret_ref = ?1",
        )?;
        let mut rows = stmt.query(params![secret_ref])?;

        if let Some(row) = rows.next()? {
            Ok(Some(SecretRecord {
                secret_ref: secret_ref.to_string(),
                secret_kind: row.get(0)?,
                scope: row.get(1)?,
                allowed_roles: serde_json::from_str::<Vec<String>>(&row.get::<_, String>(2)?)?,
                allowed_guests: serde_json::from_str::<Vec<String>>(&row.get::<_, String>(3)?)?,
                ciphertext_b64: row.get(4)?,
                nonce_b64: row.get(5)?,
                created_at: row.get(6)?,
                updated_at: row.get(7)?,
            }))
        } else {
            Ok(None)
        }
    }

    fn get_hotel(&self, hotel_name: &str) -> Result<Option<HotelRecord>> {
        if let Some(node) = self.adapter.get_node(&Self::hotel_node_key(hotel_name))? {
            return Ok(Some(decode_hotel_record(node.data, hotel_name)?));
        }

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT capabilities_json, mesh_port, blob_port, execution_port, ipc_socket_path, active_pid
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
                execution_port: row.get(3)?,
                ipc_socket_path: row.get(4)?,
                active_pid: row.get(5).unwrap_or(None),
            }))
        } else {
            Ok(None)
        }
    }

    fn list_hotels(&self) -> Result<Vec<HotelRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT hotel_name, capabilities_json, mesh_port, blob_port, execution_port, ipc_socket_path, active_pid
             FROM hotels
             ORDER BY hotel_name ASC",
        )?;
        let mut rows = stmt.query([])?;
        let mut hotels = Vec::new();

        while let Some(row) = rows.next()? {
            let capabilities_json: String = row.get(1)?;
            let capabilities: NodeCapabilities = serde_json::from_str(&capabilities_json)?;
            hotels.push(HotelRecord {
                hotel_name: row.get(0)?,
                capabilities,
                mesh_port: row.get(2)?,
                blob_port: row.get(3)?,
                execution_port: row.get(4)?,
                ipc_socket_path: row.get(5)?,
                active_pid: row.get(6).unwrap_or(None),
            });
        }

        Ok(hotels)
    }

    fn upsert_hotel(&self, hotel: &HotelRecord) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let capabilities_json = serde_json::to_string(&hotel.capabilities)?;
        conn.execute(
            "INSERT INTO hotels (hotel_name, capabilities_json, mesh_port, blob_port, execution_port, ipc_socket_path, active_pid, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, CURRENT_TIMESTAMP)
             ON CONFLICT(hotel_name) DO UPDATE SET
             capabilities_json = excluded.capabilities_json,
             mesh_port = excluded.mesh_port,
             blob_port = excluded.blob_port,
             execution_port = excluded.execution_port,
             ipc_socket_path = excluded.ipc_socket_path,
             active_pid = excluded.active_pid,
             updated_at = CURRENT_TIMESTAMP",
            params![
                hotel.hotel_name,
                capabilities_json,
                hotel.mesh_port,
                hotel.blob_port,
                hotel.execution_port,
                hotel.ipc_socket_path,
                hotel.active_pid
            ],
        )?;
        drop(conn);

        self.adapter.upsert_node(&GraphNode {
            node_key: Self::hotel_node_key(&hotel.hotel_name),
            kind: "hotel".into(),
            label: Some(hotel.hotel_name.clone()),
            data: serde_json::to_value(hotel)?,
        })
    }

    fn set_hotel_pid(&self, hotel_name: &str, pid: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE hotels SET active_pid = ?1, updated_at = CURRENT_TIMESTAMP WHERE hotel_name = ?2",
            params![pid, hotel_name],
        )?;
        drop(conn);

        if let Some(mut hotel) = self.get_hotel(hotel_name)? {
            hotel.active_pid = pid.map(str::to_string);
            self.adapter.upsert_node(&GraphNode {
                node_key: Self::hotel_node_key(hotel_name),
                kind: "hotel".into(),
                label: Some(hotel_name.to_string()),
                data: serde_json::to_value(hotel)?,
            })?;
        }
        Ok(())
    }

    fn list_guests(&self, hotel_name: &str, active_only: bool) -> Result<Vec<GuestRecord>> {
        let edges = self
            .adapter
            .list_edges_from(&Self::hotel_node_key(hotel_name), Some("HAS_GUEST"))?;
        if !edges.is_empty() {
            let mut out = Vec::new();
            for edge in edges {
                if let Some(node) = self.adapter.get_node(&edge.dst_node_key)? {
                    let guest: GuestRecord = serde_json::from_value(node.data)?;
                    if !active_only || guest.is_active {
                        out.push(guest);
                    }
                }
            }
            out.sort_by(|a, b| a.guest_id.cmp(&b.guest_id));
            return Ok(out);
        }

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
        drop(conn);

        let guest_key = Self::guest_node_key(hotel_name, guest_id);
        if let Some(node) = self.adapter.get_node(&guest_key)? {
            let mut guest: GuestRecord = serde_json::from_value(node.data)?;
            guest.active_pid = pid.map(str::to_string);
            self.adapter.upsert_node(&GraphNode {
                node_key: guest_key,
                kind: "guest".into(),
                label: Some(guest.guest_id.clone()),
                data: serde_json::to_value(guest)?,
            })?;
        }
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
        drop(conn);

        for g in guests {
            let guest = GuestRecord {
                hotel_name: hotel_name.to_string(),
                ..g.clone()
            };
            let guest_key = Self::guest_node_key(hotel_name, &guest.guest_id);
            self.adapter.upsert_node(&GraphNode {
                node_key: guest_key.clone(),
                kind: "guest".into(),
                label: Some(guest.guest_id.clone()),
                data: serde_json::to_value(&guest)?,
            })?;
            self.adapter.upsert_edge(&GraphEdge {
                edge_key: Self::hotel_guest_edge_key(hotel_name, &guest.guest_id),
                src_node_key: Self::hotel_node_key(hotel_name),
                edge_kind: "HAS_GUEST".into(),
                dst_node_key: guest_key,
                data: serde_json::json!({}),
            })?;
        }
        Ok(())
    }

    fn upsert_agent_identity(&self, identity: &crate::storage::AgentIdentityRecord) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let bundle_json = serde_json::to_string(&identity.bundle_json)?;
        conn.execute(
            "INSERT INTO agent_identities (agent_id, persona_name, authority_hotel, bundle_json)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(agent_id) DO UPDATE SET
             persona_name = excluded.persona_name,
             authority_hotel = excluded.authority_hotel,
             bundle_json = excluded.bundle_json",
            params![
                identity.agent_id,
                identity.persona_name,
                identity.authority_hotel,
                bundle_json
            ],
        )?;
        drop(conn);

        self.adapter.upsert_node(&GraphNode {
            node_key: Self::agent_identity_node_key(&identity.agent_id),
            kind: "agent_identity".into(),
            label: Some(identity.persona_name.clone()),
            data: serde_json::to_value(identity)?,
        })?;

        self.adapter.upsert_node(&GraphNode {
            node_key: Self::agent_node_key(&identity.agent_id),
            kind: "agent".into(),
            label: Some(identity.agent_id.clone()),
            data: serde_json::json!({
                "agent_id": identity.agent_id,
                "persona_name": identity.persona_name,
            }),
        })?;

        self.adapter.upsert_edge(&GraphEdge {
            edge_key: format!("edge:agent:{}:has_identity", identity.agent_id),
            src_node_key: Self::agent_node_key(&identity.agent_id),
            edge_kind: "HAS_IDENTITY".into(),
            dst_node_key: Self::agent_identity_node_key(&identity.agent_id),
            data: serde_json::json!({}),
        })?;

        Ok(())
    }

    fn get_agent_identity(
        &self,
        agent_id: &str,
    ) -> Result<Option<crate::storage::AgentIdentityRecord>> {
        if let Some(node) = self
            .adapter
            .get_node(&Self::agent_identity_node_key(agent_id))?
        {
            return Ok(Some(serde_json::from_value(node.data)?));
        }

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT agent_id, persona_name, authority_hotel, bundle_json FROM agent_identities WHERE agent_id = ?1",
        )?;
        let mut rows = stmt.query(params![agent_id])?;
        if let Some(row) = rows.next()? {
            let bundle_json: String = row.get(3)?;
            Ok(Some(crate::storage::AgentIdentityRecord {
                agent_id: row.get(0)?,
                persona_name: row.get(1)?,
                authority_hotel: row.get(2)?,
                bundle_json: serde_json::from_str(&bundle_json)
                    .unwrap_or_else(|_| serde_json::json!({})),
            }))
        } else {
            Ok(None)
        }
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
        drop(conn);

        let agent_key = Self::agent_node_key(agent_id);
        self.adapter.upsert_node(&GraphNode {
            node_key: agent_key.clone(),
            kind: "agent".into(),
            label: Some(agent_id.to_string()),
            data: serde_json::json!({
                "agent_id": agent_id,
            }),
        })?;
        let apartment_key = Self::apartment_node_key(agent_id, memory_type);
        self.adapter.upsert_node(&GraphNode {
            node_key: apartment_key.clone(),
            kind: "memory_apartment".into(),
            label: Some(format!("{agent_id}:{memory_type}")),
            data: serde_json::json!({
                "agent_id": agent_id,
                "memory_type": memory_type,
                "content_json": content_json,
            }),
        })?;
        self.adapter.upsert_edge(&GraphEdge {
            edge_key: Self::agent_apartment_edge_key(agent_id, memory_type),
            src_node_key: agent_key,
            edge_kind: "OWNS_APARTMENT".into(),
            dst_node_key: apartment_key,
            data: serde_json::json!({}),
        })?;

        debug!(
            "Synchronized Memory Apartment for {} ({})",
            agent_id, memory_type
        );
        Ok(())
    }

    fn get_apartment(
        &self,
        agent_id: &str,
        memory_type: &str,
    ) -> Result<Option<serde_json::Value>> {
        if let Some(node) = self
            .adapter
            .get_node(&Self::apartment_node_key(agent_id, memory_type))?
        {
            if let Some(content) = node.data.get("content_json") {
                return Ok(Some(content.clone()));
            }
        }

        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT content FROM memory_apartments
             WHERE agent_id = ?1 AND memory_type = ?2
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query(params![agent_id, memory_type])?;
        if let Some(row) = rows.next()? {
            let content: String = row.get(0)?;
            Ok(Some(serde_json::from_str(&content)?))
        } else {
            Ok(None)
        }
    }

    fn upsert_session(&self, session: &SessionRecord) -> Result<()> {
        self.adapter.upsert_node(&GraphNode {
            node_key: Self::session_node_key(&session.session_id),
            kind: "session".into(),
            label: Some(session.session_id.clone()),
            data: serde_json::to_value(session)?,
        })
    }

    fn get_session(&self, session_id: &str) -> Result<Option<SessionRecord>> {
        match self.adapter.get_node(&Self::session_node_key(session_id))? {
            Some(node) => Ok(Some(serde_json::from_value(node.data)?)),
            None => Ok(None),
        }
    }

    fn upsert_role_incarnation(&self, role: &RoleIncarnationRecord) -> Result<()> {
        self.adapter.upsert_node(&GraphNode {
            node_key: format!("role_incarnation:{}:{}", role.agent_id, role.role_name),
            kind: "role_incarnation".into(),
            label: Some(role.guest_id.clone()),
            data: serde_json::to_value(role)?,
        })
    }

    fn get_role_incarnation(
        &self,
        agent_id: &str,
        role_name: &str,
    ) -> Result<Option<RoleIncarnationRecord>> {
        match self
            .adapter
            .get_node(&format!("role_incarnation:{agent_id}:{role_name}"))?
        {
            Some(node) => Ok(Some(serde_json::from_value(node.data)?)),
            None => Ok(None),
        }
    }

    fn list_role_incarnations(&self, agent_id: &str) -> Result<Vec<RoleIncarnationRecord>> {
        self.adapter
            .list_nodes_by_kind("role_incarnation")?
            .into_iter()
            .map(|node| {
                serde_json::from_value::<RoleIncarnationRecord>(node.data).map_err(Into::into)
            })
            .filter(|result| {
                result
                    .as_ref()
                    .map(|role| role.agent_id == agent_id)
                    .unwrap_or(true)
            })
            .collect()
    }

    fn upsert_session_participant(&self, participant: &SessionParticipantRecord) -> Result<()> {
        let participant_key =
            Self::session_participant_node_key(&participant.session_id, &participant.component_id);
        self.adapter.upsert_node(&GraphNode {
            node_key: participant_key.clone(),
            kind: "session_participant".into(),
            label: Some(participant.component_id.clone()),
            data: serde_json::to_value(participant)?,
        })?;
        self.adapter.upsert_edge(&GraphEdge {
            edge_key: Self::session_participant_edge_key(
                &participant.session_id,
                &participant.component_id,
            ),
            src_node_key: Self::session_node_key(&participant.session_id),
            edge_kind: "HAS_PARTICIPANT".into(),
            dst_node_key: participant_key,
            data: serde_json::json!({
                "role": participant.role,
            }),
        })
    }

    fn list_session_participants(&self, session_id: &str) -> Result<Vec<SessionParticipantRecord>> {
        let mut out: Vec<SessionParticipantRecord> = Vec::new();
        for edge in self
            .adapter
            .list_edges_from(&Self::session_node_key(session_id), Some("HAS_PARTICIPANT"))?
        {
            if let Some(node) = self.adapter.get_node(&edge.dst_node_key)? {
                out.push(serde_json::from_value(node.data)?);
            }
        }
        out.sort_by(|a, b| a.component_id.cmp(&b.component_id));
        Ok(out)
    }

    fn upsert_session_turn(&self, turn: &SessionTurnRecord) -> Result<()> {
        let turn_key = Self::session_turn_node_key(&turn.session_id, &turn.turn_id);
        self.adapter.upsert_node(&GraphNode {
            node_key: turn_key.clone(),
            kind: "session_turn".into(),
            label: Some(turn.turn_id.clone()),
            data: serde_json::to_value(turn)?,
        })?;
        self.adapter.upsert_edge(&GraphEdge {
            edge_key: Self::session_turn_edge_key(&turn.session_id, &turn.turn_id),
            src_node_key: Self::session_node_key(&turn.session_id),
            edge_kind: "HAS_TURN".into(),
            dst_node_key: turn_key,
            data: serde_json::json!({
                "status": turn.status,
            }),
        })
    }

    fn get_session_turn(
        &self,
        session_id: &str,
        turn_id: &str,
    ) -> Result<Option<SessionTurnRecord>> {
        match self
            .adapter
            .get_node(&Self::session_turn_node_key(session_id, turn_id))?
        {
            Some(node) => Ok(Some(serde_json::from_value(node.data)?)),
            None => Ok(None),
        }
    }

    fn list_session_turns(&self, session_id: &str, limit: usize) -> Result<Vec<SessionTurnRecord>> {
        let mut out: Vec<SessionTurnRecord> = Vec::new();
        for edge in self
            .adapter
            .list_edges_from(&Self::session_node_key(session_id), Some("HAS_TURN"))?
        {
            if let Some(node) = self.adapter.get_node(&edge.dst_node_key)? {
                out.push(serde_json::from_value(node.data)?);
            }
        }
        out.sort_by(|a: &SessionTurnRecord, b: &SessionTurnRecord| {
            a.started_at
                .unwrap_or(0)
                .cmp(&b.started_at.unwrap_or(0))
                .then_with(|| a.turn_id.cmp(&b.turn_id))
        });
        if out.len() > limit {
            let drain = out.len() - limit;
            out.drain(0..drain);
        }
        Ok(out)
    }

    fn append_session_event(&self, event: &SessionEventRecord) -> Result<()> {
        let event_key = Self::session_event_node_key(&event.session_id, &event.event_id);
        self.adapter.upsert_node(&GraphNode {
            node_key: event_key.clone(),
            kind: "session_event".into(),
            label: Some(event.event_id.clone()),
            data: serde_json::to_value(event)?,
        })?;
        self.adapter.upsert_edge(&GraphEdge {
            edge_key: Self::session_event_edge_key(&event.session_id, &event.event_id),
            src_node_key: Self::session_node_key(&event.session_id),
            edge_kind: "HAS_EVENT".into(),
            dst_node_key: event_key.clone(),
            data: serde_json::json!({
                "kind": event.kind,
                "created_at": event.created_at,
            }),
        })?;

        if let Some(turn_id) = event.turn_id.as_deref() {
            self.adapter.upsert_edge(&GraphEdge {
                edge_key: Self::turn_event_edge_key(&event.session_id, turn_id, &event.event_id),
                src_node_key: Self::session_turn_node_key(&event.session_id, turn_id),
                edge_kind: "HAS_EVENT".into(),
                dst_node_key: event_key,
                data: serde_json::json!({
                    "kind": event.kind,
                    "created_at": event.created_at,
                }),
            })?;
        }

        Ok(())
    }

    fn list_session_events(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<SessionEventRecord>> {
        let mut out: Vec<SessionEventRecord> = Vec::new();
        for edge in self
            .adapter
            .list_edges_from(&Self::session_node_key(session_id), Some("HAS_EVENT"))?
        {
            if let Some(node) = self.adapter.get_node(&edge.dst_node_key)? {
                out.push(serde_json::from_value(node.data)?);
            }
        }
        out.sort_by(|a: &SessionEventRecord, b: &SessionEventRecord| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.event_id.cmp(&b.event_id))
        });
        if out.len() > limit {
            let drain = out.len() - limit;
            out.drain(0..drain);
        }
        Ok(out)
    }

    fn upsert_abstract_tool(&self, tool: &AbstractToolRecord) -> Result<()> {
        self.adapter.upsert_node(&GraphNode {
            node_key: format!("abstract_tool:{}", tool.tool_name),
            kind: "abstract_tool".into(),
            label: Some(tool.tool_name.clone()),
            data: serde_json::to_value(tool)?,
        })
    }

    fn get_abstract_tool(&self, tool_name: &str) -> Result<Option<AbstractToolRecord>> {
        match self
            .adapter
            .get_node(&format!("abstract_tool:{tool_name}"))?
        {
            Some(node) => Ok(Some(serde_json::from_value(node.data)?)),
            None => Ok(None),
        }
    }

    fn list_abstract_tools(&self) -> Result<Vec<AbstractToolRecord>> {
        self.adapter
            .list_nodes_by_kind("abstract_tool")?
            .into_iter()
            .map(|node| serde_json::from_value(node.data).map_err(Into::into))
            .collect()
    }

    fn upsert_abstract_skill(&self, skill: &AbstractSkillRecord) -> Result<()> {
        self.adapter.upsert_node(&GraphNode {
            node_key: format!("abstract_skill:{}", skill.skill_name),
            kind: "abstract_skill".into(),
            label: Some(skill.skill_name.clone()),
            data: serde_json::to_value(skill)?,
        })
    }

    fn get_abstract_skill(&self, skill_name: &str) -> Result<Option<AbstractSkillRecord>> {
        match self
            .adapter
            .get_node(&format!("abstract_skill:{skill_name}"))?
        {
            Some(node) => Ok(Some(serde_json::from_value(node.data)?)),
            None => Ok(None),
        }
    }

    fn list_abstract_skills(&self) -> Result<Vec<AbstractSkillRecord>> {
        self.adapter
            .list_nodes_by_kind("abstract_skill")?
            .into_iter()
            .map(|node| serde_json::from_value(node.data).map_err(Into::into))
            .collect()
    }
}

fn decode_hotel_record(data: serde_json::Value, hotel_name: &str) -> Result<HotelRecord> {
    #[derive(Deserialize)]
    struct LegacyHotelRecord {
        hotel_name: Option<String>,
        capabilities: NodeCapabilities,
        mesh_port: u16,
        blob_port: u16,
        execution_port: Option<u16>,
        ipc_socket_path: String,
        active_pid: Option<String>,
    }

    match serde_json::from_value::<HotelRecord>(data.clone()) {
        Ok(hotel) => Ok(hotel),
        Err(_) => {
            let legacy: LegacyHotelRecord = serde_json::from_value(data)?;
            Ok(HotelRecord {
                hotel_name: legacy.hotel_name.unwrap_or_else(|| hotel_name.to_string()),
                capabilities: legacy.capabilities,
                mesh_port: legacy.mesh_port,
                blob_port: legacy.blob_port,
                execution_port: legacy
                    .execution_port
                    .unwrap_or_else(|| legacy.blob_port.saturating_add(1)),
                ipc_socket_path: legacy.ipc_socket_path,
                active_pid: legacy.active_pid,
            })
        }
    }
}
