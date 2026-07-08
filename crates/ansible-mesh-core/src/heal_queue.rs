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

pub trait HealQueueStorage: Send + Sync {
    fn push_error(&self, guest_id: &str, raw_text: &str) -> Result<String>;
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
