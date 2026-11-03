//! Experimental read-only grant snapshot. The guest has an opt-in candidate
//! loader; the hotel configuration and protected publisher are not wired.
//!
//! This candidate contains no publisher, accepted-revision store or authenticated
//! mutation interface. It is not production-ready and has no legacy fallback.
//! This pilot candidate deliberately requires identical OAuth subject and local
//! owner identifiers; it is not a general identity-mapping mechanism.
use crate::{Error, Grant, TrustedIssuer, response::ResponseVerifier};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::File,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
};

const LIMIT: u64 = 65536;
const MODE_MASK: u32 = 0o7777;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub schema: u8,
    pub authority_id: String,
    pub generation: u64,
    pub grant_id: String,
    pub grant: Grant,
    pub issuer_key: TrustedIssuer,
    pub response_verifier: ResponseVerifier,
    pub oauth_issuer: String,
    pub oauth_resource: String,
    pub oauth_metadata_url: String,
    pub authority_socket: std::path::PathBuf,
    pub oauth_client_id: String,
    pub oauth_subject: String,
    pub oauth_grant_version: String,
}

/// Values must come from the hotel's authenticated, pinned local binding,
/// never directly from an incoming request. This candidate is not wired there.
pub struct ExpectedBinding<'a> {
    pub authority: &'a str,
    pub grant_id: &'a str,
    pub owner: &'a str,
    pub namespace: &'a str,
    pub destination: &'a str,
    pub runner: &'a str,
    pub ingress_issuer: &'a str,
    pub reply_node: &'a str,
    pub reply_guest: &'a str,
    pub issuer_key_id: &'a str,
    pub issuer_public_key: [u8; 32],
    pub response_key_id: &'a str,
    pub response_public_key: [u8; 32],
    pub oauth_issuer: &'a str,
    pub oauth_resource: &'a str,
    pub oauth_metadata_url: &'a str,
    pub authority_socket: &'a Path,
    pub oauth_client_id: &'a str,
    pub oauth_subject: &'a str,
    pub oauth_grant_version: &'a str,
}

/// Non-secret pins loaded from a root-owned 0640 file. Never build these from
/// routed arguments or from the snapshot being authenticated.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedBinding {
    pub authority: String,
    pub grant_id: String,
    pub owner: String,
    pub namespace: String,
    pub destination: String,
    pub runner: String,
    pub ingress_issuer: String,
    pub reply_node: String,
    pub reply_guest: String,
    pub issuer_key_id: String,
    pub issuer_public_key: [u8; 32],
    pub response_key_id: String,
    pub response_public_key: [u8; 32],
    pub oauth_issuer: String,
    pub oauth_resource: String,
    pub oauth_metadata_url: String,
    pub authority_socket: std::path::PathBuf,
    pub oauth_client_id: String,
    pub oauth_subject: String,
    pub oauth_grant_version: String,
}
impl OwnedBinding {
    pub fn borrowed(&self) -> ExpectedBinding<'_> {
        ExpectedBinding {
            authority: &self.authority,
            grant_id: &self.grant_id,
            owner: &self.owner,
            namespace: &self.namespace,
            destination: &self.destination,
            runner: &self.runner,
            ingress_issuer: &self.ingress_issuer,
            reply_node: &self.reply_node,
            reply_guest: &self.reply_guest,
            issuer_key_id: &self.issuer_key_id,
            issuer_public_key: self.issuer_public_key,
            response_key_id: &self.response_key_id,
            response_public_key: self.response_public_key,
            oauth_issuer: &self.oauth_issuer,
            oauth_resource: &self.oauth_resource,
            oauth_metadata_url: &self.oauth_metadata_url,
            authority_socket: &self.authority_socket,
            oauth_client_id: &self.oauth_client_id,
            oauth_subject: &self.oauth_subject,
            oauth_grant_version: &self.oauth_grant_version,
        }
    }
}
pub fn load_root_binding(path: &Path, group: u32) -> Result<OwnedBinding, Error> {
    load_binding_with_owner(path, 0, group)
}
fn load_binding_with_owner(path: &Path, uid: u32, group: u32) -> Result<OwnedBinding, Error> {
    let bytes = read_with_owner(path, uid, group)?;
    let binding: OwnedBinding = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
    validate_binding(&binding.borrowed())?;
    Ok(binding)
}

pub struct LoadedSnapshot {
    snapshot: Snapshot,
    sha256: [u8; 32],
}

impl LoadedSnapshot {
    /// This reference cannot mutate the validated snapshot. Runtime integration
    /// must still revalidate its digest and revision at commit and release.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub fn digest(&self) -> [u8; 32] {
        self.sha256
    }

    /// Build the single-grant receiver policy from this validated snapshot.
    /// Authority still depends on trusted custody or protected transfer.
    pub fn receiver_policy(&self) -> crate::Policy {
        crate::Policy {
            issuers: std::collections::BTreeMap::from([(
                self.snapshot.grant.issuer.clone(),
                self.snapshot.issuer_key.clone(),
            )]),
            grants: std::collections::BTreeMap::from([(
                self.snapshot.grant_id.clone(),
                self.snapshot.grant.clone(),
            )]),
        }
    }

    /// Compare only derived public keys. Callers retain responsibility for
    /// secret custody and zeroization; this candidate does not load secrets.
    pub fn verify_ingress_seed(&self, seed: &[u8; 32]) -> Result<(), Error> {
        if ed25519_dalek::SigningKey::from_bytes(seed)
            .verifying_key()
            .to_bytes()
            != self.snapshot.issuer_key.public_key
        {
            return Err(Error::Denied);
        }
        Ok(())
    }

    pub fn verify_response_seed(&self, seed: &[u8; 32]) -> Result<(), Error> {
        if ed25519_dalek::SigningKey::from_bytes(seed)
            .verifying_key()
            .to_bytes()
            != self.snapshot.response_verifier.public_key
        {
            return Err(Error::Denied);
        }
        Ok(())
    }
}

fn checked_dir(fd: &File, uid: u32) -> Result<(), Error> {
    let m = fd.metadata().map_err(|_| Error::Unavailable)?;
    if !m.is_dir() || ![0, uid].contains(&m.uid()) || m.mode() & 0o022 != 0 {
        return Err(Error::Denied);
    }
    Ok(())
}

/// The production owner is fixed to root. This does not establish rollback
/// resistance or authorize callers to mutate a snapshot.
pub fn load_root_owned(
    path: &Path,
    group: u32,
    expected: &ExpectedBinding<'_>,
) -> Result<LoadedSnapshot, Error> {
    load_with_owner(path, 0, group, expected)
}

#[cfg(test)]
pub(crate) fn load_test_owned(
    path: &Path,
    uid: u32,
    gid: u32,
    expected: &ExpectedBinding<'_>,
) -> Result<LoadedSnapshot, Error> {
    load_with_owner(path, uid, gid, expected)
}

pub(crate) fn load_with_owner(
    path: &Path,
    uid: u32,
    gid: u32,
    expected: &ExpectedBinding<'_>,
) -> Result<LoadedSnapshot, Error> {
    validate_binding(expected)?;
    let bytes = read_with_owner(path, uid, gid)?;
    validate_snapshot_bytes(&bytes, expected)
}

pub(crate) fn read_with_owner(path: &Path, uid: u32, gid: u32) -> Result<Vec<u8>, Error> {
    if !path.is_absolute() {
        return Err(Error::Invalid);
    }
    let mut parts = path.components();
    if parts.next() != Some(Component::RootDir) {
        return Err(Error::Invalid);
    }
    let mut components = Vec::new();
    for part in parts {
        let Component::Normal(name) = part else {
            return Err(Error::Invalid);
        };
        components.push(CString::new(name.as_bytes()).map_err(|_| Error::Invalid)?);
    }
    let (name, parents) = components.split_last().ok_or(Error::Invalid)?;
    let slash = CString::new("/").map_err(|_| Error::Invalid)?;
    let raw = unsafe {
        libc::open(
            slash.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(Error::Denied);
    }
    let mut dir = unsafe { File::from_raw_fd(raw) };
    checked_dir(&dir, uid)?;
    for component in parents {
        let raw = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            return Err(Error::Denied);
        }
        dir = unsafe { File::from_raw_fd(raw) };
        checked_dir(&dir, uid)?;
    }
    let parent = dir.metadata().map_err(|_| Error::Unavailable)?;
    if parent.uid() != uid || parent.gid() != gid || parent.mode() & MODE_MASK != 0o750 {
        return Err(Error::Denied);
    }
    let raw = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(Error::Denied);
    }
    let mut file = unsafe { File::from_raw_fd(raw) };
    let before = file.metadata().map_err(|_| Error::Unavailable)?;
    if !before.is_file()
        || before.uid() != uid
        || before.gid() != gid
        || before.mode() & MODE_MASK != 0o640
        || before.nlink() != 1
        || before.len() > LIMIT
    {
        return Err(Error::Denied);
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Unavailable)?;
    let after = file.metadata().map_err(|_| Error::Unavailable)?;
    if bytes.len() as u64 != before.len()
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(Error::Denied);
    }
    Ok(bytes)
}

fn validate_binding(expected: &ExpectedBinding<'_>) -> Result<(), Error> {
    if [
        expected.authority,
        expected.grant_id,
        expected.owner,
        expected.namespace,
        expected.destination,
        expected.runner,
        expected.ingress_issuer,
        expected.reply_node,
        expected.reply_guest,
        expected.issuer_key_id,
        expected.response_key_id,
        expected.oauth_issuer,
        expected.oauth_resource,
        expected.oauth_metadata_url,
        expected.oauth_client_id,
        expected.oauth_subject,
        expected.oauth_grant_version,
    ]
    .iter()
    .any(|s| s.is_empty())
    {
        return Err(Error::Invalid);
    }
    if [
        expected.authority,
        expected.grant_id,
        expected.owner,
        expected.namespace,
        expected.destination,
        expected.runner,
        expected.ingress_issuer,
        expected.reply_node,
        expected.reply_guest,
        expected.issuer_key_id,
        expected.response_key_id,
        expected.oauth_client_id,
        expected.oauth_subject,
    ]
    .iter()
    .any(|value| !crate::label(value))
    {
        return Err(Error::Invalid);
    }
    for value in [expected.oauth_issuer, expected.oauth_resource] {
        let url = url::Url::parse(value).map_err(|_| Error::Invalid)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::Invalid);
        }
    }
    Ok(())
}

/// Validate exact candidate bytes against an independently trusted binding.
/// This does not establish the provenance of that binding or authorize a change.
pub fn validate_snapshot_bytes(
    bytes: &[u8],
    expected: &ExpectedBinding<'_>,
) -> Result<LoadedSnapshot, Error> {
    validate_binding(expected)?;
    if bytes.len() as u64 > LIMIT {
        return Err(Error::Invalid);
    }
    let s: Snapshot = serde_json::from_slice(bytes).map_err(|_| Error::Invalid)?;
    if s.schema != 1
        || s.authority_id != expected.authority
        || s.grant_id != expected.grant_id
        || s.generation == 0
        || s.generation != s.grant.revision
        || s.grant.owner != s.grant.principal
        || s.grant.owner != expected.owner
        || s.grant.namespace != expected.namespace
        || s.grant.destination != expected.destination
        || s.grant.runner != expected.runner
        || s.grant.issuer != expected.ingress_issuer
        || s.grant.reply_node != expected.reply_node
        || s.grant.reply_guest != expected.reply_guest
        || s.grant.operations != ["remember", "recall"]
        || s.grant.expires_at == 0
        || s.issuer_key.key_id != expected.issuer_key_id
        || s.issuer_key.public_key != expected.issuer_public_key
        || !ed25519_dalek::VerifyingKey::from_bytes(&s.issuer_key.public_key)
            .is_ok_and(|key| !key.is_weak())
        || s.response_verifier.key_id != expected.response_key_id
        || s.response_verifier.public_key != expected.response_public_key
        || s.response_verifier.destination != expected.destination
        || s.response_verifier.runner != expected.runner
        || !ed25519_dalek::VerifyingKey::from_bytes(&s.response_verifier.public_key)
            .is_ok_and(|key| !key.is_weak())
        || s.oauth_issuer != expected.oauth_issuer
        || s.oauth_resource != expected.oauth_resource
        || s.oauth_metadata_url != expected.oauth_metadata_url
        || s.authority_socket != expected.authority_socket
        || s.oauth_client_id != expected.oauth_client_id
        || s.oauth_subject != expected.oauth_subject
        || s.oauth_subject != s.grant.owner
        || s.oauth_grant_version != expected.oauth_grant_version
        || !crate::oauth::version(&s.oauth_grant_version)
    {
        return Err(Error::Denied);
    }
    crate::oauth::ResourceConfig {
        issuer: s.oauth_issuer.clone(),
        resource: s.oauth_resource.clone(),
        metadata_url: s.oauth_metadata_url.clone(),
        authority_socket: s.authority_socket.clone(),
        grant_id: s.grant_id.clone(),
    }
    .validate()?;
    Ok(LoadedSnapshot {
        snapshot: s,
        sha256: Sha256::digest(bytes).into(),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    pub(crate) fn sample() -> Snapshot {
        Snapshot {
            schema: 1,
            authority_id: "synthetic-hotel".into(),
            generation: 3,
            grant_id: "synthetic-grant".into(),
            issuer_key: TrustedIssuer {
                key_id: "key".into(),
                public_key: ed25519_dalek::SigningKey::from_bytes(&[7; 32])
                    .verifying_key()
                    .to_bytes(),
            },
            response_verifier: ResponseVerifier {
                destination: "hotel".into(),
                runner: "runner".into(),
                key_id: "result".into(),
                public_key: ed25519_dalek::SigningKey::from_bytes(&[8; 32])
                    .verifying_key()
                    .to_bytes(),
            },
            oauth_issuer: "https://issuer.example".into(),
            oauth_resource: "https://memory.example/native/mcp".into(),
            oauth_metadata_url:
                "https://memory.example/.well-known/oauth-protected-resource/native/mcp".into(),
            authority_socket: "/run/synthetic/authority.sock".into(),
            oauth_client_id: "dot".into(),
            oauth_subject: "synthetic".into(),
            oauth_grant_version: "0:7".into(),
            grant: Grant {
                issuer: "synthetic-issuer".into(),
                reply_node: "hotel".into(),
                reply_guest: "membrane".into(),
                principal: "synthetic".into(),
                owner: "synthetic".into(),
                namespace: "synthetic".into(),
                destination: "hotel".into(),
                runner: "runner".into(),
                operations: vec!["remember".into(), "recall".into()],
                revision: 3,
                expires_at: 1000,
                revoked: false,
            },
        }
    }
    pub(crate) fn expected() -> ExpectedBinding<'static> {
        ExpectedBinding {
            authority: "synthetic-hotel",
            grant_id: "synthetic-grant",
            owner: "synthetic",
            namespace: "synthetic",
            destination: "hotel",
            runner: "runner",
            ingress_issuer: "synthetic-issuer",
            reply_node: "hotel",
            reply_guest: "membrane",
            issuer_key_id: "key",
            issuer_public_key: ed25519_dalek::SigningKey::from_bytes(&[7; 32])
                .verifying_key()
                .to_bytes(),
            response_key_id: "result",
            response_public_key: ed25519_dalek::SigningKey::from_bytes(&[8; 32])
                .verifying_key()
                .to_bytes(),
            oauth_issuer: "https://issuer.example",
            oauth_resource: "https://memory.example/native/mcp",
            oauth_metadata_url: "https://memory.example/.well-known/oauth-protected-resource/native/mcp",
            authority_socket: Path::new("/run/synthetic/authority.sock"),
            oauth_client_id: "dot",
            oauth_subject: "synthetic",
            oauth_grant_version: "0:7",
        }
    }
    pub(crate) fn fixture() -> (tempfile::TempDir, std::path::PathBuf, u32, u32) {
        let root = tempfile::Builder::new()
            .prefix("grant-snapshot-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o750)).unwrap();
        let path = root.path().join("snapshot.json");
        fs::write(&path, serde_json::to_vec(&sample()).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let m = fs::metadata(&path).unwrap();
        (root, path, m.uid(), m.gid())
    }
    fn read(path: &Path, uid: u32, gid: u32) -> Result<LoadedSnapshot, Error> {
        load_with_owner(path, uid, gid, &expected())
    }
    #[test]
    fn accepts_only_pinned_owner_group_mode_and_binding() {
        let (_root, path, uid, gid) = fixture();
        let loaded = read(&path, uid, gid).unwrap();
        assert_eq!(loaded.snapshot.generation, 3);
        assert!(loaded.verify_ingress_seed(&[7; 32]).is_ok());
        assert!(loaded.verify_ingress_seed(&[9; 32]).is_err());
        assert!(loaded.verify_response_seed(&[8; 32]).is_ok());
        assert!(loaded.verify_response_seed(&[9; 32]).is_err());
        assert_eq!(
            loaded.sha256,
            <[u8; 32]>::from(Sha256::digest(fs::read(&path).unwrap()))
        );
        assert!(read(&path, uid.wrapping_add(1), gid).is_err());
        assert!(read(&path, uid, gid.wrapping_add(1)).is_err());
        let mut wrong = expected();
        wrong.authority = "other-hotel";
        assert!(load_with_owner(&path, uid, gid, &wrong).is_err());
        let mut wrong = expected();
        wrong.oauth_resource = "https://other.example/native/mcp";
        assert!(load_with_owner(&path, uid, gid, &wrong).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o660)).unwrap();
        assert!(read(&path, uid, gid).is_err());
    }
    #[test]
    fn binding_file_and_response_signer_fail_closed_on_drift() {
        let (_root, path, uid, gid) = fixture();
        let b = expected();
        let value = serde_json::json!({"authority":b.authority,"grant_id":b.grant_id,"owner":b.owner,"namespace":b.namespace,"destination":b.destination,"runner":b.runner,"ingress_issuer":b.ingress_issuer,"reply_node":b.reply_node,"reply_guest":b.reply_guest,"issuer_key_id":b.issuer_key_id,"issuer_public_key":b.issuer_public_key,"response_key_id":b.response_key_id,"response_public_key":b.response_public_key,"oauth_issuer":b.oauth_issuer,"oauth_resource":b.oauth_resource,"oauth_metadata_url":b.oauth_metadata_url,"authority_socket":b.authority_socket,"oauth_client_id":b.oauth_client_id,"oauth_subject":b.oauth_subject,"oauth_grant_version":b.oauth_grant_version});
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let pins = load_binding_with_owner(&path, uid, gid).unwrap();
        assert_eq!(pins.borrowed().authority, "synthetic-hotel");
        let mut wrong = value.clone();
        wrong["unexpected"] = serde_json::json!(true);
        fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(load_binding_with_owner(&path, uid, gid).is_err());
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o660)).unwrap();
        assert!(load_binding_with_owner(&path, uid, gid).is_err());
        let bytes = serde_json::to_vec(&sample()).unwrap();
        let snapshot = validate_snapshot_bytes(&bytes, &expected()).unwrap();
        assert!(
            crate::response::ResponseSigner::new("result".into(), [8; 32])
                .verify_snapshot(&snapshot)
                .is_ok()
        );
        assert!(
            crate::response::ResponseSigner::new("other".into(), [8; 32])
                .verify_snapshot(&snapshot)
                .is_err()
        );
        assert!(
            crate::response::ResponseSigner::new("result".into(), [9; 32])
                .verify_snapshot(&snapshot)
                .is_err()
        );
    }
    #[test]
    fn loaded_receiver_policy_blocks_real_runner_commit_after_revocation() {
        use std::sync::atomic::AtomicBool;
        let (root, path, uid, gid) = fixture();
        let initial = read(&path, uid, gid).unwrap();
        let grants = std::collections::BTreeMap::from([(
            "synthetic-grant".into(),
            initial.snapshot().grant.clone(),
        )]);
        let ingress = crate::Ingress::new(
            "synthetic-issuer".into(),
            "key".into(),
            ed25519_dalek::SigningKey::from_bytes(&[7; 32]),
            grants,
        );
        let envelope = ingress
            .issue(
                "synthetic-grant",
                crate::Command::Remember {
                    id: "id".into(),
                    subject: "synthetic".into(),
                    content: "harmless synthetic".into(),
                    source: "fixture".into(),
                    source_version: "1".into(),
                    lineage: "synthetic".into(),
                },
                "event".into(),
                "nonce".into(),
                101,
            )
            .unwrap();
        let db_path = root.path().join("runner.sqlite");
        let mut runner = crate::Runner::open(&db_path, "hotel".into(), "runner".into()).unwrap();
        let mut reads = 0;
        let result = runner.execute_current(
            &envelope,
            || {
                reads += 1;
                if reads == 2 {
                    let mut next = sample();
                    next.generation += 1;
                    next.grant.revision += 1;
                    next.grant.revoked = true;
                    fs::write(&path, serde_json::to_vec(&next).unwrap()).unwrap();
                }
                Ok((read(&path, uid, gid)?.receiver_policy(), 101))
            },
            &AtomicBool::new(false),
        );
        assert_eq!(result, Err(Error::Denied));
        assert_eq!(reads, 2);
        drop(runner);
        let db = rusqlite::Connection::open(db_path).unwrap();
        for table in ["observations", "receipts", "nonces"] {
            let count: i64 = db
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0);
        }
    }
    #[test]
    fn rejects_symlink_ancestors_and_hard_links() {
        let (root, path, uid, gid) = fixture();
        let link = root.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(read(&link, uid, gid).is_err());
        let sub = root.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::set_permissions(&sub, fs::Permissions::from_mode(0o750)).unwrap();
        let valid = sub.join("snapshot.json");
        fs::copy(&path, &valid).unwrap();
        fs::set_permissions(&valid, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(read(&valid, uid, gid).is_ok());
        let alias = root.path().join("dirlink");
        symlink(&sub, &alias).unwrap();
        assert!(read(&alias.join("snapshot.json"), uid, gid).is_err());
        fs::hard_link(&path, root.path().join("hardlink")).unwrap();
        assert!(read(&path, uid, gid).is_err());
    }
    #[test]
    fn rejects_writable_directory_and_nonregular_file() {
        let (root, path, uid, gid) = fixture();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o770)).unwrap();
        assert!(read(&path, uid, gid).is_err());
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o750)).unwrap();
        fs::remove_file(&path).unwrap();
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o640) }, 0);
        assert!(read(&path, uid, gid).is_err());
    }
    #[test]
    fn rejects_legacy_unknown_duplicate_and_wrong_generation() {
        let (_root, path, uid, gid) = fixture();
        let mut value = serde_json::to_value(sample()).unwrap();
        value["legacy_grants"] = serde_json::json!({});
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(read(&path, uid, gid).is_err());
        let raw = serde_json::to_string(&sample()).unwrap().replacen(
            "\"schema\":1",
            "\"schema\":1,\"schema\":1",
            1,
        );
        fs::write(&path, raw).unwrap();
        assert!(read(&path, uid, gid).is_err());
        let mut wrong = sample();
        wrong.generation = 4;
        fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(read(&path, uid, gid).is_err());
    }

    #[test]
    fn rejects_mismatched_response_routing_and_oauth_binding() {
        let (_root, path, uid, gid) = fixture();
        let mut wrong = sample();
        wrong.response_verifier.runner = "other".into();
        fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(read(&path, uid, gid).is_err());
        let mut wrong = sample();
        wrong.oauth_issuer = "https://other.example".into();
        fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(read(&path, uid, gid).is_err());
        let mut wrong = sample();
        wrong.grant.reply_guest = "other".into();
        fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(read(&path, uid, gid).is_err());
        let mut wrong = sample();
        wrong.authority_socket = "/run/other/authority.sock".into();
        fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(read(&path, uid, gid).is_err());
        let mut wrong = sample();
        wrong.oauth_metadata_url = "https://other.example/metadata".into();
        fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(read(&path, uid, gid).is_err());
        let mut wrong = sample();
        wrong.oauth_client_id = "other-client".into();
        fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(read(&path, uid, gid).is_err());
        let mut wrong = sample();
        wrong.oauth_client_id = "bad\nclient".into();
        let mut binding = expected();
        binding.oauth_client_id = "bad\nclient";
        fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(load_with_owner(&path, uid, gid, &binding).is_err());
        let mut wrong = sample();
        wrong.oauth_subject = "other-subject".into();
        fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(read(&path, uid, gid).is_err());
        let mut wrong = sample();
        wrong.oauth_grant_version = "0:8".into();
        fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(read(&path, uid, gid).is_err());
        let mut wrong = sample();
        wrong.oauth_grant_version = "01:7".into();
        let mut binding = expected();
        binding.oauth_grant_version = "01:7";
        fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(load_with_owner(&path, uid, gid, &binding).is_err());
    }

    #[test]
    fn rejects_substituted_or_weak_public_verifiers() {
        let (_root, path, uid, gid) = fixture();
        let mut wrong = sample();
        wrong.issuer_key.key_id = "other".into();
        fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(read(&path, uid, gid).is_err());
        let mut wrong = sample();
        wrong.response_verifier.key_id = "other".into();
        fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(read(&path, uid, gid).is_err());
        let mut wrong = sample();
        wrong.issuer_key.public_key = ed25519_dalek::SigningKey::from_bytes(&[9; 32])
            .verifying_key()
            .to_bytes();
        fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(read(&path, uid, gid).is_err());
        let mut wrong = sample();
        wrong.response_verifier.public_key = ed25519_dalek::SigningKey::from_bytes(&[9; 32])
            .verifying_key()
            .to_bytes();
        fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(read(&path, uid, gid).is_err());
        assert!(
            ed25519_dalek::VerifyingKey::from_bytes(&[0; 32])
                .unwrap()
                .is_weak()
        );
        let mut weak = sample();
        weak.issuer_key.public_key = [0; 32];
        let mut binding = expected();
        binding.issuer_public_key = [0; 32];
        fs::write(&path, serde_json::to_vec(&weak).unwrap()).unwrap();
        assert!(load_with_owner(&path, uid, gid, &binding).is_err());
        let mut weak = sample();
        weak.response_verifier.public_key = [0; 32];
        let mut binding = expected();
        binding.response_public_key = [0; 32];
        fs::write(&path, serde_json::to_vec(&weak).unwrap()).unwrap();
        assert!(load_with_owner(&path, uid, gid, &binding).is_err());
    }

    #[test]
    fn candidate_snapshot_revocation_denies_real_runner_before_commit() {
        use crate::{Command, Ingress, Policy, Runner};
        use ed25519_dalek::SigningKey;
        use std::{collections::BTreeMap, sync::atomic::AtomicBool};
        let (root, path, uid, gid) = fixture();
        let key = SigningKey::from_bytes(&[7; 32]);
        let policy = |loaded: LoadedSnapshot| Policy {
            issuers: BTreeMap::from([("synthetic-issuer".into(), loaded.snapshot.issuer_key)]),
            grants: BTreeMap::from([(loaded.snapshot.grant_id, loaded.snapshot.grant)]),
        };
        let initial = policy(read(&path, uid, gid).unwrap());
        let ingress = Ingress::new(
            "synthetic-issuer".into(),
            "key".into(),
            key,
            initial.grants.clone(),
        );
        let envelope = ingress
            .issue(
                "synthetic-grant",
                Command::Remember {
                    id: "note".into(),
                    subject: "synthetic".into(),
                    content: "harmless fixture".into(),
                    source: "synthetic".into(),
                    source_version: "1".into(),
                    lineage: "fixture".into(),
                },
                "event".into(),
                "nonce".into(),
                100,
            )
            .unwrap();
        let db = root.path().join("fixture.sqlite");
        let mut runner = Runner::open(&db, "hotel".into(), "runner".into()).unwrap();
        let mut calls = 0;
        let result = runner.execute_current(
            &envelope,
            || {
                calls += 1;
                if calls == 2 {
                    let mut revoked = sample();
                    revoked.generation = 4;
                    revoked.grant.revision = 4;
                    revoked.grant.revoked = true;
                    let next = root.path().join("snapshot.next");
                    fs::write(&next, serde_json::to_vec(&revoked).unwrap()).unwrap();
                    fs::set_permissions(&next, fs::Permissions::from_mode(0o640)).unwrap();
                    fs::rename(next, &path).unwrap();
                }
                Ok((policy(read(&path, uid, gid)?), 101))
            },
            &AtomicBool::new(false),
        );
        assert_eq!(result, Err(Error::Denied));
        assert_eq!(calls, 2);
        drop(runner);
        let db = rusqlite::Connection::open(db).unwrap();
        for table in ["observations", "receipts", "nonces"] {
            let count: i64 = db
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(count, 0);
        }
        // Synthetic current-user ownership; no publisher, authenticated
        // authority, hotel fence or root/service separation is exercised.
    }
}
