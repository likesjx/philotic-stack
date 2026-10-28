//! Private, bounded request/reply transport. Never uses EmitTask or a mesh route.
//!
//! Tokens exist only in the ingress/executor processes and socket buffers.
//! Peer UID is the deployment trust boundary; other processes under that UID
//! are trusted. This is not process attestation. No socket is replaced/unlinked.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::{
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
};

pub const DEADLINE: Duration = Duration::from_secs(10);
const MAX_FRAME: usize = 4096;

/// Fail-fast concurrency and fixed-window request bound; no waiting queue,
/// token cache, token-derived keys or per-IP allocation. One shared instance
/// per resource, held for the whole operation including post-verification.
pub struct Admission {
    slots: std::sync::Arc<tokio::sync::Semaphore>,
    window: std::sync::Mutex<(std::time::Instant, usize)>,
    burst: usize,
}
impl Admission {
    pub fn new(concurrency: usize, burst: usize) -> Self {
        assert!(concurrency > 0 && burst > 0);
        Self {
            slots: std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency)),
            window: std::sync::Mutex::new((std::time::Instant::now(), 0)),
            burst,
        }
    }
    pub fn enter(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        let permit = self.slots.clone().try_acquire_owned().ok()?;
        let mut window = self.window.lock().ok()?;
        if window.0.elapsed() >= Duration::from_secs(1) {
            *window = (std::time::Instant::now(), 0);
        }
        if window.1 >= self.burst {
            return None;
        }
        window.1 += 1;
        Some(permit)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refusal {
    Refused,
    Unavailable,
    Timeout,
    AuditRejected,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claims {
    pub active: bool,
    #[serde(default)]
    pub iss: String,
    #[serde(default)]
    pub aud: String,
    #[serde(default)]
    pub sub: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub iat: u64,
    #[serde(default)]
    pub exp: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    token: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Ok { claims: Claims },
    Error { reason: Refusal },
}

/// Implemented inside the actual egress guest. The implementation must complete
/// its content-free audit before returning claims. It must not persist token.
#[async_trait]
pub trait Authority: Send {
    async fn introspect(&mut self, token: &str) -> Result<Claims, Refusal>;
    /// Called after cancellation/deadline, before this authority may be reused.
    fn abandon(&mut self);
}

pub struct PrivateListener {
    listener: UnixListener,
    peer_uid: u32,
    _path: PathBuf,
}

fn private_parent(path: &Path, owner_uid: u32) -> Result<(), Refusal> {
    let parent = path
        .parent()
        .filter(|p| p.is_absolute())
        .ok_or(Refusal::Refused)?;
    let real = std::fs::canonicalize(parent).map_err(|_| Refusal::Refused)?;
    // No symlinked path components. Ancestor writers cannot substitute the
    // private directory without changing the path/peer authorization boundary.
    if real != parent {
        return Err(Refusal::Refused);
    }
    let m = std::fs::symlink_metadata(parent).map_err(|_| Refusal::Refused)?;
    if !m.is_dir() || m.uid() != owner_uid || m.mode() & 0o777 != 0o700 {
        return Err(Refusal::Refused);
    }
    // Refuse writable ancestors other than root-owned sticky directories.
    for ancestor in parent.ancestors().skip(1) {
        let m = std::fs::symlink_metadata(ancestor).map_err(|_| Refusal::Refused)?;
        if (m.uid() != 0 && m.uid() != owner_uid)
            || (m.mode() & 0o022 != 0 && !(m.uid() == 0 && m.mode() & 0o1000 != 0))
        {
            return Err(Refusal::Refused);
        }
    }
    Ok(())
}

impl PrivateListener {
    pub fn bind(path: &Path, server_uid: u32, peer_uid: u32) -> Result<Self, Refusal> {
        private_parent(path, server_uid)?;
        // bind refuses an existing path, including a stale socket or symlink.
        let listener = UnixListener::bind(path).map_err(|_| Refusal::Refused)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| Refusal::Refused)?;
        if std::fs::symlink_metadata(path)
            .map_err(|_| Refusal::Refused)?
            .uid()
            != server_uid
        {
            return Err(Refusal::Refused);
        }
        Ok(Self {
            listener,
            peer_uid,
            _path: path.to_owned(),
        })
    }
    pub async fn accept(&self) -> Result<UnixStream, Refusal> {
        let (stream, _) = self
            .listener
            .accept()
            .await
            .map_err(|_| Refusal::Unavailable)?;
        if stream.peer_cred().map_err(|_| Refusal::Refused)?.uid() != self.peer_uid {
            return Err(Refusal::Refused);
        }
        Ok(stream)
    }
}

async fn read_frame(stream: &mut UnixStream) -> Result<Vec<u8>, Refusal> {
    let length = stream.read_u32().await.map_err(|_| Refusal::Refused)? as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(Refusal::Refused);
    }
    let mut bytes = vec![0; length];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|_| Refusal::Refused)?;
    Ok(bytes)
}
async fn write_frame(stream: &mut UnixStream, bytes: &[u8]) -> Result<(), Refusal> {
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(Refusal::Refused);
    }
    stream
        .write_u32(bytes.len() as u32)
        .await
        .map_err(|_| Refusal::Unavailable)?;
    stream
        .write_all(bytes)
        .await
        .map_err(|_| Refusal::Unavailable)
}
fn token_valid(token: &str) -> bool {
    !token.is_empty() && token.len() <= 2048 && token.bytes().all(|b| b.is_ascii_graphic())
}

pub async fn serve(stream: &mut UnixStream, authority: &mut dyn Authority) -> Result<(), Refusal> {
    serve_with_deadline(stream, authority, DEADLINE).await
}
async fn serve_with_deadline(
    stream: &mut UnixStream,
    authority: &mut dyn Authority,
    duration: Duration,
) -> Result<(), Refusal> {
    let outcome = tokio::time::timeout(duration, async {
        let request: Request =
            serde_json::from_slice(&read_frame(stream).await?).map_err(|_| Refusal::Refused)?;
        if !token_valid(&request.token) {
            return Err(Refusal::Refused);
        }
        let reply = match authority.introspect(&request.token).await {
            Ok(claims) => Reply::Ok { claims },
            Err(reason) => Reply::Error { reason },
        };
        // No request reflection, raw HTTP response, exception strings or audit
        // internals enter the response frame.
        write_frame(
            stream,
            &serde_json::to_vec(&reply).map_err(|_| Refusal::Refused)?,
        )
        .await
    })
    .await;
    match outcome {
        Ok(result) => result,
        Err(_) => {
            authority.abandon();
            Err(Refusal::Timeout)
        }
    }
}

/// One connection per check. Timeout/cancellation drops the connection and all
/// late replies; no shared client can correlate them with a subsequent token.
pub async fn introspect_at(path: &Path, server_uid: u32, token: &str) -> Result<Claims, Refusal> {
    if !token_valid(token) {
        return Err(Refusal::Refused);
    }
    private_parent(path, server_uid)?;
    tokio::time::timeout(DEADLINE, async {
        let mut stream = UnixStream::connect(path)
            .await
            .map_err(|_| Refusal::Unavailable)?;
        if stream.peer_cred().map_err(|_| Refusal::Refused)?.uid() != server_uid {
            return Err(Refusal::Refused);
        }
        let bytes = serde_json::to_vec(&Request {
            token: token.into(),
        })
        .map_err(|_| Refusal::Refused)?;
        write_frame(&mut stream, &bytes).await?;
        match serde_json::from_slice(&read_frame(&mut stream).await?)
            .map_err(|_| Refusal::Refused)?
        {
            Reply::Ok { claims } => Ok(claims),
            Reply::Error { reason } => Err(reason),
        }
    })
    .await
    .map_err(|_| Refusal::Timeout)?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admission_bounds_concurrency_and_rate_without_queue_or_token_state() {
        let gate = Admission::new(1, 2);
        let held = gate.enter().unwrap();
        assert!(gate.enter().is_none());
        drop(held);
        drop(gate.enter().unwrap());
        for _ in 0..100 {
            assert!(gate.enter().is_none());
        }
        *gate.window.lock().unwrap() = (std::time::Instant::now() - Duration::from_secs(2), 2);
        assert!(gate.enter().is_some());
    }

    #[tokio::test]
    async fn refused_audit_and_timeout_do_not_damage_independent_transport() {
        let (mut ordinary, mut hotel) = UnixStream::pair().unwrap();
        let mut a = authority();
        a.refusal = Some(Refusal::AuditRejected);
        assert!(exchange(br#"{"token":"synthetic"}"#, &mut a).await.is_ok());
        let (mut slow, mut server) = UnixStream::pair().unwrap();
        slow.write_all(&[0]).await.unwrap();
        assert_eq!(
            serve_with_deadline(&mut server, &mut a, Duration::from_millis(5)).await,
            Err(Refusal::Timeout)
        );
        ordinary
            .write_all(b"ordinary-still-connected")
            .await
            .unwrap();
        let mut result = [0; 24];
        hotel.read_exact(&mut result).await.unwrap();
        assert_eq!(&result, b"ordinary-still-connected");
    }
    struct Mock {
        calls: usize,
        abandon: bool,
        refusal: Option<Refusal>,
        hang: bool,
    }
    #[async_trait]
    impl Authority for Mock {
        async fn introspect(&mut self, _: &str) -> Result<Claims, Refusal> {
            self.calls += 1;
            if self.hang {
                std::future::pending::<()>().await;
            }
            if let Some(r) = self.refusal {
                return Err(r);
            }
            Ok(Claims {
                active: false,
                iss: String::new(),
                aud: String::new(),
                sub: String::new(),
                client_id: String::new(),
                scope: String::new(),
                iat: 0,
                exp: 0,
            })
        }
        fn abandon(&mut self) {
            self.abandon = true;
        }
    }
    fn authority() -> Mock {
        Mock {
            calls: 0,
            abandon: false,
            refusal: None,
            hang: false,
        }
    }
    async fn exchange(bytes: &[u8], mock: &mut Mock) -> Result<Vec<u8>, Refusal> {
        let (mut c, mut s) = UnixStream::pair().unwrap();
        let (server, client) = tokio::join!(serve(&mut s, mock), async {
            write_frame(&mut c, bytes).await?;
            // Server rejection has no response; shutdown arrives when pair drops.
            tokio::time::timeout(Duration::from_millis(20), read_frame(&mut c))
                .await
                .map_err(|_| Refusal::Refused)?
        });
        server?;
        client
    }
    #[tokio::test]
    async fn response_is_typed_and_never_reflects_token() {
        let mut a = authority();
        let result = exchange(br#"{"token":"SYNTHETIC_SECRET_ONLY"}"#, &mut a)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&result).contains("SYNTHETIC_SECRET_ONLY"));
        assert_eq!(a.calls, 1);
    }
    #[tokio::test]
    async fn unknown_fields_and_invalid_tokens_never_reach_authority() {
        for body in [
            br#"{"token":"synthetic","url":"https://evil.invalid"}"#.as_slice(),
            br#"{"token":""}"#,
            br#"{"token":"a\nb"}"#,
            br#"{"token":5}"#,
        ] {
            let mut a = authority();
            assert!(exchange(body, &mut a).await.is_err());
            assert_eq!(a.calls, 0);
        }
    }
    #[tokio::test]
    async fn audit_failure_is_a_bounded_refusal_not_claims() {
        let mut a = authority();
        a.refusal = Some(Refusal::AuditRejected);
        let out = exchange(br#"{"token":"synthetic"}"#, &mut a).await.unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            r#"{"status":"error","reason":"audit_rejected"}"#
        );
    }
    #[tokio::test]
    async fn deadline_abandons_authority_and_partial_frame() {
        let (mut c, mut s) = UnixStream::pair().unwrap();
        let mut a = authority();
        c.write_all(&[0, 0]).await.unwrap();
        assert_eq!(
            serve_with_deadline(&mut s, &mut a, Duration::from_millis(5)).await,
            Err(Refusal::Timeout)
        );
        assert!(a.abandon);
        assert_eq!(a.calls, 0);
    }
    #[tokio::test]
    async fn stalled_authority_is_abandoned() {
        let (mut c, mut s) = UnixStream::pair().unwrap();
        let mut a = authority();
        a.hang = true;
        write_frame(&mut c, br#"{"token":"synthetic"}"#)
            .await
            .unwrap();
        assert_eq!(
            serve_with_deadline(&mut s, &mut a, Duration::from_millis(5)).await,
            Err(Refusal::Timeout)
        );
        assert!(a.abandon);
        assert_eq!(a.calls, 1);
    }
    #[tokio::test]
    async fn oversize_frame_and_truncated_body_refuse() {
        for length in [0, 4097, u32::MAX] {
            let (mut c, mut s) = UnixStream::pair().unwrap();
            c.write_u32(length).await.unwrap();
            assert!(read_frame(&mut s).await.is_err());
        }
        let (mut c, mut s) = UnixStream::pair().unwrap();
        c.write_u32(10).await.unwrap();
        c.write_all(b"{}").await.unwrap();
        c.shutdown().await.unwrap();
        assert!(read_frame(&mut s).await.is_err());
    }
    #[tokio::test]
    async fn real_socket_checks_uid_mode_parent_and_existing_path() {
        let dir = std::fs::canonicalize("/tmp").unwrap().join(format!(
            "mt{}",
            &uuid::Uuid::new_v4().simple().to_string()[..8]
        ));
        std::fs::create_dir(&dir).unwrap();
        let uid = std::fs::metadata(&dir).unwrap().uid();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.join("verify.sock");
        let listener = PrivateListener::bind(&path, uid, uid).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert!(PrivateListener::bind(&path, uid, uid).is_err());
        let mut a = authority();
        let (client, server) = tokio::join!(introspect_at(&path, uid, "SYNTHETIC_TOKEN"), async {
            let mut stream = listener.accept().await.unwrap();
            serve(&mut stream, &mut a).await
        });
        assert!(client.is_ok());
        assert!(server.is_ok());
        let wrong = PrivateListener {
            listener: listener.listener,
            peer_uid: uid.wrapping_add(1),
            _path: path.clone(),
        };
        let (_, server) = tokio::join!(UnixStream::connect(&path), wrong.accept());
        assert!(server.is_err());
        drop(wrong);
        std::fs::remove_file(&path).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(PrivateListener::bind(&path, uid, uid).is_err());
        std::fs::remove_dir(dir).unwrap();
    }
}
