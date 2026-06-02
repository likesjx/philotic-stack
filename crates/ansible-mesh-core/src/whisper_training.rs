//! `WhisperTrainingStorage` — durable capture of (audio, transcript) pairs for
//! Whisper ASR fine-tuning.
//!
//! Written exclusively by the `router-listener` guest. Reads are for operator
//! inspection (`phil training list/export`) and the HuggingFace upload pipeline.
//! Node-local by design — no mesh sync.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Filter for [`WhisperTrainingStorage::list_filtered`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TrainingFilter {
    #[default]
    All,
    Uncorrected,
    Eligible,
    Exported,
}

/// Summary counts returned by [`WhisperTrainingStorage::count_status`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrainingStatusCounts {
    pub total: u64,
    pub uncorrected: u64,
    pub eligible: u64,
    pub exported: u64,
}

/// Format for [`WhisperTrainingStorage::export_samples`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrainingExportFormat {
    HuggingFace,
    Nemo,
}

// ──────────────────────────────────────────────────────────────────────────────
// Records
// ──────────────────────────────────────────────────────────────────────────────

/// A single captured (audio, transcript) pair eligible for Whisper fine-tuning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WhisperTrainingSample {
    /// ULID — lexicographically sortable, unique.
    pub sample_id: String,
    /// Agent whose session produced this voice turn.
    pub agent_id: String,
    /// Session identifier.
    pub session_id: String,
    /// Turn identifier — used as the correction lookup key.
    pub turn_id: String,
    /// Raw transcript from Whisper, unmodified.
    pub raw_transcript: String,
    /// Operator-corrected transcript. `None` until a `/correct` command is received.
    #[serde(default)]
    pub corrected_transcript: Option<String>,
    /// Who supplied the correction: `"operator"` | `"auto"`.
    #[serde(default)]
    pub correction_source: Option<String>,
    /// Provenance token from `WhisperBackend::model_gen()` — `"{repo}@{sha8}"`.
    pub model_gen: String,
    /// Absolute path to the copied WAV on this node's filesystem.
    #[serde(default)]
    pub audio_path: Option<String>,
    /// Unix epoch seconds when the sample was captured.
    pub timestamp: u64,
    /// `true` once the sample is ready for export to HuggingFace.
    pub training_eligible: bool,
    /// Confidence score from Whisper (avg log-prob normalised to 0–1), if available.
    #[serde(default)]
    pub confidence: Option<f32>,
    /// Unix epoch seconds when this sample was exported; `None` until first export.
    #[serde(default)]
    pub exported_at: Option<u64>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Trait
// ──────────────────────────────────────────────────────────────────────────────

/// Append-mostly storage for Whisper training samples.
pub trait WhisperTrainingStorage: Send + Sync {
    /// Insert a freshly captured sample. Ignores duplicates (by `sample_id`).
    fn insert_sample(&self, sample: &WhisperTrainingSample) -> Result<()>;

    /// Apply an operator correction to the sample identified by `turn_id`.
    ///
    /// Sets `corrected_transcript`, `correction_source`, and `training_eligible = true`.
    /// Returns `true` if a row was updated.
    fn update_correction(
        &self,
        turn_id: &str,
        corrected_transcript: &str,
        correction_source: &str,
    ) -> Result<bool>;

    /// Return the most recent `limit` samples, newest first.
    fn list_samples(&self, limit: usize) -> Result<Vec<WhisperTrainingSample>>;

    /// Return samples where `training_eligible = true` and not yet exported.
    fn list_eligible(&self, limit: usize) -> Result<Vec<WhisperTrainingSample>>;

    /// Mark a batch of samples as exported (sets `training_eligible = false` to
    /// avoid re-exporting). Caller supplies the sample IDs.
    fn mark_exported(&self, sample_ids: &[String]) -> Result<()>;

    /// Look up a sample by `turn_id`.
    fn get_by_turn_id(&self, turn_id: &str) -> Result<Option<WhisperTrainingSample>>;

    /// Return samples matching `filter`, optionally narrowed to one agent.
    fn list_filtered(
        &self,
        filter: &TrainingFilter,
        agent_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<WhisperTrainingSample>>;

    /// Return aggregate counts grouped by correction/export state.
    fn count_status(&self, agent_id: Option<&str>) -> Result<TrainingStatusCounts>;

    /// Mark a batch of samples exported — sets `training_eligible = false` and
    /// records `exported_at` as current Unix epoch seconds.
    fn mark_exported_at(&self, sample_ids: &[String], exported_at: u64) -> Result<()>;
}

// ──────────────────────────────────────────────────────────────────────────────
// SqliteWhisperTrainingStorage
// ──────────────────────────────────────────────────────────────────────────────

/// SQLite-backed Whisper training store.
///
/// Shares the same database file as `SqliteRouterTraceStorage` when both are
/// opened on the same path (tables are independent).
pub struct SqliteWhisperTrainingStorage {
    conn: Arc<Mutex<Connection>>,
}

impl SqliteWhisperTrainingStorage {
    /// Open (or create) the training database at `path`.
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
        // Enable WAL for concurrent access with router-listener.
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        // Migrate: add exported_at to existing DBs that predate this column.
        let _ = conn
            .execute_batch("ALTER TABLE whisper_training_samples ADD COLUMN exported_at INTEGER;");
        conn.execute_batch(
            "
            BEGIN;

            CREATE TABLE IF NOT EXISTS whisper_training_samples (
                sample_id              TEXT PRIMARY KEY,
                agent_id               TEXT NOT NULL,
                session_id             TEXT NOT NULL DEFAULT '',
                turn_id                TEXT NOT NULL DEFAULT '',
                raw_transcript         TEXT NOT NULL,
                corrected_transcript   TEXT,
                correction_source      TEXT,
                model_gen              TEXT NOT NULL DEFAULT '',
                audio_path             TEXT,
                timestamp              INTEGER NOT NULL,
                training_eligible      INTEGER NOT NULL DEFAULT 0,
                confidence             REAL,
                exported_at            INTEGER
            );

            CREATE INDEX IF NOT EXISTS idx_whisper_training_ts
                ON whisper_training_samples (timestamp DESC);

            CREATE INDEX IF NOT EXISTS idx_whisper_training_eligible
                ON whisper_training_samples (training_eligible, timestamp DESC);

            CREATE INDEX IF NOT EXISTS idx_whisper_training_turn
                ON whisper_training_samples (turn_id);

            COMMIT;
            ",
        )?;
        Ok(())
    }
}

impl WhisperTrainingStorage for SqliteWhisperTrainingStorage {
    fn insert_sample(&self, s: &WhisperTrainingSample) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO whisper_training_samples
             (sample_id, agent_id, session_id, turn_id, raw_transcript,
              corrected_transcript, correction_source, model_gen, audio_path,
              timestamp, training_eligible, confidence, exported_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                s.sample_id,
                s.agent_id,
                s.session_id,
                s.turn_id,
                s.raw_transcript,
                s.corrected_transcript,
                s.correction_source,
                s.model_gen,
                s.audio_path,
                s.timestamp as i64,
                s.training_eligible as i64,
                s.confidence.map(|f| f as f64),
                s.exported_at.map(|t| t as i64),
            ],
        )?;
        Ok(())
    }

    fn update_correction(
        &self,
        turn_id: &str,
        corrected_transcript: &str,
        correction_source: &str,
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let n = conn.execute(
            "UPDATE whisper_training_samples
             SET corrected_transcript = ?1,
                 correction_source    = ?2,
                 training_eligible    = 1
             WHERE turn_id = ?3",
            params![corrected_transcript, correction_source, turn_id],
        )?;
        Ok(n > 0)
    }

    fn list_samples(&self, limit: usize) -> Result<Vec<WhisperTrainingSample>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT sample_id, agent_id, session_id, turn_id, raw_transcript,
                    corrected_transcript, correction_source, model_gen, audio_path,
                    timestamp, training_eligible, confidence, exported_at
             FROM whisper_training_samples
             ORDER BY timestamp DESC LIMIT ?1",
        )?;
        collect_samples(&mut stmt, params![limit as i64])
    }

    fn list_eligible(&self, limit: usize) -> Result<Vec<WhisperTrainingSample>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT sample_id, agent_id, session_id, turn_id, raw_transcript,
                    corrected_transcript, correction_source, model_gen, audio_path,
                    timestamp, training_eligible, confidence, exported_at
             FROM whisper_training_samples
             WHERE training_eligible = 1
             ORDER BY timestamp DESC LIMIT ?1",
        )?;
        collect_samples(&mut stmt, params![limit as i64])
    }

    fn mark_exported(&self, sample_ids: &[String]) -> Result<()> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.mark_exported_at(sample_ids, now)
    }

    fn get_by_turn_id(&self, turn_id: &str) -> Result<Option<WhisperTrainingSample>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT sample_id, agent_id, session_id, turn_id, raw_transcript,
                    corrected_transcript, correction_source, model_gen, audio_path,
                    timestamp, training_eligible, confidence, exported_at
             FROM whisper_training_samples
             WHERE turn_id = ?1
             LIMIT 1",
        )?;
        let mut samples = collect_samples(&mut stmt, params![turn_id])?;
        Ok(samples.pop())
    }

    fn list_filtered(
        &self,
        filter: &TrainingFilter,
        agent_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<WhisperTrainingSample>> {
        let conn = self.conn.lock().unwrap();
        let base = "SELECT sample_id, agent_id, session_id, turn_id, raw_transcript,
                           corrected_transcript, correction_source, model_gen, audio_path,
                           timestamp, training_eligible, confidence, exported_at
                    FROM whisper_training_samples";
        let where_filter = match filter {
            TrainingFilter::All => "",
            TrainingFilter::Uncorrected => "corrected_transcript IS NULL",
            TrainingFilter::Eligible => "training_eligible = 1",
            TrainingFilter::Exported => "exported_at IS NOT NULL",
        };
        let where_agent = agent_id.map(|_| "agent_id = ?2");
        let where_clause = match (where_filter.is_empty(), where_agent) {
            (true, None) => String::new(),
            (false, None) => format!("WHERE {}", where_filter),
            (true, Some(a)) => format!("WHERE {}", a),
            (false, Some(a)) => format!("WHERE {} AND {}", where_filter, a),
        };
        let sql = format!("{} {} ORDER BY timestamp DESC LIMIT ?1", base, where_clause);
        let mut stmt = conn.prepare(&sql)?;
        if let Some(aid) = agent_id {
            collect_samples(&mut stmt, params![limit as i64, aid])
        } else {
            collect_samples(&mut stmt, params![limit as i64])
        }
    }

    fn count_status(&self, agent_id: Option<&str>) -> Result<TrainingStatusCounts> {
        let conn = self.conn.lock().unwrap();
        let agent_clause = if agent_id.is_some() {
            "WHERE agent_id = ?1"
        } else {
            ""
        };
        let sql = format!(
            "SELECT
               COUNT(*),
               SUM(CASE WHEN corrected_transcript IS NULL THEN 1 ELSE 0 END),
               SUM(CASE WHEN training_eligible = 1 THEN 1 ELSE 0 END),
               SUM(CASE WHEN exported_at IS NOT NULL THEN 1 ELSE 0 END)
             FROM whisper_training_samples {}",
            agent_clause
        );
        let (total, uncorrected, eligible, exported) = if let Some(aid) = agent_id {
            conn.query_row(&sql, params![aid], |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, i64>(2)? as u64,
                    row.get::<_, i64>(3)? as u64,
                ))
            })?
        } else {
            conn.query_row(&sql, [], |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, i64>(2)? as u64,
                    row.get::<_, i64>(3)? as u64,
                ))
            })?
        };
        Ok(TrainingStatusCounts {
            total,
            uncorrected,
            eligible,
            exported,
        })
    }

    fn mark_exported_at(&self, sample_ids: &[String], exported_at: u64) -> Result<()> {
        if sample_ids.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;
        for id in sample_ids {
            tx.execute(
                "UPDATE whisper_training_samples
                 SET training_eligible = 0, exported_at = ?1
                 WHERE sample_id = ?2",
                params![exported_at as i64, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
}

fn collect_samples(
    stmt: &mut rusqlite::Statement<'_>,
    params: impl rusqlite::Params,
) -> Result<Vec<WhisperTrainingSample>> {
    let rows = stmt.query_map(params, |row| {
        Ok(WhisperTrainingSample {
            sample_id: row.get(0)?,
            agent_id: row.get(1)?,
            session_id: row.get(2)?,
            turn_id: row.get(3)?,
            raw_transcript: row.get(4)?,
            corrected_transcript: row.get(5)?,
            correction_source: row.get(6)?,
            model_gen: row.get(7)?,
            audio_path: row.get(8)?,
            timestamp: row.get::<_, i64>(9)? as u64,
            training_eligible: row.get::<_, i64>(10)? != 0,
            confidence: row.get::<_, Option<f64>>(11)?.map(|f| f as f32),
            exported_at: row.get::<_, Option<i64>>(12)?.map(|t| t as u64),
        })
    })?;
    let mut samples = Vec::new();
    for row in rows {
        samples.push(row?);
    }
    Ok(samples)
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn open_tmp() -> (SqliteWhisperTrainingStorage, NamedTempFile) {
        let f = NamedTempFile::new().unwrap();
        let s = SqliteWhisperTrainingStorage::open(f.path()).unwrap();
        (s, f)
    }

    fn make_sample(n: u64) -> WhisperTrainingSample {
        WhisperTrainingSample {
            sample_id: format!("sample-{n:04}"),
            agent_id: "bjork".into(),
            session_id: format!("sess-{n}"),
            turn_id: format!("turn-{n}"),
            raw_transcript: format!("this is transcript number {n}"),
            corrected_transcript: None,
            correction_source: None,
            model_gen: "whisper-small@abc12345".into(),
            audio_path: Some(format!("/training/turn-{n}.wav")),
            timestamp: 1_700_000_000 + n,
            training_eligible: false,
            confidence: Some(0.87),
            exported_at: None,
        }
    }

    #[test]
    fn insert_and_list() {
        let (s, _f) = open_tmp();
        for i in 0..5u64 {
            s.insert_sample(&make_sample(i)).unwrap();
        }
        let samples = s.list_samples(3).unwrap();
        assert_eq!(samples.len(), 3);
        // Newest first.
        assert_eq!(samples[0].sample_id, "sample-0004");
    }

    #[test]
    fn correction_marks_eligible() {
        let (s, _f) = open_tmp();
        s.insert_sample(&make_sample(1)).unwrap();

        let updated = s
            .update_correction("turn-1", "corrected text here", "operator")
            .unwrap();
        assert!(updated);

        let sample = s.get_by_turn_id("turn-1").unwrap().unwrap();
        assert_eq!(
            sample.corrected_transcript.as_deref(),
            Some("corrected text here")
        );
        assert_eq!(sample.correction_source.as_deref(), Some("operator"));
        assert!(sample.training_eligible);
    }

    #[test]
    fn correction_returns_false_for_unknown_turn() {
        let (s, _f) = open_tmp();
        let updated = s
            .update_correction("nonexistent-turn", "text", "operator")
            .unwrap();
        assert!(!updated);
    }

    #[test]
    fn list_eligible_only_returns_corrected() {
        let (s, _f) = open_tmp();
        s.insert_sample(&make_sample(1)).unwrap();
        s.insert_sample(&make_sample(2)).unwrap();
        s.update_correction("turn-2", "fixed", "operator").unwrap();

        let eligible = s.list_eligible(10).unwrap();
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].turn_id, "turn-2");
    }

    #[test]
    fn mark_exported_clears_eligibility() {
        let (s, _f) = open_tmp();
        s.insert_sample(&make_sample(1)).unwrap();
        s.update_correction("turn-1", "fixed", "operator").unwrap();
        assert_eq!(s.list_eligible(10).unwrap().len(), 1);

        s.mark_exported(&["sample-0001".to_string()]).unwrap();
        assert_eq!(s.list_eligible(10).unwrap().len(), 0);
    }

    #[test]
    fn idempotent_insert() {
        let (s, _f) = open_tmp();
        let sample = make_sample(1);
        s.insert_sample(&sample).unwrap();
        s.insert_sample(&sample).unwrap(); // duplicate — no error
        assert_eq!(s.list_samples(10).unwrap().len(), 1);
    }

    #[test]
    fn confidence_round_trips() {
        let (s, _f) = open_tmp();
        let mut sample = make_sample(1);
        sample.confidence = Some(0.923);
        s.insert_sample(&sample).unwrap();
        let loaded = s.get_by_turn_id("turn-1").unwrap().unwrap();
        let conf = loaded.confidence.unwrap();
        assert!((conf - 0.923_f32).abs() < 0.001);
    }

    #[test]
    fn get_by_turn_id_returns_none_for_missing() {
        let (s, _f) = open_tmp();
        assert!(s.get_by_turn_id("ghost-turn").unwrap().is_none());
    }

    #[test]
    fn list_filtered_uncorrected() {
        let (s, _f) = open_tmp();
        s.insert_sample(&make_sample(1)).unwrap();
        s.insert_sample(&make_sample(2)).unwrap();
        s.update_correction("turn-2", "fixed", "operator").unwrap();

        let uncorrected = s
            .list_filtered(&TrainingFilter::Uncorrected, None, 10)
            .unwrap();
        assert_eq!(uncorrected.len(), 1);
        assert_eq!(uncorrected[0].turn_id, "turn-1");
    }

    #[test]
    fn list_filtered_by_agent() {
        let (s, _f) = open_tmp();
        let mut sample = make_sample(1);
        sample.agent_id = "aria".into();
        s.insert_sample(&sample).unwrap();
        s.insert_sample(&make_sample(2)).unwrap();

        let aria_only = s
            .list_filtered(&TrainingFilter::All, Some("aria"), 10)
            .unwrap();
        assert_eq!(aria_only.len(), 1);
        assert_eq!(aria_only[0].agent_id, "aria");
    }

    #[test]
    fn count_status_aggregates_correctly() {
        let (s, _f) = open_tmp();
        s.insert_sample(&make_sample(1)).unwrap();
        s.insert_sample(&make_sample(2)).unwrap();
        s.insert_sample(&make_sample(3)).unwrap();
        s.update_correction("turn-2", "fixed", "operator").unwrap();
        s.update_correction("turn-3", "fixed2", "operator").unwrap();
        s.mark_exported_at(&["sample-0003".to_string()], 1_700_001_000)
            .unwrap();

        let counts = s.count_status(None).unwrap();
        assert_eq!(counts.total, 3);
        assert_eq!(counts.uncorrected, 1);
        assert_eq!(counts.eligible, 1); // turn-2 eligible, turn-3 was exported (eligible=0)
        assert_eq!(counts.exported, 1);
    }

    #[test]
    fn list_filtered_exported() {
        let (s, _f) = open_tmp();
        s.insert_sample(&make_sample(1)).unwrap();
        s.insert_sample(&make_sample(2)).unwrap();
        s.update_correction("turn-1", "fixed", "operator").unwrap();
        s.mark_exported_at(&["sample-0001".to_string()], 1_700_001_000)
            .unwrap();

        let exported = s
            .list_filtered(&TrainingFilter::Exported, None, 10)
            .unwrap();
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0].sample_id, "sample-0001");
        assert!(exported[0].exported_at.is_some());
    }
}
