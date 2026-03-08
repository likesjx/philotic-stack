use anyhow::{anyhow, Result};
use hmac::{Hmac, Mac};
use rusqlite::Connection;
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

const REPLAY_WINDOW_SECS: u64 = 300; // 5 minutes

pub struct MeshAuth {
    psk: String,
}

impl MeshAuth {
    pub fn new(psk: impl Into<String>) -> Self {
        Self { psk: psk.into() }
    }

    /// Generates an HMAC-SHA256 signature for the given payload and msg_id.
    pub fn sign(&self, msg_id: &uuid::Uuid, seq: u64, payload: &[u8], timestamp: u64) -> Vec<u8> {
        let mut mac =
            HmacSha256::new_from_slice(self.psk.as_bytes()).expect("HMAC can take key of any size");
        mac.update(msg_id.as_bytes());
        mac.update(&seq.to_be_bytes());
        mac.update(&timestamp.to_be_bytes());
        mac.update(payload);
        mac.finalize().into_bytes().to_vec()
    }

    /// Validates the packet signature and enforces the 5-minute time window.
    pub fn validate(
        &self,
        msg_id: &uuid::Uuid,
        seq: u64,
        payload: &[u8],
        timestamp: u64,
        provided_hmac: &[u8],
    ) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // 1. Time-Window Validation (Replay Guard 1)
        if timestamp > now + 60 {
            return Err(anyhow!("Packet timestamp is in the future"));
        }
        if now.saturating_sub(timestamp) > REPLAY_WINDOW_SECS {
            return Err(anyhow!(
                "Packet timestamp is outside the 5-minute replay window"
            ));
        }

        // 2. Cryptographic Validation
        let mut mac =
            HmacSha256::new_from_slice(self.psk.as_bytes()).expect("HMAC can take key of any size");
        mac.update(msg_id.as_bytes());
        mac.update(&seq.to_be_bytes());
        mac.update(&timestamp.to_be_bytes());
        mac.update(payload);

        mac.verify_slice(provided_hmac)
            .map_err(|_| anyhow!("HMAC signature verification failed"))
    }
}

pub struct NonceTracker {
    conn: Connection,
}

impl NonceTracker {
    pub fn open(db_path: &str) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS mesh_nonces (
                nonce TEXT PRIMARY KEY,
                seen_at INTEGER NOT NULL
            )",
            [],
        )?;
        Ok(Self { conn })
    }

    /// Attempts to register a nonce. Returns an Error if it has already been seen (Replay attack).
    /// If successful, it records the nonce and the current time.
    pub fn assert_and_record_nonce(&self, nonce_uuid: &uuid::Uuid) -> Result<()> {
        let nonce_str = nonce_uuid.to_string();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        match self.conn.execute(
            "INSERT INTO mesh_nonces (nonce, seen_at) VALUES (?1, ?2)",
            (&nonce_str, now),
        ) {
            Ok(_) => Ok(()), // Successfully recorded new nonce
            Err(rusqlite::Error::SqliteFailure(err, _))
                if err.code == rusqlite::ffi::ErrorCode::ConstraintViolation =>
            {
                Err(anyhow!(
                    "Replay detected: Blocked duplicate packet msg_id {}",
                    nonce_str
                ))
            }
            Err(e) => Err(anyhow!("Database error during nonce tracking: {}", e)),
        }
    }

    /// Sweeps nonces older than 5 minutes to prevent SQLite DB bloat.
    pub fn clean_expired_nonces(&self) -> Result<()> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let threshold = now.saturating_sub(REPLAY_WINDOW_SECS);

        self.conn
            .execute("DELETE FROM mesh_nonces WHERE seen_at < ?1", [threshold])?;
        Ok(())
    }
}
