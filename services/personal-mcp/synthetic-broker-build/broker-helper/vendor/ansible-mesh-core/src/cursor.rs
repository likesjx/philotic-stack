use rusqlite::{params, Connection, Result as SqlResult};
use std::path::Path;

use std::sync::{Arc, Mutex};

/// Tracks standard outbound sequence ACKs per remote node cursor.
#[derive(Clone)]
pub struct CursorTracker {
    pub conn: Arc<Mutex<Connection>>,
}

impl CursorTracker {
    pub fn open(db_path: impl AsRef<Path>) -> SqlResult<Self> {
        let conn = Connection::open(db_path)?;
        let tracker = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        tracker.init_schema()?;
        Ok(tracker)
    }

    fn init_schema(&self) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "CREATE TABLE IF NOT EXISTS mesh_cursors (
                consumer_node_id TEXT PRIMARY KEY,
                last_acked_seq INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL
            )",
            [],
        )?;
        Ok(())
    }

    pub fn get_cursor(&self, consumer_node_id: &str) -> SqlResult<u64> {
        let conn = self.conn.lock().unwrap();
        let seq = conn
            .query_row(
                "SELECT last_acked_seq FROM mesh_cursors WHERE consumer_node_id = ?1",
                params![consumer_node_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        Ok(seq)
    }

    pub fn advance_cursor(&self, consumer_node_id: &str, acked_seq: u64, ts: u64) -> SqlResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO mesh_cursors (consumer_node_id, last_acked_seq, updated_at) 
             VALUES (?1, ?2, ?3)
             ON CONFLICT(consumer_node_id) DO UPDATE SET 
             last_acked_seq = MAX(last_acked_seq, excluded.last_acked_seq),
             updated_at = excluded.updated_at",
            params![consumer_node_id, acked_seq, ts],
        )?;
        Ok(())
    }
}
