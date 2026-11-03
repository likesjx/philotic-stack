//! Experimental durable operation ledger and snapshot transaction. Not connected
//! to the hotel, root control service, or reader.
//!
//! This database is not yet a runtime publication mechanism. It does not
//! authenticate callers, coordinate writers outside this database, or prevent
//! a root/VM rollback.
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};
use std::{
    os::unix::fs::MetadataExt,
    path::{Component, Path},
};

#[derive(Debug, PartialEq, Eq)]
pub enum LedgerError {
    Denied,
    Invalid,
    Conflict,
    /// An earlier publication has not reached a recorded completed installation.
    PendingRecovery,
    Unavailable,
    /// A commit error may have occurred after the database committed. Reopen
    /// and reconcile this same operation ID before any subsequent mutation.
    /// A remaining SQLite sidecar blocks reopening and needs separately
    /// qualified recovery; never remove it merely to make this check pass.
    Uncertain,
}
type Result<T> = std::result::Result<T, LedgerError>;

pub struct Ledger {
    connection: Connection,
    uncertain: bool,
}

/// Read-only evidence, not authorization to clear a dirty hotel journal or
/// assume an interrupted installation did or did not reach its destination.
#[derive(Debug, PartialEq, Eq)]
pub struct RecoveryObservation {
    pub current_matches: bool,
    pub installation: InstallationState,
}
#[derive(Debug, PartialEq, Eq)]
pub enum InstallationState {
    NotStarted,
    Pending,
    Complete,
}
const CURRENT_SCHEMA: &str = "CREATE TABLE current(singleton INTEGER PRIMARY KEY CHECK(singleton=1),digest TEXT NOT NULL,snapshot BLOB NOT NULL)";
const RECEIPTS_SCHEMA: &str =
    "CREATE TABLE receipts(id TEXT PRIMARY KEY,expected TEXT NOT NULL,replacement TEXT NOT NULL)";
const CONSUMED_SCHEMA: &str = "CREATE TABLE consumed(id TEXT PRIMARY KEY,digest TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('pending','complete')))";

fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn operation_id(s: &str) -> bool {
    uuid::Uuid::parse_str(s).is_ok_and(|id| id.to_string() == s)
}

fn check_path(path: &Path, uid: u32) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(LedgerError::Invalid);
    }
    let parent = path.parent().ok_or(LedgerError::Invalid)?;
    let mut p = Some(parent);
    while let Some(dir) = p {
        let m = std::fs::symlink_metadata(dir).map_err(|_| LedgerError::Unavailable)?;
        if !m.is_dir() || ![0, uid].contains(&m.uid()) || m.mode() & 0o022 != 0 {
            return Err(LedgerError::Denied);
        }
        p = dir.parent();
    }
    let m = std::fs::symlink_metadata(path).map_err(|_| LedgerError::Unavailable)?;
    let dir = std::fs::symlink_metadata(parent).map_err(|_| LedgerError::Unavailable)?;
    if dir.uid() != uid
        || dir.mode() & 0o7777 != 0o700
        || !m.is_file()
        || m.uid() != uid
        || m.mode() & 0o7777 != 0o600
        || m.nlink() != 1
    {
        return Err(LedgerError::Denied);
    }
    Ok(())
}

impl Ledger {
    /// Opens only an already provisioned root-private database. Root is part
    /// of the trusted computing base; parent-path validation assumes no
    /// hostile root can swap paths between validation and SQLite open.
    pub fn open_root_private(path: &Path) -> Result<Self> {
        Self::open_with_owner(path, 0)
    }
    pub(crate) fn open_with_owner(path: &Path, uid: u32) -> Result<Self> {
        check_path(path, uid)?;
        // Do not silently recover or use unreviewed SQLite sidecars. Recovery
        // requires an explicit, separately qualified operator procedure.
        for suffix in ["-wal", "-shm", "-journal"] {
            let mut name = path.as_os_str().to_os_string();
            name.push(suffix);
            match std::fs::symlink_metadata(name) {
                Ok(_) => return Err(LedgerError::Denied),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(LedgerError::Unavailable),
            }
        }
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|_| LedgerError::Unavailable)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(2))
            .map_err(|_| LedgerError::Unavailable)?;
        connection
            .pragma_update(None, "journal_mode", "DELETE")
            .map_err(|_| LedgerError::Unavailable)?;
        connection
            .pragma_update(None, "synchronous", "FULL")
            .map_err(|_| LedgerError::Unavailable)?;
        let mode: String = connection
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .map_err(|_| LedgerError::Unavailable)?;
        let synchronous: i64 = connection
            .pragma_query_value(None, "synchronous", |r| r.get(0))
            .map_err(|_| LedgerError::Unavailable)?;
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(|_| LedgerError::Unavailable)?;
        if mode != "delete" || synchronous != 2 || version != 2 {
            return Err(LedgerError::Denied);
        }
        let mut stmt = connection
            .prepare("SELECT type,name,sql FROM sqlite_master WHERE type != 'index' ORDER BY name")
            .map_err(|_| LedgerError::Unavailable)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })
            .map_err(|_| LedgerError::Unavailable)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| LedgerError::Unavailable)?;
        if rows
            != vec![
                ("table".into(), "consumed".into(), CONSUMED_SCHEMA.into()),
                ("table".into(), "current".into(), CURRENT_SCHEMA.into()),
                ("table".into(), "receipts".into(), RECEIPTS_SCHEMA.into()),
            ]
        {
            return Err(LedgerError::Denied);
        }
        // Only automatically created primary-key indexes are permitted.
        let unexpected: i64 = connection.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND (name NOT IN ('sqlite_autoindex_receipts_1','sqlite_autoindex_consumed_1') OR sql IS NOT NULL)",[],|r|r.get(0)).map_err(|_|LedgerError::Unavailable)?;
        if unexpected != 0 {
            return Err(LedgerError::Denied);
        }
        drop(stmt);
        Ok(Self {
            connection,
            uncertain: false,
        })
    }

    /// Atomically records one digest transition and its receipt. The caller
    /// must separately hold the hotel fence and coordinate actual publication.
    #[cfg(test)]
    fn record(&mut self, id: &str, expected: &str, replacement: &str) -> Result<()> {
        if self.uncertain {
            return Err(LedgerError::Uncertain);
        }
        if !operation_id(id) || !digest(expected) || !digest(replacement) || expected == replacement
        {
            return Err(LedgerError::Invalid);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| LedgerError::Unavailable)?;
        let prior: Option<String> = tx
            .query_row("SELECT replacement FROM receipts WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|_| LedgerError::Unavailable)?;
        let current: String = tx
            .query_row("SELECT digest FROM current WHERE singleton=1", [], |r| {
                r.get(0)
            })
            .map_err(|_| LedgerError::Unavailable)?;
        if prior.is_some() || current != expected {
            return Err(LedgerError::Conflict);
        }
        tx.execute(
            "INSERT INTO receipts(id,expected,replacement) VALUES(?1,?2,?3)",
            params![id, expected, replacement],
        )
        .map_err(|_| LedgerError::Unavailable)?;
        if tx
            .execute(
                "UPDATE current SET digest=?1 WHERE singleton=1 AND digest=?2",
                params![replacement, expected],
            )
            .map_err(|_| LedgerError::Unavailable)?
            != 1
        {
            return Err(LedgerError::Conflict);
        }
        match tx.commit() {
            Ok(()) => Ok(()),
            Err(_) => {
                self.uncertain = true;
                Err(LedgerError::Uncertain)
            }
        }
    }

    /// Atomically changes the root-private candidate snapshot and receipt.
    /// Binding, time, owner authorization and hotel fence must come from a
    /// trusted authenticated coordinator; this function does not supply them.
    pub fn publish_snapshot(
        &mut self,
        id: &str,
        expected_digest: &str,
        bytes: &[u8],
        binding: &crate::grant_snapshot_candidate::ExpectedBinding<'_>,
        clock: &dyn Fn() -> u64,
        max_expiry: u64,
        deadline: u64,
    ) -> Result<String> {
        use sha2::{Digest, Sha256};
        if self.uncertain {
            return Err(LedgerError::Uncertain);
        }
        if !operation_id(id) || !digest(expected_digest) {
            return Err(LedgerError::Invalid);
        }
        let new = crate::grant_snapshot_candidate::validate_snapshot_bytes(bytes, binding)
            .map_err(|_| LedgerError::Denied)?;
        let replacement = format!("{:x}", Sha256::digest(bytes));
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| LedgerError::Unavailable)?;
        let prior: Option<String> = tx
            .query_row("SELECT replacement FROM receipts WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|_| LedgerError::Unavailable)?;
        let (old_digest, old_bytes): (String, Vec<u8>) = tx
            .query_row(
                "SELECT digest,snapshot FROM current WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| LedgerError::Unavailable)?;
        let unresolved:i64=tx.query_row("SELECT COUNT(*) FROM receipts r LEFT JOIN consumed c ON c.id=r.id WHERE c.id IS NULL OR c.state!='complete'",[],|r|r.get(0)).map_err(|_|LedgerError::Unavailable)?;
        if unresolved != 0 {
            return Err(LedgerError::PendingRecovery);
        }
        // Authorization is evaluated after acquiring the SQLite write lock.
        let now = clock();
        if now >= deadline || max_expiry <= now {
            return Err(LedgerError::Denied);
        }
        if prior.is_some() || old_digest != expected_digest {
            return Err(LedgerError::Conflict);
        }
        if format!("{:x}", Sha256::digest(&old_bytes)) != old_digest {
            return Err(LedgerError::Denied);
        }
        let old = crate::grant_snapshot_candidate::validate_snapshot_bytes(&old_bytes, binding)
            .map_err(|_| LedgerError::Denied)?;
        let before = old.snapshot();
        let after = new.snapshot();
        if before.grant.revoked
            || after.generation
                != before
                    .generation
                    .checked_add(1)
                    .ok_or(LedgerError::Invalid)?
        {
            return Err(LedgerError::Denied);
        }
        if after.grant.revoked {
            if after.grant.expires_at != before.grant.expires_at {
                return Err(LedgerError::Denied);
            }
        } else if after.grant.expires_at <= before.grant.expires_at
            || after.grant.expires_at <= now
            || after.grant.expires_at > max_expiry
        {
            return Err(LedgerError::Denied);
        }
        let mut normalized = after.clone();
        normalized.generation = before.generation;
        normalized.grant.revision = before.grant.revision;
        normalized.grant.expires_at = before.grant.expires_at;
        normalized.grant.revoked = before.grant.revoked;
        if serde_json::to_value(&normalized).map_err(|_| LedgerError::Invalid)?
            != serde_json::to_value(before).map_err(|_| LedgerError::Invalid)?
        {
            return Err(LedgerError::Denied);
        }
        tx.execute(
            "INSERT INTO receipts(id,expected,replacement) VALUES(?1,?2,?3)",
            params![id, expected_digest, replacement],
        )
        .map_err(|_| LedgerError::Unavailable)?;
        if tx
            .execute(
                "UPDATE current SET digest=?1,snapshot=?2 WHERE singleton=1 AND digest=?3",
                params![replacement, bytes, expected_digest],
            )
            .map_err(|_| LedgerError::Unavailable)?
            != 1
        {
            return Err(LedgerError::Conflict);
        }
        // This check is the proposed time linearization point. The commit can
        // finish later; an elapsed window after writes is conservative/dirty.
        let final_time = clock();
        if final_time < now
            || final_time >= deadline
            || max_expiry <= final_time
            || (!after.grant.revoked && after.grant.expires_at <= final_time)
        {
            drop(tx);
            self.uncertain = true;
            return Err(LedgerError::Uncertain);
        }
        match tx.commit() {
            Ok(()) => Ok(replacement),
            Err(_) => {
                self.uncertain = true;
                Err(LedgerError::Uncertain)
            }
        }
    }

    pub fn status(&self, id: &str) -> Result<Option<(String, String)>> {
        if self.uncertain {
            return Err(LedgerError::Uncertain);
        }
        if !operation_id(id) {
            return Err(LedgerError::Invalid);
        }
        self.connection
            .query_row(
                "SELECT expected,replacement FROM receipts WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|_| LedgerError::Unavailable)
    }

    /// Consume a published operation exactly once before an effect while the
    /// hotel fence remains held. A consumed operation is not proof that its
    /// subsequent effect occurred; interruption requires explicit recovery.
    pub fn consume_for_effect(&mut self, id: &str, expected_digest: &str) -> Result<()> {
        if self.uncertain {
            return Err(LedgerError::Uncertain);
        }
        if !operation_id(id) || !digest(expected_digest) {
            return Err(LedgerError::Invalid);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| LedgerError::Unavailable)?;
        let published: Option<String> = tx
            .query_row("SELECT replacement FROM receipts WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|_| LedgerError::Unavailable)?;
        let current: String = tx
            .query_row("SELECT digest FROM current WHERE singleton=1", [], |r| {
                r.get(0)
            })
            .map_err(|_| LedgerError::Unavailable)?;
        let used: Option<String> = tx
            .query_row("SELECT digest FROM consumed WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|_| LedgerError::Unavailable)?;
        if published.as_deref() != Some(expected_digest)
            || current != expected_digest
            || used.is_some()
        {
            return Err(LedgerError::Conflict);
        }
        tx.execute(
            "INSERT INTO consumed(id,digest,state) VALUES(?1,?2,'pending')",
            params![id, expected_digest],
        )
        .map_err(|_| LedgerError::Unavailable)?;
        match tx.commit() {
            Ok(()) => Ok(()),
            Err(_) => {
                self.uncertain = true;
                Err(LedgerError::Uncertain)
            }
        }
    }

    /// Mark only a known-completed installation. Unknown outcomes stay pending.
    pub fn complete_consumption(&mut self, id: &str, expected_digest: &str) -> Result<()> {
        if self.uncertain {
            return Err(LedgerError::Uncertain);
        }
        if !operation_id(id) || !digest(expected_digest) {
            return Err(LedgerError::Invalid);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| LedgerError::Unavailable)?;
        if tx.execute("UPDATE consumed SET state='complete' WHERE id=?1 AND digest=?2 AND state='pending'",params![id,expected_digest]).map_err(|_|LedgerError::Unavailable)? != 1 {return Err(LedgerError::Conflict);}
        match tx.commit() {
            Ok(()) => Ok(()),
            Err(_) => {
                self.uncertain = true;
                Err(LedgerError::Uncertain)
            }
        }
    }

    pub fn consumption_status(&self, id: &str) -> Result<Option<(String, String)>> {
        if self.uncertain {
            return Err(LedgerError::Uncertain);
        }
        if !operation_id(id) {
            return Err(LedgerError::Invalid);
        }
        self.connection
            .query_row("SELECT digest,state FROM consumed WHERE id=?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .map_err(|_| LedgerError::Unavailable)
    }

    /// Classify durable local records in one read transaction. A pending result
    /// never triggers retry, rollback, receipt edits, or journal cleanup.
    pub fn inspect_recovery(
        &mut self,
        id: &str,
        expected_digest: &str,
    ) -> Result<Option<RecoveryObservation>> {
        use sha2::{Digest, Sha256};
        if self.uncertain {
            return Err(LedgerError::Uncertain);
        }
        if !operation_id(id) || !digest(expected_digest) {
            return Err(LedgerError::Invalid);
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(|_| LedgerError::Unavailable)?;
        let receipt: Option<String> = tx
            .query_row("SELECT replacement FROM receipts WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|_| LedgerError::Unavailable)?;
        let consumed: Option<(String, String)> = tx
            .query_row("SELECT digest,state FROM consumed WHERE id=?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .map_err(|_| LedgerError::Unavailable)?;
        let Some(receipt) = receipt else {
            return if consumed.is_none() {
                Ok(None)
            } else {
                Err(LedgerError::Denied)
            };
        };
        if receipt != expected_digest {
            return Err(LedgerError::Denied);
        }
        let (current, bytes): (String, Vec<u8>) = tx
            .query_row(
                "SELECT digest,snapshot FROM current WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| LedgerError::Unavailable)?;
        if format!("{:x}", Sha256::digest(bytes)) != current {
            return Err(LedgerError::Denied);
        }
        let installation = match consumed {
            None => InstallationState::NotStarted,
            Some((ref d, ref state)) if d == expected_digest && state == "pending" => {
                InstallationState::Pending
            }
            Some((ref d, ref state)) if d == expected_digest && state == "complete" => {
                InstallationState::Complete
            }
            _ => return Err(LedgerError::Denied),
        };
        Ok(Some(RecoveryObservation {
            current_matches: current == expected_digest,
            installation,
        }))
    }

    /// Reads a single consistent committed snapshot. The caller must bind the
    /// result to its hotel fence and independently trusted binding; rollback
    /// protection and service-to-root transport are not supplied here.
    pub fn current_snapshot(
        &self,
        binding: &crate::grant_snapshot_candidate::ExpectedBinding<'_>,
    ) -> Result<crate::grant_snapshot_candidate::LoadedSnapshot> {
        use sha2::{Digest, Sha256};
        if self.uncertain {
            return Err(LedgerError::Uncertain);
        }
        let (digest, bytes): (String, Vec<u8>) = self
            .connection
            .query_row(
                "SELECT digest,snapshot FROM current WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|_| LedgerError::Unavailable)?;
        if format!("{:x}", Sha256::digest(&bytes)) != digest {
            return Err(LedgerError::Denied);
        }
        crate::grant_snapshot_candidate::validate_snapshot_bytes(&bytes, binding)
            .map_err(|_| LedgerError::Denied)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    pub(crate) fn fixture() -> (tempfile::TempDir, std::path::PathBuf, u32) {
        let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join("ledger.db");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .unwrap();
        let uid = file.metadata().unwrap().uid();
        drop(file);
        let db = Connection::open(&path).unwrap();
        db.execute_batch(&format!(
            "{CURRENT_SCHEMA}; {RECEIPTS_SCHEMA}; {CONSUMED_SCHEMA}; PRAGMA user_version=2;"
        ))
        .unwrap();
        db.execute(
            "INSERT INTO current VALUES(1,?1,?2)",
            params!["a".repeat(64), b"test".as_slice()],
        )
        .unwrap();
        drop(db);
        (dir, path, uid)
    }
    #[test]
    fn atomic_receipt_survives_reopen_and_rejects_replay_and_stale() {
        let (_dir, path, uid) = fixture();
        let id = uuid::Uuid::new_v4().to_string();
        let mut db = Ledger::open_with_owner(&path, uid).unwrap();
        db.record(&id, &"a".repeat(64), &"b".repeat(64)).unwrap();
        drop(db);
        let mut db = Ledger::open_with_owner(&path, uid).unwrap();
        assert_eq!(
            db.status(&id).unwrap(),
            Some(("a".repeat(64), "b".repeat(64)))
        );
        assert_eq!(
            db.record(&id, &"b".repeat(64), &"c".repeat(64)),
            Err(LedgerError::Conflict)
        );
        assert_eq!(
            db.record(
                &uuid::Uuid::new_v4().to_string(),
                &"a".repeat(64),
                &"c".repeat(64)
            ),
            Err(LedgerError::Conflict)
        );
        assert_eq!(db.status(&uuid::Uuid::new_v4().to_string()).unwrap(), None);
    }
    #[test]
    fn rejects_unsafe_custody_and_invalid_requests() {
        let (dir, path, uid) = fixture();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            Ledger::open_with_owner(&path, uid),
            Err(LedgerError::Denied)
        ));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&path, dir.path().join("link")).unwrap();
        assert!(matches!(
            Ledger::open_with_owner(&dir.path().join("link"), uid),
            Err(LedgerError::Denied)
        ));
        let mut db = Ledger::open_with_owner(&path, uid).unwrap();
        assert_eq!(
            db.record("invalid", &"a".repeat(64), &"b".repeat(64)),
            Err(LedgerError::Invalid)
        );
        assert_eq!(
            db.record(
                &uuid::Uuid::new_v4().to_string(),
                &"a".repeat(64),
                &"a".repeat(64)
            ),
            Err(LedgerError::Invalid)
        );
    }

    #[test]
    fn rejects_unexpected_schema_and_unreviewed_sidecar() {
        let (dir, path, uid) = fixture();
        let sidecar = dir.path().join("ledger.db-journal");
        std::fs::write(&sidecar, b"unreviewed").unwrap();
        assert!(matches!(
            Ledger::open_with_owner(&path, uid),
            Err(LedgerError::Denied)
        ));
        std::fs::remove_file(sidecar).unwrap();
        let db = Connection::open(&path).unwrap();
        db.execute_batch("CREATE TRIGGER unexpected AFTER UPDATE ON current BEGIN SELECT 1; END;")
            .unwrap();
        drop(db);
        assert!(matches!(
            Ledger::open_with_owner(&path, uid),
            Err(LedgerError::Denied)
        ));
    }

    #[test]
    fn concurrent_writers_cannot_both_advance_same_digest() {
        let (_dir, path, uid) = fixture();
        let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = ["b", "c"]
            .into_iter()
            .map(|next| {
                let path = path.clone();
                let gate = gate.clone();
                std::thread::spawn(move || {
                    let mut ledger = Ledger::open_with_owner(&path, uid).unwrap();
                    gate.wait();
                    ledger.record(
                        &uuid::Uuid::new_v4().to_string(),
                        &"a".repeat(64),
                        &next.repeat(64),
                    )
                })
            })
            .collect();
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|r| **r == Err(LedgerError::Conflict))
                .count(),
            1
        );
        let ledger = Ledger::open_with_owner(&path, uid).unwrap();
        assert_eq!(
            ledger
                .connection
                .query_row("SELECT COUNT(*) FROM receipts", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn competing_consumers_cannot_both_reserve_the_operation() {
        let (_dir, path, uid) = fixture();
        let id = uuid::Uuid::new_v4().to_string();
        let db = Connection::open(&path).unwrap();
        db.execute(
            "INSERT INTO receipts(id,expected,replacement) VALUES(?1,?2,?3)",
            params![id, "b".repeat(64), "a".repeat(64)],
        )
        .unwrap();
        drop(db);
        let gate = std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = (0..2)
            .map(|_| {
                let path = path.clone();
                let gate = gate.clone();
                let id = id.clone();
                std::thread::spawn(move || {
                    let mut ledger = Ledger::open_with_owner(&path, uid).unwrap();
                    gate.wait();
                    ledger.consume_for_effect(&id, &"a".repeat(64))
                })
            })
            .collect();
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|r| **r == Err(LedgerError::Conflict))
                .count(),
            1
        );
        let ledger = Ledger::open_with_owner(&path, uid).unwrap();
        assert_eq!(
            ledger.consumption_status(&id).unwrap(),
            Some(("a".repeat(64), "pending".into()))
        );
    }

    #[test]
    fn uncommitted_transaction_has_no_receipt_or_transition_after_reopen() {
        let (_dir, path, uid) = fixture();
        let id = uuid::Uuid::new_v4().to_string();
        let mut ledger = Ledger::open_with_owner(&path, uid).unwrap();
        {
            let tx = ledger
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .unwrap();
            tx.execute(
                "INSERT INTO receipts(id,expected,replacement) VALUES(?1,?2,?3)",
                params![id, "a".repeat(64), "b".repeat(64)],
            )
            .unwrap();
            tx.execute(
                "UPDATE current SET digest=?1 WHERE singleton=1",
                ["b".repeat(64)],
            )
            .unwrap();
            // Drop models ordinary transaction abort, not power-loss testing.
        }
        drop(ledger);
        let mut ledger = Ledger::open_with_owner(&path, uid).unwrap();
        assert_eq!(ledger.status(&id).unwrap(), None);
        ledger
            .record(&id, &"a".repeat(64), &"b".repeat(64))
            .unwrap();
    }
    #[test]
    fn uncertain_handle_refuses_further_mutation() {
        let (_dir, path, uid) = fixture();
        let mut ledger = Ledger::open_with_owner(&path, uid).unwrap();
        // Simulates a commit error, not an actual I/O failure.
        ledger.uncertain = true;
        assert_eq!(
            ledger.record(
                &uuid::Uuid::new_v4().to_string(),
                &"a".repeat(64),
                &"b".repeat(64)
            ),
            Err(LedgerError::Uncertain)
        );
    }

    #[test]
    fn snapshot_and_receipt_commit_together_with_scope_and_replay_guards() {
        use sha2::{Digest, Sha256};
        let (_dir, path, uid) = fixture();
        let original = crate::grant_snapshot_candidate::tests::sample();
        let original_bytes = serde_json::to_vec(&original).unwrap();
        let old = format!("{:x}", Sha256::digest(&original_bytes));
        let db = Connection::open(&path).unwrap();
        db.execute(
            "UPDATE current SET digest=?1,snapshot=?2 WHERE singleton=1",
            params![old, original_bytes],
        )
        .unwrap();
        drop(db);
        let mut ledger = Ledger::open_with_owner(&path, uid).unwrap();
        let binding = crate::grant_snapshot_candidate::tests::expected();
        let mut next = original.clone();
        next.generation += 1;
        next.grant.revision += 1;
        next.grant.expires_at = 1100;
        let mut widened = next.clone();
        widened.grant.operations.push("delete".into());
        let id = uuid::Uuid::new_v4().to_string();
        let mut invalid_binding = crate::grant_snapshot_candidate::tests::expected();
        invalid_binding.owner = "bad owner with spaces";
        assert_eq!(
            ledger.publish_snapshot(
                &id,
                &old,
                &serde_json::to_vec(&next).unwrap(),
                &invalid_binding,
                &|| 101,
                1200,
                1000
            ),
            Err(LedgerError::Denied)
        );
        for (final_time, deadline) in [(1000, 1000), (1100, 2000), (1200, 2000), (100, 2000)] {
            let ticks = std::cell::Cell::new(0);
            let clock = || {
                let n = ticks.get();
                ticks.set(n + 1);
                if n == 0 { 101 } else { final_time }
            };
            assert_eq!(
                ledger.publish_snapshot(
                    &id,
                    &old,
                    &serde_json::to_vec(&next).unwrap(),
                    &binding,
                    &clock,
                    1200,
                    deadline
                ),
                Err(LedgerError::Uncertain)
            );
            drop(ledger);
            ledger = Ledger::open_with_owner(&path, uid).unwrap();
            assert_eq!(ledger.status(&id).unwrap(), None);
        }
        assert_eq!(
            ledger.publish_snapshot(
                &id,
                &old,
                &serde_json::to_vec(&widened).unwrap(),
                &binding,
                &|| 101,
                1200,
                1000
            ),
            Err(LedgerError::Denied)
        );
        let bytes = serde_json::to_vec(&next).unwrap();
        let new = ledger
            .publish_snapshot(&id, &old, &bytes, &binding, &|| 101, 1200, 1000)
            .unwrap();
        assert_eq!(
            ledger.status(&id).unwrap(),
            Some((old.clone(), new.clone()))
        );
        let stored: (String, Vec<u8>) = ledger
            .connection
            .query_row(
                "SELECT digest,snapshot FROM current WHERE singleton=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored, (new.clone(), bytes));
        assert_eq!(
            ledger.inspect_recovery(&id, &new).unwrap(),
            Some(RecoveryObservation {
                current_matches: true,
                installation: InstallationState::NotStarted
            })
        );
        assert_eq!(
            ledger.consume_for_effect(&uuid::Uuid::new_v4().to_string(), &new),
            Err(LedgerError::Conflict)
        );
        assert_eq!(
            ledger.consume_for_effect(&id, &old),
            Err(LedgerError::Conflict)
        );
        ledger.consume_for_effect(&id, &new).unwrap();
        assert_eq!(
            ledger.inspect_recovery(&id, &new).unwrap(),
            Some(RecoveryObservation {
                current_matches: true,
                installation: InstallationState::Pending
            })
        );
        assert_eq!(
            ledger.consume_for_effect(&id, &new),
            Err(LedgerError::Conflict)
        );
        drop(ledger);
        let mut ledger = Ledger::open_with_owner(&path, uid).unwrap();
        assert_eq!(
            ledger.consumption_status(&id).unwrap(),
            Some((new.clone(), "pending".into()))
        );
        ledger.complete_consumption(&id, &new).unwrap();
        assert_eq!(
            ledger.inspect_recovery(&id, &new).unwrap(),
            Some(RecoveryObservation {
                current_matches: true,
                installation: InstallationState::Complete
            })
        );
        assert_eq!(
            ledger.consumption_status(&id).unwrap(),
            Some((new.clone(), "complete".into()))
        );
        assert_eq!(
            ledger.complete_consumption(&id, &new),
            Err(LedgerError::Conflict)
        );
        assert_eq!(
            ledger.consume_for_effect(&id, &new),
            Err(LedgerError::Conflict)
        );
        assert_eq!(
            ledger
                .current_snapshot(&binding)
                .unwrap()
                .snapshot()
                .generation,
            next.generation
        );
        assert_eq!(
            ledger.publish_snapshot(
                &id,
                &new,
                &serde_json::to_vec(&next).unwrap(),
                &binding,
                &|| 101,
                1200,
                1000
            ),
            Err(LedgerError::Conflict)
        );
        let mut revoked = next.clone();
        revoked.generation += 1;
        revoked.grant.revision += 1;
        revoked.grant.revoked = true;
        let revoke_id = uuid::Uuid::new_v4().to_string();
        let revoked_hash = ledger
            .publish_snapshot(
                &revoke_id,
                &new,
                &serde_json::to_vec(&revoked).unwrap(),
                &binding,
                &|| 101,
                1200,
                1000,
            )
            .unwrap();
        ledger
            .consume_for_effect(&revoke_id, &revoked_hash)
            .unwrap();
        ledger
            .complete_consumption(&revoke_id, &revoked_hash)
            .unwrap();
        assert_eq!(
            ledger.inspect_recovery(&id, &new).unwrap(),
            Some(RecoveryObservation {
                current_matches: false,
                installation: InstallationState::Complete
            })
        );
        revoked.generation += 1;
        revoked.grant.revision += 1;
        revoked.grant.revoked = false;
        revoked.grant.expires_at = 1150;
        assert_eq!(
            ledger.publish_snapshot(
                &uuid::Uuid::new_v4().to_string(),
                &revoked_hash,
                &serde_json::to_vec(&revoked).unwrap(),
                &binding,
                &|| 101,
                1200,
                1000
            ),
            Err(LedgerError::Denied)
        );
    }
}
