use anyhow::{anyhow, Result};
use hmac::{Hmac, Mac};
use rusqlite::Connection;
use sha2::Sha256;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

type HmacSha256 = Hmac<Sha256>;

const REPLAY_WINDOW_SECS: u64 = 300; // 5 minutes

/// A sender's clock may run behind ours; a packet stamped up to this many
/// seconds before this process started is still accepted (see
/// [`process_start`]).
const RESTART_SKEW_SECS: u64 = 10;

/// Unix time this process first touched mesh auth.
fn process_start() -> u64 {
    static START: OnceLock<u64> = OnceLock::new();
    *START.get_or_init(unix_now)
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub struct MeshAuth {
    psk: String,
}

impl MeshAuth {
    pub fn new(psk: impl Into<String>) -> Self {
        // Pin the process start early: the replay window is held in memory
        // now, so packets stamped before this process started are refused
        // instead of being remembered across a restart.
        let _ = process_start();
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
        // The replay window lives in memory, so a restart forgets what was
        // seen. Refuse anything stamped before this process started: it is
        // exactly what an attacker could have captured earlier and replays now.
        // A live sender re-sends with a fresh stamp within a second or a
        // heartbeat, so this costs at most a few seconds after a restart.
        if timestamp.saturating_add(RESTART_SKEW_SECS) < process_start() {
            return Err(anyhow!(
                "Packet predates this process (restart replay guard)"
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

/// Most nonces held at once. A legitimate peer sends a few thousand packets
/// in a five-minute window; only authenticated packets reach the tracker.
const MAX_TRACKED_NONCES: usize = 500_000;

/// How often the tracker sweeps expired nonces while recording new ones.
const NONCE_SWEEP_INTERVAL_SECS: u64 = 30;

/// Replay guard: remembers every accepted `msg_id` for the replay window.
///
/// It used to be a SQLite table, and it was written on EVERY inbound packet —
/// heartbeats every 3 s from every peer, plus each execution message on a
/// brand-new connection to the live hotel DB — while the sweep that should
/// have pruned it was never called. Live 2026-09-19: 2.7-4.1M rows / 274-411 MB
/// per hotel since May, another 826k rows in the hotel DB, and `database is
/// locked` packet drops. A five-minute window belongs in memory.
pub struct NonceTracker {
    seen: Mutex<NonceState>,
}

struct NonceState {
    nonces: HashMap<uuid::Uuid, u64>,
    last_sweep: u64,
}

impl Default for NonceTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl NonceTracker {
    pub fn new() -> Self {
        let _ = process_start();
        Self {
            seen: Mutex::new(NonceState {
                nonces: HashMap::new(),
                last_sweep: unix_now(),
            }),
        }
    }

    /// The process-wide tracker for the execution plane, whose connections
    /// are handled by independent tasks.
    pub fn shared() -> &'static NonceTracker {
        static SHARED: OnceLock<NonceTracker> = OnceLock::new();
        SHARED.get_or_init(NonceTracker::new)
    }

    /// Attempts to register a nonce. Returns an Error if it has already been
    /// seen inside the replay window (Replay attack).
    pub fn assert_and_record_nonce(&self, nonce_uuid: &uuid::Uuid) -> Result<()> {
        let now = unix_now();
        let mut state = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        if now.saturating_sub(state.last_sweep) >= NONCE_SWEEP_INTERVAL_SECS
            || state.nonces.len() >= MAX_TRACKED_NONCES
        {
            let threshold = now.saturating_sub(REPLAY_WINDOW_SECS);
            state.nonces.retain(|_, seen_at| *seen_at >= threshold);
            state.last_sweep = now;
        }
        if let Some(seen_at) = state.nonces.get(nonce_uuid) {
            // An entry past the window is stale, not a replay — the packet's
            // own timestamp check would already have refused it.
            if now.saturating_sub(*seen_at) <= REPLAY_WINDOW_SECS {
                return Err(anyhow!(
                    "Replay detected: Blocked duplicate packet msg_id {}",
                    nonce_uuid
                ));
            }
        }
        if state.nonces.len() >= MAX_TRACKED_NONCES {
            return Err(anyhow!(
                "Nonce window saturated ({} entries); dropping packet {}",
                state.nonces.len(),
                nonce_uuid
            ));
        }
        state.nonces.insert(*nonce_uuid, now);
        Ok(())
    }

    /// Drop nonces older than the replay window. Recording does this itself
    /// every [`NONCE_SWEEP_INTERVAL_SECS`]; this is for an explicit sweep.
    pub fn clean_expired_nonces(&self) -> Result<()> {
        let threshold = unix_now().saturating_sub(REPLAY_WINDOW_SECS);
        let mut state = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        state.nonces.retain(|_, seen_at| *seen_at >= threshold);
        Ok(())
    }

    /// Nonces currently held.
    pub fn len(&self) -> usize {
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .nonces
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Retire the SQLite nonce store this tracker replaced. A sidecar file
    /// (`…/nonces.db`) is deleted outright; in any other database the
    /// `mesh_nonces` table is dropped. Best effort: returns what it removed.
    pub fn retire_legacy_store(db_path: &str) -> Option<String> {
        let path = Path::new(db_path);
        if path.file_name().and_then(|n| n.to_str()) == Some("nonces.db") {
            let existed = path.exists();
            for suffix in ["", "-journal", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{db_path}{suffix}"));
            }
            return existed.then(|| format!("removed legacy sidecar {db_path}"));
        }
        let conn = Connection::open(db_path).ok()?;
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
        let had_table: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='mesh_nonces'",
                [],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if !had_table {
            return None;
        }
        conn.execute("DROP TABLE IF EXISTS mesh_nonces", []).ok()?;
        Some(format!("dropped legacy mesh_nonces table in {db_path}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_nonce_is_accepted_once_and_a_replay_is_refused() {
        let tracker = NonceTracker::new();
        let id = uuid::Uuid::new_v4();
        assert!(tracker.assert_and_record_nonce(&id).is_ok());
        let err = tracker.assert_and_record_nonce(&id).unwrap_err();
        assert!(err.to_string().contains("Replay detected"), "{err}");
        assert!(tracker
            .assert_and_record_nonce(&uuid::Uuid::new_v4())
            .is_ok());
    }

    #[test]
    fn expired_nonces_are_swept_and_the_memory_stays_bounded() {
        let tracker = NonceTracker::new();
        let old = uuid::Uuid::new_v4();
        tracker.assert_and_record_nonce(&old).unwrap();
        {
            let mut state = tracker.seen.lock().unwrap();
            state
                .nonces
                .insert(old, unix_now() - REPLAY_WINDOW_SECS - 5);
            state.last_sweep = unix_now() - NONCE_SWEEP_INTERVAL_SECS - 1;
        }
        // Recording sweeps: the stale entry goes, and it is not a replay.
        assert!(tracker
            .assert_and_record_nonce(&uuid::Uuid::new_v4())
            .is_ok());
        assert_eq!(tracker.len(), 1, "only the fresh nonce is held");
        assert!(tracker.assert_and_record_nonce(&old).is_ok());

        tracker.clean_expired_nonces().unwrap();
        assert_eq!(tracker.len(), 2);
    }

    #[test]
    fn a_saturated_window_refuses_new_packets_instead_of_growing() {
        let tracker = NonceTracker::new();
        {
            let mut state = tracker.seen.lock().unwrap();
            let now = unix_now();
            for _ in 0..MAX_TRACKED_NONCES {
                state.nonces.insert(uuid::Uuid::new_v4(), now);
            }
        }
        let err = tracker
            .assert_and_record_nonce(&uuid::Uuid::new_v4())
            .unwrap_err();
        assert!(err.to_string().contains("saturated"), "{err}");
        assert_eq!(tracker.len(), MAX_TRACKED_NONCES);
    }

    /// The window is in memory now, so a restart forgets what was seen: a
    /// packet captured before this process started must not be replayable.
    #[test]
    fn a_packet_stamped_before_this_process_started_is_refused() {
        let auth = MeshAuth::new("k");
        let id = uuid::Uuid::new_v4();
        let before_start = process_start() - RESTART_SKEW_SECS - 30;
        let hmac = auth.sign(&id, 1, b"p", before_start);
        let err = auth
            .validate(&id, 1, b"p", before_start, &hmac)
            .unwrap_err();
        assert!(err.to_string().contains("restart replay guard"), "{err}");

        // A packet stamped now is fine, and one only slightly behind (a peer's
        // slow clock) is tolerated.
        let now = unix_now();
        let ok = auth.sign(&id, 1, b"p", now);
        assert!(auth.validate(&id, 1, b"p", now, &ok).is_ok());
        let skewed = process_start().saturating_sub(RESTART_SKEW_SECS / 2);
        let ok = auth.sign(&id, 1, b"p", skewed);
        assert!(auth.validate(&id, 1, b"p", skewed, &ok).is_ok());
    }

    #[test]
    fn the_legacy_store_is_retired_without_touching_other_tables() {
        let dir = std::env::temp_dir().join(format!("nonce-retire-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        // A sidecar nonces.db is removed outright.
        let sidecar = dir.join("nonces.db");
        std::fs::write(&sidecar, b"not really sqlite").unwrap();
        let msg = NonceTracker::retire_legacy_store(sidecar.to_str().unwrap()).unwrap();
        assert!(msg.contains("removed legacy sidecar"), "{msg}");
        assert!(!sidecar.exists());
        assert!(NonceTracker::retire_legacy_store(sidecar.to_str().unwrap()).is_none());

        // In a hotel DB only mesh_nonces is dropped.
        let db = dir.join("context.db");
        let conn = Connection::open(&db).unwrap();
        conn.execute(
            "CREATE TABLE mesh_nonces (nonce TEXT PRIMARY KEY, seen_at INTEGER)",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO mesh_nonces VALUES ('a', 1)", [])
            .unwrap();
        conn.execute("CREATE TABLE keep_me (x INTEGER)", [])
            .unwrap();
        drop(conn);
        let msg = NonceTracker::retire_legacy_store(db.to_str().unwrap()).unwrap();
        assert!(msg.contains("dropped legacy mesh_nonces"), "{msg}");
        let conn = Connection::open(&db).unwrap();
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(tables, vec!["keep_me".to_string()]);
        assert!(NonceTracker::retire_legacy_store(db.to_str().unwrap()).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
