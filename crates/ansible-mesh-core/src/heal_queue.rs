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
