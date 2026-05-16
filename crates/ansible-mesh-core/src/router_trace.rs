//! `RouterTraceStorage` — append-only training-tap for model-router decisions.
//!
//! Every routing decision made by the model-router is recorded here as a
//! `RouterTrainingRecord`. This is the raw input to the RL flywheel (Seam 6).
//!
//! # Authority
//!
//! Only `model-router` writes. Reads are for local inspection and future
//! RL pipeline consumption. No mesh sync — traces are node-local by design.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};

// ──────────────────────────────────────────────────────────────────────────────
// Records
// ──────────────────────────────────────────────────────────────────────────────

/// A single routing decision recorded by the model-router.
///
/// Fields are kept narrow and flat for cheap SQL storage and easy DataFrame
/// construction. Optional fields default to `None` when not yet tracked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouterTrainingRecord {
    /// ULID — lexicographically sortable, unique.
    pub trace_id: String,
    /// Agent whose session produced this routing call.
    pub agent_id: String,
    /// Session identifier (if available in task context).
    pub session_id: String,
    /// Turn identifier (if available in task context).
    pub turn_id: String,
    /// Provider that handled the request (e.g. `"gemini"`, `"elevenlabs"`, `"stub"`).
    pub provider_id: String,
    /// Specific model used, if the provider reported it.
    #[serde(default)]
    pub model_id: Option<String>,
    /// Task capability kind (e.g. `"text.generate"`, `"voice.synthesize"`).
    pub task_kind: String,
    /// Routing outcome: `"success"`, `"tool_call"`, `"failure"`.
    pub outcome: String,
    /// Provider-reported failure code or short description on failure.
    #[serde(default)]
    pub failure_code: Option<String>,
    /// Wall-clock time from task dispatch to provider response (milliseconds).
    #[serde(default)]
    pub latency_ms: Option<u64>,
    /// Total tokens consumed (prompt + completion), if reported by the provider.
    #[serde(default)]
    pub token_count: Option<u64>,
    /// Unix epoch (seconds) when the routing call was made.
    pub timestamp: u64,
}

// ──────────────────────────────────────────────────────────────────────────────
// Trait
// ──────────────────────────────────────────────────────────────────────────────

/// Append-only storage for router training records.
pub trait RouterTraceStorage: Send + Sync {
    /// Append a routing record to the store.
    fn record_trace(&self, record: &RouterTrainingRecord) -> Result<()>;

    /// Return the most recent `limit` records, newest first.
    fn list_traces(&self, limit: usize) -> Result<Vec<RouterTrainingRecord>>;

    /// Return the most recent `limit` records for a specific agent, newest first.
    fn list_traces_by_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<RouterTrainingRecord>>;

    /// Return the most recent `limit` records for a specific provider, newest first.
    fn list_traces_by_provider(
        &self,
        provider_id: &str,
        limit: usize,
    ) -> Result<Vec<RouterTrainingRecord>>;
}

// ──────────────────────────────────────────────────────────────────────────────
// SqliteRouterTraceStorage
// ──────────────────────────────────────────────────────────────────────────────

/// SQLite-backed router trace store.
///
/// One shared file per hotel. Created on first use; append-only schema.
pub struct SqliteRouterTraceStorage {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteRouterTraceStorage {
    /// Open (or create) the router trace database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "
            BEGIN;

            CREATE TABLE IF NOT EXISTS router_traces (
                trace_id     TEXT PRIMARY KEY,
                agent_id     TEXT NOT NULL,
                session_id   TEXT NOT NULL DEFAULT '',
                turn_id      TEXT NOT NULL DEFAULT '',
                provider_id  TEXT NOT NULL,
                model_id     TEXT,
                task_kind    TEXT NOT NULL,
                outcome      TEXT NOT NULL,
                failure_code TEXT,
                latency_ms   INTEGER,
                token_count  INTEGER,
                timestamp    INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_router_traces_ts
                ON router_traces (timestamp DESC);

            CREATE INDEX IF NOT EXISTS idx_router_traces_agent
                ON router_traces (agent_id, timestamp DESC);

            CREATE INDEX IF NOT EXISTS idx_router_traces_provider
                ON router_traces (provider_id, timestamp DESC);

            COMMIT;
            ",
        )?;
        // Idempotent migration for existing databases.
        let _ = conn.execute(
            "ALTER TABLE router_traces ADD COLUMN token_count INTEGER",
            [],
        );
        Ok(())
    }
}

impl RouterTraceStorage for SqliteRouterTraceStorage {
    fn record_trace(&self, r: &RouterTrainingRecord) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO router_traces
             (trace_id, agent_id, session_id, turn_id, provider_id, model_id,
              task_kind, outcome, failure_code, latency_ms, token_count, timestamp)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                r.trace_id,
                r.agent_id,
                r.session_id,
                r.turn_id,
                r.provider_id,
                r.model_id,
                r.task_kind,
                r.outcome,
                r.failure_code,
                r.latency_ms.map(|v| v as i64),
                r.token_count.map(|v| v as i64),
                r.timestamp as i64,
            ],
        )?;
        Ok(())
    }

    fn list_traces(&self, limit: usize) -> Result<Vec<RouterTrainingRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT trace_id, agent_id, session_id, turn_id, provider_id, model_id,
                    task_kind, outcome, failure_code, latency_ms, token_count, timestamp
             FROM router_traces ORDER BY timestamp DESC LIMIT ?1",
        )?;
        collect_records(&mut stmt, params![limit as i64])
    }

    fn list_traces_by_agent(
        &self,
        agent_id: &str,
        limit: usize,
    ) -> Result<Vec<RouterTrainingRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT trace_id, agent_id, session_id, turn_id, provider_id, model_id,
                    task_kind, outcome, failure_code, latency_ms, token_count, timestamp
             FROM router_traces WHERE agent_id = ?1
             ORDER BY timestamp DESC LIMIT ?2",
        )?;
        collect_records(&mut stmt, params![agent_id, limit as i64])
    }

    fn list_traces_by_provider(
        &self,
        provider_id: &str,
        limit: usize,
    ) -> Result<Vec<RouterTrainingRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT trace_id, agent_id, session_id, turn_id, provider_id, model_id,
                    task_kind, outcome, failure_code, latency_ms, token_count, timestamp
             FROM router_traces WHERE provider_id = ?1
             ORDER BY timestamp DESC LIMIT ?2",
        )?;
        collect_records(&mut stmt, params![provider_id, limit as i64])
    }
}

fn collect_records(
    stmt: &mut rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
) -> Result<Vec<RouterTrainingRecord>> {
    let rows = stmt.query_map(params, |row| {
        Ok(RouterTrainingRecord {
            trace_id: row.get(0)?,
            agent_id: row.get(1)?,
            session_id: row.get(2)?,
            turn_id: row.get(3)?,
            provider_id: row.get(4)?,
            model_id: row.get(5)?,
            task_kind: row.get(6)?,
            outcome: row.get(7)?,
            failure_code: row.get(8)?,
            latency_ms: row.get::<_, Option<i64>>(9)?.map(|v| v as u64),
            token_count: row.get::<_, Option<i64>>(10)?.map(|v| v as u64),
            timestamp: row.get::<_, i64>(11)? as u64,
        })
    })?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn open_tmp() -> (SqliteRouterTraceStorage, NamedTempFile) {
        let f = NamedTempFile::new().unwrap();
        let s = SqliteRouterTraceStorage::open(f.path()).unwrap();
        (s, f)
    }

    fn make_record(n: u64, provider: &str, agent: &str, outcome: &str) -> RouterTrainingRecord {
        RouterTrainingRecord {
            trace_id: format!("trace-{n:04}"),
            agent_id: agent.into(),
            session_id: format!("sess-{n}"),
            turn_id: format!("turn-{n}"),
            provider_id: provider.into(),
            model_id: Some("gemini-2.0-flash".into()),
            task_kind: "text.generate".into(),
            outcome: outcome.into(),
            failure_code: None,
            latency_ms: Some(n * 100),
            token_count: None,
            timestamp: 1_000_000 + n,
        }
    }

    #[test]
    fn record_and_list() {
        let (s, _f) = open_tmp();
        for i in 0..5u64 {
            s.record_trace(&make_record(i, "gemini", "aria", "success"))
                .unwrap();
        }
        let traces = s.list_traces(3).unwrap();
        assert_eq!(traces.len(), 3);
        // Newest first.
        assert_eq!(traces[0].trace_id, "trace-0004");
    }

    #[test]
    fn filter_by_agent() {
        let (s, _f) = open_tmp();
        s.record_trace(&make_record(1, "gemini", "aria", "success"))
            .unwrap();
        s.record_trace(&make_record(2, "gemini", "bjork", "success"))
            .unwrap();
        s.record_trace(&make_record(3, "gemini", "aria", "tool_call"))
            .unwrap();

        let aria = s.list_traces_by_agent("aria", 10).unwrap();
        assert_eq!(aria.len(), 2);
        assert!(aria.iter().all(|r| r.agent_id == "aria"));
    }

    #[test]
    fn filter_by_provider() {
        let (s, _f) = open_tmp();
        s.record_trace(&make_record(1, "gemini", "aria", "success"))
            .unwrap();
        s.record_trace(&make_record(2, "elevenlabs", "aria", "success"))
            .unwrap();
        s.record_trace(&make_record(3, "gemini", "aria", "failure"))
            .unwrap();

        let gemini = s.list_traces_by_provider("gemini", 10).unwrap();
        assert_eq!(gemini.len(), 2);
        assert!(gemini.iter().all(|r| r.provider_id == "gemini"));
    }

    #[test]
    fn idempotent_insert() {
        let (s, _f) = open_tmp();
        let r = make_record(1, "gemini", "aria", "success");
        s.record_trace(&r).unwrap();
        s.record_trace(&r).unwrap(); // duplicate — should not error
        assert_eq!(s.list_traces(10).unwrap().len(), 1);
    }

    #[test]
    fn failure_record_with_code() {
        let (s, _f) = open_tmp();
        let mut r = make_record(1, "gemini", "aria", "failure");
        r.failure_code = Some("MODEL_INVALID_TOOL_CALL".into());
        s.record_trace(&r).unwrap();
        let traces = s.list_traces(1).unwrap();
        assert_eq!(
            traces[0].failure_code.as_deref(),
            Some("MODEL_INVALID_TOOL_CALL")
        );
    }

    #[test]
    fn router_listener_resource_type_round_trips() {
        use crate::resources::ResourceType;
        let json = serde_json::to_string(&ResourceType::RouterListener).unwrap();
        assert_eq!(json, r#""router_listener""#);
        let back: ResourceType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ResourceType::RouterListener);
    }
}
