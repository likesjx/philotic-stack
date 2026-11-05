//! Hotel-owned native commit and release authority. Never accepts registration
//! claims as identity. All peers use the hotel's concrete supervised processes.
//! Tokens stay in volatile local authority storage, never in routed envelopes.
mod control;
pub mod runtime;
use ansible_mesh_core::{
    authority_fence::{Fence, Journal},
    privacy_local::{LocalConsumer, LocalLaunchRegistry, VerifiedLocalSession},
};
use anyhow::{Result, anyhow, ensure};
use native_memory::oauth::{Authority, Binding, Introspection};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path, sync::Arc};
use tokio::sync::Mutex;
use uuid::Uuid;

#[async_trait::async_trait]
pub trait NativeIntrospector: Send + Sync {
    /// Confidential direct issuer request; no cache, redirects or caller claims.
    async fn inspect(&self, token: &str) -> Result<Introspection>;
}

struct Entry {
    token: zeroize::Zeroizing<String>,
    token_digest: [u8; 32],
    authority: Authority,
    operation: String,
    request_digest: [u8; 32],
    request_expires: u64,
    origin: LocalConsumer,
    receiver: LocalConsumer,
    cancelled: bool,
    committed: bool,
}

pub struct NativeAuthority {
    hotel: String,
    membrane: String,
    runner: String,
    binding: Binding,
    launches: Arc<LocalLaunchRegistry>,
    journal: Arc<Mutex<Journal>>,
    entries: Mutex<BTreeMap<Uuid, Entry>>,
    issuer: Arc<dyn NativeIntrospector>,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
    #[cfg(test)]
    publisher_waiting: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    #[cfg(test)]
    publisher_never_started: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    #[cfg(test)]
    publisher_boundary: std::sync::Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
    #[cfg(test)]
    publisher_spawned: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

/// A reservation is deliberately not serializable. Its owner must retain it
/// through commit acknowledgement or known rollback. Drop poisons uncertainty.
pub struct NativeReservation {
    fence: Fence,
    authority: Arc<NativeAuthority>,
    handle: Uuid,
    peer: Arc<VerifiedLocalSession>,
    deadline: std::time::Instant,
    release: bool,
}

impl NativeAuthority {
    pub fn owns_launches(&self, launches: &Arc<LocalLaunchRegistry>) -> bool {
        Arc::ptr_eq(&self.launches, launches)
    }

    /// Hotel lifecycle retirement shares the write fence. Acknowledged stop
    /// cannot race a previously reserved native commit; new generations deny old permits.
    pub async fn retire(&self, guest: &str, generation: Uuid) -> Result<()> {
        let mut fence = self.fence().await?;
        let result = self.launches.retire(guest, generation);
        fence.finish()?;
        result
    }
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        hotel: String,
        membrane: String,
        runner: String,
        binding: Binding,
        launches: Arc<LocalLaunchRegistry>,
        journal: &Path,
        uid: u32,
        issuer: Arc<dyn NativeIntrospector>,
    ) -> Result<Arc<Self>> {
        ensure!(membrane != runner, "distinct native peers required");
        ensure!(
            binding.subject == binding.owner,
            "current native runner requires an explicit same-ID personal owner mapping"
        );
        Ok(Arc::new(Self {
            hotel,
            membrane,
            runner,
            binding,
            launches,
            journal: Arc::new(Mutex::new(Journal::open(journal, uid)?)),
            entries: Mutex::new(BTreeMap::new()),
            issuer,
            #[cfg(test)]
            publisher_waiting: std::sync::Mutex::new(None),
            #[cfg(test)]
            publisher_never_started: std::sync::Mutex::new(None),
            #[cfg(test)]
            publisher_boundary: std::sync::Mutex::new(None),
            #[cfg(test)]
            publisher_spawned: std::sync::Mutex::new(None),
            clock: Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|v| v.as_secs())
                    .unwrap_or(0)
            }),
        }))
    }

    async fn fence(&self) -> Result<Fence> {
        Fence::begin(
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                self.journal.clone().lock_owned(),
            )
            .await?,
        )
    }
    async fn inspect(&self, token: &str) -> Result<Introspection> {
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.issuer.inspect(token),
        )
        .await?
    }

    fn origin(&self, peer: &VerifiedLocalSession) -> Result<()> {
        peer.principal()?;
        ensure!(
            peer.consumer() == self.launches.target(&self.hotel, &self.membrane)?,
            "wrong membrane incarnation"
        );
        Ok(())
    }

    /// Called over authenticated private local control IPC, never task routing.
    /// Request digest covers the complete signed native request, including nonce.
    pub async fn admit(
        &self,
        peer: &VerifiedLocalSession,
        token: String,
        request: &native_memory::VerifiedRequest,
    ) -> Result<Uuid> {
        self.origin(peer)?;
        ensure!(
            token.len() == 43
                && token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
            "invalid token"
        );
        let mut fence = self.fence().await?;
        // Failure before external effect is known safe; close this reservation.
        let outcome = async {
            self.origin(peer)?;
            let envelope = request.envelope();
            ensure!(
                envelope.request.owner == self.binding.owner
                    && envelope.request.namespace == self.binding.namespace
                    && envelope.claims.caller == self.binding.subject
                    && envelope.claims.destination == self.hotel
                    && envelope.claims.runner == self.runner
                    && envelope.claims.reply_node == self.hotel
                    && envelope.claims.reply_guest == self.membrane,
                "native request scope mismatch"
            );
            let operation = envelope.request.command.operation();
            let claims = self.inspect(&token).await?;
            let authority = self.binding.validate(&claims, operation, (self.clock)())?;
            let receiver = self.launches.target(&self.hotel, &self.runner)?;
            let mut entries = self.entries.lock().await;
            entries.retain(|_, entry| !entry.cancelled && entry.request_expires > (self.clock)());
            ensure!(entries.len() < 4096, "native authority capacity");
            let handle = Uuid::new_v4();
            entries.insert(
                handle,
                Entry {
                    token_digest: Sha256::digest(token.as_bytes()).into(),
                    token: zeroize::Zeroizing::new(token),
                    authority,
                    operation: operation.into(),
                    request_digest: request.binding(),
                    request_expires: envelope.claims.expires_at,
                    origin: peer.consumer(),
                    receiver,
                    cancelled: false,
                    committed: false,
                },
            );
            Ok(handle)
        }
        .await;
        fence.finish()?;
        outcome
    }

    async fn check(
        &self,
        handle: Uuid,
        peer: &VerifiedLocalSession,
        digest: [u8; 32],
        release: bool,
    ) -> Result<()> {
        peer.principal()?;
        let entries = self.entries.lock().await;
        let entry = entries
            .get(&handle)
            .ok_or_else(|| anyhow!("unknown native permit"))?;
        ensure!(
            !entry.cancelled
                && entry.request_digest == digest
                && (self.clock)() < entry.request_expires,
            "inactive native permit"
        );
        ensure!(
            entry.origin == self.launches.target(&self.hotel, &self.membrane)?
                && entry.receiver == self.launches.target(&self.hotel, &self.runner)?,
            "retired native incarnation"
        );
        ensure!(
            peer.consumer()
                == if release {
                    entry.origin.clone()
                } else {
                    entry.receiver.clone()
                },
            "wrong native peer"
        );
        ensure!(!release || entry.committed, "no committed native result");
        ensure!(
            entry.token_digest == <[u8; 32]>::from(Sha256::digest(entry.token.as_bytes())),
            "token binding changed"
        );
        let current = self.inspect(&entry.token).await?;
        entry
            .authority
            .revalidate(&self.binding, &current, (self.clock)())?;
        ensure!(
            self.binding.operations.contains(&entry.operation),
            "operation removed"
        );
        peer.principal()?;
        Ok(())
    }

    /// Acquire immediately before the native transaction; retain through its
    /// acknowledgement. Revocation cannot acknowledge while this is outstanding.
    pub async fn reserve(
        self: &Arc<Self>,
        handle: Uuid,
        peer: Arc<VerifiedLocalSession>,
        digest: [u8; 32],
    ) -> Result<NativeReservation> {
        self.reserve_kind(handle, peer, digest, false).await
    }
    async fn reserve_kind(
        self: &Arc<Self>,
        handle: Uuid,
        peer: Arc<VerifiedLocalSession>,
        digest: [u8; 32],
        release: bool,
    ) -> Result<NativeReservation> {
        let mut fence = self.fence().await?;
        if let Err(error) = self.check(handle, &peer, digest, release).await {
            fence.finish()?;
            return Err(error);
        }
        Ok(NativeReservation {
            fence,
            authority: self.clone(),
            handle,
            peer,
            deadline: std::time::Instant::now() + std::time::Duration::from_secs(5),
            release,
        })
    }

    /// Result publication must occur inside `publish`, under this reservation.
    /// The caller verifies the existing signed response/request binding first.
    pub async fn release<T>(
        &self,
        handle: Uuid,
        peer: &VerifiedLocalSession,
        digest: [u8; 32],
        publish: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let mut fence = self.fence().await?;
        if let Err(error) = self.check(handle, peer, digest, true).await {
            fence.finish()?;
            return Err(error);
        }
        let result = peer.with_current_launch(|_| publish());
        // Unknown publication retains dirty denial, never retries automatically.
        match result {
            Ok(value) => {
                self.entries.lock().await.remove(&handle);
                fence.finish()?;
                Ok(value)
            }
            Err(error) => Err(error),
        }
    }

    /// Linearizes at the shared fence, after an already reserved commit. The
    /// returned boolean says a write committed before cancellation, not rollback.
    pub async fn cancel(&self, handle: Uuid, peer: &VerifiedLocalSession) -> Result<bool> {
        self.origin(peer)?;
        let mut fence = self.fence().await?;
        let result = async {
            self.origin(peer)?;
            let mut entries = self.entries.lock().await;
            let entry = entries
                .get_mut(&handle)
                .ok_or_else(|| anyhow!("unknown native permit"))?;
            ensure!(entry.origin == peer.consumer(), "foreign cancellation");
            entry.cancelled = true;
            let committed = entry.committed;
            entries.remove(&handle);
            Ok(committed)
        }
        .await;
        fence.finish()?;
        result
    }

    /// Existing issuer coordination protocol must use this same journal, not a
    /// separately constructed fence. Peer enrollment is checked by its adapter.
    pub async fn issuer_change<T>(
        &self,
        change: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        let mut fence = self.fence().await?;
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), change).await??; // Unknown outcome poisons admission.
        fence.finish()?;
        Ok(result)
    }

    /// Disabled lifecycle building block: no runtime route or configuration
    /// invokes this operation. A child exit is not proof that descendants are
    /// quiescent or that grant state was reconciled, so even success leaves the
    /// durable fence dirty. Without descendant-quiescence evidence the owner
    /// deliberately retains the fence until runtime shutdown, even after the
    /// direct child has exited or been reaped. There is no completion path.
    #[allow(dead_code)]
    async fn publisher_child_unreconciled(
        self: &Arc<Self>,
        mut command: tokio::process::Command,
        deadline: std::time::Duration,
    ) -> Result<std::process::ExitStatus> {
        ensure!(
            !deadline.is_zero() && deadline <= std::time::Duration::from_secs(30),
            "invalid publisher deadline"
        );
        // Caller cancellation closes this channel, but never aborts the
        // spawned owner task. The owner retains the fence until it has waited
        // for the exact direct child or cannot establish its termination.
        let (caller_alive, mut caller_gone) = tokio::sync::oneshot::channel::<()>();
        let authority = self.clone();
        let owner = tokio::spawn(async move {
            #[cfg(test)]
            if let Some(ready) = authority.publisher_waiting.lock().unwrap().take() {
                let _ = ready.send(());
            }
            let mut fence = authority.fence().await?;
            // This check is the spawn-authorization linearization point.
            // Cancellation observed here prevents spawn. Cancellation after
            // it may still race with spawn and must take the uncertain path.
            if matches!(
                caller_gone.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed)
            ) {
                // No child was spawned and no publisher effect began.
                fence.finish()?;
                #[cfg(test)]
                if let Some(done) = authority.publisher_never_started.lock().unwrap().take() {
                    let _ = done.send(());
                }
                return Err(anyhow!("publisher caller cancelled before spawn"));
            }
            #[cfg(test)]
            {
                let hook = authority.publisher_boundary.lock().unwrap().take();
                if let Some((ready, proceed)) = hook {
                    let _ = ready.send(());
                    let _ = proceed.await;
                }
            }
            command.kill_on_drop(true);
            let mut child = command.spawn()?;
            #[cfg(test)]
            if let Some(spawned) = authority.publisher_spawned.lock().unwrap().take() {
                let _ = spawned.send(());
            }
            let wait = tokio::select! {
                status = child.wait() => Some(status),
                _ = tokio::time::sleep(deadline) => None,
                _ = &mut caller_gone => None,
            };
            match wait {
                Some(Ok(_)) => {}
                Some(Err(_)) => {
                    // Unknown status: retain the fence while this runtime lives.
                    return std::future::pending::<Result<std::process::ExitStatus>>().await;
                }
                None => {
                    if child.kill().await.is_err() {
                        // A failed kill/wait is not evidence of quiescence.
                        return std::future::pending::<Result<std::process::ExitStatus>>().await;
                    }
                }
            }
            // Direct-child exit or reap does not establish descendant
            // quiescence. Never release the fence on that evidence alone.
            let _keep_fence = fence;
            std::future::pending::<Result<std::process::ExitStatus>>().await
        });
        let result = owner
            .await
            .map_err(|_| anyhow!("publisher owner unavailable"))?;
        drop(caller_alive);
        result
    }
}

impl NativeReservation {
    /// Recheck immediately before sending COMMIT while retaining the same fence.
    pub async fn validate_before_commit(&self, digest: [u8; 32]) -> Result<()> {
        ensure!(
            std::time::Instant::now() < self.deadline,
            "native reservation deadline"
        );
        self.authority
            .check(self.handle, &self.peer, digest, self.release)
            .await
    }
    /// Call only after a known commit or rollback acknowledgement. A lost graph
    /// acknowledgement must drop the reservation dirty, even if retry may succeed.
    pub async fn finish(mut self, committed: bool) -> Result<()> {
        ensure!(
            std::time::Instant::now() < self.deadline,
            "native reservation deadline"
        );
        self.peer.principal()?;
        let mut entries = self.authority.entries.lock().await;
        let entry = entries
            .get_mut(&self.handle)
            .ok_or_else(|| anyhow!("unknown native permit"))?;
        ensure!(
            (if self.release {
                &entry.origin
            } else {
                &entry.receiver
            }) == &self.peer.consumer(),
            "retired peer"
        );
        if self.release {
            ensure!(!committed, "release cannot claim write");
            entries.remove(&self.handle);
            return self.fence.finish();
        }
        entry.committed |= committed;
        self.fence.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::privacy_local::{LaunchPrincipal, SupervisedProcess};
    use std::{
        collections::BTreeSet,
        os::unix::fs::{MetadataExt, PermissionsExt},
        sync::atomic::{AtomicBool, AtomicU64, Ordering},
    };
    struct Process(Arc<AtomicBool>);
    impl SupervisedProcess for Process {
        fn pid(&self) -> Result<u32> {
            Ok(std::process::id())
        }
        fn alive(&self) -> Result<bool> {
            Ok(self.0.load(Ordering::SeqCst))
        }
    }
    struct Issuer {
        active: AtomicBool,
        version: AtomicU64,
        read_only: AtomicBool,
    }
    #[async_trait::async_trait]
    impl NativeIntrospector for Issuer {
        async fn inspect(&self, token: &str) -> Result<Introspection> {
            ensure!(token == "a".repeat(43), "different token");
            Ok(Introspection {
                active: self.active.load(Ordering::SeqCst),
                iss: Some("https://issuer.example".into()),
                aud: Some("https://memory.example/native/mcp".into()),
                sub: Some("synthetic".into()),
                client_id: Some("dot".into()),
                grant_version: Some(format!("0:{}", self.version.load(Ordering::SeqCst))),
                scope: Some(
                    if self.read_only.load(Ordering::SeqCst) {
                        "native-memory:recall"
                    } else {
                        "native-memory:remember native-memory:recall"
                    }
                    .into(),
                ),
                iat: Some(100),
                exp: Some(1000),
            })
        }
    }
    struct Fixture {
        authority: Arc<NativeAuthority>,
        issuer: Arc<Issuer>,
        clock: Arc<AtomicU64>,
        origin: Arc<VerifiedLocalSession>,
        receiver: Arc<VerifiedLocalSession>,
        alive: Arc<AtomicBool>,
        dir: std::path::PathBuf,
        request: native_memory::VerifiedRequest,
        policy: native_memory::Policy,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    impl Fixture {
        fn new() -> Self {
            // Keep the private socket path below macOS SUN_LEN as well as Linux.
            let dir =
                std::path::Path::new("/tmp").join(format!("native-authority-{}", Uuid::new_v4()));
            std::fs::create_dir(&dir).unwrap();
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
            let journal = dir.join("uncertainty");
            std::fs::write(&journal, b"clean\n").unwrap();
            std::fs::set_permissions(&journal, std::fs::Permissions::from_mode(0o600)).unwrap();
            let uid = std::fs::metadata(&journal).unwrap().uid();
            let launches = Arc::new(LocalLaunchRegistry::default());
            let alive = Arc::new(AtomicBool::new(true));
            for name in ["membrane", "runner"] {
                launches
                    .attach(
                        name,
                        uid,
                        LaunchPrincipal {
                            stable_agent_id: name.into(),
                            roles: BTreeSet::new(),
                        },
                        Arc::new(Process(alive.clone())),
                    )
                    .unwrap();
            }
            let (left, _right) = tokio::net::UnixStream::pair().unwrap();
            let origin = launches.authenticate("hotel", &left, "membrane").unwrap();
            let receiver = launches.authenticate("hotel", &left, "runner").unwrap();
            let binding = Binding {
                issuer: "https://issuer.example".into(),
                resource: "https://memory.example/native/mcp".into(),
                client_id: "dot".into(),
                subject: "synthetic".into(),
                grant_version: "0:7".into(),
                owner: "synthetic".into(),
                namespace: "synthetic".into(),
                operations: vec!["remember".into(), "recall".into()],
            };
            let issuer = Arc::new(Issuer {
                active: AtomicBool::new(true),
                version: AtomicU64::new(7),
                read_only: AtomicBool::new(false),
            });
            let clock = Arc::new(AtomicU64::new(101));
            let time = clock.clone();
            let mut authority = NativeAuthority::new(
                "hotel".into(),
                "membrane".into(),
                "runner".into(),
                binding,
                launches,
                &journal,
                uid,
                issuer.clone(),
            )
            .unwrap();
            Arc::get_mut(&mut authority).unwrap().clock =
                Arc::new(move || time.load(Ordering::SeqCst));
            let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
            let grant = native_memory::Grant {
                issuer: "test-ingress".into(),
                reply_node: "hotel".into(),
                reply_guest: "membrane".into(),
                principal: "synthetic".into(),
                owner: "synthetic".into(),
                namespace: "synthetic".into(),
                destination: "hotel".into(),
                runner: "runner".into(),
                operations: vec!["remember".into(), "recall".into()],
                revision: 1,
                expires_at: 1000,
                revoked: false,
            };
            let grants = BTreeMap::from([("test".into(), grant)]);
            let policy = native_memory::Policy {
                issuers: BTreeMap::from([(
                    "test-ingress".into(),
                    native_memory::TrustedIssuer {
                        key_id: "key".into(),
                        public_key: key.verifying_key().to_bytes(),
                    },
                )]),
                grants: grants.clone(),
            };
            let ingress =
                native_memory::Ingress::new("test-ingress".into(), "key".into(), key, grants);
            let envelope = ingress
                .issue(
                    "test",
                    native_memory::Command::Remember {
                        id: "id".into(),
                        subject: "synthetic".into(),
                        content: "harmless fixture".into(),
                        source: "synthetic".into(),
                        source_version: "1".into(),
                        lineage: "independent".into(),
                    },
                    "event".into(),
                    "nonce".into(),
                    101,
                )
                .unwrap();
            let request = native_memory::VerifiedRequest::validate(
                &envelope, &policy, "hotel", "runner", 101,
            )
            .unwrap();
            Self {
                authority,
                issuer,
                clock,
                origin,
                receiver,
                alive,
                dir,
                request,
                policy,
            }
        }
        async fn admit(&self) -> Uuid {
            self.authority
                .admit(&self.origin, "a".repeat(43), &self.request)
                .await
                .unwrap()
        }
    }
    #[tokio::test]
    #[ignore = "synthetic owned child invoked by control composition test"]
    async fn native_control_child() {
        use native_memory::authority_control::{Client, Reply, Request};
        let dir = std::path::PathBuf::from(std::env::var("NATIVE_CONTROL_FIXTURE").unwrap());
        let role = std::env::var("NATIVE_CONTROL_ROLE").unwrap();
        async fn wait(path: &Path) {
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                while !path.exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
        }
        wait(&dir.join("start")).await;
        let envelope: native_memory::Envelope =
            serde_json::from_slice(&std::fs::read(dir.join("request.json")).unwrap()).unwrap();
        let digest = native_memory::response::request_binding(&envelope).unwrap();
        let socket = dir.join("control.sock");
        if role == "membrane" {
            let mut client = Client::connect(&socket).await.unwrap();
            let reply = client
                .send(&Request::Admit {
                    guest: role.clone(),
                    token: "a".repeat(43),
                    envelope: Box::new(envelope),
                })
                .await
                .unwrap();
            let Reply::Permit { handle } = reply else {
                panic!("admission denied")
            };
            // Publish the whole frame atomically: existence alone must not let
            // the runner read a just-created, still-empty fixture file.
            std::fs::write(dir.join("handle.pending"), &handle).unwrap();
            std::fs::rename(dir.join("handle.pending"), dir.join("handle")).unwrap();
            wait(&dir.join("committed")).await;
            let mut client = Client::connect(&socket).await.unwrap();
            assert!(matches!(
                client
                    .send(&Request::Release {
                        guest: role,
                        handle,
                        digest
                    })
                    .await
                    .unwrap(),
                Reply::Ready
            ));
            client.validate(digest).await.unwrap();
            client.finish(false).await.unwrap();
            std::fs::write(dir.join("done"), b"released").unwrap();
        } else {
            wait(&dir.join("handle")).await;
            let handle = std::fs::read_to_string(dir.join("handle")).unwrap();
            let mut client = Client::connect(&socket).await.unwrap();
            assert!(matches!(
                client
                    .send(&Request::Reserve {
                        guest: role,
                        handle,
                        digest
                    })
                    .await
                    .unwrap(),
                Reply::Ready
            ));
            client.validate(digest).await.unwrap();
            // Mock storage acknowledgement only: this fixture does not execute Memgraph.
            client.finish(true).await.unwrap();
            std::fs::write(dir.join("committed"), b"mock commit").unwrap();
        }
        // Keep both authenticated generations alive until parent verifies release.
        wait(&dir.join("stop")).await;
    }

    #[tokio::test]
    async fn actual_owned_children_admit_reserve_validate_and_release_over_private_control() {
        use super::super::protected_local_guest::ProtectedLocalGuest;
        let mut f = Fixture::new();
        let registry = Arc::new(LocalLaunchRegistry::default());
        Arc::get_mut(&mut f.authority).unwrap().launches = registry.clone();
        std::fs::write(
            f.dir.join("request.json"),
            serde_json::to_vec(f.request.envelope()).unwrap(),
        )
        .unwrap();
        let socket = f.dir.join("control.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let policy = f.policy.clone();
        let server = tokio::spawn(
            f.authority
                .clone()
                .serve_control(listener, Arc::new(move || Ok(policy.clone()))),
        );
        let mut children = Vec::new();
        for role in ["membrane", "runner"] {
            let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "service::native_authority::tests::native_control_child",
                    "--ignored",
                ])
                .env("NATIVE_CONTROL_FIXTURE", &f.dir)
                .env("NATIVE_CONTROL_ROLE", role)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            children.push(
                ProtectedLocalGuest::spawn(
                    &mut command,
                    registry.clone(),
                    role,
                    unsafe { libc::geteuid() },
                    LaunchPrincipal {
                        stable_agent_id: role.into(),
                        roles: BTreeSet::new(),
                    },
                )
                .unwrap()
                .with_native_authority(f.authority.clone())
                .unwrap(),
            );
        }
        std::fs::write(f.dir.join("start"), b"enrolled").unwrap();
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(12), async {
            while !f.dir.join("done").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        if outcome.is_ok() {
            assert!(f.authority.entries.lock().await.is_empty());
        }
        for child in &children {
            child.stop().await.unwrap();
        }
        server.abort();
        outcome.expect("child control composition completed");
    }
    #[tokio::test]
    async fn recall_token_cannot_authorize_remember_envelope_and_terminal_token_is_removed() {
        let f = Fixture::new();
        f.issuer.read_only.store(true, Ordering::SeqCst);
        assert!(
            f.authority
                .admit(&f.origin, "a".repeat(43), &f.request)
                .await
                .is_err()
        );
        f.issuer.read_only.store(false, Ordering::SeqCst);
        let handle = f.admit().await;
        f.authority
            .reserve(handle, f.receiver.clone(), f.request.binding())
            .await
            .unwrap()
            .finish(true)
            .await
            .unwrap();
        assert!(f.authority.cancel(handle, &f.origin).await.unwrap());
        assert!(f.authority.entries.lock().await.is_empty());
    }
    #[tokio::test]
    async fn queued_revoked_version_changed_expired_cancelled_and_tampered_deny() {
        for case in 0..5 {
            let f = Fixture::new();
            let handle = f.admit().await;
            match case {
                0 => {
                    f.authority
                        .issuer_change(async {
                            f.issuer.active.store(false, Ordering::SeqCst);
                            Ok(())
                        })
                        .await
                        .unwrap();
                }
                1 => {
                    f.authority
                        .issuer_change(async {
                            f.issuer.version.store(8, Ordering::SeqCst);
                            Ok(())
                        })
                        .await
                        .unwrap();
                }
                2 => {
                    f.clock.store(1000, Ordering::SeqCst);
                }
                3 => {
                    assert!(!f.authority.cancel(handle, &f.origin).await.unwrap());
                }
                _ => {}
            }
            assert!(
                f.authority
                    .reserve(
                        handle,
                        f.receiver.clone(),
                        if case == 4 {
                            [8; 32]
                        } else {
                            f.request.binding()
                        }
                    )
                    .await
                    .is_err()
            );
            assert_eq!(
                std::fs::read(f.dir.join("uncertainty")).unwrap(),
                b"clean\n"
            );
        }
    }
    #[tokio::test]
    async fn revocation_waits_for_commit_then_denies_release_without_claiming_rollback() {
        let f = Fixture::new();
        let handle = f.admit().await;
        let lease = f
            .authority
            .reserve(handle, f.receiver.clone(), f.request.binding())
            .await
            .unwrap();
        lease
            .validate_before_commit(f.request.binding())
            .await
            .unwrap();
        let authority = f.authority.clone();
        let issuer = f.issuer.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let change = tokio::spawn(async move {
            let _ = started.send(());
            authority
                .issuer_change(async {
                    issuer.active.store(false, Ordering::SeqCst);
                    Ok(())
                })
                .await
        });
        ready.await.unwrap();
        assert!(f.issuer.active.load(Ordering::SeqCst));
        assert!(!change.is_finished());
        lease.finish(true).await.unwrap();
        change.await.unwrap().unwrap();
        assert!(f.authority.entries.lock().await[&handle].committed);
        assert!(
            f.authority
                .release(handle, &f.origin, f.request.binding(), || Ok(()))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn expiry_before_commit_recheck_rolls_back_and_known_commit_releases() {
        let f = Fixture::new();
        let handle = f.admit().await;
        let lease = f
            .authority
            .reserve(handle, f.receiver.clone(), f.request.binding())
            .await
            .unwrap();
        f.clock.store(1000, Ordering::SeqCst);
        assert!(
            lease
                .validate_before_commit(f.request.binding())
                .await
                .is_err()
        );
        lease.finish(false).await.unwrap();
        assert!(!f.authority.entries.lock().await[&handle].committed);
        let f = Fixture::new();
        let handle = f.admit().await;
        f.authority
            .reserve(handle, f.receiver.clone(), f.request.binding())
            .await
            .unwrap()
            .finish(true)
            .await
            .unwrap();
        assert_eq!(
            f.authority
                .release(handle, &f.origin, f.request.binding(), || Ok("published"))
                .await
                .unwrap(),
            "published"
        );
        assert!(f.authority.entries.lock().await.is_empty());
        assert!(f.authority.cancel(handle, &f.origin).await.is_err());
        assert!(
            f.authority
                .release(handle, &f.origin, f.request.binding(), || Ok(()))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn unknown_commit_poison_survives_restart_and_retired_generation_denies() {
        let f = Fixture::new();
        let handle = f.admit().await;
        let lease = f
            .authority
            .reserve(handle, f.receiver.clone(), f.request.binding())
            .await
            .unwrap();
        drop(lease);
        assert!(f.authority.cancel(handle, &f.origin).await.is_err());
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"dirty\n"
        );
        let path = f.dir.join("uncertainty");
        let uid = std::fs::metadata(&path).unwrap().uid();
        assert!(Journal::open(&path, uid).is_err());
        let f = Fixture::new();
        let handle = f.admit().await;
        f.alive.store(false, Ordering::SeqCst);
        assert!(
            f.authority
                .reserve(handle, f.receiver.clone(), f.request.binding())
                .await
                .is_err()
        );
        f.alive.store(true, Ordering::SeqCst);
        let uid = std::fs::metadata(&f.dir).unwrap().uid();
        f.authority
            .launches
            .attach(
                "runner",
                uid,
                LaunchPrincipal {
                    stable_agent_id: "runner".into(),
                    roles: BTreeSet::new(),
                },
                Arc::new(Process(f.alive.clone())),
            )
            .unwrap();
        assert!(
            f.authority
                .reserve(handle, f.receiver.clone(), f.request.binding())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn publisher_child_exit_keeps_unreconciled_authority_closed() {
        let f = Fixture::new();
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "exit 0"]);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                f.authority
                    .publisher_child_unreconciled(command, std::time::Duration::from_secs(2))
            )
            .await
            .is_err()
        );
        assert!(f.authority.journal.try_lock().is_err());
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"dirty\n"
        );
    }

    #[tokio::test]
    async fn publisher_child_timeout_keeps_unreconciled_authority_closed() {
        let f = Fixture::new();
        let mut command = tokio::process::Command::new("/bin/sleep");
        command.arg("5");
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                f.authority
                    .publisher_child_unreconciled(command, std::time::Duration::from_millis(25))
            )
            .await
            .is_err()
        );
        assert!(f.authority.journal.try_lock().is_err());
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"dirty\n"
        );
    }

    #[tokio::test]
    async fn publisher_child_failure_keeps_unreconciled_authority_closed() {
        let f = Fixture::new();
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", "exit 1"]);
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(100),
                f.authority
                    .publisher_child_unreconciled(command, std::time::Duration::from_secs(2))
            )
            .await
            .is_err()
        );
        assert!(f.authority.journal.try_lock().is_err());
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"dirty\n"
        );
    }

    #[tokio::test]
    async fn publisher_cancelled_before_spawn_does_not_start_or_dirty_authority() {
        let f = Fixture::new();
        let mut blocker = f.authority.fence().await.unwrap();
        let marker = f.dir.join("must-not-start");
        let path = marker.clone();
        let owner = f.authority.clone();
        let (waiting, waiting_rx) = tokio::sync::oneshot::channel();
        let (never_started, never_started_rx) = tokio::sync::oneshot::channel();
        *f.authority.publisher_waiting.lock().unwrap() = Some(waiting);
        *f.authority.publisher_never_started.lock().unwrap() = Some(never_started);
        let caller = tokio::spawn(async move {
            let mut command = tokio::process::Command::new("/bin/sh");
            command
                .arg("-c")
                .arg("printf started > \"$1\"")
                .arg("synthetic-publisher")
                .arg(path);
            owner
                .publisher_child_unreconciled(command, std::time::Duration::from_secs(2))
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), waiting_rx)
            .await
            .unwrap()
            .unwrap();
        caller.abort();
        assert!(caller.await.is_err());
        blocker.finish().unwrap();
        drop(blocker);
        tokio::time::timeout(std::time::Duration::from_secs(2), never_started_rx)
            .await
            .unwrap()
            .unwrap();
        assert!(!marker.exists());
        assert!(f.authority.journal.lock().await.validate().is_ok());
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"clean\n"
        );
    }

    #[tokio::test]
    async fn publisher_cancelled_after_boundary_may_spawn_but_remains_closed() {
        let f = Fixture::new();
        let (at_boundary, at_boundary_rx) = tokio::sync::oneshot::channel();
        let (proceed, proceed_rx) = tokio::sync::oneshot::channel();
        let (spawned, spawned_rx) = tokio::sync::oneshot::channel();
        *f.authority.publisher_boundary.lock().unwrap() = Some((at_boundary, proceed_rx));
        *f.authority.publisher_spawned.lock().unwrap() = Some(spawned);
        let owner = f.authority.clone();
        let caller = tokio::spawn(async move {
            let mut command = tokio::process::Command::new("/bin/sleep");
            command.arg("5");
            owner
                .publisher_child_unreconciled(command, std::time::Duration::from_secs(2))
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), at_boundary_rx)
            .await
            .unwrap()
            .unwrap();
        caller.abort();
        assert!(caller.await.is_err());
        proceed.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), spawned_rx)
            .await
            .unwrap()
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(f.authority.journal.try_lock().is_err());
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"dirty\n"
        );
    }

    #[tokio::test]
    async fn publisher_caller_cancellation_reaps_direct_child_and_retains_fence() {
        let f = Fixture::new();
        let pid_file = f.dir.join("publisher-pid");
        let owner = f.authority.clone();
        let path = pid_file.clone();
        let caller = tokio::spawn(async move {
            let mut command = tokio::process::Command::new("/bin/sh");
            command
                .arg("-c")
                .arg("printf '%s' \"$$\" > \"$1\"; exec /bin/sleep 5")
                .arg("synthetic-publisher")
                .arg(path);
            owner
                .publisher_child_unreconciled(command, std::time::Duration::from_secs(10))
                .await
        });
        let pid: libc::pid_t = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let Ok(contents) = std::fs::read_to_string(&pid_file) {
                    if let Ok(pid) = contents.parse() {
                        break pid;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        caller.abort();
        assert!(caller.await.is_err());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(f.authority.journal.try_lock().is_err());
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[tokio::test]
    async fn publisher_parent_exit_does_not_release_fence_while_descendant_runs() {
        let f = Fixture::new();
        let marker = f.dir.join("descendant-finished");
        let owner = f.authority.clone();
        let path = marker.clone();
        let caller = tokio::spawn(async move {
            let mut command = tokio::process::Command::new("/bin/sh");
            command
                .arg("-c")
                .arg("(/bin/sleep 0.1; printf done > \"$1\") & exit 0")
                .arg("synthetic-publisher")
                .arg(path);
            owner
                .publisher_child_unreconciled(command, std::time::Duration::from_secs(2))
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while std::fs::read(&marker).ok().as_deref() != Some(b"done") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(std::fs::read(&marker).unwrap(), b"done");
        assert!(f.authority.journal.try_lock().is_err());
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"dirty\n"
        );
        caller.abort();
    }

    #[test]
    fn publisher_runtime_shutdown_leaves_durable_journal_dirty() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (mut f, replacement) = runtime.block_on(async {
            let f = Fixture::new();
            let replacement = Fixture::new();
            let marker = f.dir.join("publisher-started");
            let owner = f.authority.clone();
            let path = marker.clone();
            tokio::spawn(async move {
                let mut command = tokio::process::Command::new("/bin/sh");
                command
                    .arg("-c")
                    .arg("printf started > \"$1\"; exec /bin/sleep 5")
                    .arg("synthetic-publisher")
                    .arg(path);
                let _ = owner
                    .publisher_child_unreconciled(command, std::time::Duration::from_secs(10))
                    .await;
            });
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while !marker.exists() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            (f, replacement)
        });
        drop(runtime);
        let binding = f.authority.binding.clone();
        let issuer = f.authority.issuer.clone();
        let old = std::mem::replace(&mut f.authority, replacement.authority.clone());
        drop(old);
        let path = f.dir.join("uncertainty");
        assert_eq!(std::fs::read(&path).unwrap(), b"dirty\n");
        // Verify rejection is due to durable state, not the old owner's lock.
        let probe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        probe.try_lock().unwrap();
        drop(probe);
        let uid = std::fs::metadata(&path).unwrap().uid();
        assert!(
            NativeAuthority::new(
                "hotel".into(),
                "membrane".into(),
                "runner".into(),
                binding,
                Arc::new(LocalLaunchRegistry::default()),
                &path,
                uid,
                issuer
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn publisher_child_keeps_issuer_blocked_after_unreconciled_exit() {
        let f = Fixture::new();
        let owner = f.authority.clone();
        let child = tokio::spawn(async move {
            let mut command = tokio::process::Command::new("/bin/sleep");
            command.arg("0.2");
            owner
                .publisher_child_unreconciled(command, std::time::Duration::from_secs(2))
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while std::fs::read(f.dir.join("uncertainty")).unwrap() != b"dirty\n" {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let ran = Arc::new(AtomicBool::new(false));
        let attempted = ran.clone();
        let owner = f.authority.clone();
        let issuer = tokio::spawn(async move {
            owner
                .issuer_change(async move {
                    attempted.store(true, Ordering::SeqCst);
                    Ok(())
                })
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!ran.load(Ordering::SeqCst));
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        assert!(!child.is_finished());
        assert!(!issuer.is_finished());
        assert!(!ran.load(Ordering::SeqCst));
        child.abort();
        issuer.abort();
    }
}
