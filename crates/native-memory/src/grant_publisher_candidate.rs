//! Unwired protected snapshot publication candidate. Not production qualified.
//!
//! The caller MUST authenticate the root publisher, retain the real hotel fence
//! across this whole call, and ensure every competing writer uses that fence.
//! Root and the trusted binding remain part of the TCB. This module does not
//! establish mutual process identity or defend against root/VM rollback.
use crate::{
    grant_ledger_candidate::{Ledger, LedgerError},
    grant_snapshot_candidate::{ExpectedBinding, load_with_owner},
    protected_transfer_candidate::MAX_TRANSFER_SNAPSHOT,
};
use std::{
    ffi::CString,
    io::Write,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
};

#[derive(Debug, PartialEq, Eq)]
pub enum PublishError {
    Rejected,
    RecoveryRequired,
}
type Result<T> = std::result::Result<T, PublishError>;
fn hex(bytes: [u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicReceipt {
    version: u8,
    operation_id: String,
    digest: String,
}

/// Read-only receipt check for the hotel. It proves only that protected bytes
/// assert completion; it does not by itself prove process identity, lifecycle
/// fencing, anti-rollback, or that a guest has loaded the snapshot.
pub fn verify_root_receipt(
    directory: &Path,
    group: u32,
    id: uuid::Uuid,
    digest: [u8; 32],
) -> std::result::Result<bool, crate::Error> {
    verify_receipt_with_owner(directory, 0, group, id, digest)
}
fn verify_receipt_with_owner(
    directory: &Path,
    uid: u32,
    gid: u32,
    id: uuid::Uuid,
    digest: [u8; 32],
) -> std::result::Result<bool, crate::Error> {
    let path = directory.join(format!("receipt-{id}.json"));
    let bytes = crate::grant_snapshot_candidate::read_with_owner(&path, uid, gid)?;
    let receipt: PublicReceipt =
        serde_json::from_slice(&bytes).map_err(|_| crate::Error::Invalid)?;
    Ok(receipt.version == 1
        && receipt.operation_id == id.to_string()
        && receipt.digest == hex(digest))
}

/// The trusted caller must provide the exact operation, prior digest, binding,
/// approved deadline and maximum expiry; no argument is authorization itself.
pub fn publish_root_owned(
    ledger: &mut Ledger,
    path: &Path,
    group: u32,
    binding: &ExpectedBinding<'_>,
    operation: (&str, &str, &[u8]),
    clock: &dyn Fn() -> u64,
    window: (u64, u64),
) -> Result<String> {
    let (id, expected, bytes) = operation;
    let (max_expiry, deadline) = window;
    publish_for_owner(
        ledger,
        path,
        0,
        group,
        binding,
        id,
        expected,
        bytes,
        clock,
        max_expiry,
        deadline,
        |_| Ok(()),
    )
}

/// Unwired Linux composition: use the hotel-fenced, per-fragment-authenticated
/// stream session to perform the protected root publication. The caller must
/// obtain manager identity and all operation parameters from protected config.
/// Any error requires reconciliation and must never trigger an automatic retry.
#[cfg(all(target_os = "linux", test))]
#[allow(clippy::too_many_arguments)]
pub fn publish_through_hotel(
    stream: std::os::unix::net::UnixStream,
    hotel: crate::protected_peer_candidate::ManagerPinnedProcess,
    ledger: &mut Ledger,
    path: &Path,
    group: u32,
    binding: &ExpectedBinding<'_>,
    guest: &str,
    id: uuid::Uuid,
    expected_old: &str,
    bytes: &[u8],
    clock: &dyn Fn() -> u64,
    max_expiry: u64,
    deadline: u64,
) -> crate::Result<()> {
    use sha2::{Digest, Sha256};
    let expected: [u8; 32] = Sha256::digest(bytes).into();
    let mut client = crate::protected_stream_candidate::PublisherClient::new(stream, hotel)?;
    client.publish(guest, id, expected, || {
        let actual = publish_root_owned(
            ledger,
            path,
            group,
            binding,
            (&id.to_string(), expected_old, bytes),
            clock,
            (max_expiry, deadline),
        )
        .map_err(|_| crate::Error::Uncertain)?;
        if actual != hex(expected) {
            return Err(crate::Error::Uncertain);
        }
        Ok(expected)
    })
}

#[allow(clippy::too_many_arguments)]
fn publish_for_owner(
    ledger: &mut Ledger,
    path: &Path,
    uid: u32,
    gid: u32,
    binding: &ExpectedBinding<'_>,
    id: &str,
    expected: &str,
    bytes: &[u8],
    clock: &dyn Fn() -> u64,
    max_expiry: u64,
    deadline: u64,
    hook: impl Fn(&str) -> Result<()>,
) -> Result<String> {
    if bytes.len() > MAX_TRANSFER_SNAPSHOT {
        return Err(PublishError::Rejected);
    }
    let initial = clock();
    if initial >= deadline || initial >= max_expiry {
        return Err(PublishError::Rejected);
    }
    let last = std::cell::Cell::new(initial);
    let backwards = std::cell::Cell::new(false);
    let observed_clock = || {
        let value = clock();
        if value < last.get() {
            backwards.set(true);
        }
        last.set(value);
        value
    };
    let candidate = crate::grant_snapshot_candidate::validate_snapshot_bytes(bytes, binding)
        .map_err(|_| PublishError::Rejected)?;
    let installed =
        load_with_owner(path, uid, gid, binding).map_err(|_| PublishError::RecoveryRequired)?;
    let current = ledger
        .current_snapshot(binding)
        .map_err(|_| PublishError::RecoveryRequired)?;
    if current.digest() != installed.digest() {
        return Err(PublishError::RecoveryRequired);
    }
    if hex(installed.digest()) != expected {
        return Err(PublishError::Rejected);
    }
    let parent = path.parent().ok_or(PublishError::Rejected)?;
    let name = match path.file_name() {
        Some(s) if matches!(Path::new(s).components().next(), Some(Component::Normal(_))) => {
            CString::new(s.as_bytes()).map_err(|_| PublishError::Rejected)?
        }
        _ => return Err(PublishError::Rejected),
    };
    let parent_c =
        CString::new(parent.as_os_str().as_bytes()).map_err(|_| PublishError::Rejected)?;
    let raw = unsafe {
        libc::open(
            parent_c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(PublishError::RecoveryRequired);
    }
    let dir = unsafe { std::fs::File::from_raw_fd(raw) };
    let meta = dir.metadata().map_err(|_| PublishError::RecoveryRequired)?;
    if !meta.is_dir() || meta.uid() != uid || meta.gid() != gid || meta.mode() & 0o7777 != 0o750 {
        return Err(PublishError::RecoveryRequired);
    }
    let replacement = match ledger.publish_snapshot(
        id,
        expected,
        bytes,
        binding,
        &observed_clock,
        (max_expiry, deadline),
    ) {
        Ok(value) => value,
        Err(LedgerError::Invalid | LedgerError::Denied | LedgerError::Conflict) => {
            return Err(PublishError::Rejected);
        }
        Err(_) => return Err(PublishError::RecoveryRequired),
    };
    // Once the durable ledger has advanced, all failures require explicit
    // reconciliation. Never retry or clear a hotel journal automatically.
    hook("ledger").map_err(|_| PublishError::RecoveryRequired)?;
    ledger
        .consume_for_effect(id, &replacement)
        .map_err(|_| PublishError::RecoveryRequired)?;
    hook("pending").map_err(|_| PublishError::RecoveryRequired)?;
    // The validated operation ID deterministically identifies any retained
    // temporary file for later inspection; it is never reused automatically.
    let temp =
        CString::new(format!(".snapshot-{id}.tmp")).map_err(|_| PublishError::RecoveryRequired)?;
    let raw = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            temp.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if raw < 0 {
        return Err(PublishError::RecoveryRequired);
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(raw) };
    let m = file
        .metadata()
        .map_err(|_| PublishError::RecoveryRequired)?;
    if (m.uid() != uid || m.gid() != gid)
        && unsafe { libc::fchown(file.as_raw_fd(), uid, gid) } != 0
    {
        return Err(PublishError::RecoveryRequired);
    }
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o640) } != 0 {
        return Err(PublishError::RecoveryRequired);
    }
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| PublishError::RecoveryRequired)?;
    let m = file
        .metadata()
        .map_err(|_| PublishError::RecoveryRequired)?;
    if !m.is_file()
        || m.uid() != uid
        || m.gid() != gid
        || m.nlink() != 1
        || m.mode() & 0o7777 != 0o640
    {
        return Err(PublishError::RecoveryRequired);
    }
    if hex(load_with_owner(path, uid, gid, binding)
        .map_err(|_| PublishError::RecoveryRequired)?
        .digest())
        != expected
    {
        return Err(PublishError::RecoveryRequired);
    }
    let final_time = observed_clock();
    if backwards.get()
        || final_time >= deadline
        || final_time >= max_expiry
        || (!candidate.snapshot().grant.revoked
            && candidate.snapshot().grant.expires_at <= final_time)
    {
        return Err(PublishError::RecoveryRequired);
    }
    if unsafe {
        libc::renameat(
            dir.as_raw_fd(),
            temp.as_ptr(),
            dir.as_raw_fd(),
            name.as_ptr(),
        )
    } != 0
    {
        return Err(PublishError::RecoveryRequired);
    }
    dir.sync_all().map_err(|_| PublishError::RecoveryRequired)?;
    hook("renamed").map_err(|_| PublishError::RecoveryRequired)?;
    if hex(load_with_owner(path, uid, gid, binding)
        .map_err(|_| PublishError::RecoveryRequired)?
        .digest())
        != replacement
    {
        return Err(PublishError::RecoveryRequired);
    }
    let receipt_name =
        CString::new(format!("receipt-{id}.json")).map_err(|_| PublishError::RecoveryRequired)?;
    let raw = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            receipt_name.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            0o600,
        )
    };
    if raw < 0 {
        return Err(PublishError::RecoveryRequired);
    }
    let mut receipt = unsafe { std::fs::File::from_raw_fd(raw) };
    hook("receipt_created").map_err(|_| PublishError::RecoveryRequired)?;
    let metadata = receipt
        .metadata()
        .map_err(|_| PublishError::RecoveryRequired)?;
    if (metadata.uid() != uid || metadata.gid() != gid)
        && unsafe { libc::fchown(receipt.as_raw_fd(), uid, gid) } != 0
    {
        return Err(PublishError::RecoveryRequired);
    }
    if unsafe { libc::fchmod(receipt.as_raw_fd(), 0o640) } != 0 {
        return Err(PublishError::RecoveryRequired);
    }
    let contents = serde_json::to_vec(&PublicReceipt {
        version: 1,
        operation_id: id.into(),
        digest: replacement.clone(),
    })
    .map_err(|_| PublishError::RecoveryRequired)?;
    receipt
        .write_all(&contents)
        .map_err(|_| PublishError::RecoveryRequired)?;
    hook("receipt_written").map_err(|_| PublishError::RecoveryRequired)?;
    receipt
        .sync_all()
        .map_err(|_| PublishError::RecoveryRequired)?;
    hook("receipt_file_synced").map_err(|_| PublishError::RecoveryRequired)?;
    dir.sync_all().map_err(|_| PublishError::RecoveryRequired)?;
    let operation = uuid::Uuid::parse_str(id).map_err(|_| PublishError::RecoveryRequired)?;
    let expected_digest = crate::grant_snapshot_candidate::load_with_owner(path, uid, gid, binding)
        .map_err(|_| PublishError::RecoveryRequired)?
        .digest();
    if !verify_receipt_with_owner(parent, uid, gid, operation, expected_digest)
        .map_err(|_| PublishError::RecoveryRequired)?
    {
        return Err(PublishError::RecoveryRequired);
    }
    hook("receipt_durable").map_err(|_| PublishError::RecoveryRequired)?;
    // The receipt is visible before ledger completion, but the hotel must not
    // accept it without a Finish from the pinned publisher while holding its
    // fence. A failure here leaves the ledger pending and requires recovery.
    ledger
        .complete_consumption(id, &replacement)
        .map_err(|_| PublishError::RecoveryRequired)?;
    hook("complete").map_err(|_| PublishError::RecoveryRequired)?;
    Ok(replacement)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grant_ledger_candidate::InstallationState;
    use sha2::{Digest, Sha256};
    fn fixture() -> (
        tempfile::TempDir,
        tempfile::TempDir,
        std::path::PathBuf,
        Ledger,
        u32,
        u32,
        String,
        Vec<u8>,
    ) {
        let (file_dir, path, uid, gid) = crate::grant_snapshot_candidate::tests::fixture();
        let (db_dir, db_path, db_uid) = crate::grant_ledger_candidate::tests::fixture();
        assert_eq!(uid, db_uid);
        let old = std::fs::read(&path).unwrap();
        let digest = format!("{:x}", Sha256::digest(&old));
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute(
            "UPDATE current SET digest=?1,snapshot=?2 WHERE singleton=1",
            rusqlite::params![digest, old],
        )
        .unwrap();
        drop(db);
        let ledger = Ledger::open_with_owner(&db_path, uid).unwrap();
        let mut next = crate::grant_snapshot_candidate::tests::sample();
        next.generation += 1;
        next.grant.revision += 1;
        next.grant.expires_at = 1100;
        (
            file_dir,
            db_dir,
            path,
            ledger,
            uid,
            gid,
            digest,
            serde_json::to_vec(&next).unwrap(),
        )
    }
    #[test]
    fn candidate_atomically_installs_synthetic_snapshot_and_records_completion() {
        let (_files, _db, path, mut ledger, uid, gid, old, next) = fixture();
        let binding = crate::grant_snapshot_candidate::tests::expected();
        let id = uuid::Uuid::new_v4().to_string();
        let digest = publish_for_owner(
            &mut ledger,
            &path,
            uid,
            gid,
            &binding,
            &id,
            &old,
            &next,
            &|| 101,
            1200,
            500,
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(digest, format!("{:x}", Sha256::digest(&next)));
        assert_eq!(std::fs::read(&path).unwrap(), next);
        assert_eq!(
            ledger.consumption_status(&id).unwrap(),
            Some((digest.clone(), "complete".into()))
        );
        assert_eq!(
            ledger.current_snapshot(&binding).unwrap().digest(),
            load_with_owner(&path, uid, gid, &binding).unwrap().digest()
        );
        let operation = uuid::Uuid::parse_str(&id).unwrap();
        let expected = load_with_owner(&path, uid, gid, &binding).unwrap().digest();
        assert!(
            verify_receipt_with_owner(path.parent().unwrap(), uid, gid, operation, expected)
                .unwrap()
        );
        assert!(
            !verify_receipt_with_owner(path.parent().unwrap(), uid, gid, operation, [0; 32])
                .unwrap()
        );
        let receipt = path.parent().unwrap().join(format!("receipt-{id}.json"));
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(receipt, std::fs::Permissions::from_mode(0o660)).unwrap();
        assert!(
            verify_receipt_with_owner(path.parent().unwrap(), uid, gid, operation, expected)
                .is_err()
        );
    }
    #[test]
    fn interruptions_preserve_recovery_evidence_and_never_claim_automatic_rollback() {
        for phase in [
            "ledger",
            "pending",
            "renamed",
            "receipt_created",
            "receipt_written",
            "receipt_file_synced",
            "receipt_durable",
            "complete",
        ] {
            let (_files, db_dir, path, mut ledger, uid, gid, old, next) = fixture();
            let binding = crate::grant_snapshot_candidate::tests::expected();
            let id = uuid::Uuid::new_v4().to_string();
            let expected_new = format!("{:x}", Sha256::digest(&next));
            let result = publish_for_owner(
                &mut ledger,
                &path,
                uid,
                gid,
                &binding,
                &id,
                &old,
                &next,
                &|| 101,
                1200,
                500,
                |at| {
                    if at == phase {
                        Err(PublishError::Rejected)
                    } else {
                        Ok(())
                    }
                },
            );
            assert_eq!(result, Err(PublishError::RecoveryRequired));
            drop(ledger);
            let mut ledger =
                Ledger::open_with_owner(&db_dir.path().join("ledger.db"), uid).unwrap();
            let observed = ledger
                .inspect_recovery(&id, &expected_new)
                .unwrap()
                .unwrap();
            assert_eq!(
                observed.installation,
                match phase {
                    "ledger" => InstallationState::NotStarted,
                    "pending"
                    | "renamed"
                    | "receipt_created"
                    | "receipt_written"
                    | "receipt_file_synced"
                    | "receipt_durable" => InstallationState::Pending,
                    _ => InstallationState::Complete,
                }
            );
            let installed = hex(load_with_owner(&path, uid, gid, &binding).unwrap().digest());
            assert_eq!(
                installed,
                if phase != "ledger" && phase != "pending" {
                    expected_new.clone()
                } else {
                    old.clone()
                }
            );
            assert_eq!(
                path.parent()
                    .unwrap()
                    .join(format!("receipt-{id}.json"))
                    .exists(),
                phase.starts_with("receipt_") || phase == "complete"
            );
            assert_eq!(
                publish_for_owner(
                    &mut ledger,
                    &path,
                    uid,
                    gid,
                    &binding,
                    &id,
                    &old,
                    &next,
                    &|| 101,
                    1200,
                    500,
                    |_| Ok(())
                ),
                Err(if phase == "ledger" || phase == "pending" {
                    PublishError::RecoveryRequired
                } else {
                    PublishError::Rejected
                })
            );
            if phase != "complete" {
                let mut later = crate::grant_snapshot_candidate::tests::sample();
                later.generation += 2;
                later.grant.revision += 2;
                later.grant.expires_at = 1150;
                assert_eq!(
                    publish_for_owner(
                        &mut ledger,
                        &path,
                        uid,
                        gid,
                        &binding,
                        &uuid::Uuid::new_v4().to_string(),
                        &expected_new,
                        &serde_json::to_vec(&later).unwrap(),
                        &|| 101,
                        1200,
                        500,
                        |_| Ok(())
                    ),
                    Err(PublishError::RecoveryRequired)
                );
            }
        }
    }
    #[test]
    fn elapsed_or_backwards_time_before_rename_requires_recovery() {
        for (final_time, deadline) in [(500, 500), (1100, 2000), (1200, 2000), (100, 2000)] {
            let (_files, _db, path, mut ledger, uid, gid, old, next) = fixture();
            let binding = crate::grant_snapshot_candidate::tests::expected();
            let id = uuid::Uuid::new_v4().to_string();
            let ticks = std::cell::Cell::new(0);
            let clock = || {
                let n = ticks.get();
                ticks.set(n + 1);
                if n < 3 { 101 } else { final_time }
            };
            assert_eq!(
                publish_for_owner(
                    &mut ledger,
                    &path,
                    uid,
                    gid,
                    &binding,
                    &id,
                    &old,
                    &next,
                    &clock,
                    1200,
                    deadline,
                    |_| Ok(())
                ),
                Err(PublishError::RecoveryRequired)
            );
            assert_eq!(
                hex(load_with_owner(&path, uid, gid, &binding).unwrap().digest()),
                old
            );
            assert_eq!(
                ledger.consumption_status(&id).unwrap().unwrap().1,
                "pending"
            );
            assert!(
                path.parent()
                    .unwrap()
                    .join(format!(".snapshot-{id}.tmp"))
                    .exists()
            );
        }
    }
    #[test]
    fn preexisting_receipt_symlink_is_not_followed_or_overwritten() {
        use std::os::unix::fs::symlink;
        let (_files, _db, path, mut ledger, uid, gid, old, next) = fixture();
        let binding = crate::grant_snapshot_candidate::tests::expected();
        let id = uuid::Uuid::new_v4().to_string();
        let victim = path.parent().unwrap().join("victim");
        std::fs::write(&victim, b"unchanged").unwrap();
        let receipt = path.parent().unwrap().join(format!("receipt-{id}.json"));
        symlink(&victim, &receipt).unwrap();
        assert_eq!(
            publish_for_owner(
                &mut ledger,
                &path,
                uid,
                gid,
                &binding,
                &id,
                &old,
                &next,
                &|| 101,
                1200,
                500,
                |_| Ok(())
            ),
            Err(PublishError::RecoveryRequired)
        );
        assert_eq!(std::fs::read(&victim).unwrap(), b"unchanged");
        assert!(
            std::fs::symlink_metadata(receipt)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            ledger.consumption_status(&id).unwrap().unwrap().1,
            "pending"
        );
    }
}
