//! Socket-activated approval-state broker.
//!
//! The process that serves this socket is the only writer of
//! `/var/lib/philotic-approval/approval.db`. Callers are admitted by kernel
//! peer uid. This module stores policy rows, pinned public keys, requests,
//! and the burn ledger. It does not verify signatures. That is the S1 verifier.

use serde::{Deserialize, Serialize};
use std::{io, path::Path, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{Mutex, Semaphore},
    time::timeout,
};

pub const APPROVAL_DB_PATH: &str = "/var/lib/philotic-approval/approval.db";
pub const BROKER_SOCKET_PATH: &str = "/run/philotic-approval/broker.sock";
const MAX_FRAME: usize = 4096;
const MAX_HANDLERS: usize = 8;
const READ_DEADLINE: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrokerPolicy {
    allowed_uid: u32,
}

impl BrokerPolicy {
    pub fn new(allowed_uid: u32) -> io::Result<Self> {
        if allowed_uid == 0 {
            return Err(denied());
        }
        Ok(Self { allowed_uid })
    }
}

pub fn peer_is_allowed(peer: Option<u32>, policy: BrokerPolicy) -> bool {
    peer == Some(policy.allowed_uid) && policy.allowed_uid != 0
}

/// Directory is safe against `hotel_uid` when that uid is not the owner and
/// the mode is not group- or world-writable. This does not itself drop
/// privileges; the integration test performs the write as the hotel uid.
pub fn approval_dir_excludes_hotel(dir: &Path, hotel_uid: u32) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() == hotel_uid || meta.uid() == 0 || meta.mode() & 0o022 != 0 {
        return Err(denied());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurnOutcome {
    Burned,
    AlreadyBurned,
}

pub struct ApprovalStore {
    conn: rusqlite::Connection,
}

impl ApprovalStore {
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            if parent.as_os_str().is_empty() {
                return Err(denied());
            }
        }
        let conn = rusqlite::Connection::open(path).map_err(io_err)?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(io_err)?;
        conn.execute_batch(
            "
            PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS policy (
                action TEXT PRIMARY KEY,
                tier INTEGER NOT NULL CHECK (tier >= 0)
            );
            CREATE TABLE IF NOT EXISTS pinned_keys (
                device_key_id TEXT PRIMARY KEY,
                public_key BLOB NOT NULL,
                owner TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS requests (
                request_id TEXT PRIMARY KEY,
                nonce TEXT NOT NULL,
                action TEXT NOT NULL,
                state TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS burns (
                request_id TEXT NOT NULL,
                nonce TEXT NOT NULL,
                burned_at INTEGER NOT NULL,
                PRIMARY KEY (request_id, nonce)
            );
            ",
        )
        .map_err(io_err)?;
        tighten_private(path)?;
        Ok(Self { conn })
    }

    /// Re-open only when the existing file is mode 0600 and owned by `owner_uid`.
    pub fn open_owned(path: &Path, owner_uid: u32) -> io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path)?;
        if !meta.is_file() || meta.uid() != owner_uid || meta.mode() & 0o077 != 0 {
            return Err(denied());
        }
        Self::open(path)
    }

    pub fn put_policy(&mut self, action: &str, tier: i64) -> io::Result<()> {
        validate_token(action)?;
        if tier < 0 {
            return Err(denied());
        }
        self.conn
            .execute(
                "INSERT INTO policy (action, tier) VALUES (?1, ?2)
                 ON CONFLICT(action) DO UPDATE SET tier = excluded.tier",
                (action, tier),
            )
            .map_err(io_err)?;
        Ok(())
    }

    pub fn get_policy_tier(&mut self, action: &str) -> io::Result<Option<i64>> {
        validate_token(action)?;
        match self.conn.query_row(
            "SELECT tier FROM policy WHERE action = ?1",
            [action],
            |row| row.get(0),
        ) {
            Ok(tier) => Ok(Some(tier)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(io_err(err)),
        }
    }

    pub fn pin_key(
        &mut self,
        device_key_id: &str,
        public_key: &[u8],
        owner: &str,
    ) -> io::Result<()> {
        validate_token(device_key_id)?;
        validate_token(owner)?;
        if public_key.is_empty() || public_key.len() > 512 {
            return Err(denied());
        }
        self.conn
            .execute(
                "INSERT INTO pinned_keys (device_key_id, public_key, owner) VALUES (?1, ?2, ?3)
                 ON CONFLICT(device_key_id) DO UPDATE SET
                    public_key = excluded.public_key,
                    owner = excluded.owner",
                (device_key_id, public_key, owner),
            )
            .map_err(io_err)?;
        Ok(())
    }

    pub fn submit_request(
        &mut self,
        request_id: &str,
        nonce: &str,
        action: &str,
    ) -> io::Result<()> {
        validate_token(request_id)?;
        validate_token(nonce)?;
        validate_token(action)?;
        self.conn
            .execute(
                "INSERT INTO requests (request_id, nonce, action, state) VALUES (?1, ?2, ?3, 'open')",
                (request_id, nonce, action),
            )
            .map_err(io_err)?;
        Ok(())
    }

    pub fn burn_count(&mut self) -> io::Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM burns", [], |row| row.get(0))
            .map_err(io_err)
    }

    /// Insert the burn row and mark the request in one immediate transaction.
    /// A second caller of the same pair observes [`BurnOutcome::AlreadyBurned`]
    /// and leaves a single burn row.
    pub fn burn(&mut self, request_id: &str, nonce: &str) -> io::Result<BurnOutcome> {
        validate_token(request_id)?;
        validate_token(nonce)?;
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(io_err)?;
        let already: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM burns WHERE request_id = ?1 AND nonce = ?2",
                (request_id, nonce),
                |row| row.get(0),
            )
            .map_err(io_err)?;
        if already > 0 {
            tx.commit().map_err(io_err)?;
            return Ok(BurnOutcome::AlreadyBurned);
        }
        let open: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM requests
                 WHERE request_id = ?1 AND nonce = ?2 AND state = 'open'",
                (request_id, nonce),
                |row| row.get(0),
            )
            .map_err(io_err)?;
        if open != 1 {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "approval_request_unknown",
            ));
        }
        let burned_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        tx.execute(
            "INSERT INTO burns (request_id, nonce, burned_at) VALUES (?1, ?2, ?3)",
            (request_id, nonce, burned_at),
        )
        .map_err(io_err)?;
        let updated = tx
            .execute(
                "UPDATE requests SET state = 'burned'
                 WHERE request_id = ?1 AND nonce = ?2 AND state = 'open'",
                (request_id, nonce),
            )
            .map_err(io_err)?;
        if updated != 1 {
            return Err(denied());
        }
        tx.commit().map_err(io_err)?;
        Ok(BurnOutcome::Burned)
    }
}

fn tighten_private(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)
}

fn validate_token(value: &str) -> io::Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'.' || c == b':')
    {
        return Err(denied());
    }
    Ok(())
}

fn denied() -> io::Error {
    io::Error::other("approval_unavailable")
}

fn io_err(err: rusqlite::Error) -> io::Error {
    io::Error::other(err.to_string())
}

fn peer_uid(stream: &UnixStream) -> io::Result<Option<u32>> {
    // Linux SO_PEERCRED and macOS getpeereid both surface through tokio.
    #[cfg(any(
        target_os = "android",
        target_os = "linux",
        target_os = "macos",
        target_os = "ios"
    ))]
    {
        Ok(Some(stream.peer_cred()?.uid()))
    }
    #[cfg(not(any(
        target_os = "android",
        target_os = "linux",
        target_os = "macos",
        target_os = "ios"
    )))]
    {
        let _ = stream;
        Err(denied())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    operation: String,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    tier: Option<i64>,
    #[serde(default)]
    device_key_id: Option<String>,
    #[serde(default)]
    public_key_b64: Option<String>,
    #[serde(default)]
    owner: Option<String>,
}

#[derive(Serialize)]
struct Reply<'a> {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tier: Option<i64>,
}

pub struct Broker {
    policy: BrokerPolicy,
    store: Arc<Mutex<ApprovalStore>>,
}

impl Broker {
    pub fn new(policy: BrokerPolicy, store: ApprovalStore) -> Self {
        Self {
            policy,
            store: Arc::new(Mutex::new(store)),
        }
    }
}

pub async fn serve(listener: UnixListener, broker: Broker) -> io::Result<()> {
    let slots = Arc::new(Semaphore::new(MAX_HANDLERS));
    loop {
        let (stream, _) = listener.accept().await?;
        let Ok(permit) = slots.clone().try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let policy = broker.policy;
        let store = Arc::clone(&broker.store);
        tokio::spawn(async move {
            let _permit = permit;
            handle_connection(stream, policy, store).await;
        });
    }
}

/// Production entry. Requires systemd socket activation (`LISTEN_FDS`) and
/// `PHILOTIC_APPROVAL_CALLER_UID`. Does not open the hotel vault.
pub fn run_socket_activated() -> anyhow::Result<()> {
    let allowed: u32 = std::env::var("PHILOTIC_APPROVAL_CALLER_UID")
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("approval_broker_caller_uid_missing"))?;
    let policy = BrokerPolicy::new(allowed)?;
    let db_path =
        std::env::var("PHILOTIC_APPROVAL_DB").unwrap_or_else(|_| APPROVAL_DB_PATH.to_string());
    let store = ApprovalStore::open(Path::new(&db_path))?;
    let listener = listener_from_activation()?;
    validate_production_listener(&listener)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve(listener, Broker::new(policy, store)))?;
    Ok(())
}

pub fn listener_from_activation() -> io::Result<UnixListener> {
    use std::os::fd::FromRawFd;
    let fds: i32 = std::env::var("LISTEN_FDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let pid: u32 = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    if fds < 1 || pid != std::process::id() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "approval_broker_not_socket_activated",
        ));
    }
    let names = std::env::var("LISTEN_FDNAMES").unwrap_or_default();
    if names.split(':').next() != Some("philotic-approval") {
        return Err(denied());
    }
    // SAFETY: systemd placed a listening unix socket on fd 3 for this pid.
    let std_listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(3) };
    std_listener.set_nonblocking(true)?;
    UnixListener::from_std(std_listener)
}

pub fn validate_production_listener(listener: &UnixListener) -> io::Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let address = listener.local_addr()?;
    let path = address.as_pathname().ok_or_else(denied)?;
    if path != Path::new(BROKER_SOCKET_PATH) {
        return Err(denied());
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket() || metadata.mode() & 0o777 != 0o600 {
        return Err(denied());
    }
    Ok(())
}

async fn handle_connection(
    stream: UnixStream,
    policy: BrokerPolicy,
    store: Arc<Mutex<ApprovalStore>>,
) {
    handle_connection_within(stream, policy, store, READ_DEADLINE).await;
}

async fn handle_connection_within(
    mut stream: UnixStream,
    policy: BrokerPolicy,
    store: Arc<Mutex<ApprovalStore>>,
    deadline: Duration,
) {
    let peer = peer_uid(&stream).ok().flatten();
    let response = if !peer_is_allowed(peer, policy) {
        reply_err()
    } else {
        match timeout(deadline, read_frame(&mut stream)).await {
            Ok(Ok(bytes)) => dispatch(&bytes, &store).await,
            _ => reply_err(),
        }
    };
    let _ = write_frame(&mut stream, &response).await;
}

async fn dispatch(bytes: &[u8], store: &Mutex<ApprovalStore>) -> Vec<u8> {
    let request: Request = match serde_json::from_slice(bytes) {
        Ok(request) => request,
        Err(_) => return reply_err(),
    };
    let mut store = store.lock().await;
    let outcome = match request.operation.as_str() {
        "health" => Ok(Reply {
            ok: true,
            result: Some("ready"),
            error: None,
            tier: None,
        }),
        "put_policy" => put_policy(&mut store, &request),
        "get_policy" => get_policy(&mut store, &request),
        "pin_key" => pin_key(&mut store, &request),
        "submit_request" => submit_request(&mut store, &request),
        "burn" => burn(&mut store, &request),
        _ => Err(denied()),
    };
    match outcome {
        Ok(reply) => serde_json::to_vec(&reply).unwrap_or_else(|_| reply_err()),
        Err(_) => reply_err(),
    }
}

fn put_policy(store: &mut ApprovalStore, request: &Request) -> io::Result<Reply<'static>> {
    let action = request.action.as_deref().ok_or_else(denied)?;
    let tier = request.tier.ok_or_else(denied)?;
    store.put_policy(action, tier)?;
    Ok(Reply {
        ok: true,
        result: Some("stored"),
        error: None,
        tier: Some(tier),
    })
}

fn get_policy(store: &mut ApprovalStore, request: &Request) -> io::Result<Reply<'static>> {
    let action = request.action.as_deref().ok_or_else(denied)?;
    let tier = store.get_policy_tier(action)?;
    Ok(Reply {
        ok: true,
        result: Some(if tier.is_some() { "found" } else { "absent" }),
        error: None,
        tier,
    })
}

fn pin_key(store: &mut ApprovalStore, request: &Request) -> io::Result<Reply<'static>> {
    use base64::Engine;
    let device_key_id = request.device_key_id.as_deref().ok_or_else(denied)?;
    let owner = request.owner.as_deref().ok_or_else(denied)?;
    let encoded = request.public_key_b64.as_deref().ok_or_else(denied)?;
    let public_key = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| denied())?;
    store.pin_key(device_key_id, &public_key, owner)?;
    Ok(Reply {
        ok: true,
        result: Some("pinned"),
        error: None,
        tier: None,
    })
}

fn submit_request(store: &mut ApprovalStore, request: &Request) -> io::Result<Reply<'static>> {
    store.submit_request(
        request.request_id.as_deref().ok_or_else(denied)?,
        request.nonce.as_deref().ok_or_else(denied)?,
        request.action.as_deref().ok_or_else(denied)?,
    )?;
    Ok(Reply {
        ok: true,
        result: Some("accepted"),
        error: None,
        tier: None,
    })
}

fn burn(store: &mut ApprovalStore, request: &Request) -> io::Result<Reply<'static>> {
    let outcome = store.burn(
        request.request_id.as_deref().ok_or_else(denied)?,
        request.nonce.as_deref().ok_or_else(denied)?,
    )?;
    Ok(Reply {
        ok: true,
        result: Some(match outcome {
            BurnOutcome::Burned => "burned",
            BurnOutcome::AlreadyBurned => "already_burned",
        }),
        error: None,
        tier: None,
    })
}

fn reply_err() -> Vec<u8> {
    serde_json::to_vec(&Reply {
        ok: false,
        result: None,
        error: Some("approval_unavailable"),
        tier: None,
    })
    .unwrap_or_else(|_| br#"{"ok":false,"error":"approval_unavailable"}"#.to_vec())
}

async fn read_frame(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let size = stream.read_u32().await? as usize;
    if size == 0 || size > MAX_FRAME {
        return Err(denied());
    }
    let mut payload = vec![0; size];
    stream.read_exact(&mut payload).await?;
    Ok(payload)
}

async fn write_frame(stream: &mut UnixStream, payload: &[u8]) -> io::Result<()> {
    stream.write_u32(payload.len() as u32).await?;
    stream.write_all(payload).await?;
    stream.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Barrier};
    use std::thread;

    fn temp_db() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("approval.db");
        (dir, path)
    }

    #[test]
    fn policy_rejects_root_and_dir_must_exclude_hotel_uid() {
        assert!(BrokerPolicy::new(0).is_err());
        let policy = BrokerPolicy::new(999).unwrap();
        assert!(!peer_is_allowed(None, policy));
        assert!(!peer_is_allowed(Some(0), policy));
        assert!(!peer_is_allowed(Some(1000), policy));
        assert!(peer_is_allowed(Some(999), policy));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let hotel = unsafe { libc::geteuid() };
        assert!(approval_dir_excludes_hotel(path, hotel).is_err());
        assert!(approval_dir_excludes_hotel(path, hotel.wrapping_add(1)).is_ok());
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o777);
        std::fs::set_permissions(path, perms).unwrap();
        assert!(approval_dir_excludes_hotel(path, hotel.wrapping_add(1)).is_err());
    }

    #[test]
    fn loose_database_file_is_refused() {
        let (_dir, path) = temp_db();
        let _store = ApprovalStore::open(&path).unwrap();
        let uid = unsafe { libc::geteuid() };
        ApprovalStore::open_owned(&path, uid).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o666);
        std::fs::set_permissions(&path, perms).unwrap();
        assert!(ApprovalStore::open_owned(&path, uid).is_err());
    }

    #[test]
    fn burn_is_durable_and_atomic() {
        let (_dir, path) = temp_db();
        let mut store = ApprovalStore::open(&path).unwrap();
        store
            .submit_request("req-1", "nonce-1", "restart-hotel")
            .unwrap();
        assert_eq!(store.burn("req-1", "nonce-1").unwrap(), BurnOutcome::Burned);
        assert!(store.burn("req-1", "nonce-1").unwrap() == BurnOutcome::AlreadyBurned);
        assert!(store.burn("missing", "nonce-1").is_err());
        assert_eq!(store.burn_count().unwrap(), 1);
        drop(store);
        let mut reopened = ApprovalStore::open(&path).unwrap();
        assert_eq!(
            reopened.burn("req-1", "nonce-1").unwrap(),
            BurnOutcome::AlreadyBurned
        );
        assert_eq!(reopened.burn_count().unwrap(), 1);
    }

    #[test]
    fn concurrent_burn_commits_once() {
        let (_dir, path) = temp_db();
        {
            let mut store = ApprovalStore::open(&path).unwrap();
            store
                .submit_request("req-race", "nonce-race", "restart-hotel")
                .unwrap();
        }
        let barrier = Arc::new(Barrier::new(2));
        let mut joins = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            joins.push(thread::spawn(move || {
                let mut store = ApprovalStore::open(&path).unwrap();
                barrier.wait();
                store.burn("req-race", "nonce-race").unwrap()
            }));
        }
        let results: Vec<_> = joins.into_iter().map(|join| join.join().unwrap()).collect();
        assert_eq!(
            results
                .iter()
                .filter(|outcome| **outcome == BurnOutcome::Burned)
                .count(),
            1,
            "{results:?}"
        );
        assert_eq!(
            results
                .iter()
                .filter(|outcome| **outcome == BurnOutcome::AlreadyBurned)
                .count(),
            1,
            "{results:?}"
        );
        let mut store = ApprovalStore::open(&path).unwrap();
        assert_eq!(store.burn_count().unwrap(), 1);
    }

    #[tokio::test]
    async fn peer_uid_denial_does_not_burn() {
        let (_dir, path) = temp_db();
        let mut setup = ApprovalStore::open(&path).unwrap();
        setup
            .submit_request("req-deny", "nonce-deny", "restart-hotel")
            .unwrap();
        drop(setup);
        let store = ApprovalStore::open(&path).unwrap();
        let broker_store = Arc::new(Mutex::new(store));
        let (mut client, server) = UnixStream::pair().unwrap();
        let uid = peer_uid(&server).unwrap().unwrap();
        let foreign = if uid == 1 { 2 } else { 1 };
        assert_ne!(uid, foreign);
        let policy = BrokerPolicy::new(foreign).unwrap();
        let task = tokio::spawn(handle_connection(server, policy, Arc::clone(&broker_store)));
        let body = serde_json::to_vec(&json!({
            "operation": "burn",
            "request_id": "req-deny",
            "nonce": "nonce-deny"
        }))
        .unwrap();
        write_frame(&mut client, &body).await.unwrap();
        let response = read_frame(&mut client).await.unwrap();
        task.await.unwrap();
        let parsed: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["error"], "approval_unavailable");
        let mut check = ApprovalStore::open(&path).unwrap();
        assert_eq!(check.burn_count().unwrap(), 0);
        assert_eq!(
            check.burn("req-deny", "nonce-deny").unwrap(),
            BurnOutcome::Burned
        );
    }

    #[tokio::test]
    async fn allowed_peer_can_store_and_burn_once() {
        let (_dir, path) = temp_db();
        let store = ApprovalStore::open(&path).unwrap();
        let shared = Arc::new(Mutex::new(store));
        let (probe, _peer) = UnixStream::pair().unwrap();
        let uid = peer_uid(&probe).unwrap().unwrap();

        async fn roundtrip(
            shared: &Arc<Mutex<ApprovalStore>>,
            uid: u32,
            body: serde_json::Value,
        ) -> serde_json::Value {
            let (mut client, server) = UnixStream::pair().unwrap();
            let policy = BrokerPolicy::new(uid).unwrap();
            let store = Arc::clone(shared);
            let task = tokio::spawn(handle_connection(server, policy, store));
            let bytes = serde_json::to_vec(&body).unwrap();
            write_frame(&mut client, &bytes).await.unwrap();
            let response = read_frame(&mut client).await.unwrap();
            task.await.unwrap();
            serde_json::from_slice(&response).unwrap()
        }

        let stored = roundtrip(
            &shared,
            uid,
            json!({"operation": "put_policy", "action": "restart-hotel", "tier": 2}),
        )
        .await;
        assert_eq!(stored["ok"], true);
        assert_eq!(stored["tier"], 2);
        let pinned = roundtrip(
            &shared,
            uid,
            json!({
                "operation": "pin_key",
                "device_key_id": "device-1",
                "public_key_b64": "AQIDBA==",
                "owner": "jared"
            }),
        )
        .await;
        assert_eq!(pinned["result"], "pinned");
        let forged = roundtrip(
            &shared,
            uid,
            json!({"operation": "burn", "request_id": "req-1", "nonce": "n-1", "signature": "nope"}),
        )
        .await;
        assert_eq!(forged["ok"], false);
        let submitted = roundtrip(
            &shared,
            uid,
            json!({
                "operation": "submit_request",
                "request_id": "req-1",
                "nonce": "n-1",
                "action": "restart-hotel"
            }),
        )
        .await;
        assert_eq!(submitted["result"], "accepted");
        let burned = roundtrip(
            &shared,
            uid,
            json!({"operation": "burn", "request_id": "req-1", "nonce": "n-1"}),
        )
        .await;
        assert_eq!(burned["result"], "burned");
        let again = roundtrip(
            &shared,
            uid,
            json!({"operation": "burn", "request_id": "req-1", "nonce": "n-1"}),
        )
        .await;
        assert_eq!(again["result"], "already_burned");
    }

    #[tokio::test]
    async fn production_listener_path_is_fixed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broker.sock");
        let listener = UnixListener::bind(&path).unwrap();
        assert!(validate_production_listener(&listener).is_err());
    }

    #[test]
    fn activation_without_listen_fds_fails_closed() {
        let previous_fds = std::env::var_os("LISTEN_FDS");
        let previous_pid = std::env::var_os("LISTEN_PID");
        unsafe {
            std::env::remove_var("LISTEN_FDS");
            std::env::remove_var("LISTEN_PID");
        }
        assert!(listener_from_activation().is_err());
        unsafe {
            match previous_fds {
                Some(value) => std::env::set_var("LISTEN_FDS", value),
                None => std::env::remove_var("LISTEN_FDS"),
            }
            match previous_pid {
                Some(value) => std::env::set_var("LISTEN_PID", value),
                None => std::env::remove_var("LISTEN_PID"),
            }
        }
    }

    #[tokio::test]
    async fn idle_allowed_peer_fails_closed() {
        let (_dir, path) = temp_db();
        let store = Arc::new(Mutex::new(ApprovalStore::open(&path).unwrap()));
        let (mut client, server) = UnixStream::pair().unwrap();
        let uid = peer_uid(&server).unwrap().unwrap();
        let policy = BrokerPolicy::new(uid).unwrap();
        let started = std::time::Instant::now();
        let task = tokio::spawn(handle_connection_within(
            server,
            policy,
            store,
            Duration::from_millis(50),
        ));
        let response = read_frame(&mut client).await.unwrap();
        task.await.unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        let parsed: serde_json::Value = serde_json::from_slice(&response).unwrap();
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["error"], "approval_unavailable");
    }
}
