use anyhow::Result;
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const TTL_SECS: u64 = 7 * 24 * 3600;

/// SQLite-backed cache for transcoded audio blobs.
/// Key: (SHA-256 of source bytes, target MIME type). TTL: 7 days.
pub struct CodecCache {
    conn: Mutex<Connection>,
}

impl CodecCache {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path.as_ref())?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS codec_cache (
                 source_sha  TEXT    NOT NULL,
                 target_mime TEXT    NOT NULL,
                 result      BLOB    NOT NULL,
                 created_at  INTEGER NOT NULL,
                 PRIMARY KEY (source_sha, target_mime)
             );",
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Look up a cached transcode result. Returns `None` on miss or TTL expiry.
    pub fn get(&self, source_bytes: &[u8], target_mime: &str) -> Result<Option<Vec<u8>>> {
        let sha = sha256_hex(source_bytes);
        let now = unix_now();
        let conn = self.conn.lock().unwrap();
        match conn.query_row(
            "SELECT result, created_at FROM codec_cache \
             WHERE source_sha = ?1 AND target_mime = ?2",
            params![sha, target_mime],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?)),
        ) {
            Ok((bytes, created_at)) => {
                let age = now.saturating_sub(created_at as u64);
                if age < TTL_SECS {
                    Ok(Some(bytes))
                } else {
                    Ok(None)
                }
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Store a transcode result.
    pub fn put(&self, source_bytes: &[u8], target_mime: &str, result: &[u8]) -> Result<()> {
        let sha = sha256_hex(source_bytes);
        let now = unix_now() as i64;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO codec_cache \
             (source_sha, target_mime, result, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![sha, target_mime, result, now],
        )?;
        Ok(())
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
