use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealQueueRow {
    pub id: String,
    pub guest_id: String,
    pub timestamp: i64,
    pub raw_text: String,
    pub severity: String,
    pub status: String,
    pub pattern_tag: Option<String>,
    pub heal_action: Option<String>,
    pub outcome: Option<String>,
}

// ── Heal work items (Autopoiesis Slice A3) ───────────────────────────────────

/// Status of a filed heal work item that is awaiting pickup by a coding agent.
pub const HEAL_WORK_ITEM_STATUS_OPEN: &str = "open";
/// Status of a heal work item that was resolved or reversed by the operator.
pub const HEAL_WORK_ITEM_STATUS_CLOSED: &str = "closed";

/// Maximum number of raw evidence lines attached to a heal work item.
pub const MAX_HEAL_EVIDENCE_LINES: usize = 20;
/// Maximum total bytes of evidence attached to a heal work item.
pub const MAX_HEAL_EVIDENCE_BYTES: usize = 4096;

/// A fix request filed by the heal-dispatcher when a `(pattern_tag, guest_id)`
/// failure pattern recurs past the filing threshold (Autopoiesis Slice A3,
/// lane `fleet.heal_slices`).
///
/// Persisted via `GraphDomain` as node kind `heal_work_item`
/// (key `heal_work_item:{work_item_id}`). Dedup invariant: at most one
/// **open** work item per `(pattern_tag, guest_id)` — a re-breach while one is
/// open bumps `count` and `last_seen` instead of filing a duplicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealWorkItemRecord {
    /// Unique id (node key suffix).
    pub work_item_id: String,
    /// Classifier pattern tag (e.g. `"connection_refused"`, `"panic"`).
    pub pattern_tag: String,
    /// The guest the pattern recurred on.
    pub guest_id: String,
    /// Total occurrences observed across all breaches while open.
    pub count: u32,
    /// The sliding-window length (seconds) the breach was measured over.
    pub window_secs: u64,
    /// Capped raw log lines from the breach window (see [`cap_evidence_lines`]).
    pub evidence: Vec<String>,
    /// [`HEAL_WORK_ITEM_STATUS_OPEN`] or [`HEAL_WORK_ITEM_STATUS_CLOSED`].
    pub status: String,
    /// Which component filed the item (e.g. `"heal-dispatcher"`).
    pub filed_by: String,
    /// The `autonomy_audit` record written when this item was filed.
    #[serde(default)]
    pub audit_id: Option<String>,
    /// Unix timestamp (seconds) when the item was filed.
    pub created_at: u64,
    /// Unix timestamp (seconds) of the most recent breach.
    pub last_seen: u64,
}

/// Cap evidence to the most recent [`MAX_HEAL_EVIDENCE_LINES`] lines and
/// [`MAX_HEAL_EVIDENCE_BYTES`] total bytes.
///
/// Keeps the newest lines: older lines are dropped first, and any single
/// oversized line is truncated on a char boundary with a truncation marker.
pub fn cap_evidence_lines(lines: &[String]) -> Vec<String> {
    const MARKER: &str = "… [truncated]";
    let start = lines.len().saturating_sub(MAX_HEAL_EVIDENCE_LINES);
    let mut kept: Vec<String> = lines[start..].to_vec();
    for line in &mut kept {
        if line.len() > MAX_HEAL_EVIDENCE_BYTES {
            let mut cut = MAX_HEAL_EVIDENCE_BYTES - MARKER.len();
            while !line.is_char_boundary(cut) {
                cut -= 1;
            }
            line.truncate(cut);
            line.push_str(MARKER);
        }
    }
    let total = |v: &[String]| v.iter().map(|l| l.len()).sum::<usize>();
    while kept.len() > 1 && total(&kept) > MAX_HEAL_EVIDENCE_BYTES {
        kept.remove(0);
    }
    kept
}

// ── Turn-level failure intake (self-heal) ─────────────────────────────────────

/// Flood-control window: a second heal entry with the same
/// `(guest_id, pattern_tag)` arriving within this many seconds of the previous
/// one is collapsed instead of inserted (see
/// [`HealQueueStorage::push_classified`]). Keeps a burst of identical turn
/// failures from flooding the queue while still letting the heal-dispatcher's
/// A3 recurrence counter (threshold measured over a 30-minute window) see the
/// pattern recur.
pub const HEAL_FLOOD_WINDOW_SECS: u64 = 60;

/// Maximum bytes of raw failure text stored per turn-failure heal entry.
pub const MAX_TURN_FAILURE_RAW_BYTES: usize = 2048;

/// Cap a turn-failure raw line to [`MAX_TURN_FAILURE_RAW_BYTES`], truncating
/// on a char boundary with a truncation marker.
pub fn cap_turn_failure_text(line: &str) -> String {
    const MARKER: &str = "… [truncated]";
    if line.len() <= MAX_TURN_FAILURE_RAW_BYTES {
        return line.to_string();
    }
    let mut cut = MAX_TURN_FAILURE_RAW_BYTES - MARKER.len();
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut out = line[..cut].to_string();
    out.push_str(MARKER);
    out
}

/// Classification of a turn-level failure line (provider errors, model
/// empty responses) into the heal-queue triage triple. Shared between the
/// hotel's FailTask intake (which pre-triages entries at insert) and the
/// heal-dispatcher's `rule_classify` (so both sides always agree on the
/// `pattern_tag` that keys A3 recurrence aggregation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnFailureClass {
    pub severity: String,
    pub pattern_tag: String,
    pub heal_action: String,
    /// Provider parsed from a `provider=<name>` marker, when present.
    pub provider: Option<String>,
}

/// Extract the value of a `key=value` marker (as emitted by
/// `TaskErrorPayload::display_message`, ` | `-separated) from a failure line.
fn marker_value(text: &str, key: &str) -> Option<String> {
    let start = text.find(key)? + key.len();
    let rest = &text[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '|')
        .unwrap_or(rest.len());
    let value = rest[..end].trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// True when the text contains a standalone 3-digit token within `lo..=hi`
/// (an HTTP status code). Tokens glued to letters/digits/dots (`400s`,
/// `v1.400`, `gemini400`) do not match.
fn has_standalone_status(text: &str, lo: u32, hi: u32) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let prev_glued = start > 0
                && (bytes[start - 1].is_ascii_alphanumeric()
                    || bytes[start - 1] == b'.'
                    || bytes[start - 1] == b'_');
            let next_glued =
                i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'.');
            if i - start == 3 && !prev_glued && !next_glued {
                if let Ok(n) = text[start..i].parse::<u32>() {
                    if (lo..=hi).contains(&n) {
                        return true;
                    }
                }
            }
        } else {
            i += 1;
        }
    }
    false
}

/// Classify a turn-level failure line into `(severity, pattern_tag,
/// heal_action, provider)`. Returns `None` for lines that are not
/// provider/model failures — including the failure classes philote reports
/// explicitly via `PushHealEvent` (watchdog evictions, fallback-ladder
/// exhaustion, paracrine budget breaches), so hotel-side FailTask intake never
/// double-reports them.
///
/// Tags produced (all keyed for A3 recurrence aggregation):
/// - `provider_4xx:{provider}` — HTTP 4xx from a provider (escalate — a 400
///   is a request/config problem; restarting the guest cannot fix it)
/// - `provider_5xx:{provider}` — HTTP 5xx from a provider (escalate)
/// - `provider_timeout:{provider}` — provider timed out (noop; recurrence
///   still counts toward a work-item filing)
/// - `provider_error:{provider}` — any other provider failure (escalate)
/// - `model_empty_response` — model returned nothing usable (escalate)
/// - `response_route_unresolved` — an outbound response-like payload (tool
///   result / model response / …) couldn't be resolved to any concrete
///   return guest (escalate; 2026-07-09 stuck-turn forensic RC-4)
/// - `cross_hotel_misroute` — a session's active incarnation was a poisoned
///   non-agent infra guest (tool/datasource/gateway/model/*-runner) and no
///   agent fallback could be resolved either (escalate; RC-2/RC-4)
/// - `duplicate_finalization` — a second terminal send was dropped for a turn
///   whose final response was already delivered (escalate; RC-3/RC-4)
/// - `orphaned_tool_result` — a tool/datasource result was left with no live
///   consumer to deliver to (escalate; RC-2/RC-4)
pub fn classify_turn_failure(line: &str) -> Option<TurnFailureClass> {
    let lower = line.to_lowercase();

    // Failure classes philote pushes explicitly (with richer tags) via
    // PushHealEvent — skip so FailTask intake doesn't double-report them.
    if lower.contains("all model providers failed")
        || lower.contains("delegation chain exceeded")
        || lower.contains("turn_watchdog_timeout")
        || lower.contains("turn watchdog evicted")
    {
        return None;
    }

    // 2026-07-09 stuck-turn forensic (RC-2/RC-3/RC-4): routing mis-routes and
    // duplicate-finalization drops previously filed zero heal rows because
    // nothing classified them. Recognize the stable bracket markers the RC-2
    // reject path, the RC-3 dispatch-dedup drop path, and any orphaned
    // tool-result signature stamp on their raw text.
    if lower.contains("response_route_unresolved") {
        return Some(TurnFailureClass {
            severity: "medium".into(),
            pattern_tag: "response_route_unresolved".into(),
            heal_action: "escalate".into(),
            provider: None,
        });
    }
    if lower.contains("cross_hotel_misroute") {
        return Some(TurnFailureClass {
            severity: "medium".into(),
            pattern_tag: "cross_hotel_misroute".into(),
            heal_action: "escalate".into(),
            provider: None,
        });
    }
    if lower.contains("duplicate_finalization") {
        return Some(TurnFailureClass {
            severity: "medium".into(),
            pattern_tag: "duplicate_finalization".into(),
            heal_action: "escalate".into(),
            provider: None,
        });
    }
    if lower.contains("orphaned_tool_result") {
        return Some(TurnFailureClass {
            severity: "medium".into(),
            pattern_tag: "orphaned_tool_result".into(),
            heal_action: "escalate".into(),
            provider: None,
        });
    }

    let provider = marker_value(line, "provider=").filter(|p| p != "unknown");
    let is_provider_failure = lower.contains("kind=provider_failure")
        || lower.contains("component=model-router")
        || lower.contains("component=model_router");

    if !is_provider_failure {
        // philote's fail_active_turn error code for "model produced no
        // usable output" — intake even without provider markers.
        if lower.contains("model_empty_response") {
            return Some(TurnFailureClass {
                severity: "medium".into(),
                pattern_tag: "model_empty_response".into(),
                heal_action: "escalate".into(),
                provider,
            });
        }
        return None;
    }

    // NOTE: philote's fail_active_turn stamps `[MODEL_EMPTY_RESPONSE]` on
    // every failed turn, so inside a provider-marked line only the actual
    // empty-response *wording* selects the model_empty_response tag.
    let provider_label = provider.as_deref().unwrap_or("unknown");
    let (pattern_tag, heal_action) =
        if lower.contains("empty response") || lower.contains("empty candidate") {
            ("model_empty_response".to_string(), "escalate")
        } else if has_standalone_status(&lower, 400, 499) {
            (format!("provider_4xx:{provider_label}"), "escalate")
        } else if has_standalone_status(&lower, 500, 599) {
            (format!("provider_5xx:{provider_label}"), "escalate")
        } else if lower.contains("timed out")
            || lower.contains("timeout")
            || lower.contains("deadline exceeded")
        {
            (format!("provider_timeout:{provider_label}"), "noop")
        } else {
            (format!("provider_error:{provider_label}"), "escalate")
        };

    Some(TurnFailureClass {
        severity: "medium".into(),
        pattern_tag,
        heal_action: heal_action.into(),
        provider,
    })
}

/// Map a pre-assigned turn-failure `pattern_tag` (base, before any `:` suffix)
/// to the heal action the dispatcher should take. Used for heal-queue rows
/// that arrive pre-triaged (hotel FailTask intake, philote `PushHealEvent`).
///
/// `provider_4xx` deliberately maps to `escalate` (→ A3 work-item path), NOT
/// `restart_guest` — a 400 is not fixed by restarting the guest.
pub fn heal_action_for_pattern_tag(tag: &str) -> &'static str {
    let base = tag.split(':').next().unwrap_or(tag);
    match base {
        "provider_4xx"
        | "provider_5xx"
        | "provider_error"
        | "model_empty_response"
        | "stuck_turn_evicted"
        | "fallback_exhausted"
        | "paracrine_budget_exhausted"
        | "response_route_unresolved"
        | "cross_hotel_misroute"
        | "duplicate_finalization"
        | "orphaned_tool_result" => "escalate",
        "provider_timeout" => "noop",
        _ => "noop",
    }
}

pub trait HealQueueStorage: Send + Sync {
    fn push_error(&self, guest_id: &str, raw_text: &str) -> Result<String>;
    /// Push an entry that is already classified (severity + pattern_tag known
    /// at insert), with flood control: a second entry with the same
    /// `(guest_id, pattern_tag)` within [`HEAL_FLOOD_WINDOW_SECS`] collapses
    /// and returns `Ok(None)`.
    ///
    /// The default implementation falls back to [`push_error`]
    /// (no dedup, severity/pattern dropped) so existing trait mocks keep
    /// compiling; real stores should override it.
    ///
    /// [`push_error`]: HealQueueStorage::push_error
    fn push_classified(
        &self,
        guest_id: &str,
        raw_text: &str,
        _severity: &str,
        _pattern_tag: &str,
    ) -> Result<Option<String>> {
        self.push_error(guest_id, raw_text).map(Some)
    }
    fn pending_errors(&self, limit: usize) -> Result<Vec<HealQueueRow>>;
    fn update_triage(
        &self,
        id: &str,
        severity: &str,
        pattern_tag: &str,
        heal_action: &str,
    ) -> Result<()>;
    fn resolve(&self, id: &str, outcome: &str) -> Result<()>;
    fn vacuum_old(&self, older_than_secs: u64) -> Result<usize>;
}

// ── SQLite impl ───────────────────────────────────────────────────────────────

pub struct SqliteHealQueueStorage {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteHealQueueStorage {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.init_schema()?;
        Ok(store)
    }

    /// [`HealQueueStorage::push_classified`] with an explicit clock, for
    /// deterministic flood-window tests.
    fn push_classified_at(
        &self,
        guest_id: &str,
        raw_text: &str,
        severity: &str,
        pattern_tag: &str,
        now: i64,
    ) -> Result<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let cutoff = now - HEAL_FLOOD_WINDOW_SECS as i64;
        // Flood control: collapse into any same-(guest, tag) entry seen inside
        // the window, regardless of its triage status.
        let recent: i64 = conn.query_row(
            "SELECT COUNT(*) FROM heal_queue
             WHERE guest_id = ?1 AND pattern_tag = ?2 AND timestamp >= ?3",
            params![guest_id, pattern_tag, cutoff],
            |row| row.get(0),
        )?;
        if recent > 0 {
            return Ok(None);
        }
        let id = ulid::Ulid::new().to_string();
        conn.execute(
            "INSERT INTO heal_queue (id, guest_id, timestamp, raw_text, severity, pattern_tag)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![id, guest_id, now, raw_text, severity, pattern_tag],
        )?;
        Ok(Some(id))
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch(
            "
            BEGIN;

            CREATE TABLE IF NOT EXISTS heal_queue (
                id           TEXT PRIMARY KEY,
                guest_id     TEXT NOT NULL,
                timestamp    INTEGER NOT NULL,
                raw_text     TEXT NOT NULL,
                severity     TEXT NOT NULL DEFAULT 'unknown',
                status       TEXT NOT NULL DEFAULT 'pending',
                pattern_tag  TEXT,
                heal_action  TEXT,
                outcome      TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_heal_queue_status
                ON heal_queue (status, timestamp DESC);

            CREATE INDEX IF NOT EXISTS idx_heal_queue_guest
                ON heal_queue (guest_id, timestamp DESC);

            COMMIT;
            ",
        )?;
        Ok(())
    }
}

impl HealQueueStorage for SqliteHealQueueStorage {
    fn push_error(&self, guest_id: &str, raw_text: &str) -> Result<String> {
        let id = ulid::Ulid::new().to_string();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO heal_queue (id, guest_id, timestamp, raw_text) VALUES (?1, ?2, ?3, ?4)",
            params![id, guest_id, now, raw_text],
        )?;
        Ok(id)
    }

    fn push_classified(
        &self,
        guest_id: &str,
        raw_text: &str,
        severity: &str,
        pattern_tag: &str,
    ) -> Result<Option<String>> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.push_classified_at(guest_id, raw_text, severity, pattern_tag, now)
    }

    fn pending_errors(&self, limit: usize) -> Result<Vec<HealQueueRow>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, guest_id, timestamp, raw_text, severity, status,
                    pattern_tag, heal_action, outcome
             FROM heal_queue WHERE status = 'pending'
             ORDER BY timestamp DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(HealQueueRow {
                id: row.get(0)?,
                guest_id: row.get(1)?,
                timestamp: row.get(2)?,
                raw_text: row.get(3)?,
                severity: row.get(4)?,
                status: row.get(5)?,
                pattern_tag: row.get(6)?,
                heal_action: row.get(7)?,
                outcome: row.get(8)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }

    fn update_triage(
        &self,
        id: &str,
        severity: &str,
        pattern_tag: &str,
        heal_action: &str,
    ) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE heal_queue SET severity = ?1, pattern_tag = ?2, heal_action = ?3,
                                   status = 'assigned'
             WHERE id = ?4",
            params![severity, pattern_tag, heal_action, id],
        )?;
        Ok(())
    }

    fn resolve(&self, id: &str, outcome: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE heal_queue SET outcome = ?1, status = 'resolved' WHERE id = ?2",
            params![outcome, id],
        )?;
        Ok(())
    }

    fn vacuum_old(&self, older_than_secs: u64) -> Result<usize> {
        let cutoff = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            .saturating_sub(older_than_secs) as i64;
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "DELETE FROM heal_queue WHERE status = 'resolved' AND timestamp < ?1",
            params![cutoff],
        )?;
        Ok(n)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_evidence_keeps_newest_lines_within_line_and_byte_budgets() {
        // Under both budgets: untouched.
        let small: Vec<String> = (0..3).map(|i| format!("line {i}")).collect();
        assert_eq!(cap_evidence_lines(&small), small);

        // Over the line budget: only the newest MAX_HEAL_EVIDENCE_LINES survive.
        let many: Vec<String> = (0..50).map(|i| format!("line {i}")).collect();
        let capped = cap_evidence_lines(&many);
        assert_eq!(capped.len(), MAX_HEAL_EVIDENCE_LINES);
        assert_eq!(capped.first().unwrap(), "line 30");
        assert_eq!(capped.last().unwrap(), "line 49");

        // Over the byte budget: oldest lines dropped until the total fits.
        let fat: Vec<String> = (0..30).map(|i| format!("{i:0>300}")).collect();
        let capped = cap_evidence_lines(&fat);
        assert!(capped.len() < MAX_HEAL_EVIDENCE_LINES);
        assert!(capped.iter().map(|l| l.len()).sum::<usize>() <= MAX_HEAL_EVIDENCE_BYTES);
        assert_eq!(capped.last().unwrap(), &format!("{:0>300}", 29));
    }

    #[test]
    fn cap_evidence_truncates_single_oversized_line_on_char_boundary() {
        let huge = vec!["é".repeat(MAX_HEAL_EVIDENCE_BYTES)];
        let capped = cap_evidence_lines(&huge);
        assert_eq!(capped.len(), 1);
        assert!(capped[0].len() <= MAX_HEAL_EVIDENCE_BYTES);
        assert!(capped[0].ends_with("[truncated]"));
    }

    // ── Turn-failure classifier matrix ────────────────────────────────────────

    fn tag_of(line: &str) -> Option<String> {
        classify_turn_failure(line).map(|c| c.pattern_tag)
    }

    #[test]
    fn classify_turn_failure_provider_matrix() {
        // 4xx with provider marker → provider_4xx:{provider}, escalate.
        let c = classify_turn_failure(
            "[MODEL_EMPTY_RESPONSE] Model failed: Gemini API error 400 Bad Request \
             | kind=provider_failure | component=model-router | provider=gemini \
             | capability=text.generate",
        )
        .expect("classified");
        assert_eq!(c.pattern_tag, "provider_4xx:gemini");
        assert_eq!(c.heal_action, "escalate", "a 400 is never fixed by restart");
        assert_eq!(c.severity, "medium");
        assert_eq!(c.provider.as_deref(), Some("gemini"));

        // 429 rate limit is a 4xx.
        assert_eq!(
            tag_of("http 429 too many requests | kind=provider_failure | provider=anthropic")
                .as_deref(),
            Some("provider_4xx:anthropic")
        );

        // 5xx → provider_5xx.
        assert_eq!(
            tag_of("HTTP 503 service unavailable | kind=provider_failure | provider=gemini")
                .as_deref(),
            Some("provider_5xx:gemini")
        );

        // Timeout without a status code → provider_timeout, noop.
        let c = classify_turn_failure(
            "streaming request timed out after 120s | kind=provider_failure | provider=gemini",
        )
        .expect("classified");
        assert_eq!(c.pattern_tag, "provider_timeout:gemini");
        assert_eq!(c.heal_action, "noop");

        // Other provider failure → provider_error.
        assert_eq!(
            tag_of("tls handshake broke | component=model-router | provider=elevenlabs").as_deref(),
            Some("provider_error:elevenlabs")
        );

        // Missing/unknown provider marker → :unknown label, provider None.
        let c = classify_turn_failure("something odd | kind=provider_failure").expect("classified");
        assert_eq!(c.pattern_tag, "provider_error:unknown");
        assert_eq!(c.provider, None);
    }

    #[test]
    fn classify_turn_failure_model_empty_response() {
        // Bare MODEL_EMPTY_RESPONSE (no provider markers) still classifies.
        let c = classify_turn_failure("[MODEL_EMPTY_RESPONSE] The model returned nothing.")
            .expect("classified");
        assert_eq!(c.pattern_tag, "model_empty_response");
        assert_eq!(c.heal_action, "escalate");

        // Empty-response wording inside a provider failure wins over 4xx/timeout.
        assert_eq!(
            tag_of("provider returned empty response | kind=provider_failure | provider=gemini")
                .as_deref(),
            Some("model_empty_response")
        );
    }

    // RC-4 (2026-07-09 stuck-turn forensic): the new RC-2/RC-3 emit points stamp
    // stable bracket markers on their raw text; the classifier must recognize all
    // four so these failure classes stop filing zero heal rows.
    #[test]
    fn classify_turn_failure_recognizes_2026_07_09_forensic_markers() {
        let c = classify_turn_failure(
            "[response_route_unresolved] response-like action [tool_result] targeted role [agent] without a concrete return guest",
        )
        .expect("classified");
        assert_eq!(c.pattern_tag, "response_route_unresolved");
        assert_eq!(c.severity, "medium");
        assert_eq!(c.heal_action, "escalate");

        let c = classify_turn_failure(
            "[cross_hotel_misroute] response-like action for session [sess-1] targeted non-agent infra guest [vps-jane:life-graph-runner] with no resolvable agent fallback",
        )
        .expect("classified");
        assert_eq!(c.pattern_tag, "cross_hotel_misroute");
        assert_eq!(c.severity, "medium");
        assert_eq!(c.heal_action, "escalate");

        let c = classify_turn_failure(
            "[duplicate_finalization] dropped second terminal send for turn [turn-1]; final response already sent",
        )
        .expect("classified");
        assert_eq!(c.pattern_tag, "duplicate_finalization");
        assert_eq!(c.severity, "medium");
        assert_eq!(c.heal_action, "escalate");

        let c = classify_turn_failure(
            "[orphaned_tool_result] tool result for turn [turn-1] has no live consumer to deliver to",
        )
        .expect("classified");
        assert_eq!(c.pattern_tag, "orphaned_tool_result");
        assert_eq!(c.severity, "medium");
        assert_eq!(c.heal_action, "escalate");
    }

    #[test]
    fn classify_turn_failure_skips_non_turn_failures_and_philote_reported_classes() {
        // Not a provider/model failure at all.
        assert_eq!(tag_of("[APPROVAL_CANCELLED] operator cancelled"), None);
        assert_eq!(tag_of("connection refused"), None);
        // Classes philote reports explicitly via PushHealEvent.
        assert_eq!(
            tag_of("[MODEL_EMPTY_RESPONSE] All model providers failed. Please try again later."),
            None
        );
        assert_eq!(
            tag_of("[TURN_WATCHDOG_TIMEOUT] Turn watchdog evicted stuck turn after 91s in WaitingTool."),
            None
        );
        assert_eq!(
            tag_of("[MODEL_EMPTY_RESPONSE] This delegation chain exceeded its budget of 4 hops"),
            None
        );
    }

    #[test]
    fn standalone_status_detection_ignores_glued_tokens() {
        assert!(has_standalone_status("error 400 bad request", 400, 499));
        assert!(!has_standalone_status("evicted after 400s", 400, 499));
        assert!(!has_standalone_status("gemini400 alerts", 400, 499));
        assert!(!has_standalone_status("v1.400 build", 400, 499));
        assert!(!has_standalone_status("code 4000", 400, 499));
    }

    #[test]
    fn heal_action_for_pattern_tag_matrix() {
        assert_eq!(
            heal_action_for_pattern_tag("provider_4xx:gemini"),
            "escalate"
        );
        assert_eq!(
            heal_action_for_pattern_tag("provider_5xx:gemini"),
            "escalate"
        );
        assert_eq!(
            heal_action_for_pattern_tag("provider_timeout:gemini"),
            "noop"
        );
        assert_eq!(
            heal_action_for_pattern_tag("provider_error:onnx"),
            "escalate"
        );
        assert_eq!(
            heal_action_for_pattern_tag("model_empty_response"),
            "escalate"
        );
        assert_eq!(
            heal_action_for_pattern_tag("stuck_turn_evicted:WaitingTool"),
            "escalate"
        );
        assert_eq!(
            heal_action_for_pattern_tag("fallback_exhausted:gemini"),
            "escalate"
        );
        assert_eq!(
            heal_action_for_pattern_tag("paracrine_budget_exhausted"),
            "escalate"
        );
        assert_eq!(heal_action_for_pattern_tag("something_else"), "noop");

        // RC-4 (2026-07-09 stuck-turn forensic) additions.
        assert_eq!(
            heal_action_for_pattern_tag("response_route_unresolved"),
            "escalate"
        );
        assert_eq!(
            heal_action_for_pattern_tag("cross_hotel_misroute"),
            "escalate"
        );
        assert_eq!(
            heal_action_for_pattern_tag("duplicate_finalization"),
            "escalate"
        );
        assert_eq!(
            heal_action_for_pattern_tag("orphaned_tool_result"),
            "escalate"
        );
    }

    #[test]
    fn cap_turn_failure_text_truncates_on_char_boundary() {
        let short = "short line";
        assert_eq!(cap_turn_failure_text(short), short);
        let huge = "é".repeat(MAX_TURN_FAILURE_RAW_BYTES);
        let capped = cap_turn_failure_text(&huge);
        assert!(capped.len() <= MAX_TURN_FAILURE_RAW_BYTES);
        assert!(capped.ends_with("[truncated]"));
    }

    // ── Flood control ─────────────────────────────────────────────────────────

    #[test]
    fn push_classified_collapses_same_tag_and_guest_within_window() {
        let store = SqliteHealQueueStorage::open(":memory:").expect("open");
        const T0: i64 = 1_750_000_000;

        let first = store
            .push_classified_at(
                "model-controller-gemini",
                "400 A",
                "medium",
                "provider_4xx:gemini",
                T0,
            )
            .expect("push");
        assert!(first.is_some(), "first entry inserts");

        // Same (guest, tag) inside the window: collapsed.
        let burst = store
            .push_classified_at(
                "model-controller-gemini",
                "400 B",
                "medium",
                "provider_4xx:gemini",
                T0 + 30,
            )
            .expect("push");
        assert!(
            burst.is_none(),
            "burst inside {HEAL_FLOOD_WINDOW_SECS}s collapses"
        );

        // Different tag, same guest: inserts.
        assert!(store
            .push_classified_at(
                "model-controller-gemini",
                "t/o",
                "medium",
                "provider_timeout:gemini",
                T0 + 30
            )
            .expect("push")
            .is_some());

        // Same tag, different guest: inserts.
        assert!(store
            .push_classified_at(
                "model-controller-openai",
                "400 C",
                "medium",
                "provider_4xx:openai",
                T0 + 30
            )
            .expect("push")
            .is_some());

        // Same (guest, tag) after the window: inserts again.
        assert!(store
            .push_classified_at(
                "model-controller-gemini",
                "400 D",
                "medium",
                "provider_4xx:gemini",
                T0 + HEAL_FLOOD_WINDOW_SECS as i64 + 1,
            )
            .expect("push")
            .is_some());

        // Collapse still applies after the earlier entry is triaged (status
        // change must not reopen the window).
        let rows = store.pending_errors(10).expect("pending");
        let timeout_row = rows
            .iter()
            .find(|r| r.pattern_tag.as_deref() == Some("provider_timeout:gemini"))
            .expect("timeout row present");
        store
            .update_triage(&timeout_row.id, "medium", "provider_timeout:gemini", "noop")
            .expect("triage");
        assert!(store
            .push_classified_at(
                "model-controller-gemini",
                "t/o 2",
                "medium",
                "provider_timeout:gemini",
                T0 + 59
            )
            .expect("push")
            .is_none());
    }

    #[test]
    fn push_classified_rows_surface_pre_triaged_fields() {
        let store = SqliteHealQueueStorage::open(":memory:").expect("open");
        store
            .push_classified("g-1", "boom 400", "medium", "provider_4xx:gemini")
            .expect("push")
            .expect("inserted");
        let rows = store.pending_errors(10).expect("pending");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].severity, "medium");
        assert_eq!(rows[0].pattern_tag.as_deref(), Some("provider_4xx:gemini"));
        assert_eq!(rows[0].status, "pending");
    }

    #[test]
    fn heal_work_item_serde_round_trip_and_legacy_without_audit_id() {
        let item = HealWorkItemRecord {
            work_item_id: "wi-1".into(),
            pattern_tag: "connection_refused".into(),
            guest_id: "membrane-telegram-01".into(),
            count: 5,
            window_secs: 1800,
            evidence: vec!["connection refused".into()],
            status: HEAL_WORK_ITEM_STATUS_OPEN.into(),
            filed_by: "heal-dispatcher".into(),
            audit_id: Some("heal_filing:wi-1".into()),
            created_at: 1_750_000_000,
            last_seen: 1_750_000_000,
        };
        let json = serde_json::to_value(&item).expect("serialize");
        let back: HealWorkItemRecord = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, item);

        // Records written without audit_id still deserialize.
        let legacy = serde_json::json!({
            "work_item_id": "wi-2",
            "pattern_tag": "panic",
            "guest_id": "philote-01",
            "count": 7,
            "window_secs": 900,
            "evidence": [],
            "status": "open",
            "filed_by": "heal-dispatcher",
            "created_at": 1,
            "last_seen": 2,
        });
        let back: HealWorkItemRecord = serde_json::from_value(legacy).expect("legacy");
        assert_eq!(back.audit_id, None);
    }
}
