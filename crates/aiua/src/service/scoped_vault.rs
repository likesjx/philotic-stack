//! Hotel-owned credential listener. Deliberately independent of general IPC.
//! Not wired into the installed hotel. See the adjacent integration patch.
// The endpoint is intentionally unavailable outside Linux; keep its processing
// helpers compilable there for synthetic protocol/deadline tests.
#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]
use serde::{Deserialize, Serialize};
use std::{future::Future, io, path::Path, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{OwnedSemaphorePermit, Semaphore},
    time::{Instant, timeout_at},
};
use zeroize::Zeroizing;

pub const ROLE: &str = "percival-personal-recall";
pub const GUEST: &str = "percival-personal-gateway";
const INTROSPECTION_PREFIX: &str = "secret://hotel/default/percival-issuer-introspection/";
const PREFIX: &str = "secret://hotel/default/percival-muninn-observe/";
const MAX_REQUEST: usize = 4096;
const WORKERS: usize = 4;
const DEADLINE: Duration = Duration::from_secs(3);
const CLEANUP: Duration = Duration::from_millis(100);

// Shared across listener generations: timed-out synchronous jobs must not
// accumulate if a caller drops and recreates the endpoint in one hotel process.
fn worker_slots() -> Arc<Semaphore> {
    static SLOTS: std::sync::OnceLock<Arc<Semaphore>> = std::sync::OnceLock::new();
    SLOTS
        .get_or_init(|| Arc::new(Semaphore::new(WORKERS)))
        .clone()
}

#[derive(Clone)]
pub struct Policy {
    caller_uid: u32,
    hotel_uid: u32,
    secret_ref: String,
    introspection_secret_ref: Option<String>,
}
impl Policy {
    pub fn new(caller_uid: u32, hotel_uid: u32, secret_ref: String) -> io::Result<Self> {
        let suffix = secret_ref.strip_prefix(PREFIX).unwrap_or("");
        if caller_uid == 0
            || hotel_uid == 0
            || caller_uid == hotel_uid
            || suffix.is_empty()
            || suffix.len() > 128
            || !suffix
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err(unavailable());
        }
        Ok(Self {
            caller_uid,
            hotel_uid,
            secret_ref,
            introspection_secret_ref: None,
        })
    }
    pub fn with_introspection(mut self, reference: String) -> io::Result<Self> {
        let suffix = reference.strip_prefix(INTROSPECTION_PREFIX).unwrap_or("");
        if suffix.is_empty()
            || suffix.len() > 128
            || !suffix
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err(unavailable());
        }
        self.introspection_secret_ref = Some(reference);
        Ok(self)
    }
    fn for_request(&self, bytes: &[u8]) -> io::Result<(Self, bool)> {
        let request: Request = serde_json::from_slice(bytes).map_err(|_| unavailable())?;
        let mut selected = self.clone();
        match request.operation.as_str() {
            "get_percival_credential" => Ok((selected, false)),
            "get_percival_introspection_credential" => {
                selected.secret_ref = self
                    .introspection_secret_ref
                    .clone()
                    .ok_or_else(unavailable)?;
                Ok((selected, true))
            }
            _ => Err(unavailable()),
        }
    }
    pub fn secret_ref(&self) -> &str {
        &self.secret_ref
    }
    pub fn caller_uid(&self) -> u32 {
        self.caller_uid
    }
    pub fn hotel_uid(&self) -> u32 {
        self.hotel_uid
    }
}

pub const POLICY_PATH: &str = "/etc/percival-personal-mcp/scoped-vault-policy.json";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyDocument {
    caller_uid: u32,
    hotel_uid: u32,
    secret_ref: String,
    #[serde(default)]
    introspection_secret_ref: Option<String>,
}
fn parse_policy(bytes: &[u8]) -> io::Result<Policy> {
    if bytes.len() > 4096 {
        return Err(unavailable());
    }
    let document: PolicyDocument = serde_json::from_slice(bytes).map_err(|_| unavailable())?;
    let policy = Policy::new(document.caller_uid, document.hotel_uid, document.secret_ref)?;
    match document.introspection_secret_ref {
        Some(reference) => policy.with_introspection(reference),
        None => Ok(policy),
    }
}

/// Load only a nonsecret reference/UID policy. All ancestors and the opened
/// regular file must be root controlled. Verify descriptor identity to detect
/// replacement between metadata validation and open. No credential is loaded.
pub fn load_policy(path: &Path) -> io::Result<Policy> {
    use std::{io::Read, os::unix::fs::MetadataExt};
    if path != Path::new(POLICY_PATH) {
        return Err(unavailable());
    }
    trusted_policy_path(path).map_err(|_| unavailable())?;
    let before = std::fs::symlink_metadata(path).map_err(|_| unavailable())?;
    let file = std::fs::File::open(path).map_err(|_| unavailable())?;
    let opened = file.metadata().map_err(|_| unavailable())?;
    if !opened.is_file()
        || opened.uid() != 0
        || opened.mode() & 0o022 != 0
        || opened.dev() != before.dev()
        || opened.ino() != before.ino()
        || opened.len() > 4096
    {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    file.take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    let after = std::fs::symlink_metadata(path).map_err(|_| unavailable())?;
    if !after.is_file()
        || after.dev() != opened.dev()
        || after.ino() != opened.ino()
        || after.uid() != 0
        || after.mode() & 0o022 != 0
    {
        return Err(unavailable());
    }
    parse_policy(&bytes)
}

/// Optional production entry point: root-controlled loading is actually invoked.
/// Bootstrap still must obtain the correctly prebound supervisor listener.
pub async fn serve_from_policy(
    listener: UnixListener,
    path: &Path,
    resolver: Arc<dyn Resolver>,
) -> io::Result<()> {
    let policy = load_policy(path)?;
    serve(listener, policy, resolver).await
}

// Resolver is supplied by the hotel. No request field can influence its authority.
// The adapter checks mandatory exact role/guest ACLs before calling resolve_secret.
pub trait Resolver: Send + Sync + 'static {
    fn resolve(&self, policy: &Policy) -> io::Result<Zeroizing<String>>;
}
fn unavailable() -> io::Error {
    io::Error::other("credential_unavailable")
}
fn authorized(policy: &Policy, peer: Option<u32>) -> bool {
    peer == Some(policy.caller_uid)
}
fn peer_uid(stream: &UnixStream) -> io::Result<Option<u32>> {
    #[cfg(target_os = "linux")]
    {
        Ok(Some(stream.peer_cred()?.uid()))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stream;
        Err(unavailable())
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    operation: String,
}
#[cfg(test)]
fn valid_request(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Request>(bytes).is_ok_and(|r| r.operation == "get_percival_credential")
}
fn valid_introspection_credential(secret: &str) -> bool {
    !secret.starts_with("mk_")
        && (43..=128).contains(&secret.len())
        && secret
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}
fn valid_credential(secret: &str) -> bool {
    secret.len() <= 4096
        && secret.strip_prefix("mk_").is_some_and(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        })
}
async fn frame(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let size = stream.read_u32().await? as usize;
    if size == 0 || size > MAX_REQUEST {
        return Err(unavailable());
    }
    let mut payload = vec![0; size];
    stream.read_exact(&mut payload).await?;
    // One frame per connection. Reject already-buffered additional input.
    let mut extra = [0];
    match stream.try_read(&mut extra) {
        Ok(0) => {}
        Ok(_) => return Err(unavailable()),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
        Err(_) => return Err(unavailable()),
    }
    Ok(payload)
}
#[derive(Serialize)]
struct Reply<'a> {
    credential: &'a str,
}
async fn send(stream: &mut UnixStream, bytes: &[u8]) -> io::Result<()> {
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(bytes).await?;
    stream.shutdown().await
}
async fn within<T>(deadline: Instant, work: impl Future<Output = io::Result<T>>) -> io::Result<T> {
    timeout_at(deadline, work)
        .await
        .map_err(|_| unavailable())?
}
async fn handle(
    stream: UnixStream,
    policy: Policy,
    resolver: Arc<dyn Resolver>,
    permit: Arc<OwnedSemaphorePermit>,
    budget: Duration,
) {
    let peer = peer_uid(&stream).ok().flatten();
    handle_peer(stream, policy, resolver, permit, budget, peer).await;
}
// Private processing function lets non-Linux tests exercise the frame/deadline
// path. Production callers always pass kernel peer credentials through handle.
async fn handle_peer(
    mut stream: UnixStream,
    policy: Policy,
    resolver: Arc<dyn Resolver>,
    permit: Arc<OwnedSemaphorePermit>,
    budget: Duration,
    peer: Option<u32>,
) {
    let deadline = Instant::now() + budget;
    let result = within(deadline, async {
        // Authentication precedes all parsing/resolution. Unsupported platforms fail closed.
        if !authorized(&policy, peer) {
            return Err(unavailable());
        }
        let request = frame(&mut stream).await?;
        let (policy, introspection) = policy.for_request(&request)?;
        // A timed-out blocking resolver retains its worker permit until it finishes.
        // An uncooperative backend cannot create unbounded background jobs.
        let worker_permit = permit.clone();
        let secret = tokio::task::spawn_blocking(move || {
            let _held = worker_permit;
            resolver.resolve(&policy)
        })
        .await
        .map_err(|_| unavailable())??;
        if !(if introspection {
            valid_introspection_credential(&secret)
        } else {
            valid_credential(&secret)
        }) {
            return Err(unavailable());
        }
        let reply = Zeroizing::new(
            serde_json::to_vec(&Reply {
                credential: secret.as_str(),
            })
            .map_err(|_| unavailable())?,
        );
        send(&mut stream, &reply).await
    })
    .await;
    if result.is_err() {
        let _ = within(
            Instant::now() + CLEANUP,
            send(&mut stream, br#"{"error":"credential_unavailable"}"#),
        )
        .await;
    }
}

/// Check endpoint identity and filesystem policy before hotel materialization.
pub fn validate_listener(listener: &UnixListener, policy: &Policy) -> io::Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (listener, policy);
        Err(unavailable())
    }
    #[cfg(target_os = "linux")]
    {
        // Kernel credentials of a socketpair establish our real effective UID;
        // configured identity cannot silently substitute a root/shared process.
        let (self_peer, _other) = UnixStream::pair()?;
        if peer_uid(&self_peer)? != Some(policy.hotel_uid) {
            return Err(unavailable());
        }
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let address = listener.local_addr()?;
        let path = address.as_pathname().ok_or_else(unavailable)?;
        if path != Path::new("/run/percival-personal-vault-broker.sock") {
            return Err(unavailable());
        }
        let metadata = std::fs::symlink_metadata(path)?;
        let parent = std::fs::symlink_metadata(path.parent().ok_or_else(unavailable)?)?;
        if !metadata.file_type().is_socket()
            || metadata.uid() != policy.caller_uid
            || metadata.mode() & 0o777 != 0o600
            || parent.uid() != 0
            || !parent.is_dir()
            || parent.mode() & 0o022 != 0
        {
            return Err(unavailable());
        }
        Ok(())
    }
}

/// Listener must be prebound by the hotel to a root-controlled path with mode0600
/// and gateway ownership. This API never removes/binds/chmods a caller path.
/// Production Linux kernel peer credentials are mandatory.
pub async fn serve(
    listener: UnixListener,
    policy: Policy,
    resolver: Arc<dyn Resolver>,
) -> io::Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (listener, policy, resolver);
        Err(unavailable())
    }
    #[cfg(target_os = "linux")]
    {
        validate_listener(&listener, &policy)?;
        let slots = worker_slots();
        // JoinSet owns handlers. Dropping this listener future aborts them and
        // closes their sockets, so detached blocking work cannot deliver a key
        // after endpoint disablement. Its retained permit still bounds work.
        let mut handlers = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (stream, _) = accepted?;
                    // Bound completed JoinSet entries as well as active workers.
                    if handlers.len() >= WORKERS { drop(stream); continue; }
                    let Ok(permit) = slots.clone().try_acquire_owned() else {
                        drop(stream); continue;
                    };
                    handlers.spawn(handle(stream, policy.clone(), resolver.clone(),
                        Arc::new(permit), DEADLINE));
                }
                _ = handlers.join_next(), if !handlers.is_empty() => {}
            }
        }
    }
}

/// Assert root-controlled reference/config ancestry before operator-owned startup
/// loads nonsecret policy. Does not read credential contents.
pub fn trusted_policy_path(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    if !path.is_absolute() {
        return Err(unavailable());
    }
    for (i, ancestor) in path.ancestors().enumerate() {
        let metadata = std::fs::symlink_metadata(ancestor)?;
        if metadata.uid() != 0
            || metadata.mode() & 0o022 != 0
            || (i == 0 && !metadata.is_file())
            || (i > 0 && !metadata.is_dir())
        {
            return Err(unavailable());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy() -> Policy {
        Policy::new(1234, 999, format!("{PREFIX}synthetic")).unwrap()
    }
    #[tokio::test]
    async fn worker_bound_survives_listener_generation_replacement() {
        let first = worker_slots();
        let held = first.clone().acquire_owned().await.unwrap();
        drop(first);
        let replacement = worker_slots();
        assert_eq!(replacement.available_permits(), WORKERS - 1);
        drop(held);
        assert_eq!(replacement.available_permits(), WORKERS);
    }
    #[test]
    fn bounded_nonsecret_policy_parser_rejects_overrides_and_unsafe_identities() {
        let reference = format!("{PREFIX}synthetic");
        let raw = serde_json::to_vec(
            &serde_json::json!({"caller_uid":1234,"hotel_uid":999,"secret_ref":reference}),
        )
        .unwrap();
        let parsed = parse_policy(&raw).unwrap();
        assert_eq!(parsed.caller_uid(), 1234);
        assert_eq!(parsed.hotel_uid(), 999);
        for bytes in [
            br#"{"caller_uid":1234,"hotel_uid":999,"secret_ref":"other"}"#.as_slice(),
            br#"{"caller_uid":0,"hotel_uid":999,"secret_ref":"other"}"#,
            br#"{"caller_uid":1234,"hotel_uid":999,"role":"hotel.internal"}"#,
            br#"{"caller_uid":1234,"caller_uid":1235}"#,
            br#"[]"#,
        ] {
            assert!(parse_policy(bytes).is_err());
        }
        assert!(parse_policy(&vec![b' '; 4097]).is_err());
        assert!(load_policy(Path::new("/tmp/caller-selected-policy")).is_err());
    }
    #[test]
    fn fixed_policy_rejects_root_shared_uid_and_wrong_ref() {
        for (caller, hotel, reference) in [
            (0, 999, format!("{PREFIX}synthetic")),
            (1234, 0, format!("{PREFIX}synthetic")),
            (999, 999, format!("{PREFIX}synthetic")),
            (1234, 999, "secret://hotel/default/other/synthetic".into()),
            (1234, 999, format!("{PREFIX}../other")),
        ] {
            assert!(Policy::new(caller, hotel, reference).is_err());
        }
    }
    #[test]
    fn missing_wrong_root_and_hotel_peer_denied() {
        for uid in [None, Some(0), Some(999), Some(1235)] {
            assert!(!authorized(&policy(), uid));
        }
        assert!(authorized(&policy(), Some(1234)));
    }
    #[test]
    fn caller_cannot_select_identity_reference_or_dispatch() {
        assert!(valid_request(br#"{"operation":"get_percival_credential"}"#));
        for bytes in [
            br#"{"operation":"get_percival_credential","role":"hotel.internal"}"#.as_slice(),
            br#"{"operation":"GetSecret","secret_ref":"other"}"#,
            br#"{"Register":{"role":"hotel.internal"}}"#,
            br#"{"operation":"get_percival_credential","uid":1234}"#,
            br#"{"operation":"get_percival_credential","operation":"GetSecret"}"#,
            br#"[]"#,
            br#"not JSON"#,
        ] {
            assert!(!valid_request(bytes));
        }
    }
    #[tokio::test]
    async fn oversized_truncated_and_slow_frames_are_bounded() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        a.write_u32(4097).await.unwrap();
        assert!(frame(&mut b).await.is_err());
        let (mut a, mut b) = UnixStream::pair().unwrap();
        a.write_u32(4).await.unwrap();
        a.write_all(b"{").await.unwrap();
        a.shutdown().await.unwrap();
        assert!(frame(&mut b).await.is_err());
        let (_a, mut b) = UnixStream::pair().unwrap();
        let start = Instant::now();
        assert!(
            within(start + Duration::from_millis(30), frame(&mut b))
                .await
                .is_err()
        );
        assert!(start.elapsed() < Duration::from_millis(500));
    }
    #[tokio::test]
    async fn resolver_permits_survive_timeout_and_bound_work() {
        let slots = Arc::new(Semaphore::new(WORKERS));
        let mut held = Vec::new();
        for _ in 0..WORKERS {
            held.push(Arc::new(slots.clone().try_acquire_owned().unwrap()));
        }
        assert!(slots.clone().try_acquire_owned().is_err());
        let worker = held.pop().unwrap();
        let retained = worker.clone();
        drop(worker);
        assert!(slots.clone().try_acquire_owned().is_err());
        drop(retained);
        assert!(slots.clone().try_acquire_owned().is_ok());
    }
    #[tokio::test]
    async fn introspection_roundtrip_uses_only_second_fixed_ref_after_uid_auth() {
        struct Second;
        impl Resolver for Second {
            fn resolve(&self, p: &Policy) -> io::Result<Zeroizing<String>> {
                assert_eq!(p.secret_ref(), format!("{INTROSPECTION_PREFIX}synthetic"));
                Ok(Zeroizing::new("i".repeat(43)))
            }
        }
        let p = policy()
            .with_introspection(format!("{INTROSPECTION_PREFIX}synthetic"))
            .unwrap();
        assert!(
            policy()
                .for_request(br#"{"operation":"get_percival_introspection_credential"}"#)
                .is_err()
        );
        assert!(
            p.for_request(
                br#"{"operation":"get_percival_introspection_credential","secret_ref":"other"}"#
            )
            .is_err()
        );
        assert!(
            policy()
                .with_introspection(format!("{PREFIX}synthetic"))
                .is_err()
        );
        for uid in [1234, 999, 0] {
            let (mut client, server) = UnixStream::pair().unwrap();
            let permit = Arc::new(Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap());
            let task = tokio::spawn(handle_peer(
                server,
                p.clone(),
                Arc::new(Second),
                permit,
                Duration::from_millis(100),
                Some(uid),
            ));
            send(
                &mut client,
                br#"{"operation":"get_percival_introspection_credential"}"#,
            )
            .await
            .unwrap();
            let length = client.read_u32().await.unwrap();
            let mut bytes = vec![0; length as usize];
            client.read_exact(&mut bytes).await.unwrap();
            task.await.unwrap();
            let reply: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            if uid == 1234 {
                assert_eq!(reply["credential"], "i".repeat(43));
            } else {
                assert_eq!(reply["error"], "credential_unavailable");
            }
        }
    }

    #[test]
    fn credential_shape_and_untrusted_policy_path_fail_closed() {
        assert!(valid_credential("mk_synthetic"));
        assert!(!valid_introspection_credential(&format!(
            "mk_{}",
            "a".repeat(43)
        )));
        for value in ["", "not-a-key", "mk_", "mk_x\n"] {
            assert!(!valid_credential(value));
        }
        assert!(trusted_policy_path(Path::new("relative")).is_err());
        assert!(trusted_policy_path(Path::new("/tmp/not-present-percival-policy")).is_err());
    }
    struct Fixture(std::sync::atomic::AtomicUsize);
    impl Resolver for Fixture {
        fn resolve(&self, p: &Policy) -> io::Result<Zeroizing<String>> {
            assert_eq!(p.secret_ref(), format!("{PREFIX}synthetic"));
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Zeroizing::new("mk_synthetic".into()))
        }
    }
    async fn exercise(
        bytes: &[u8],
        peer: Option<u32>,
        resolver: Arc<Fixture>,
    ) -> serde_json::Value {
        let (mut client, server) = UnixStream::pair().unwrap();
        let permit = Arc::new(Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap());
        let task = tokio::spawn(handle_peer(
            server,
            policy(),
            resolver,
            permit,
            Duration::from_millis(100),
            peer,
        ));
        send(&mut client, bytes).await.unwrap();
        let size = client.read_u32().await.unwrap();
        let mut reply = vec![0; size as usize];
        client.read_exact(&mut reply).await.unwrap();
        task.await.unwrap();
        serde_json::from_slice(&reply).unwrap()
    }
    #[tokio::test]
    async fn complete_processing_resolves_only_fixed_ref_after_authentication() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let resolver = Arc::new(Fixture(AtomicUsize::new(0)));
        let request = br#"{"operation":"get_percival_credential"}"#;
        assert_eq!(
            exercise(request, Some(1234), resolver.clone()).await["credential"],
            "mk_synthetic"
        );
        for uid in [None, Some(0), Some(999), Some(1235)] {
            assert_eq!(
                exercise(request, uid, resolver.clone()).await["error"],
                "credential_unavailable"
            );
        }
        for request in [
            br#"{"operation":"get_percival_credential","secret_ref":"other"}"#.as_slice(),
            br#"{"operation":"get_percival_credential","role":"hotel.internal"}"#,
            br#"{"operation":"Register"}"#,
        ] {
            assert_eq!(
                exercise(request, Some(1234), resolver.clone()).await["error"],
                "credential_unavailable"
            );
        }
        assert_eq!(resolver.0.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn slow_input_times_out_without_blocking_overlapping_request() {
        use std::sync::atomic::AtomicUsize;
        let (mut slow, server) = UnixStream::pair().unwrap();
        let resolver = Arc::new(Fixture(AtomicUsize::new(0)));
        let slots = Arc::new(Semaphore::new(WORKERS));
        let permit = Arc::new(slots.acquire_owned().await.unwrap());
        let task = tokio::spawn(handle_peer(
            server,
            policy(),
            resolver.clone(),
            permit,
            Duration::from_millis(40),
            Some(1234),
        ));
        slow.write_u32(100).await.unwrap();
        slow.write_all(b"{").await.unwrap();
        assert_eq!(
            exercise(
                br#"{"operation":"get_percival_credential"}"#,
                Some(1234),
                resolver
            )
            .await["credential"],
            "mk_synthetic"
        );
        let start = Instant::now();
        let size = slow.read_u32().await.unwrap();
        let mut reply = vec![0; size as usize];
        slow.read_exact(&mut reply).await.unwrap();
        task.await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&reply).unwrap()["error"],
            "credential_unavailable"
        );
        assert!(start.elapsed() < Duration::from_millis(500));
    }
    #[tokio::test]
    async fn timed_out_blocking_resolver_retains_worker_until_completion() {
        struct Slow {
            started: Arc<tokio::sync::Notify>,
            release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl Resolver for Slow {
            fn resolve(&self, _: &Policy) -> io::Result<Zeroizing<String>> {
                self.started.notify_one();
                // Sender drop also releases this job if the test fails early.
                let _ = self.release.lock().unwrap().recv();
                Ok(Zeroizing::new("mk_synthetic".into()))
            }
        }
        let (release, wait) = std::sync::mpsc::channel();
        let started = Arc::new(tokio::sync::Notify::new());
        let slots = Arc::new(Semaphore::new(1));
        let permit = Arc::new(slots.clone().acquire_owned().await.unwrap());
        let (mut client, server) = UnixStream::pair().unwrap();
        let task = tokio::spawn(handle_peer(
            server,
            policy(),
            Arc::new(Slow {
                started: started.clone(),
                release: std::sync::Mutex::new(wait),
            }),
            permit,
            Duration::from_secs(2),
            Some(1234),
        ));
        send(&mut client, br#"{"operation":"get_percival_credential"}"#)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), started.notified())
            .await
            .unwrap();
        let size = client.read_u32().await.unwrap();
        let mut bytes = vec![0; size as usize];
        client.read_exact(&mut bytes).await.unwrap();
        task.await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["error"],
            "credential_unavailable"
        );
        assert_eq!(slots.available_permits(), 0);
        release.send(()).unwrap();
        // Wait for the actual retained permit, not a scheduler-dependent sleep.
        let returned = tokio::time::timeout(Duration::from_secs(5), slots.acquire())
            .await
            .unwrap()
            .unwrap();
        drop(returned);
        assert_eq!(slots.available_permits(), 1);
    }
    #[tokio::test]
    async fn listener_owned_handlers_close_sockets_on_disable_without_late_secret_delivery() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Slow {
            started: Arc<AtomicBool>,
            release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl Resolver for Slow {
            fn resolve(&self, _: &Policy) -> io::Result<Zeroizing<String>> {
                self.started.store(true, Ordering::SeqCst);
                let _ = self.release.lock().unwrap().recv();
                Ok(Zeroizing::new("mk_synthetic".into()))
            }
        }
        let started = Arc::new(AtomicBool::new(false));
        let (release, wait) = std::sync::mpsc::channel();
        let slots = Arc::new(Semaphore::new(1));
        let permit = Arc::new(slots.clone().acquire_owned().await.unwrap());
        let (mut client, server) = UnixStream::pair().unwrap();
        let mut handlers = tokio::task::JoinSet::new();
        handlers.spawn(handle_peer(
            server,
            policy(),
            Arc::new(Slow {
                started: started.clone(),
                release: std::sync::Mutex::new(wait),
            }),
            permit,
            DEADLINE,
            Some(1234),
        ));
        send(&mut client, br#"{"operation":"get_percival_credential"}"#)
            .await
            .unwrap();
        within(Instant::now() + Duration::from_secs(5), async {
            while !started.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            Ok(())
        })
        .await
        .unwrap();
        drop(handlers); // Same ownership/drop behavior as production serve.
        let mut byte = [0];
        assert_eq!(
            within(
                Instant::now() + Duration::from_secs(5),
                client.read(&mut byte)
            )
            .await
            .unwrap(),
            0
        );
        assert_eq!(slots.available_permits(), 0);
        release.send(()).unwrap();
        let returned = tokio::time::timeout(Duration::from_secs(5), slots.acquire())
            .await
            .unwrap()
            .unwrap();
        drop(returned);
        assert_eq!(slots.available_permits(), 1);
    }
    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn non_linux_peer_auth_fails_closed() {
        let (_a, b) = UnixStream::pair().unwrap();
        assert!(peer_uid(&b).is_err());
    }
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn actual_kernel_uid_and_fixed_ref_roundtrip() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Fixture(AtomicUsize);
        impl Resolver for Fixture {
            fn resolve(&self, p: &Policy) -> io::Result<Zeroizing<String>> {
                assert_eq!(p.secret_ref(), format!("{PREFIX}synthetic"));
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(Zeroizing::new("mk_synthetic".into()))
            }
        }
        let (mut client, server) = UnixStream::pair().unwrap();
        let uid = peer_uid(&server).unwrap().unwrap();
        // Root is intentionally rejected; run this acceptance case under a nonroot UID.
        assert_ne!(uid, 0, "Linux acceptance must run as an unprivileged UID");
        let p = Policy::new(
            uid,
            if uid == 999 { 998 } else { 999 },
            format!("{PREFIX}synthetic"),
        )
        .unwrap();
        let resolver = Arc::new(Fixture(AtomicUsize::new(0)));
        let permit = Arc::new(Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap());
        let task = tokio::spawn(handle(
            server,
            p,
            resolver.clone(),
            permit,
            Duration::from_secs(1),
        ));
        send(&mut client, br#"{"operation":"get_percival_credential"}"#)
            .await
            .unwrap();
        let size = client.read_u32().await.unwrap();
        let mut bytes = vec![0; size as usize];
        client.read_exact(&mut bytes).await.unwrap();
        task.await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["credential"],
            "mk_synthetic"
        );
        assert_eq!(resolver.0.load(Ordering::SeqCst), 1);
    }
}
