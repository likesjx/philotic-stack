use crate::event::EventEnvelope;
use rusqlite::{params, Connection, Result as SqlResult};
use std::path::Path;

use std::sync::{Arc, Mutex};

/// Manages the local persistent store for outbound durable events.
#[derive(Clone)]
pub struct EventLedger {
    pub conn: Arc<Mutex<Connection>>,
}

impl EventLedger {
    pub fn open(db_path: impl AsRef<Path>) -> SqlResult<Self> {
        let conn = Connection::open(db_path)?;
        let ledger = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        ledger.init_schema()?;
        Ok(ledger)
    }

    fn init_schema(&self) -> SqlResult<()> {
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

    /// Durably commit a new event. Returns the assigned sequence monotonically strictly ordered.
    pub fn append_event(&self, env: &mut EventEnvelope) -> SqlResult<u64> {
        let (payload_type, payload_json) = match &env.payload {
            crate::event::EventPayload::Inline { data } => ("inline", data.clone()),
            crate::event::EventPayload::BlobRef { blob_id, size, mime, source_hotel_ip } => {
                ("attachment", serde_json::json!({ "blob_id": blob_id, "size": size, "mime": mime, "source_hotel_ip": source_hotel_ip }).to_string())
            }
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
            Err(err) => return Err(err),
        }

        let seq = conn.last_insert_rowid() as u64;
        env.seq = seq;

        // info!("Committed Event {} at SEQ {}", env.event_id, seq);
        Ok(seq)
    }

    /// Discard an event definitively (upon processing completion).
    pub fn delete_event(&self, event_id: &crate::event::EventId) -> SqlResult<usize> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM mesh_events WHERE event_id = ?1",
            params![event_id.to_string()],
        )
    }

    /// Fetch events that have a sequence number greater than the provided cursor.
    pub fn query_unacked_events(
        &self,
        target_node_id: &str,
        cursor_seq: u64,
        limit: u32,
    ) -> SqlResult<Vec<crate::event::EventEnvelope>> {
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

            let payload0 = match payload_type.as_str() {
                "inline" => crate::event::EventPayload::Inline { data: payload_json },
                "attachment" => {
                    let v: serde_json::Value =
                        serde_json::from_str(&payload_json).unwrap_or(serde_json::json!({}));
                    crate::event::EventPayload::BlobRef {
                        blob_id: v["blob_id"].as_str().unwrap_or("").to_string(),
                        size: v["size"].as_u64().unwrap_or(0),
                        mime: v["mime"].as_str().unwrap_or("").to_string(),
                        source_hotel_ip: v["source_hotel_ip"].as_str().unwrap_or("").to_string(),
                    }
                }
                _ => crate::event::EventPayload::Inline { data: payload_json }, // Fallback
            };

            let trace_json: String = row.get(13)?;
            let trace0: Vec<String> = serde_json::from_str(&trace_json).unwrap_or_else(|_| vec![]);

            let kind_str: String = row.get(6)?;
            // Add quotes to make it a valid JSON string for Enum deserialization
            let kind_json = format!("\"{}\"", kind_str);
            let kind0: crate::event::EventKind =
                serde_json::from_str(&kind_json).unwrap_or(crate::event::EventKind::TaskInvoke);

            events.push(crate::event::EventEnvelope {
                seq: row.get(0)?,
                event_id: uuid::Uuid::parse_str(&row.get::<_, String>(1)?).unwrap_or_default(),
                source_node_id: row.get(2)?,
                target_node_id: row.get(3)?,
                source_agent_id: row.get(4)?,
                target_agent_id: row.get(5)?,
                kind: kind0,
                corr_id: row.get(7)?,
                attempt: row.get(8)?,
                created_at: row.get(9)?,
                expires_at: row.get(10)?,
                payload: payload0,
                trace: trace0,
            });
        }

        Ok(events)
    }
}
