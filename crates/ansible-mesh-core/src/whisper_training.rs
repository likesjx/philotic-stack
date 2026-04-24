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
                confidence             REAL
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
              timestamp, training_eligible, confidence)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
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
                    timestamp, training_eligible, confidence
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
                    timestamp, training_eligible, confidence
             FROM whisper_training_samples
             WHERE training_eligible = 1
             ORDER BY timestamp DESC LIMIT ?1",
        )?;
        collect_samples(&mut stmt, params![limit as i64])
    }

    fn mark_exported(&self, sample_ids: &[String]) -> Result<()> {
        if sample_ids.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        // SQLite doesn't support bound arrays — use a transaction with individual updates.
        let tx = conn.unchecked_transaction()?;
        for id in sample_ids {
            tx.execute(
                "UPDATE whisper_training_samples SET training_eligible = 0 WHERE sample_id = ?1",
                params![id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn get_by_turn_id(&self, turn_id: &str) -> Result<Option<WhisperTrainingSample>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT sample_id, agent_id, session_id, turn_id, raw_transcript,
                    corrected_transcript, correction_source, model_gen, audio_path,
                    timestamp, training_eligible, confidence
             FROM whisper_training_samples
             WHERE turn_id = ?1
             LIMIT 1",
        )?;
        let mut samples = collect_samples(&mut stmt, params![turn_id])?;
        Ok(samples.pop())
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
}
