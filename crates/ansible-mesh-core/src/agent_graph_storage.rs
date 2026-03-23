//! `AgentGraphStorage` — per-agent cognitive graph persistence trait.
//!
//! This is the storage substrate for Seam 3 (`agent-graph-toolrunner`) of the
//! agent-resource-model workstream. It extends the concept of `GraphStorage`
//! (which serves the hotel-wide context graph) with domain operations specific
//! to an individual agent's cognitive harness.
//!
//! # Authority boundary
//!
//! - The hotel CG (via `GraphStorage`) owns rights, provisioning grants, and
//!   routing state. Only the hotel writes there.
//! - The agent graph (this trait) owns cognitive configuration, resource grant
//!   *copies*, tool preferences, resource declarations, and experience traces.
//!   The agent writes here; the hotel writes grant/revoke records on its behalf.
//!
//! # Backend
//!
//! `SqliteAgentGraphStorage` uses an **embedded per-agent SQLite file**.
//! Choosing a per-agent file favors portability (the file travels with the
//! agent across hotels during mesh migration) over operational simplicity.
//! A shared-namespace backend is achievable via a single impl swap later.

use crate::resources::{ResourceDeclaration, ResourceType};
use anyhow::Result;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};

// ──────────────────────────────────────────────────────────────────────────────
// Records
// ──────────────────────────────────────────────────────────────────────────────

/// A copy of a hotel-issued resource grant, stored in the agent graph.
///
/// The hotel CG is authoritative; this is a cache. On revocation, the hotel
/// sends `ResourceRevoked` and this record becomes stale — the agent graph
/// copy does not grant authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentResourceGrant {
    pub agent_id: String,
    /// Stable instance identifier issued by the hotel.
    pub instance_id: String,
    pub resource_type: ResourceType,
    /// Unix epoch (seconds) when the grant was received.
    pub granted_at: u64,
}

/// Per-agent preference for a named tool.
///
/// `preference_level`: `-1` = suppress (never project this tool),
/// `0` = default (no opinion), `1` = prefer (always project if available).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentToolPreference {
    pub agent_id: String,
    pub tool_name: String,
    pub preference_level: i32,
    #[serde(default)]
    pub config_json: serde_json::Value,
}

/// A single entry in the agent experience ledger.
///
/// Feeds the RL training pipeline via the router-listener tap (Seam 5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentExperienceTrace {
    /// ULID — lexicographically sortable, unique.
    pub trace_id: String,
    pub agent_id: String,
    /// e.g. `"tool_call"`, `"resource_request"`, `"model_invocation"`, `"approval_decision"`
    pub event_type: String,
    pub input_json: serde_json::Value,
    pub output_json: serde_json::Value,
    /// e.g. `"success"`, `"failure"`, `"approved_by_operator"`, `"denied_by_operator"`, `"timeout"`, `"revoked"`
    pub outcome: String,
    #[serde(default)]
    pub outcome_detail: Option<String>,
    /// Unix epoch (seconds).
    pub timestamp: u64,
}

// ──────────────────────────────────────────────────────────────────────────────
// Trait
// ──────────────────────────────────────────────────────────────────────────────

/// Per-agent cognitive graph storage.
///
/// All methods are scoped to a single agent. Implementors are expected to own
/// one storage unit (file, namespace partition, etc.) per agent.
pub trait AgentGraphStorage: Send + Sync {
    // ── Resource grants ──────────────────────────────────────────────────────

    /// Persist a copy of a hotel-issued resource grant.
    fn upsert_resource_grant(&self, grant: &AgentResourceGrant) -> Result<()>;

    /// Return all active resource grant copies for this agent.
    fn list_resource_grants(&self) -> Result<Vec<AgentResourceGrant>>;

    /// Remove a resource grant copy (called on `ResourceRevoked`).
    fn remove_resource_grant(&self, instance_id: &str) -> Result<()>;

    // ── Tool preferences ─────────────────────────────────────────────────────

    /// Upsert a tool preference entry.
    fn upsert_tool_preference(&self, pref: &AgentToolPreference) -> Result<()>;

    /// Return all tool preferences for this agent.
    fn list_tool_preferences(&self) -> Result<Vec<AgentToolPreference>>;

    /// Return the preference for a specific tool, or `None` if not set.
    fn get_tool_preference(&self, tool_name: &str) -> Result<Option<AgentToolPreference>>;

    // ── Resource declarations ────────────────────────────────────────────────

    /// Upsert a static resource declaration.
    ///
    /// One declaration per `resource_type` — upsert replaces the config_hint.
    fn upsert_resource_declaration(&self, decl: &ResourceDeclaration) -> Result<()>;

    /// Return all static resource declarations for this agent.
    fn list_resource_declarations(&self) -> Result<Vec<ResourceDeclaration>>;

    /// Remove a static resource declaration.
    fn remove_resource_declaration(&self, resource_type: &ResourceType) -> Result<()>;

    // ── Experience traces ────────────────────────────────────────────────────

    /// Append an experience trace entry.
    fn record_experience_trace(&self, trace: &AgentExperienceTrace) -> Result<()>;

    /// Return the most recent `limit` experience traces, newest first.
    fn list_experience_traces(&self, limit: usize) -> Result<Vec<AgentExperienceTrace>>;

    /// Return traces matching a specific `event_type`, newest first.
    fn list_traces_by_event_type(
        &self,
        event_type: &str,
        limit: usize,
    ) -> Result<Vec<AgentExperienceTrace>>;
}

// ──────────────────────────────────────────────────────────────────────────────
// SqliteAgentGraphStorage
// ──────────────────────────────────────────────────────────────────────────────

/// SQLite-backed per-agent graph storage.
///
/// One `.db` file per agent. The `agent_id` is stored as a column in every
/// table for readability; the file itself is the scope boundary.
pub struct SqliteAgentGraphStorage {
    agent_id: String,
    conn: Arc<Mutex<Connection>>,
}

impl SqliteAgentGraphStorage {
    /// Open (or create) the agent graph database at `path`.
    pub fn open(agent_id: impl Into<String>, path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        let storage = Self {
            agent_id: agent_id.into(),
            conn: Arc::new(Mutex::new(conn)),
        };
        storage.init_schema()?;
        Ok(storage)
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "
            BEGIN;

            CREATE TABLE IF NOT EXISTS resource_grants (
                instance_id   TEXT PRIMARY KEY,
                agent_id      TEXT NOT NULL,
                resource_type TEXT NOT NULL,
                granted_at    INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS tool_preferences (
                agent_id         TEXT NOT NULL,
                tool_name        TEXT NOT NULL,
                preference_level INTEGER NOT NULL DEFAULT 0,
                config_json      TEXT NOT NULL DEFAULT '{}',
                PRIMARY KEY (agent_id, tool_name)
            );

            CREATE TABLE IF NOT EXISTS resource_declarations (
                agent_id      TEXT NOT NULL,
                resource_type TEXT NOT NULL,
                config_hint   TEXT,
                PRIMARY KEY (agent_id, resource_type)
            );

            CREATE TABLE IF NOT EXISTS experience_traces (
                trace_id       TEXT PRIMARY KEY,
                agent_id       TEXT NOT NULL,
                event_type     TEXT NOT NULL,
                input_json     TEXT NOT NULL,
                output_json    TEXT NOT NULL,
                outcome        TEXT NOT NULL,
                outcome_detail TEXT,
                timestamp      INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_traces_agent_ts
                ON experience_traces (agent_id, timestamp DESC);

            CREATE INDEX IF NOT EXISTS idx_traces_event_type
                ON experience_traces (agent_id, event_type, timestamp DESC);

            COMMIT;
            ",
        )?;
        Ok(())
    }

    fn now_epoch() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

impl AgentGraphStorage for SqliteAgentGraphStorage {
    // ── Resource grants ──────────────────────────────────────────────────────

    fn upsert_resource_grant(&self, grant: &AgentResourceGrant) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let rt = serde_json::to_string(&grant.resource_type)?;
        conn.execute(
            "INSERT INTO resource_grants (instance_id, agent_id, resource_type, granted_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(instance_id) DO UPDATE SET
               agent_id = excluded.agent_id,
               resource_type = excluded.resource_type,
               granted_at = excluded.granted_at",
            params![grant.instance_id, grant.agent_id, rt, grant.granted_at],
        )?;
        Ok(())
    }

    fn list_resource_grants(&self) -> Result<Vec<AgentResourceGrant>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT instance_id, agent_id, resource_type, granted_at
             FROM resource_grants ORDER BY granted_at ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, u64>(3)?,
            ))
        })?;
        let mut grants = Vec::new();
        for row in rows {
            let (instance_id, agent_id, rt_str, granted_at) = row?;
            let resource_type: ResourceType = serde_json::from_str(&rt_str)
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e)))?;
            grants.push(AgentResourceGrant {
                instance_id,
                agent_id,
                resource_type,
                granted_at,
            });
        }
        Ok(grants)
    }

    fn remove_resource_grant(&self, instance_id: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM resource_grants WHERE instance_id = ?1",
            params![instance_id],
        )?;
        Ok(())
    }

    // ── Tool preferences ─────────────────────────────────────────────────────

    fn upsert_tool_preference(&self, pref: &AgentToolPreference) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let config = serde_json::to_string(&pref.config_json)?;
        conn.execute(
            "INSERT INTO tool_preferences (agent_id, tool_name, preference_level, config_json)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(agent_id, tool_name) DO UPDATE SET
               preference_level = excluded.preference_level,
               config_json = excluded.config_json",
            params![pref.agent_id, pref.tool_name, pref.preference_level, config],
        )?;
        Ok(())
    }

    fn list_tool_preferences(&self) -> Result<Vec<AgentToolPreference>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT agent_id, tool_name, preference_level, config_json
             FROM tool_preferences WHERE agent_id = ?1 ORDER BY tool_name",
        )?;
        let rows = stmt.query_map(params![self.agent_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i32>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut prefs = Vec::new();
        for row in rows {
            let (agent_id, tool_name, level, cfg_str) = row?;
            prefs.push(AgentToolPreference {
                agent_id,
                tool_name,
                preference_level: level,
                config_json: serde_json::from_str(&cfg_str).unwrap_or_default(),
            });
        }
        Ok(prefs)
    }

    fn get_tool_preference(&self, tool_name: &str) -> Result<Option<AgentToolPreference>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT agent_id, tool_name, preference_level, config_json
             FROM tool_preferences WHERE agent_id = ?1 AND tool_name = ?2",
        )?;
        let mut rows = stmt.query(params![self.agent_id, tool_name])?;
        if let Some(row) = rows.next()? {
            let cfg_str: String = row.get(3)?;
            return Ok(Some(AgentToolPreference {
                agent_id: row.get(0)?,
                tool_name: row.get(1)?,
                preference_level: row.get(2)?,
                config_json: serde_json::from_str(&cfg_str).unwrap_or_default(),
            }));
        }
        Ok(None)
    }

    // ── Resource declarations ────────────────────────────────────────────────

    fn upsert_resource_declaration(&self, decl: &ResourceDeclaration) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let rt = serde_json::to_string(&decl.resource_type)?;
        conn.execute(
            "INSERT INTO resource_declarations (agent_id, resource_type, config_hint)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(agent_id, resource_type) DO UPDATE SET
               config_hint = excluded.config_hint",
            params![self.agent_id, rt, decl.config_hint],
        )?;
        Ok(())
    }

    fn list_resource_declarations(&self) -> Result<Vec<ResourceDeclaration>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT resource_type, config_hint FROM resource_declarations WHERE agent_id = ?1",
        )?;
        let rows = stmt.query_map(params![self.agent_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        let mut decls = Vec::new();
        for row in rows {
            let (rt_str, config_hint) = row?;
            let resource_type: ResourceType = serde_json::from_str(&rt_str)
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?;
            decls.push(ResourceDeclaration {
                resource_type,
                config_hint,
            });
        }
        Ok(decls)
    }

    fn remove_resource_declaration(&self, resource_type: &ResourceType) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let rt = serde_json::to_string(resource_type)?;
        conn.execute(
            "DELETE FROM resource_declarations WHERE agent_id = ?1 AND resource_type = ?2",
            params![self.agent_id, rt],
        )?;
        Ok(())
    }

    // ── Experience traces ────────────────────────────────────────────────────

    fn record_experience_trace(&self, trace: &AgentExperienceTrace) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let input = serde_json::to_string(&trace.input_json)?;
        let output = serde_json::to_string(&trace.output_json)?;
        conn.execute(
            "INSERT OR IGNORE INTO experience_traces
             (trace_id, agent_id, event_type, input_json, output_json, outcome, outcome_detail, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                trace.trace_id,
                trace.agent_id,
                trace.event_type,
                input,
                output,
                trace.outcome,
                trace.outcome_detail,
                trace.timestamp
            ],
        )?;
        Ok(())
    }

    fn list_experience_traces(&self, limit: usize) -> Result<Vec<AgentExperienceTrace>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT trace_id, agent_id, event_type, input_json, output_json, outcome, outcome_detail, timestamp
             FROM experience_traces WHERE agent_id = ?1
             ORDER BY timestamp DESC LIMIT ?2",
        )?;
        self.collect_traces(&mut stmt, params![self.agent_id, limit as i64])
    }

    fn list_traces_by_event_type(
        &self,
        event_type: &str,
        limit: usize,
    ) -> Result<Vec<AgentExperienceTrace>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT trace_id, agent_id, event_type, input_json, output_json, outcome, outcome_detail, timestamp
             FROM experience_traces WHERE agent_id = ?1 AND event_type = ?2
             ORDER BY timestamp DESC LIMIT ?3",
        )?;
        self.collect_traces(&mut stmt, params![self.agent_id, event_type, limit as i64])
    }
}

impl SqliteAgentGraphStorage {
    fn collect_traces(
        &self,
        stmt: &mut rusqlite::Statement<'_>,
        params: impl rusqlite::Params,
    ) -> Result<Vec<AgentExperienceTrace>> {
        let rows = stmt.query_map(params, |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, u64>(7)?,
            ))
        })?;
        let mut traces = Vec::new();
        for row in rows {
            let (trace_id, agent_id, event_type, input, output, outcome, detail, ts) = row?;
            traces.push(AgentExperienceTrace {
                trace_id,
                agent_id,
                event_type,
                input_json: serde_json::from_str(&input).unwrap_or_default(),
                output_json: serde_json::from_str(&output).unwrap_or_default(),
                outcome,
                outcome_detail: detail,
                timestamp: ts,
            });
        }
        Ok(traces)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::ResourceType;
    use tempfile::NamedTempFile;

    // Returns both the storage and the NamedTempFile so the caller keeps the
    // file alive for the duration of the test. Dropping the file would make the
    // SQLite connection report "database file has moved".
    fn open_tmp(agent_id: &str) -> (SqliteAgentGraphStorage, NamedTempFile) {
        let f = NamedTempFile::new().unwrap();
        let s = SqliteAgentGraphStorage::open(agent_id, f.path()).unwrap();
        (s, f)
    }

    // ── resource grants ───────────────────────────────────────────────────────

    #[test]
    fn grant_upsert_and_list() {
        let (s, _f) = open_tmp("aria");
        let grant = AgentResourceGrant {
            agent_id: "aria".into(),
            instance_id: "inst-1".into(),
            resource_type: ResourceType::ModelRouter,
            granted_at: 1_000_000,
        };
        s.upsert_resource_grant(&grant).unwrap();
        let grants = s.list_resource_grants().unwrap();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].instance_id, "inst-1");
        assert_eq!(grants[0].resource_type, ResourceType::ModelRouter);
    }

    #[test]
    fn grant_remove() {
        let (s, _f) = open_tmp("aria");
        let grant = AgentResourceGrant {
            agent_id: "aria".into(),
            instance_id: "inst-2".into(),
            resource_type: ResourceType::ToolRunner,
            granted_at: 1_000_001,
        };
        s.upsert_resource_grant(&grant).unwrap();
        s.remove_resource_grant("inst-2").unwrap();
        assert!(s.list_resource_grants().unwrap().is_empty());
    }

    // ── tool preferences ──────────────────────────────────────────────────────

    #[test]
    fn tool_preference_upsert_get() {
        let (s, _f) = open_tmp("bjork");
        let pref = AgentToolPreference {
            agent_id: "bjork".into(),
            tool_name: "bash.exec".into(),
            preference_level: 1,
            config_json: serde_json::json!({"timeout_secs": 30}),
        };
        s.upsert_tool_preference(&pref).unwrap();
        let got = s.get_tool_preference("bash.exec").unwrap().unwrap();
        assert_eq!(got.preference_level, 1);
        assert_eq!(got.config_json["timeout_secs"], 30);
    }

    #[test]
    fn tool_preference_suppressed() {
        let (s, _f) = open_tmp("bjork");
        s.upsert_tool_preference(&AgentToolPreference {
            agent_id: "bjork".into(),
            tool_name: "dangerous.tool".into(),
            preference_level: -1,
            config_json: serde_json::json!({}),
        })
        .unwrap();
        let prefs = s.list_tool_preferences().unwrap();
        assert_eq!(prefs.len(), 1);
        assert_eq!(prefs[0].preference_level, -1);
    }

    #[test]
    fn get_tool_preference_missing_returns_none() {
        let (s, _f) = open_tmp("x");
        assert!(s.get_tool_preference("missing.tool").unwrap().is_none());
    }

    // ── resource declarations ─────────────────────────────────────────────────

    #[test]
    fn declaration_upsert_list_remove() {
        let (s, _f) = open_tmp("aria");
        s.upsert_resource_declaration(&ResourceDeclaration {
            resource_type: ResourceType::AgentGraph,
            config_hint: None,
        })
        .unwrap();
        s.upsert_resource_declaration(&ResourceDeclaration {
            resource_type: ResourceType::ModelRouter,
            config_hint: Some("gemini".into()),
        })
        .unwrap();
        assert_eq!(s.list_resource_declarations().unwrap().len(), 2);

        s.remove_resource_declaration(&ResourceType::ModelRouter)
            .unwrap();
        let remaining = s.list_resource_declarations().unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].resource_type, ResourceType::AgentGraph);
    }

    // ── experience traces ─────────────────────────────────────────────────────

    #[test]
    fn trace_record_and_recall() {
        let (s, _f) = open_tmp("aria");
        for i in 0..5u64 {
            s.record_experience_trace(&AgentExperienceTrace {
                trace_id: format!("trace-{i:02}"),
                agent_id: "aria".into(),
                event_type: "tool_call".into(),
                input_json: serde_json::json!({"tool": "bash.exec", "seq": i}),
                output_json: serde_json::json!({"exit_code": 0}),
                outcome: "success".into(),
                outcome_detail: None,
                timestamp: 1_000_000 + i,
            })
            .unwrap();
        }
        let traces = s.list_experience_traces(3).unwrap();
        assert_eq!(traces.len(), 3);
        // Newest first.
        assert_eq!(traces[0].trace_id, "trace-04");
    }

    #[test]
    fn trace_filter_by_event_type() {
        let (s, _f) = open_tmp("aria");
        s.record_experience_trace(&AgentExperienceTrace {
            trace_id: "t-1".into(),
            agent_id: "aria".into(),
            event_type: "tool_call".into(),
            input_json: serde_json::json!({}),
            output_json: serde_json::json!({}),
            outcome: "success".into(),
            outcome_detail: None,
            timestamp: 1,
        })
        .unwrap();
        s.record_experience_trace(&AgentExperienceTrace {
            trace_id: "t-2".into(),
            agent_id: "aria".into(),
            event_type: "resource_request".into(),
            input_json: serde_json::json!({}),
            output_json: serde_json::json!({}),
            outcome: "success".into(),
            outcome_detail: None,
            timestamp: 2,
        })
        .unwrap();

        let tool_traces = s.list_traces_by_event_type("tool_call", 10).unwrap();
        assert_eq!(tool_traces.len(), 1);
        assert_eq!(tool_traces[0].trace_id, "t-1");
    }

    #[test]
    fn trace_insert_or_ignore_is_idempotent() {
        let (s, _f) = open_tmp("x");
        let trace = AgentExperienceTrace {
            trace_id: "dup-id".into(),
            agent_id: "x".into(),
            event_type: "tool_call".into(),
            input_json: serde_json::json!({}),
            output_json: serde_json::json!({}),
            outcome: "success".into(),
            outcome_detail: None,
            timestamp: 1,
        };
        s.record_experience_trace(&trace).unwrap();
        s.record_experience_trace(&trace).unwrap(); // should not error
        assert_eq!(s.list_experience_traces(10).unwrap().len(), 1);
    }
}
