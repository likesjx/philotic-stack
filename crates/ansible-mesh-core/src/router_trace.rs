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
    /// Shadow-mode (`PHILOTIC_SHADOW_ORACLE`): the routing oracle's top-ranked
    /// pick at dispatch time (`"role:provider"`), for oracle-vs-ladder
    /// agreement analysis. `None` when shadow mode was off or the oracle was
    /// unavailable. Log-only — never influenced the actual routing decision.
    #[serde(default)]
    pub oracle_pick: Option<String>,
    /// Shadow-mode: whether the oracle's top pick agreed with the ladder's
    /// resolved role. `None` when shadow mode was off or the oracle was
    /// unavailable (divergence-unknown).
    #[serde(default)]
    pub oracle_agreement: Option<bool>,
    /// Unix epoch (seconds) when the routing call was made.
    pub timestamp: u64,
}

// ──────────────────────────────────────────────────────────────────────────────
// Aggregates
// ──────────────────────────────────────────────────────────────────────────────

/// Per-provider routing performance summary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderStats {
    pub provider_id: String,
    pub task_kind: String,
    pub total_calls: u64,
    pub success_count: u64,
    pub failure_count: u64,
    /// 0.0–1.0 success fraction.
    pub success_rate: f64,
    pub avg_latency_ms: Option<f64>,
    pub p90_latency_ms: Option<u64>,
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

    /// Aggregate per-(provider, task_kind) stats over the last `window_secs` seconds.
    /// Pass `None` to aggregate all time.
    fn provider_stats(&self, window_secs: Option<u64>) -> Result<Vec<ProviderStats>>;
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
                oracle_pick      TEXT,
                oracle_agreement INTEGER,
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
        // Idempotent migrations for existing databases. Each ADD COLUMN is
        // guarded — SQLite errors with "duplicate column name" when the column
        // already exists, which we intentionally ignore so old DBs upgrade in
        // place and existing rows stay valid (new columns default to NULL).
        let _ = conn.execute(
            "ALTER TABLE router_traces ADD COLUMN token_count INTEGER",
            [],
        );
        let _ = conn.execute("ALTER TABLE router_traces ADD COLUMN oracle_pick TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE router_traces ADD COLUMN oracle_agreement INTEGER",
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
              task_kind, outcome, failure_code, latency_ms, token_count,
              oracle_pick, oracle_agreement, timestamp)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
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
                r.oracle_pick,
                r.oracle_agreement.map(|b| b as i64),
                r.timestamp as i64,
            ],
        )?;
        Ok(())
    }

    fn list_traces(&self, limit: usize) -> Result<Vec<RouterTrainingRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT trace_id, agent_id, session_id, turn_id, provider_id, model_id,
                    task_kind, outcome, failure_code, latency_ms, token_count,
                    oracle_pick, oracle_agreement, timestamp
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
                    task_kind, outcome, failure_code, latency_ms, token_count,
                    oracle_pick, oracle_agreement, timestamp
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
                    task_kind, outcome, failure_code, latency_ms, token_count,
                    oracle_pick, oracle_agreement, timestamp
             FROM router_traces WHERE provider_id = ?1
             ORDER BY timestamp DESC LIMIT ?2",
        )?;
        collect_records(&mut stmt, params![provider_id, limit as i64])
    }

    fn provider_stats(&self, window_secs: Option<u64>) -> Result<Vec<ProviderStats>> {
        let conn = self.conn.lock().unwrap();
        let since_ts: i64 = if let Some(secs) = window_secs {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            (now.saturating_sub(secs)) as i64
        } else {
            0
        };

        let mut stmt = conn.prepare(
            "SELECT provider_id, task_kind, outcome, latency_ms
             FROM router_traces
             WHERE timestamp >= ?1
             ORDER BY provider_id, task_kind",
        )?;

        #[derive(Debug)]
        struct Row {
            provider_id: String,
            task_kind: String,
            outcome: String,
            latency_ms: Option<i64>,
        }

        let rows: Vec<Row> = stmt
            .query_map(params![since_ts], |r| {
                Ok(Row {
                    provider_id: r.get(0)?,
                    task_kind: r.get(1)?,
                    outcome: r.get(2)?,
                    latency_ms: r.get(3)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        // Group by (provider_id, task_kind)
        use std::collections::HashMap;
        let mut buckets: HashMap<(String, String), (u64, u64, Vec<u64>)> = HashMap::new();
        for row in &rows {
            let key = (row.provider_id.clone(), row.task_kind.clone());
            let entry = buckets.entry(key).or_default();
            entry.0 += 1;
            if row.outcome == "success" || row.outcome == "tool_call" {
                entry.1 += 1;
            }
            if let Some(ms) = row.latency_ms {
                entry.2.push(ms as u64);
            }
        }

        let mut stats: Vec<ProviderStats> = buckets
            .into_iter()
            .map(
                |((provider_id, task_kind), (total, successes, mut latencies))| {
                    let failures = total - successes;
                    let success_rate = if total == 0 {
                        0.0
                    } else {
                        successes as f64 / total as f64
                    };
                    let avg_latency_ms = if latencies.is_empty() {
                        None
                    } else {
                        Some(latencies.iter().sum::<u64>() as f64 / latencies.len() as f64)
                    };
                    let p90_latency_ms = if latencies.is_empty() {
                        None
                    } else {
                        latencies.sort_unstable();
                        let idx = (latencies.len() as f64 * 0.90) as usize;
                        Some(latencies[idx.min(latencies.len() - 1)])
                    };
                    ProviderStats {
                        provider_id,
                        task_kind,
                        total_calls: total,
                        success_count: successes,
                        failure_count: failures,
                        success_rate,
                        avg_latency_ms,
                        p90_latency_ms,
                    }
                },
            )
            .collect();

        stats.sort_by(|a, b| {
            a.provider_id
                .cmp(&b.provider_id)
                .then(a.task_kind.cmp(&b.task_kind))
        });
        Ok(stats)
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
            oracle_pick: row.get::<_, Option<String>>(11)?,
            oracle_agreement: row.get::<_, Option<i64>>(12)?.map(|v| v != 0),
            timestamp: row.get::<_, i64>(13)? as u64,
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
            oracle_pick: None,
            oracle_agreement: None,
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
    fn provider_stats_aggregates_success_and_failure() {
        let (s, _f) = open_tmp();
        // 3 gemini successes + 1 failure for text.generate
        s.record_trace(&make_record(1, "gemini", "aria", "success"))
            .unwrap();
        s.record_trace(&make_record(2, "gemini", "aria", "success"))
            .unwrap();
        s.record_trace(&make_record(3, "gemini", "aria", "success"))
            .unwrap();
        s.record_trace(&make_record(4, "gemini", "aria", "failure"))
            .unwrap();
        // 1 elevenlabs success for text.generate
        s.record_trace(&make_record(5, "elevenlabs", "aria", "success"))
            .unwrap();

        let stats = s.provider_stats(None).unwrap();
        let gemini = stats
            .iter()
            .find(|r| r.provider_id == "gemini")
            .expect("gemini stats missing");
        assert_eq!(gemini.total_calls, 4);
        assert_eq!(gemini.success_count, 3);
        assert_eq!(gemini.failure_count, 1);
        assert!((gemini.success_rate - 0.75).abs() < 1e-9);

        let eleven = stats
            .iter()
            .find(|r| r.provider_id == "elevenlabs")
            .expect("elevenlabs stats missing");
        assert_eq!(eleven.total_calls, 1);
        assert!((eleven.success_rate - 1.0).abs() < 1e-9);
    }

    #[test]
    fn provider_stats_window_filters_old_records() {
        let (s, _f) = open_tmp();
        // Insert one record with a very old timestamp manually.
        {
            let conn = s.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO router_traces
                 (trace_id, agent_id, session_id, turn_id, provider_id, model_id,
                  task_kind, outcome, latency_ms, timestamp)
                 VALUES ('old-trace','aria','s0','t0','gemini',NULL,'text.generate','success',100,1)",
                [],
            )
            .unwrap();
        }
        // One recent record via make_record (timestamp = 1_000_000 + n, which is in the past but
        // effectively "all-time"; window test needs a relative future record so we insert one now).
        let now_ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0) as i64;
        {
            let conn = s.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO router_traces
                 (trace_id, agent_id, session_id, turn_id, provider_id, model_id,
                  task_kind, outcome, latency_ms, timestamp)
                 VALUES ('new-trace','aria','s1','t1','gemini',NULL,'text.generate','failure',200,?1)",
                rusqlite::params![now_ts],
            )
            .unwrap();
        }

        // Window of 3600s should exclude the epoch-1 record and include the now record.
        let stats = s.provider_stats(Some(3600)).unwrap();
        let gemini = stats.iter().find(|r| r.provider_id == "gemini").unwrap();
        assert_eq!(gemini.total_calls, 1);
        assert_eq!(gemini.success_count, 0);
    }

    #[test]
    fn oracle_shadow_fields_round_trip_and_default_null() {
        let (s, _f) = open_tmp();
        // Default (shadow off) path records NULLs.
        s.record_trace(&make_record(1, "gemini", "aria", "success"))
            .unwrap();
        // Shadow-on path records a pick + agreement.
        let mut r = make_record(2, "gemini", "aria", "success");
        r.oracle_pick = Some("model.openrouter:openrouter".into());
        r.oracle_agreement = Some(false);
        s.record_trace(&r).unwrap();

        let traces = s.list_traces(10).unwrap();
        let shadow = traces.iter().find(|t| t.trace_id == "trace-0002").unwrap();
        assert_eq!(
            shadow.oracle_pick.as_deref(),
            Some("model.openrouter:openrouter")
        );
        assert_eq!(shadow.oracle_agreement, Some(false));

        let off = traces.iter().find(|t| t.trace_id == "trace-0001").unwrap();
        assert_eq!(off.oracle_pick, None);
        assert_eq!(off.oracle_agreement, None);
    }

    #[test]
    fn migration_adds_oracle_columns_to_old_db_and_preserves_rows() {
        let f = NamedTempFile::new().unwrap();
        // Simulate a deployed hotel's pre-shadow DB: the router_traces table
        // exists WITHOUT the oracle_pick / oracle_agreement columns, with one
        // legacy row already stored.
        {
            let conn = Connection::open(f.path()).unwrap();
            conn.execute_batch(
                "CREATE TABLE router_traces (
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
                );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO router_traces
                 (trace_id, agent_id, session_id, turn_id, provider_id, model_id,
                  task_kind, outcome, latency_ms, timestamp)
                 VALUES ('legacy-1','jane','s0','t0','gemini','gemini-2.0-flash',
                         'text.generate','success',123,1000)",
                [],
            )
            .unwrap();
        }

        // Opening via the store must migrate the schema in place without error.
        let s = SqliteRouterTraceStorage::open(f.path()).unwrap();

        // The legacy row survives and reads back with NULL oracle fields.
        let traces = s.list_traces(10).unwrap();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].trace_id, "legacy-1");
        assert_eq!(traces[0].agent_id, "jane");
        assert_eq!(traces[0].oracle_pick, None);
        assert_eq!(traces[0].oracle_agreement, None);

        // The migrated DB accepts new rows carrying the shadow fields.
        let mut r = make_record(2, "gemini", "jane", "success");
        r.oracle_pick = Some("model:gemini".into());
        r.oracle_agreement = Some(true);
        s.record_trace(&r).unwrap();
        let after = s.list_traces(10).unwrap();
        let migrated = after.iter().find(|t| t.trace_id == "trace-0002").unwrap();
        assert_eq!(migrated.oracle_pick.as_deref(), Some("model:gemini"));
        assert_eq!(migrated.oracle_agreement, Some(true));

        // Re-opening runs the guarded migration again; the duplicate-column
        // errors must be swallowed so this does not panic.
        let s2 = SqliteRouterTraceStorage::open(f.path()).unwrap();
        assert_eq!(s2.list_traces(10).unwrap().len(), 2);
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
