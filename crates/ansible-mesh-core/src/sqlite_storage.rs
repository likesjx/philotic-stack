//! SQLite-backed implementations of the storage traits.
//!
//! These wrap the existing `EventLedger`, `CursorTracker`, and `ContextGraph`
//! behind the abstract `storage::*` traits so the Ansible daemon can
//! consume them as `Arc<dyn EventStorage>`, etc.

use crate::event::{EventEnvelope, EventId, EventKind, EventPayload};
use crate::graph::{GraphEdge, GraphNode};
use crate::storage::{CursorStorage, EventStorage, GraphAdapter};
use anyhow::{Context, Result};
use rusqlite::types::{Type, ValueRef};
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

    fn delete_delivered_events(&self, target_node_id: &str, max_seq: u64) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "DELETE FROM mesh_events WHERE target_node_id = ?1 AND seq <= ?2",
            params![target_node_id, max_seq],
        )?;
        Ok(n)
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
            CREATE INDEX IF NOT EXISTS idx_graph_nodes_kind_session_id
                ON graph_nodes(kind, json_extract(data_json, '$.session_id'));
            CREATE INDEX IF NOT EXISTS idx_graph_nodes_kind_status
                ON graph_nodes(kind, json_extract(data_json, '$.status'));
            CREATE INDEX IF NOT EXISTS idx_graph_nodes_kind_updated_at
                ON graph_nodes(kind, updated_at);
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
            let data_json = json_column_as_string(row, 3)?;
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
            let data_json = json_column_as_string(row, 3)?;
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

    fn list_nodes_by_kind_json_eq(
        &self,
        kind: &str,
        field: &str,
        value: &str,
        order_field: &str,
        limit: usize,
    ) -> Result<Vec<GraphNode>> {
        // json_extract paths are inlined so expression indexes can match;
        // field names come from domain-level constants, never user input.
        for f in [field, order_field] {
            if !f.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                anyhow::bail!("list_nodes_by_kind_json_eq: invalid json field name {f:?}");
            }
        }
        let sql = format!(
            "SELECT node_key, kind, label, data_json
             FROM graph_nodes
             WHERE kind = ?1 AND json_extract(data_json, '$.{field}') = ?2
             ORDER BY json_extract(data_json, '$.{order_field}') DESC
             LIMIT ?3",
        );
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(&sql)?;
        let limit_sql: i64 = if limit == 0 { -1 } else { limit as i64 };
        let rows = stmt.query_map(params![kind, value, limit_sql], |row| {
            let data_json = json_column_as_string(row, 3)?;
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
        // Query returns newest-first for the LIMIT; callers expect ascending.
        out.reverse();
        Ok(out)
    }

    fn delete_nodes_by_kind_older_than(
        &self,
        kind: &str,
        cutoff_unix_secs: u64,
        keep_json_field_eq: Option<(&str, &str)>,
    ) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let deleted = match keep_json_field_eq {
            None => conn.execute(
                "DELETE FROM graph_nodes
                 WHERE kind = ?1 AND updated_at < datetime(?2, 'unixepoch')",
                params![kind, cutoff_unix_secs as i64],
            )?,
            Some((field, value)) => {
                if !field.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                    anyhow::bail!(
                        "delete_nodes_by_kind_older_than: invalid json field name {field:?}"
                    );
                }
                let sql = format!(
                    "DELETE FROM graph_nodes
                     WHERE kind = ?1 AND updated_at < datetime(?2, 'unixepoch')
                       AND COALESCE(json_extract(data_json, '$.{field}'), '') <> ?3",
                );
                conn.execute(&sql, params![kind, cutoff_unix_secs as i64, value])?
            }
        };
        Ok(deleted)
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
            let data_json = json_column_as_string(row, 4)?;
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

fn json_column_as_string(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<String> {
    match row.get_ref(index)? {
        ValueRef::Text(bytes) => String::from_utf8(bytes.to_vec()).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(index, Type::Text, Box::new(err))
        }),
        ValueRef::Blob(bytes) => String::from_utf8(bytes.to_vec()).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(index, Type::Blob, Box::new(err))
        }),
        other => Err(rusqlite::Error::InvalidColumnType(
            index,
            String::new(),
            other.data_type(),
        )),
    }
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

    /// Open an in-memory SQLite database. Useful for tests.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()
            .context("Failed to open in-memory GraphStorage SQLite DB")?;
        let conn = Arc::new(Mutex::new(conn));
        let adapter = SqliteGraphAdapter::new(conn.clone());
        let s = Self { conn, adapter };
        s.init_schema()?;
        Ok(s)
    }

    /// Return a clone of the inner [`SqliteGraphAdapter`].
    ///
    /// Used to construct a [`crate::domain::GraphDomain`] backed by this
    /// storage instance — the adapter implements `GraphAdapter` and can be
    /// wrapped in `Arc<dyn GraphAdapter>`.
    pub fn adapter(&self) -> SqliteGraphAdapter {
        self.adapter.clone()
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
            "ALTER TABLE materialized_guests ADD COLUMN last_active_at INTEGER",
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
}
