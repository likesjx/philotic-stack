//! `DecisionTraceStorage` — append-only, **content-free** audit of decisions calls.
//!
//! One row per decision call from a shadow or in-path site: which site, what data
//! class, how many bytes left, the resolved model, latency, usage and cost, the
//! outcome, and (for a shadow run) what the incumbent decided, what the judge
//! answered and whether they agreed. It never stores the text that was sent.
//!
//! Errors and disagreement are separate columns on purpose. In a log-only run a
//! parse failure and a disagreeing judge look identical in a log; folding them
//! together would mean calibrating on nothing.
//!
//! # Authority
//!
//! Callers write: `heal-dispatcher` in-process, and model-router for any
//! `decisions.evaluate` task that carries a [`ShadowContext`]. Reads are for
//! local analysis and the future calibration step. Traces are node-local by
//! design; no mesh sync.

use crate::decisions::{DecisionAnswer, DecisionsError, DecisionsErrorClass, DecisionsOutcome};
use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// One decisions call, without the text that was sent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionTraceRecord {
    /// ULID — lexicographically sortable, unique.
    pub trace_id: String,
    /// Unix epoch (seconds).
    pub timestamp: u64,
    /// The allow-listed call-site id (`heal.classify`).
    pub site: String,
    /// The site's data class (`A`, `B` or `C`).
    pub data_class: String,
    /// Bytes of state plus questions that left the machine. Never the text.
    pub bytes_sent: u64,
    /// `ok` or `error`.
    pub outcome: String,
    /// Set when `outcome` is `error`: `unavailable`, `timeout`, `rate_limited`,
    /// `invalid_request`, `auth` or `invalid_response`.
    #[serde(default)]
    pub error_class: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    /// The RESOLVED model (`typesafe/jev-1.13-20260917`), not the alias asked for.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub transport: Option<String>,
    #[serde(default)]
    pub latency_ms: Option<u64>,
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    /// A `score` legend did not echo the levels sent. Counted separately.
    #[serde(default)]
    pub legend_mismatch: bool,
    /// OpenRouter generation id.
    #[serde(default)]
    pub request_id: Option<String>,
    /// A system identifier for the subject (a guest id), never operator content.
    #[serde(default)]
    pub subject: Option<String>,
    /// What the incumbent decided, for a shadow run (`null` when it had no verdict).
    #[serde(default)]
    pub incumbent: Option<Value>,
    /// The judge's typed answers (probabilities), for calibration.
    #[serde(default)]
    pub answers: Option<Value>,
    /// Per question: did the judge agree with the incumbent? `None` = not
    /// comparable (no incumbent verdict, or the incumbent's label is outside the
    /// judge's options).
    #[serde(default)]
    pub agreement: BTreeMap<String, Option<bool>>,
}

/// Append-only storage for decisions traces.
pub trait DecisionTraceStorage: Send + Sync {
    fn record_trace(&self, record: &DecisionTraceRecord) -> Result<()>;
    /// The most recent `limit` records, newest first.
    fn list_traces(&self, limit: usize) -> Result<Vec<DecisionTraceRecord>>;
}

/// Where the trace database lives: `PHILOTIC_DECISION_TRACE_DB`, else
/// `~/.philotic/<profile>/decision_traces.db` next to `router_traces.db`.
pub fn default_db_path() -> PathBuf {
    if let Some(path) = std::env::var("PHILOTIC_DECISION_TRACE_DB")
        .ok()
        .filter(|p| !p.trim().is_empty())
    {
        return PathBuf::from(path);
    }
    let profile = std::env::var("PHILOTIC_PROFILE").unwrap_or_else(|_| "default".to_string());
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(format!("{home}/.philotic/{profile}/decision_traces.db"))
}

/// SQLite-backed decision trace store. Created on first use; append-only.
pub struct SqliteDecisionTraceStorage {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteDecisionTraceStorage {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
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

            CREATE TABLE IF NOT EXISTS decision_traces (
                trace_id        TEXT PRIMARY KEY,
                timestamp       INTEGER NOT NULL,
                site            TEXT NOT NULL,
                data_class      TEXT NOT NULL,
                bytes_sent      INTEGER NOT NULL,
                outcome         TEXT NOT NULL,
                error_class     TEXT,
                provider        TEXT,
                model           TEXT,
                transport       TEXT,
                latency_ms      INTEGER,
                input_tokens    INTEGER,
                output_tokens   INTEGER,
                cost_usd        REAL,
                legend_mismatch INTEGER NOT NULL DEFAULT 0,
                request_id      TEXT,
                subject         TEXT,
                incumbent_json  TEXT,
                answers_json    TEXT,
                agreement_json  TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_decision_traces_ts
                ON decision_traces (timestamp DESC);

            CREATE INDEX IF NOT EXISTS idx_decision_traces_site
                ON decision_traces (site, timestamp DESC);

            COMMIT;
            ",
        )?;
        Ok(())
    }
}

impl DecisionTraceStorage for SqliteDecisionTraceStorage {
    fn record_trace(&self, r: &DecisionTraceRecord) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO decision_traces
             (trace_id, timestamp, site, data_class, bytes_sent, outcome, error_class,
              provider, model, transport, latency_ms, input_tokens, output_tokens,
              cost_usd, legend_mismatch, request_id, subject, incumbent_json,
              answers_json, agreement_json)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
            params![
                r.trace_id,
                r.timestamp as i64,
                r.site,
                r.data_class,
                r.bytes_sent as i64,
                r.outcome,
                r.error_class,
                r.provider,
                r.model,
                r.transport,
                r.latency_ms.map(|v| v as i64),
                r.input_tokens.map(|v| v as i64),
                r.output_tokens.map(|v| v as i64),
                r.cost_usd,
                r.legend_mismatch as i64,
                r.request_id,
                r.subject,
                r.incumbent.as_ref().map(Value::to_string),
                r.answers.as_ref().map(Value::to_string),
                serde_json::to_string(&r.agreement)?,
            ],
        )?;
        Ok(())
    }

    fn list_traces(&self, limit: usize) -> Result<Vec<DecisionTraceRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT trace_id, timestamp, site, data_class, bytes_sent, outcome, error_class,
                    provider, model, transport, latency_ms, input_tokens, output_tokens,
                    cost_usd, legend_mismatch, request_id, subject, incumbent_json,
                    answers_json, agreement_json
             FROM decision_traces ORDER BY timestamp DESC, trace_id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            let json = |i: usize| -> rusqlite::Result<Option<Value>> {
                Ok(row
                    .get::<_, Option<String>>(i)?
                    .and_then(|s| serde_json::from_str(&s).ok()))
            };
            let agreement: BTreeMap<String, Option<bool>> = row
                .get::<_, Option<String>>(19)?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            Ok(DecisionTraceRecord {
                trace_id: row.get(0)?,
                timestamp: row.get::<_, i64>(1)? as u64,
                site: row.get(2)?,
                data_class: row.get(3)?,
                bytes_sent: row.get::<_, i64>(4)? as u64,
                outcome: row.get(5)?,
                error_class: row.get(6)?,
                provider: row.get(7)?,
                model: row.get(8)?,
                transport: row.get(9)?,
                latency_ms: row.get::<_, Option<i64>>(10)?.map(|v| v as u64),
                input_tokens: row.get::<_, Option<i64>>(11)?.map(|v| v as u64),
                output_tokens: row.get::<_, Option<i64>>(12)?.map(|v| v as u64),
                cost_usd: row.get(13)?,
                legend_mismatch: row.get::<_, i64>(14)? != 0,
                request_id: row.get(15)?,
                subject: row.get(16)?,
                incumbent: json(17)?,
                answers: json(18)?,
                agreement,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
}

/// Errors, disagreement and volume, kept apart.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecisionSummary {
    pub total: u64,
    /// Calls that produced typed answers.
    pub ok: u64,
    /// Calls that failed, by error class. Never mixed into agreement.
    pub errors_by_class: BTreeMap<String, u64>,
    /// Calls the data policy declined to send (`outcome = skipped`), by reason.
    /// Nothing left the machine. Neither an error nor a disagreement.
    pub skipped_by_reason: BTreeMap<String, u64>,
    /// `ok` calls whose score legend did not echo the levels sent.
    pub legend_mismatches: u64,
    /// Per question, over `ok` calls where the comparison was defined:
    /// `(agreed, compared)`.
    pub agreement: BTreeMap<String, (u64, u64)>,
    pub total_cost_usd: f64,
    /// Bytes that actually left the machine, summed over all rows.
    pub total_bytes_sent: u64,
}

/// Summarize records. Error rows contribute only to `errors_by_class` and skipped
/// rows only to `skipped_by_reason`, so neither a provider outage nor a policy
/// refusal can ever read as the judge disagreeing.
pub fn summarize(records: &[DecisionTraceRecord]) -> DecisionSummary {
    let mut summary = DecisionSummary::default();
    for record in records {
        summary.total += 1;
        summary.total_cost_usd += record.cost_usd.unwrap_or(0.0);
        summary.total_bytes_sent += record.bytes_sent;
        if record.outcome == "skipped" {
            let reason = record
                .error_class
                .clone()
                .unwrap_or_else(|| "unknown".into());
            *summary.skipped_by_reason.entry(reason).or_default() += 1;
            continue;
        }
        if record.outcome != "ok" {
            let class = record
                .error_class
                .clone()
                .unwrap_or_else(|| "unknown".into());
            *summary.errors_by_class.entry(class).or_default() += 1;
            continue;
        }
        summary.ok += 1;
        if record.legend_mismatch {
            summary.legend_mismatches += 1;
        }
        for (question, agreed) in &record.agreement {
            if let Some(agreed) = agreed {
                let entry = summary.agreement.entry(question.clone()).or_default();
                entry.1 += 1;
                if *agreed {
                    entry.0 += 1;
                }
            }
        }
    }
    summary
}

// ──────────────────────────────────────────────────────────────────────────────
// Router-side shadow traces
// ──────────────────────────────────────────────────────────────────────────────

/// What a shadow caller tells the router about the decision it already made,
/// carried on a `decisions.evaluate` task as `"shadow": {...}`. The router that
/// serves the call writes one trace row with it, so every shadow site gets the
/// same trace (and the trace lives with the router, wherever that is).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowContext {
    /// A system identifier joining the judgment to its outcome (a turn id, a
    /// heal row id), never operator content.
    pub subject: String,
    /// The site's data class (`A`, `B` or `C`), as the caller's gate saw it.
    pub data_class: String,
    /// The incumbent's verdict per question id: a bool for a `noul`, an option
    /// key for a `choice`, `null` when the incumbent had no opinion.
    #[serde(default)]
    pub incumbent: BTreeMap<String, Value>,
}

/// Per question: did the judge agree with the incumbent? A `noul` agrees when
/// `P(true) > 0.5` matches the incumbent's bool (a logging convenience, not a
/// decision threshold: the probability is stored beside it); a `choice` agrees
/// when the keys match. `None` = not comparable.
pub fn agreement_with(
    incumbent: &BTreeMap<String, Value>,
    answers: &BTreeMap<String, DecisionAnswer>,
) -> BTreeMap<String, Option<bool>> {
    incumbent
        .iter()
        .map(|(question, verdict)| {
            let agreed = match (answers.get(question), verdict) {
                (Some(DecisionAnswer::Noul { noul }), Value::Bool(expected)) => {
                    Some((*noul > 0.5) == *expected)
                }
                (Some(DecisionAnswer::Choice { choice, .. }), Value::String(expected)) => {
                    Some(choice == expected)
                }
                _ => None,
            };
            (question.clone(), agreed)
        })
        .collect()
}

/// One trace row for a shadow decision served by the router. `bytes_sent` is
/// zero here: the router path does not measure the wire body, and
/// `input_tokens` carries the size instead.
pub fn shadow_trace_record(
    site: &str,
    shadow: &ShadowContext,
    result: &std::result::Result<DecisionsOutcome, DecisionsError>,
) -> DecisionTraceRecord {
    let base = DecisionTraceRecord {
        trace_id: ulid::Ulid::new().to_string(),
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        site: site.to_string(),
        data_class: shadow.data_class.clone(),
        bytes_sent: 0,
        outcome: "ok".into(),
        error_class: None,
        provider: None,
        model: None,
        transport: None,
        latency_ms: None,
        input_tokens: None,
        output_tokens: None,
        cost_usd: None,
        legend_mismatch: false,
        request_id: None,
        subject: Some(shadow.subject.clone()),
        incumbent: serde_json::to_value(&shadow.incumbent).ok(),
        answers: None,
        agreement: BTreeMap::new(),
    };
    match result {
        Ok(outcome) => DecisionTraceRecord {
            provider: Some(outcome.trace.provider.clone()),
            model: Some(outcome.trace.model.clone()),
            transport: Some(outcome.trace.transport.as_str().to_string()),
            latency_ms: Some(outcome.trace.latency_ms),
            input_tokens: Some(outcome.trace.usage.input_tokens),
            output_tokens: Some(outcome.trace.usage.output_tokens),
            cost_usd: outcome.trace.usage.cost_usd,
            legend_mismatch: outcome.trace.legend_mismatch,
            request_id: outcome.trace.request_id.clone(),
            answers: serde_json::to_value(&outcome.result.answers).ok(),
            agreement: agreement_with(&shadow.incumbent, &outcome.result.answers),
            ..base
        },
        // The data policy declined to send: nothing left the machine.
        Err(error) if error.class == DecisionsErrorClass::PolicyRefused => DecisionTraceRecord {
            outcome: "skipped".into(),
            error_class: Some(error.class.as_str().to_string()),
            ..base
        },
        Err(error) => DecisionTraceRecord {
            outcome: "error".into(),
            error_class: Some(error.class.as_str().to_string()),
            ..base
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shadow_agreement_compares_noul_and_choice_and_skips_the_rest() {
        let incumbent: BTreeMap<String, Value> = [
            ("lesson".to_string(), json!(true)),
            ("kind".to_string(), json!("correction")),
            ("unasked".to_string(), json!(true)),
            ("no_view".to_string(), Value::Null),
        ]
        .into();
        let answers: BTreeMap<String, DecisionAnswer> = [
            ("lesson".to_string(), DecisionAnswer::Noul { noul: 0.31 }),
            (
                "kind".to_string(),
                DecisionAnswer::Choice {
                    choice: "correction".into(),
                    probabilities: BTreeMap::new(),
                    confidence: 0.9,
                },
            ),
            ("no_view".to_string(), DecisionAnswer::Noul { noul: 0.9 }),
        ]
        .into();
        let agreement = agreement_with(&incumbent, &answers);
        assert_eq!(agreement["lesson"], Some(false));
        assert_eq!(agreement["kind"], Some(true));
        assert_eq!(agreement["unasked"], None, "the judge was not asked");
        assert_eq!(agreement["no_view"], None, "the incumbent had no verdict");
    }

    #[test]
    fn a_policy_refusal_is_a_skip_not_an_error() {
        let shadow = ShadowContext {
            subject: "turn-1".into(),
            data_class: "C".into(),
            incumbent: BTreeMap::new(),
        };
        let refused = Err(DecisionsError::new(
            DecisionsErrorClass::PolicyRefused,
            "not allowed",
        ));
        let row = shadow_trace_record("distill.prescreen", &shadow, &refused);
        assert_eq!(row.outcome, "skipped");
        assert_eq!(row.subject.as_deref(), Some("turn-1"));
        assert_eq!(row.data_class, "C");
    }

    fn record(id: &str, ts: u64, outcome: &str) -> DecisionTraceRecord {
        DecisionTraceRecord {
            trace_id: id.into(),
            timestamp: ts,
            site: "heal.classify".into(),
            data_class: "A".into(),
            bytes_sent: 512,
            outcome: outcome.into(),
            error_class: None,
            provider: Some("TypeSafe".into()),
            model: Some("typesafe/jev-1.13-20260917".into()),
            transport: Some("openrouter".into()),
            latency_ms: Some(290),
            input_tokens: Some(307),
            output_tokens: Some(23),
            cost_usd: Some(0.0000129),
            legend_mismatch: false,
            request_id: Some("gen-dec-x".into()),
            subject: Some("beacon".into()),
            incumbent: Some(json!({ "severity": "high", "heal_action": "restart_guest" })),
            answers: Some(json!({ "severity": { "type": "choice", "choice": "high" } })),
            agreement: BTreeMap::from([
                ("severity".to_string(), Some(true)),
                ("needs_restart".to_string(), Some(false)),
            ]),
        }
    }

    fn error(id: &str, ts: u64, class: &str) -> DecisionTraceRecord {
        DecisionTraceRecord {
            outcome: "error".into(),
            error_class: Some(class.into()),
            model: None,
            latency_ms: None,
            input_tokens: None,
            output_tokens: None,
            cost_usd: None,
            answers: None,
            agreement: BTreeMap::new(),
            ..record(id, ts, "error")
        }
    }

    #[test]
    fn records_round_trip_newest_first_and_never_hold_text() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            SqliteDecisionTraceStorage::open(dir.path().join("t/decision_traces.db")).unwrap();
        store.record_trace(&record("a", 100, "ok")).unwrap();
        store
            .record_trace(&error("b", 200, "rate_limited"))
            .unwrap();

        let listed = store.list_traces(10).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].trace_id, "b", "newest first");
        assert_eq!(listed[1], record("a", 100, "ok"));
        assert_eq!(listed[0].error_class.as_deref(), Some("rate_limited"));

        // Content-free by construction: the schema has no column for what was sent.
        let conn = Connection::open(dir.path().join("t/decision_traces.db")).unwrap();
        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(decision_traces)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for forbidden in [
            "text", "state", "prompt", "content", "raw", "body", "message",
        ] {
            assert!(
                !columns.iter().any(|c| c.contains(forbidden)),
                "`{forbidden}` column would hold sent content: {columns:?}"
            );
        }
    }

    #[test]
    fn duplicate_trace_ids_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteDecisionTraceStorage::open(dir.path().join("d.db")).unwrap();
        store.record_trace(&record("a", 100, "ok")).unwrap();
        store.record_trace(&record("a", 100, "ok")).unwrap();
        assert_eq!(store.list_traces(10).unwrap().len(), 1);
    }

    #[test]
    fn errors_are_counted_apart_from_disagreement() {
        let mut disagree = record("d", 3, "ok");
        disagree.agreement = BTreeMap::from([
            ("severity".to_string(), Some(false)),
            ("needs_restart".to_string(), None),
        ]);
        let mut mismatch = record("e", 4, "ok");
        mismatch.legend_mismatch = true;
        let summary = summarize(&[
            record("a", 1, "ok"),
            error("b", 2, "rate_limited"),
            disagree,
            error("c", 5, "rate_limited"),
            error("f", 6, "invalid_response"),
            mismatch,
        ]);

        assert_eq!(summary.total, 6);
        assert_eq!(summary.ok, 3);
        assert_eq!(summary.errors_by_class["rate_limited"], 2);
        assert_eq!(summary.errors_by_class["invalid_response"], 1);
        assert_eq!(summary.legend_mismatches, 1);
        // Severity: agreed on a and e, disagreed on d. The three error rows do NOT
        // count as disagreement.
        assert_eq!(summary.agreement["severity"], (2, 3));
        // `needs_restart`: d was not comparable, so only a and e count.
        assert_eq!(summary.agreement["needs_restart"], (0, 2));
    }

    #[test]
    fn policy_refusals_are_skipped_rows_not_errors_and_not_disagreement() {
        let refused = |id: &str| DecisionTraceRecord {
            outcome: "skipped".into(),
            error_class: Some("policy_refused".into()),
            bytes_sent: 0,
            ..error(id, 9, "policy_refused")
        };
        let summary = summarize(&[
            record("a", 1, "ok"),
            refused("r1"),
            refused("r2"),
            error("b", 2, "timeout"),
        ]);
        assert_eq!(summary.total, 4);
        assert_eq!(summary.skipped_by_reason["policy_refused"], 2);
        assert_eq!(summary.errors_by_class.len(), 1);
        assert_eq!(summary.errors_by_class["timeout"], 1);
        assert!(!summary.errors_by_class.contains_key("policy_refused"));
        // Only the one ok row was compared.
        assert_eq!(summary.agreement["severity"], (1, 1));
        // Refusals sent nothing: only the ok and timeout rows carry bytes (512 each).
        assert_eq!(summary.total_bytes_sent, 512 + 512);
    }

    #[test]
    fn default_path_honours_the_override_then_the_profile() {
        // Only the pure fallback shape is checked; env is process-global, so no
        // variable is set here.
        let path = default_db_path();
        assert!(path.to_string_lossy().ends_with("decision_traces.db"));
    }
}
