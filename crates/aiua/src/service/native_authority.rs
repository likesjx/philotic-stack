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
    snapshot_digest: Option<[u8; 32]>,
}

type SnapshotSource =
    Arc<dyn Fn() -> Result<native_memory::grant_snapshot_candidate::LoadedSnapshot> + Send + Sync>;
type CompletionReceipt = Arc<dyn Fn(Uuid, [u8; 32]) -> Result<bool> + Send + Sync>;
/// Unwired candidate reader for a root-owned group-readable completion
/// receipt. Caller must supply the protected, pinned directory and group.
#[allow(dead_code)]
fn experimental_root_receipt_source(
    directory: std::path::PathBuf,
    group: u32,
) -> CompletionReceipt {
    Arc::new(move |id, digest| {
        Ok(
            native_memory::grant_publisher_candidate::verify_root_receipt(
                &directory, group, id, digest,
            )?,
        )
    })
}
/// Unwired one-operation plan. All fields and the receipt reader must come
/// from protected hotel configuration rather than an IPC request.
pub(crate) struct PublisherPlan {
    guest: String,
    consumer: LocalConsumer,
    operation_id: Uuid,
    old_digest: [u8; 32],
    new_digest: [u8; 32],
    deadline: u64,
    receipt: CompletionReceipt,
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
    snapshot_source: Option<SnapshotSource>,
    publisher_plan: Option<PublisherPlan>,
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
            clock: Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|v| v.as_secs())
                    .unwrap_or(0)
            }),
            snapshot_source: None,
            publisher_plan: None,
        }))
    }

    /// Unwired opt-in candidate. Install only from hotel-owned trusted
    /// configuration before sharing the authority. The source must return a
    /// custody-checked current snapshot and all writers must hold this fence.
    #[allow(dead_code)]
    pub(crate) fn experimental_set_snapshot_source(
        &mut self,
        source: SnapshotSource,
    ) -> Result<()> {
        ensure!(
            self.snapshot_source.is_none(),
            "snapshot source already configured"
        );
        self.snapshot_source = Some(source);
        Ok(())
    }

    /// No runtime installs this plan yet. The publisher must be an exact
    /// root-owned supervised child with a pinned executable and generation.
    #[allow(dead_code)]
    pub(crate) fn experimental_set_publisher_plan(&mut self, plan: PublisherPlan) -> Result<()> {
        ensure!(
            self.publisher_plan.is_none()
                && self.snapshot_source.is_some()
                && plan.guest != self.runner
                && plan.guest != self.membrane
                && !plan.guest.is_empty()
                && plan.old_digest != plan.new_digest
                && plan.deadline > (self.clock)()
                && self.launches.target(&self.hotel, &plan.guest)? == plan.consumer,
            "invalid publisher plan"
        );
        self.publisher_plan = Some(plan);
        Ok(())
    }

    fn verify_snapshot_binding(
        &self,
        loaded: &native_memory::grant_snapshot_candidate::LoadedSnapshot,
    ) -> Result<()> {
        let snapshot = loaded.snapshot();
        ensure!(
            snapshot.oauth_issuer == self.binding.issuer
                && snapshot.oauth_resource == self.binding.resource
                && snapshot.oauth_client_id == self.binding.client_id
                && snapshot.oauth_subject == self.binding.subject
                && snapshot.oauth_grant_version == self.binding.grant_version
                && snapshot.grant.owner == self.binding.owner
                && snapshot.grant.namespace == self.binding.namespace
                && snapshot.grant.destination == self.hotel
                && snapshot.grant.runner == self.runner
                && snapshot.grant.reply_node == self.hotel
                && snapshot.grant.reply_guest == self.membrane,
            "snapshot OAuth binding mismatch"
        );
        Ok(())
    }

    async fn fence(&self) -> Result<Fence> {
        let mut fence = Fence::new(
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                self.journal.clone().lock_owned(),
            )
            .await?,
        );
        fence.mark(false)?;
        Ok(fence)
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
            let snapshot_digest = if let Some(source) = &self.snapshot_source {
                let loaded = source()?;
                self.verify_snapshot_binding(&loaded)?;
                native_memory::VerifiedRequest::validate(
                    envelope,
                    &loaded.receiver_policy(),
                    &self.hotel,
                    &self.runner,
                    (self.clock)(),
                )?;
                Some(loaded.digest())
            } else {
                None
            };
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
                    snapshot_digest,
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
        if let Some(source) = &self.snapshot_source {
            let loaded = source()?;
            ensure!(
                entry.snapshot_digest == Some(loaded.digest()),
                "snapshot changed since admission"
            );
            let grant = &loaded.snapshot().grant;
            ensure!(
                !grant.revoked && (self.clock)() < grant.expires_at,
                "snapshot grant inactive"
            );
        } else {
            ensure!(entry.snapshot_digest.is_none(), "snapshot source missing");
        }
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

    /// Unwired candidate: callers MUST first authenticate the protected
    /// publisher and establish binding/time provenance. No runtime path invokes
    /// this method. It orders the durable transaction with commit and release;
    /// it does not install a reader, witness, or recovery protocol.
    #[allow(dead_code)]
    pub(crate) async fn experimental_publish_snapshot(
        &self,
        ledger: &mut native_memory::grant_ledger_candidate::Ledger,
        id: &str,
        expected_digest: &str,
        bytes: &[u8],
        binding: &native_memory::grant_snapshot_candidate::ExpectedBinding<'_>,
        max_expiry: u64,
        deadline: u64,
    ) -> Result<String> {
        use native_memory::grant_ledger_candidate::LedgerError;
        let mut fence = self.fence().await?;
        match ledger.publish_snapshot(
            id,
            expected_digest,
            bytes,
            binding,
            self.clock.as_ref(),
            max_expiry,
            deadline,
        ) {
            Ok(digest) => {
                fence.finish()?;
                Ok(digest)
            }
            Err(error @ (LedgerError::Denied | LedgerError::Invalid | LedgerError::Conflict)) => {
                fence.finish()?;
                Err(anyhow!("snapshot mutation rejected: {error:?}"))
            }
            // Any storage failure, uncertain commit, cancellation or panic must
            // retain the dirty journal until an explicit recovery procedure.
            Err(error) => Err(anyhow!("snapshot mutation requires recovery: {error:?}")),
        }
    }

    /// Unwired candidate; caller must authenticate the packet and revalidate
    /// manager-backed process incarnation before invoking. Consuming an
    /// operation does not prove the installation happened. This callback is
    /// solely for installing the validated snapshot, not for memory actions.
    /// The fence remains dirty on any error or cancellation.
    #[allow(dead_code)]
    pub(crate) async fn experimental_consume_snapshot<T>(
        &self,
        ledger: &mut native_memory::grant_ledger_candidate::Ledger,
        operation_id: &str,
        snapshot: &native_memory::grant_snapshot_candidate::LoadedSnapshot,
        effect: impl FnOnce(&native_memory::grant_snapshot_candidate::LoadedSnapshot) -> Result<T>,
    ) -> Result<T> {
        use native_memory::grant_ledger_candidate::LedgerError;
        let mut fence = self.fence().await?;
        let digest = snapshot
            .digest()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        match ledger.consume_for_effect(operation_id, &digest) {
            Ok(()) => {}
            Err(error @ (LedgerError::Denied | LedgerError::Invalid | LedgerError::Conflict)) => {
                fence.finish()?;
                return Err(anyhow!("snapshot consumption rejected: {error:?}"));
            }
            Err(error) => return Err(anyhow!("snapshot consumption requires recovery: {error:?}")),
        }
        let result = effect(snapshot)?;
        ledger
            .complete_consumption(operation_id, &digest)
            .map_err(|e| anyhow!("snapshot completion requires recovery: {e:?}"))?;
        fence.finish()?;
        Ok(result)
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

    // Research fixture only: this is not called by the production control
    // path. Validate static authority while holding the same hotel fence, and
    // distinguish a known denial before publication from an uncertain effect.
    async fn experimental_checked_release(
        f: &Fixture,
        handle: Uuid,
        load_policy: impl FnOnce() -> Result<native_memory::Policy>,
        publish: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let mut fence = f.authority.fence().await?;
        if let Err(error) = f
            .authority
            .check(handle, &f.origin, f.request.binding(), true)
            .await
        {
            fence.finish()?;
            return Err(error);
        }
        let policy = match load_policy() {
            Ok(policy) => policy,
            Err(error) => {
                fence.finish()?;
                return Err(error);
            }
        };
        if native_memory::VerifiedRequest::validate(
            f.request.envelope(),
            &policy,
            "hotel",
            "runner",
            (f.authority.clock)(),
        )
        .is_err()
        {
            fence.finish()?;
            return Err(anyhow!("static grant denied before publication"));
        }
        // Once publication is attempted, failure or cancellation is uncertain.
        // Drop leaves the fence dirty rather than falsely recording recovery.
        f.origin.with_current_launch(|_| publish())?;
        f.authority.entries.lock().await.remove(&handle);
        fence.finish()
    }

    #[derive(Debug, PartialEq, Eq)]
    enum ExperimentalChildOutcome {
        Completed,
        CancelledAndReaped,
    }

    // Test-only direct-child model. The fence stays owned until the concrete
    // child has exited and been reaped. Spawning after fence acquisition is
    // not a complete operation-authorization or guest handshake barrier. It
    // does not grant permission to signal root, stop descendants, or implement
    // a service-manager adapter; runtime shutdown can still abort the owner.
    async fn experimental_owned_publisher_lifecycle(
        authority: Arc<NativeAuthority>,
        mut command: tokio::process::Command,
        session: impl std::future::Future<Output = Result<()>> + Send + 'static,
        simulate_stop_failure: bool,
        stop_observed: Option<tokio::sync::oneshot::Sender<()>>,
        stdin_observed: Option<tokio::sync::oneshot::Sender<tokio::process::ChildStdin>>,
        owner_started: Option<tokio::sync::oneshot::Sender<()>>,
        abort_observed: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<ExperimentalChildOutcome> {
        // A dropped caller cancels before spawning if the owner is still
        // waiting for the fence if cancellation is observed at the post-fence
        // check. After that check, cancellation is not revocation:
        // the detached owner retains responsibility for termination and reap.
        // Runtime shutdown can still abort it; the durable journal stays dirty.
        let (caller_alive, mut caller_gone) = tokio::sync::oneshot::channel::<()>();
        let owner = tokio::spawn(async move {
            if let Some(signal) = owner_started {
                let _ = signal.send(());
            }
            let mut fence = authority.fence().await?;
            if matches!(
                caller_gone.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed)
            ) {
                fence.finish()?;
                return Err(anyhow!("caller cancelled before child spawn"));
            }
            let mut child = command.spawn()?;
            if let Some(send) = stdin_observed {
                let input = match child.stdin.take() {
                    Some(input) => input,
                    None => {
                        let _ = child.start_kill();
                        if child.wait().await.is_err() {
                            std::future::pending::<()>().await;
                            unreachable!();
                        }
                        return Err(anyhow!("test child stdin missing"));
                    }
                };
                let _ = send.send(input);
            }
            // Isolate session panic from the fence owner: a panicking or timed
            // out session still reaches the exact-child stop/reap path.
            let mut session_task = tokio::spawn(session);
            let completed =
                match tokio::time::timeout(std::time::Duration::from_millis(30), &mut session_task)
                    .await
                {
                    Ok(Ok(Ok(()))) => true,
                    Ok(_) => false,
                    Err(_) => {
                        session_task.abort();
                        if let Some(signal) = abort_observed {
                            let _ = signal.send(());
                        }
                        // If the session does not yield, awaiting cancellation can
                        // block indefinitely. The fence deliberately remains held;
                        // this does not guarantee timely child termination.
                        let _ = session_task.await;
                        false
                    }
                };
            if completed {
                if let Ok(Ok(status)) =
                    tokio::time::timeout(std::time::Duration::from_millis(100), child.wait()).await
                {
                    if status.success() {
                        fence.finish()?;
                        return Ok(ExperimentalChildOutcome::Completed);
                    }
                    return Err(anyhow!("publisher exited unsuccessfully"));
                }
            }
            // An actual permission failure cannot be treated as termination.
            // Retain the fence while awaiting natural exit of this exact owned child.
            if !simulate_stop_failure {
                let _ = child.start_kill();
            }
            if let Some(signal) = stop_observed {
                let _ = signal.send(());
            }
            match child.wait().await {
                Ok(_) => Ok(ExperimentalChildOutcome::CancelledAndReaped),
                Err(_) => {
                    // Unknown child state: keep the fence rather than asserting
                    // termination. Process shutdown still leaves its journal dirty.
                    std::future::pending::<()>().await;
                    unreachable!()
                }
            }
        });
        let result = owner
            .await
            .map_err(|_| anyhow!("publisher lifecycle task failed"))?;
        drop(caller_alive);
        result
    }

    // Isolated protocol experiment, never called by production. The caller's
    // expected UID is injectable solely for same-user fixtures; it is not an
    // authentication policy. The real protected publisher and durable witness
    // remain unimplemented.
    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct ExperimentalMutation {
        operation_id: Uuid,
        expected: u64,
        replacement: u64,
    }

    async fn experimental_control_mutation(
        f: &Fixture,
        mut stream: tokio::net::UnixStream,
        expected_uid: u32,
        current: &std::sync::Mutex<u64>,
        inject: impl Fn(&str) -> Result<()>,
    ) -> Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        ensure!(
            stream.peer_cred()?.uid() == expected_uid,
            "unauthorized kernel peer"
        );
        let request: ExperimentalMutation =
            native_memory::authority_control::read(&mut stream).await?;
        ensure!(
            request.replacement
                == request
                    .expected
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("overflow"))?,
            "invalid revision"
        );
        inject("before_fence")?;
        let mut fence = f.authority.fence().await?;
        let receipt = f.dir.join(format!("receipt-{}", request.operation_id));
        if *current.lock().unwrap() != request.expected || receipt.exists() {
            fence.finish()?;
            return Err(anyhow!("stale or replayed mutation"));
        }
        // This record is an intent, not evidence of completion. Any error
        // after creation leaves the real hotel fence dirty.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&receipt)?;
        writeln!(file, "{}:{}", request.expected, request.replacement)?;
        file.sync_all()?;
        std::fs::File::open(&f.dir)?.sync_all()?;
        inject("intent")?;
        *current.lock().unwrap() = request.replacement;
        inject("mutation")?;
        let completed = f.dir.join(format!("completed-{}", request.operation_id));
        let mut completion = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(completed)?;
        writeln!(completion, "{}:{}", request.expected, request.replacement)?;
        completion.sync_all()?;
        std::fs::File::open(&f.dir)?.sync_all()?;
        inject("completion")?;
        fence.finish()?;
        native_memory::authority_control::write(&mut stream, &true).await?;
        Ok(())
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

    fn install_synthetic_snapshot_source(
        f: &mut Fixture,
    ) -> Arc<std::sync::Mutex<native_memory::grant_snapshot_candidate::Snapshot>> {
        use native_memory::grant_snapshot_candidate::{
            ExpectedBinding, Snapshot, validate_snapshot_bytes,
        };
        let response_key = ed25519_dalek::SigningKey::from_bytes(&[8; 32])
            .verifying_key()
            .to_bytes();
        let issuer_key = f.policy.issuers["test-ingress"].clone();
        let snapshot = Snapshot {
            schema: 1,
            authority_id: "hotel".into(),
            generation: 1,
            grant_id: "test".into(),
            grant: f.policy.grants["test"].clone(),
            issuer_key: issuer_key.clone(),
            response_verifier: native_memory::response::ResponseVerifier {
                destination: "hotel".into(),
                runner: "runner".into(),
                key_id: "result".into(),
                public_key: response_key,
            },
            oauth_issuer: "https://issuer.example".into(),
            oauth_resource: "https://memory.example/native/mcp".into(),
            oauth_metadata_url:
                "https://memory.example/.well-known/oauth-protected-resource/native/mcp".into(),
            authority_socket: "/run/synthetic/authority.sock".into(),
            oauth_client_id: "dot".into(),
            oauth_subject: "synthetic".into(),
            oauth_grant_version: "0:7".into(),
        };
        let current = Arc::new(std::sync::Mutex::new(snapshot));
        let source = current.clone();
        let loader = Arc::new(move || {
            let snapshot = source.lock().unwrap();
            let binding = ExpectedBinding {
                authority: "hotel",
                grant_id: "test",
                owner: "synthetic",
                namespace: "synthetic",
                destination: "hotel",
                runner: "runner",
                ingress_issuer: "test-ingress",
                reply_node: "hotel",
                reply_guest: "membrane",
                issuer_key_id: "key",
                issuer_public_key: issuer_key.public_key,
                response_key_id: "result",
                response_public_key: response_key,
                oauth_issuer: "https://issuer.example",
                oauth_resource: "https://memory.example/native/mcp",
                oauth_metadata_url: "https://memory.example/.well-known/oauth-protected-resource/native/mcp",
                authority_socket: Path::new("/run/synthetic/authority.sock"),
                oauth_client_id: "dot",
                oauth_subject: "synthetic",
                // Deliberately adversarial fixture: source may validate against
                // a different binding; the hotel must enforce its own binding.
                oauth_grant_version: &snapshot.oauth_grant_version,
            };
            let bytes = serde_json::to_vec(&*snapshot)?;
            validate_snapshot_bytes(&bytes, &binding).map_err(|e| anyhow!("snapshot rejected: {e}"))
        });
        Arc::get_mut(&mut f.authority)
            .unwrap()
            .experimental_set_snapshot_source(loader)
            .unwrap();
        current
    }

    #[tokio::test]
    async fn snapshot_guard_orders_revocation_and_denies_committed_result_release() {
        let mut f = Fixture::new();
        let current = install_synthetic_snapshot_source(&mut f);
        let handle = f.admit().await;
        let lease = f
            .authority
            .reserve(handle, f.receiver.clone(), f.request.binding())
            .await
            .unwrap();
        let authority = f.authority.clone();
        let changing = current.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let change = tokio::spawn(async move {
            let _ = started.send(());
            authority
                .issuer_change(async {
                    let mut snapshot = changing.lock().unwrap();
                    snapshot.generation += 1;
                    snapshot.grant.revision += 1;
                    snapshot.grant.revoked = true;
                    Ok(())
                })
                .await
        });
        ready.await.unwrap();
        assert!(!change.is_finished());
        lease
            .validate_before_commit(f.request.binding())
            .await
            .unwrap();
        lease.finish(true).await.unwrap();
        change.await.unwrap().unwrap();
        let published = AtomicBool::new(false);
        assert!(
            f.authority
                .release(handle, &f.origin, f.request.binding(), || {
                    published.store(true, Ordering::SeqCst);
                    Ok(())
                })
                .await
                .is_err()
        );
        assert!(!published.load(Ordering::SeqCst));
        assert!(f.authority.entries.lock().await[&handle].committed);
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"clean\n"
        );
    }

    #[tokio::test]
    async fn snapshot_guard_denies_changed_or_expired_before_commit() {
        let mut f = Fixture::new();
        let current = install_synthetic_snapshot_source(&mut f);
        let handle = f.admit().await;
        current.lock().unwrap().grant.revoked = true;
        assert!(
            f.authority
                .reserve(handle, f.receiver.clone(), f.request.binding())
                .await
                .is_err()
        );
        let mut f = Fixture::new();
        let current = install_synthetic_snapshot_source(&mut f);
        current.lock().unwrap().grant.expires_at = 105;
        let handle = f.admit().await;
        f.clock.store(105, Ordering::SeqCst);
        assert!(
            f.authority
                .reserve(handle, f.receiver.clone(), f.request.binding())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn snapshot_guard_enforces_hotel_binding_and_fails_closed_on_source_error() {
        let mut f = Fixture::new();
        let current = install_synthetic_snapshot_source(&mut f);
        current.lock().unwrap().oauth_grant_version = "0:8".into();
        let error = f
            .authority
            .admit(&f.origin, "a".repeat(43), &f.request)
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "snapshot OAuth binding mismatch");
        current.lock().unwrap().oauth_grant_version = "0:7".into();
        let handle = f.admit().await;
        current.lock().unwrap().schema = 2;
        assert!(
            f.authority
                .reserve(handle, f.receiver.clone(), f.request.binding())
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"clean\n"
        );
    }

    #[tokio::test]
    async fn snapshot_guard_allows_unchanged_authority_to_release() {
        let mut f = Fixture::new();
        let _current = install_synthetic_snapshot_source(&mut f);
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
    }

    #[tokio::test]
    async fn snapshot_handshake_requires_enabled_guard_exact_digest_and_runner() {
        use native_memory::authority_control::{Reply, Request, read, write};
        for (configured, matching, guest, accepted) in [
            (false, true, "runner", false),
            (true, false, "runner", false),
            (true, true, "membrane", false),
            (true, true, "runner", true),
        ] {
            let mut f = Fixture::new();
            if configured {
                install_synthetic_snapshot_source(&mut f);
            }
            let digest = if configured {
                f.authority.snapshot_source.as_ref().unwrap()()
                    .unwrap()
                    .digest()
            } else {
                [1; 32]
            };
            let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
            let owner = f.authority.clone();
            let policy = f.policy.clone();
            let server = tokio::spawn(async move {
                owner
                    .control(server, Arc::new(move || Ok(policy.clone())))
                    .await
            });
            write(
                &mut client,
                &Request::SnapshotReady {
                    guest: guest.into(),
                    digest: if matching { digest } else { [0; 32] },
                },
            )
            .await
            .unwrap();
            let reply = read::<Reply>(&mut client).await;
            let server_result = server.await.unwrap();
            assert_eq!(matches!(reply, Ok(Reply::SnapshotReady)), accepted);
            assert_eq!(server_result.is_ok(), accepted);
            assert_eq!(
                std::fs::read(f.dir.join("uncertainty")).unwrap(),
                b"clean\n"
            );
        }
    }

    #[tokio::test]
    async fn synthetic_publisher_session_holds_fence_and_requires_completion_receipt() {
        use native_memory::authority_control::{Reply, Request, read, write};
        for mode in [
            "success",
            "missing_receipt",
            "disconnect",
            "wrong_operation",
            "replay",
            "lost_ack",
            "replaced_generation",
        ] {
            let mut f = Fixture::new();
            let current = install_synthetic_snapshot_source(&mut f);
            let source = f.authority.snapshot_source.as_ref().unwrap().clone();
            let old = source().unwrap().digest();
            let previous = current.lock().unwrap().clone();
            let mut next = previous.clone();
            next.generation += 1;
            next.grant.revision += 1;
            next.grant.revoked = true;
            *current.lock().unwrap() = next.clone();
            let new = source().unwrap().digest();
            *current.lock().unwrap() = previous;
            let receipt = Arc::new(AtomicBool::new(false));
            let check = receipt.clone();
            let receipt_checked = Arc::new(AtomicBool::new(false));
            let observed_receipt = receipt_checked.clone();
            let id = Uuid::new_v4();
            let uid = unsafe { libc::geteuid() };
            f.authority
                .launches
                .attach_new(
                    "publisher",
                    uid,
                    LaunchPrincipal {
                        stable_agent_id: "publisher".into(),
                        roles: BTreeSet::new(),
                    },
                    Arc::new(Process(f.alive.clone())),
                )
                .unwrap();
            let consumer = f.authority.launches.target("hotel", "publisher").unwrap();
            Arc::get_mut(&mut f.authority)
                .unwrap()
                .experimental_set_publisher_plan(PublisherPlan {
                    guest: "publisher".into(),
                    consumer,
                    operation_id: id,
                    old_digest: old,
                    new_digest: new,
                    deadline: 200,
                    receipt: Arc::new(move |operation, digest| {
                        observed_receipt.store(true, Ordering::SeqCst);
                        Ok(operation == id && digest == new && check.load(Ordering::SeqCst))
                    }),
                })
                .unwrap();
            if mode == "replaced_generation" {
                f.authority
                    .launches
                    .attach(
                        "publisher",
                        uid,
                        LaunchPrincipal {
                            stable_agent_id: "publisher".into(),
                            roles: BTreeSet::new(),
                        },
                        Arc::new(Process(f.alive.clone())),
                    )
                    .unwrap();
            }
            let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
            let owner = f.authority.clone();
            let policy = f.policy.clone();
            let server = tokio::spawn(async move {
                owner
                    .test_control_with_publisher_uid(
                        server,
                        Arc::new(move || Ok(policy.clone())),
                        uid,
                    )
                    .await
            });
            write(
                &mut client,
                &Request::BeginSnapshot {
                    guest: "publisher".into(),
                    operation_id: if mode == "wrong_operation" {
                        Uuid::new_v4().to_string()
                    } else {
                        id.to_string()
                    },
                },
            )
            .await
            .unwrap();
            if mode == "wrong_operation" || mode == "replaced_generation" {
                assert!(read::<Reply>(&mut client).await.is_err());
                assert!(server.await.unwrap().is_err());
                assert_eq!(
                    std::fs::read(f.dir.join("uncertainty")).unwrap(),
                    b"clean\n"
                );
                continue;
            }
            assert!(matches!(read::<Reply>(&mut client).await, Ok(Reply::Ready)));
            if mode == "disconnect" {
                drop(client);
                assert!(server.await.unwrap().is_err());
                assert_eq!(
                    std::fs::read(f.dir.join("uncertainty")).unwrap(),
                    b"dirty\n"
                );
                continue;
            }
            *current.lock().unwrap() = next;
            if mode == "success" || mode == "replay" || mode == "lost_ack" {
                receipt.store(true, Ordering::SeqCst);
            }
            write(
                &mut client,
                &Request::FinishSnapshot {
                    operation_id: id.to_string(),
                    digest: new,
                },
            )
            .await
            .unwrap();
            if mode == "lost_ack" {
                drop(client);
                assert_eq!(
                    server.await.unwrap().unwrap_err().to_string(),
                    "Unavailable"
                );
                assert!(receipt_checked.load(Ordering::SeqCst));
                assert_eq!(source().unwrap().digest(), new);
                assert_eq!(
                    std::fs::read(f.dir.join("uncertainty")).unwrap(),
                    b"clean\n"
                );
                continue;
            }
            let reply = read::<Reply>(&mut client).await;
            let result = server.await.unwrap();
            let succeeded = mode == "success" || mode == "replay";
            assert_eq!(matches!(reply, Ok(Reply::Finished)), succeeded);
            assert_eq!(result.is_ok(), succeeded);
            assert_eq!(
                std::fs::read(f.dir.join("uncertainty")).unwrap(),
                if succeeded { b"clean\n" } else { b"dirty\n" }
            );
            if mode == "replay" {
                let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
                let owner = f.authority.clone();
                let policy = f.policy.clone();
                let server = tokio::spawn(async move {
                    owner
                        .test_control_with_publisher_uid(
                            server,
                            Arc::new(move || Ok(policy.clone())),
                            uid,
                        )
                        .await
                });
                write(
                    &mut client,
                    &Request::BeginSnapshot {
                        guest: "publisher".into(),
                        operation_id: id.to_string(),
                    },
                )
                .await
                .unwrap();
                assert!(read::<Reply>(&mut client).await.is_err());
                assert_eq!(
                    server.await.unwrap().unwrap_err().to_string(),
                    "publisher operation already completed"
                );
                assert_eq!(
                    std::fs::read(f.dir.join("uncertainty")).unwrap(),
                    b"clean\n"
                );
            }
        }
    }

    #[tokio::test]
    async fn publisher_control_rejects_nonroot_even_when_guest_is_registered() {
        use native_memory::authority_control::{Reply, Request, read, write};
        let uid = unsafe { libc::geteuid() };
        assert_ne!(uid, 0, "requires non-root test process");
        let mut f = Fixture::new();
        install_synthetic_snapshot_source(&mut f);
        let id = Uuid::new_v4();
        f.authority
            .launches
            .attach_new(
                "publisher",
                uid,
                LaunchPrincipal {
                    stable_agent_id: "publisher".into(),
                    roles: BTreeSet::new(),
                },
                Arc::new(Process(f.alive.clone())),
            )
            .unwrap();
        let consumer = f.authority.launches.target("hotel", "publisher").unwrap();
        Arc::get_mut(&mut f.authority)
            .unwrap()
            .experimental_set_publisher_plan(PublisherPlan {
                guest: "publisher".into(),
                consumer,
                operation_id: id,
                old_digest: [1; 32],
                new_digest: [2; 32],
                deadline: 200,
                receipt: Arc::new(|_, _| Ok(true)),
            })
            .unwrap();
        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        let owner = f.authority.clone();
        let policy = f.policy.clone();
        let server = tokio::spawn(async move {
            owner
                .control(server, Arc::new(move || Ok(policy.clone())))
                .await
        });
        write(
            &mut client,
            &Request::BeginSnapshot {
                guest: "publisher".into(),
                operation_id: id.to_string(),
            },
        )
        .await
        .unwrap();
        assert!(read::<Reply>(&mut client).await.is_err());
        assert_eq!(
            server.await.unwrap().unwrap_err().to_string(),
            "unknown publisher peer"
        );
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"clean\n"
        );
    }

    #[tokio::test]
    async fn publisher_cannot_receive_ready_after_expiring_while_waiting_for_fence() {
        use native_memory::authority_control::{Reply, Request, read, write};
        let uid = unsafe { libc::geteuid() };
        let mut f = Fixture::new();
        install_synthetic_snapshot_source(&mut f);
        let calls = Arc::new(AtomicU64::new(0));
        let tick = calls.clone();
        let now = f.clock.clone();
        Arc::get_mut(&mut f.authority).unwrap().clock = Arc::new(move || {
            tick.fetch_add(1, Ordering::SeqCst);
            now.load(Ordering::SeqCst)
        });
        let id = Uuid::new_v4();
        f.authority
            .launches
            .attach_new(
                "publisher",
                uid,
                LaunchPrincipal {
                    stable_agent_id: "publisher".into(),
                    roles: BTreeSet::new(),
                },
                Arc::new(Process(f.alive.clone())),
            )
            .unwrap();
        let consumer = f.authority.launches.target("hotel", "publisher").unwrap();
        Arc::get_mut(&mut f.authority)
            .unwrap()
            .experimental_set_publisher_plan(PublisherPlan {
                guest: "publisher".into(),
                consumer,
                operation_id: id,
                old_digest: [1; 32],
                new_digest: [2; 32],
                deadline: 200,
                receipt: Arc::new(|_, _| Ok(true)),
            })
            .unwrap();
        let handle = f.admit().await;
        let lease = f
            .authority
            .reserve(handle, f.receiver.clone(), f.request.binding())
            .await
            .unwrap();
        calls.store(0, Ordering::SeqCst);
        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        let owner = f.authority.clone();
        let policy = f.policy.clone();
        let server = tokio::spawn(async move {
            owner
                .test_control_with_publisher_uid(server, Arc::new(move || Ok(policy.clone())), uid)
                .await
        });
        write(
            &mut client,
            &Request::BeginSnapshot {
                guest: "publisher".into(),
                operation_id: id.to_string(),
            },
        )
        .await
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        f.clock.store(200, Ordering::SeqCst);
        lease.finish(false).await.unwrap();
        assert!(read::<Reply>(&mut client).await.is_err());
        assert_eq!(
            server.await.unwrap().unwrap_err().to_string(),
            "publisher deadline elapsed before permission"
        );
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"clean\n"
        );
    }

    #[tokio::test]
    async fn experimental_direct_child_is_reaped_before_fence_release() {
        let f = Fixture::new();
        let child = tokio::process::Command::new("/usr/bin/true");
        assert_eq!(
            experimental_owned_publisher_lifecycle(
                f.authority.clone(),
                child,
                async { Ok(()) },
                false,
                None,
                None,
                None,
                None
            )
            .await
            .unwrap(),
            ExperimentalChildOutcome::Completed
        );
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"clean\n"
        );
        let g = Fixture::new();
        let mut child = tokio::process::Command::new("/bin/sleep");
        child.arg("30");
        assert_eq!(
            experimental_owned_publisher_lifecycle(
                g.authority.clone(),
                child,
                std::future::pending(),
                false,
                None,
                None,
                None,
                None
            )
            .await
            .unwrap(),
            ExperimentalChildOutcome::CancelledAndReaped
        );
        assert_eq!(
            std::fs::read(g.dir.join("uncertainty")).unwrap(),
            b"dirty\n"
        );
        assert!(g.authority.fence().await.is_err());
    }

    #[tokio::test]
    async fn experimental_panicking_session_still_stops_and_reaps_owned_child() {
        let f = Fixture::new();
        let mut child = tokio::process::Command::new("/bin/sleep");
        child.arg("30");
        let result = experimental_owned_publisher_lifecycle(
            f.authority.clone(),
            child,
            async { panic!("synthetic publisher session panic") },
            false,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(result, ExperimentalChildOutcome::CancelledAndReaped);
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"dirty\n"
        );
        assert!(f.authority.fence().await.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn experimental_non_yielding_session_keeps_fence_until_it_quiesces() {
        struct ReleaseOnDrop(Arc<AtomicBool>);
        impl Drop for ReleaseOnDrop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let f = Fixture::new();
        let mut command = tokio::process::Command::new("/bin/sleep");
        command.arg("30");
        let flag = Arc::new(AtomicBool::new(false));
        let _release = ReleaseOnDrop(flag.clone());
        let worker = flag.clone();
        let (started, active) = tokio::sync::oneshot::channel();
        let (stopping, mut stopped) = tokio::sync::oneshot::channel();
        let (abort_signal, abort_seen) = tokio::sync::oneshot::channel();
        let authority = f.authority.clone();
        let task = tokio::spawn(async move {
            experimental_owned_publisher_lifecycle(
                authority,
                command,
                async move {
                    let _ = started.send(());
                    while !worker.load(Ordering::SeqCst) {
                        std::thread::yield_now();
                    }
                    Ok(())
                },
                false,
                Some(stopping),
                None,
                None,
                Some(abort_signal),
            )
            .await
        });
        active.await.unwrap();
        abort_seen.await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(80), f.authority.fence())
                .await
                .is_err()
        );
        assert!(matches!(
            stopped.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        flag.store(true, Ordering::SeqCst);
        stopped.await.unwrap();
        assert_eq!(
            task.await.unwrap().unwrap(),
            ExperimentalChildOutcome::CancelledAndReaped
        );
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"dirty\n"
        );
    }

    #[tokio::test]
    async fn existing_supervisor_stop_waits_for_fence_before_killing_child() {
        use super::super::protected_local_guest::ProtectedLocalGuest;
        let f = Fixture::new();
        let mut command = tokio::process::Command::new("/bin/sleep");
        command.arg("30");
        let guest = ProtectedLocalGuest::spawn(
            &mut command,
            f.authority.launches.clone(),
            "synthetic-publisher",
            unsafe { libc::geteuid() },
            LaunchPrincipal {
                stable_agent_id: "synthetic-publisher".into(),
                roles: BTreeSet::new(),
            },
        )
        .unwrap()
        .with_native_authority(f.authority.clone())
        .unwrap();
        let mut held = f.authority.fence().await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), guest.stop())
                .await
                .is_err()
        );
        assert!(
            !guest.exited().await.unwrap(),
            "the existing stop cannot kill while retirement waits for the fence"
        );
        held.finish().unwrap();
        drop(held);
        guest.stop().await.unwrap();
    }

    #[tokio::test]
    async fn synthetic_fenced_stop_reaps_owned_child_without_reacquiring_fence() {
        use super::super::protected_local_guest::ProtectedLocalGuest;
        let f = Fixture::new();
        let mut command = tokio::process::Command::new("/bin/sleep");
        command.arg("30");
        let guest = ProtectedLocalGuest::spawn(
            &mut command,
            f.authority.launches.clone(),
            "synthetic-publisher",
            unsafe { libc::geteuid() },
            LaunchPrincipal {
                stable_agent_id: "synthetic-publisher".into(),
                roles: BTreeSet::new(),
            },
        )
        .unwrap()
        .with_native_authority(f.authority.clone())
        .unwrap();
        let mut held = f.authority.fence().await.unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            guest.experimental_stop_while_fenced(&mut held),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            f.authority
                .launches
                .target("hotel", "synthetic-publisher")
                .is_err()
        );
        assert!(
            f.authority
                .launches
                .attach_new(
                    "synthetic-publisher",
                    unsafe { libc::geteuid() },
                    LaunchPrincipal {
                        stable_agent_id: "replacement".into(),
                        roles: BTreeSet::new()
                    },
                    Arc::new(Process(f.alive.clone()))
                )
                .is_ok(),
            "the prior generation must actually be removed"
        );
        held.finish().unwrap();
        // Same-UID owned child only; root signaling, fence provenance and
        // production use of this test-only method remain unqualified.
    }

    #[tokio::test]
    async fn experimental_failed_stop_retains_fence_until_owned_child_exits() {
        let f = Fixture::new();
        let mut child = tokio::process::Command::new("/bin/cat");
        child
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null());
        let (stdin_tx, stdin_rx) = tokio::sync::oneshot::channel();
        let (stopping, observed) = tokio::sync::oneshot::channel();
        let authority = f.authority.clone();
        let task = tokio::spawn(async move {
            experimental_owned_publisher_lifecycle(
                authority,
                child,
                std::future::pending(),
                true,
                Some(stopping),
                Some(stdin_tx),
                None,
                None,
            )
            .await
        });
        let input = stdin_rx.await.unwrap();
        observed.await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), f.authority.fence())
                .await
                .is_err(),
            "fence must stay held while child remains alive"
        );
        drop(input); // EOF releases exactly this owned test child.
        assert_eq!(
            task.await.unwrap().unwrap(),
            ExperimentalChildOutcome::CancelledAndReaped
        );
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"dirty\n"
        );
        assert!(f.authority.fence().await.is_err());
    }

    #[tokio::test]
    async fn experimental_caller_cancellation_does_not_drop_child_owner() {
        let f = Fixture::new();
        let mut child = tokio::process::Command::new("/bin/cat");
        child
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null());
        let (stdin_tx, stdin_rx) = tokio::sync::oneshot::channel();
        let (stopping, observed) = tokio::sync::oneshot::channel();
        let authority = f.authority.clone();
        let outer = tokio::spawn(async move {
            experimental_owned_publisher_lifecycle(
                authority,
                child,
                std::future::pending(),
                true,
                Some(stopping),
                Some(stdin_tx),
                None,
                None,
            )
            .await
        });
        let input = stdin_rx.await.unwrap();
        observed.await.unwrap();
        outer.abort();
        assert!(outer.await.unwrap_err().is_cancelled());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), f.authority.fence())
                .await
                .is_err()
        );
        drop(input);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), f.authority.fence())
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"dirty\n"
        );
    }

    #[tokio::test]
    async fn experimental_cancel_while_waiting_does_not_spawn_child() {
        let f = Fixture::new();
        let mut held = f.authority.fence().await.unwrap();
        let mut command = tokio::process::Command::new("/bin/cat");
        command
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null());
        let (stdin_tx, stdin_rx) = tokio::sync::oneshot::channel();
        let (started, observed) = tokio::sync::oneshot::channel();
        let authority = f.authority.clone();
        let outer = tokio::spawn(async move {
            experimental_owned_publisher_lifecycle(
                authority,
                command,
                std::future::pending(),
                true,
                None,
                Some(stdin_tx),
                Some(started),
                None,
            )
            .await
        });
        observed.await.unwrap();
        outer.abort();
        assert!(outer.await.unwrap_err().is_cancelled());
        held.finish().unwrap();
        drop(held);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), stdin_rx)
                .await
                .unwrap()
                .is_err(),
            "cancelled waiter must not spawn a child"
        );
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"clean\n"
        );
    }

    #[tokio::test]
    async fn synthetic_file_snapshot_composes_hotel_fence_and_sqlite_runner() {
        // Same-user fixture, not the root-owned publisher or a guest process.
        let mut f = Fixture::new();
        let current = install_synthetic_snapshot_source(&mut f);
        let path = f.dir.join("snapshot.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&*current.lock().unwrap()).unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let original_loader = f.authority.snapshot_source.as_ref().unwrap().clone();
        let source_path = path.clone();
        let source_value = current.clone();
        let file_loader: SnapshotSource = Arc::new(move || {
            let next: native_memory::grant_snapshot_candidate::Snapshot =
                serde_json::from_slice(&std::fs::read(&source_path)?)?;
            *source_value.lock().unwrap() = next;
            original_loader()
        });
        Arc::get_mut(&mut f.authority).unwrap().snapshot_source = Some(file_loader.clone());
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
        let db_path = f.dir.join("composed.sqlite");
        let mut runner =
            native_memory::Runner::open(&db_path, "hotel".into(), "runner".into()).unwrap();
        runner
            .execute_current(
                f.request.envelope(),
                || {
                    Ok((
                        file_loader()
                            .map_err(|_| native_memory::Error::Unavailable)?
                            .receiver_policy(),
                        101,
                    ))
                },
                &AtomicBool::new(false),
            )
            .unwrap();
        lease.finish(true).await.unwrap();
        let mut revoked = current.lock().unwrap().clone();
        revoked.generation += 1;
        revoked.grant.revision += 1;
        revoked.grant.revoked = true;
        let tmp = f.dir.join("snapshot.next");
        f.authority
            .issuer_change(async {
                std::fs::write(&tmp, serde_json::to_vec(&revoked)?)?;
                std::fs::rename(&tmp, &path)?;
                Ok(())
            })
            .await
            .unwrap();
        let emitted = AtomicBool::new(false);
        let error = f
            .authority
            .release(handle, &f.origin, f.request.binding(), || {
                emitted.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "snapshot changed since admission");
        assert!(!emitted.load(Ordering::SeqCst));
        drop(runner);
        let db = rusqlite::Connection::open(db_path).unwrap();
        let count: i64 = db
            .query_row("SELECT count(*) FROM observations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            count, 1,
            "an earlier committed write is not retroactively rolled back"
        );
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
    async fn experimental_static_grant_change_shares_real_commit_fence() {
        // This verifies only the ordering primitive with synthetic SQLite. It
        // does not expose an IPC mutation or establish filesystem custody.
        let f = Fixture::new();
        let handle = f.admit().await;
        let lease = f
            .authority
            .reserve(handle, f.receiver.clone(), f.request.binding())
            .await
            .unwrap();
        let policy = Arc::new(std::sync::Mutex::new(f.policy.clone()));
        let candidate = policy.clone();
        let authority = f.authority.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let change = tokio::spawn(async move {
            let _ = started.send(());
            authority
                .issuer_change(async {
                    let mut p = candidate.lock().unwrap();
                    let grant = p.grants.get_mut("test").unwrap();
                    grant.revision += 1;
                    grant.revoked = true;
                    Ok(())
                })
                .await
        });
        ready.await.unwrap();
        assert!(!change.is_finished());
        lease
            .validate_before_commit(f.request.binding())
            .await
            .unwrap();
        let mut runner = native_memory::Runner::open(
            &f.dir.join("grant-fence.sqlite"),
            "hotel".into(),
            "runner".into(),
        )
        .unwrap();
        let result = runner.execute_current(
            f.request.envelope(),
            || Ok((policy.lock().unwrap().clone(), 101)),
            &AtomicBool::new(false),
        );
        assert!(
            result.is_ok(),
            "already-reserved commit may complete before revocation"
        );
        lease.finish(true).await.unwrap();
        change.await.unwrap().unwrap();
        assert!(
            native_memory::VerifiedRequest::validate(
                f.request.envelope(),
                &policy.lock().unwrap(),
                "hotel",
                "runner",
                101
            )
            .is_err()
        );
        // Result-release integration must explicitly perform the equivalent
        // fresh snapshot check under the fence; it is not implemented here.
    }

    #[tokio::test]
    async fn experimental_control_binds_peer_revision_and_replay_to_hotel_fence() {
        let f = Fixture::new();
        let uid = unsafe { libc::geteuid() };
        let current = std::sync::Mutex::new(2);
        let id = Uuid::new_v4();
        for (expected_uid, request, succeeds) in [
            (
                uid.wrapping_add(1),
                ExperimentalMutation {
                    operation_id: Uuid::new_v4(),
                    expected: 2,
                    replacement: 3,
                },
                false,
            ),
            (
                uid,
                ExperimentalMutation {
                    operation_id: id,
                    expected: 2,
                    replacement: 3,
                },
                true,
            ),
            (
                uid,
                ExperimentalMutation {
                    operation_id: id,
                    expected: 3,
                    replacement: 4,
                },
                false,
            ),
            (
                uid,
                ExperimentalMutation {
                    operation_id: Uuid::new_v4(),
                    expected: 2,
                    replacement: 3,
                },
                false,
            ),
        ] {
            let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
            native_memory::authority_control::write(&mut client, &request)
                .await
                .unwrap();
            assert_eq!(
                experimental_control_mutation(&f, server, expected_uid, &current, |_| Ok(()))
                    .await
                    .is_ok(),
                succeeds
            );
            if succeeds {
                assert!(
                    native_memory::authority_control::read::<bool>(&mut client)
                        .await
                        .unwrap()
                );
            }
        }
        assert_eq!(*current.lock().unwrap(), 3);
        assert!(f.dir.join(format!("completed-{id}")).exists());
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"clean\n"
        );
        // State is in memory here: completion proves fixture ordering only, not
        // protected snapshot durability, crash recovery, or root separation.
    }

    #[tokio::test]
    async fn experimental_control_waits_for_existing_commit_reservation() {
        let f = Fixture::new();
        let handle = f.admit().await;
        let lease = f
            .authority
            .reserve(handle, f.receiver.clone(), f.request.binding())
            .await
            .unwrap();
        let current = std::sync::Mutex::new(2);
        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        native_memory::authority_control::write(
            &mut client,
            &ExperimentalMutation {
                operation_id: Uuid::new_v4(),
                expected: 2,
                replacement: 3,
            },
        )
        .await
        .unwrap();
        let reached = AtomicBool::new(false);
        let operation = experimental_control_mutation(
            &f,
            server,
            unsafe { libc::geteuid() },
            &current,
            |phase| {
                if phase == "before_fence" {
                    reached.store(true, Ordering::SeqCst);
                }
                Ok(())
            },
        );
        tokio::pin!(operation);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), &mut operation)
                .await
                .is_err()
        );
        assert!(reached.load(Ordering::SeqCst));
        assert_eq!(*current.lock().unwrap(), 2);
        lease.finish(true).await.unwrap();
        operation.await.unwrap();
        assert!(
            native_memory::authority_control::read::<bool>(&mut client)
                .await
                .unwrap()
        );
        assert_eq!(*current.lock().unwrap(), 3);
        assert!(f.authority.entries.lock().await[&handle].committed);
    }

    #[tokio::test]
    async fn experimental_control_interruption_and_lost_ack_are_distinct() {
        let uid = unsafe { libc::geteuid() };
        for phase in ["intent", "mutation", "completion"] {
            let f = Fixture::new();
            let current = std::sync::Mutex::new(2);
            let id = Uuid::new_v4();
            let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
            native_memory::authority_control::write(
                &mut client,
                &ExperimentalMutation {
                    operation_id: id,
                    expected: 2,
                    replacement: 3,
                },
            )
            .await
            .unwrap();
            let error = experimental_control_mutation(&f, server, uid, &current, |at| {
                ensure!(at != phase, "synthetic interruption");
                Ok(())
            })
            .await
            .unwrap_err();
            assert_eq!(error.to_string(), "synthetic interruption");
            assert_eq!(
                std::fs::read(f.dir.join("uncertainty")).unwrap(),
                b"dirty\n"
            );
            assert!(f.dir.join(format!("receipt-{id}")).exists());
            assert_eq!(
                f.dir.join(format!("completed-{id}")).exists(),
                phase == "completion"
            );
            assert_eq!(
                *current.lock().unwrap(),
                if phase == "intent" { 2 } else { 3 }
            );
        }
        let f = Fixture::new();
        let current = std::sync::Mutex::new(2);
        let id = Uuid::new_v4();
        let (mut client, server) = tokio::net::UnixStream::pair().unwrap();
        native_memory::authority_control::write(
            &mut client,
            &ExperimentalMutation {
                operation_id: id,
                expected: 2,
                replacement: 3,
            },
        )
        .await
        .unwrap();
        let client = std::sync::Mutex::new(Some(client));
        assert!(
            experimental_control_mutation(&f, server, uid, &current, |phase| {
                if phase == "completion" {
                    drop(client.lock().unwrap().take());
                }
                Ok(())
            })
            .await
            .is_err()
        );
        assert_eq!(*current.lock().unwrap(), 3);
        assert!(f.dir.join(format!("completed-{id}")).exists());
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"clean\n"
        );
        // Delivery is unknown, but completion is recorded for this same-user,
        // in-memory fixture. It is not a production recovery/status protocol.
    }

    #[tokio::test]
    async fn experimental_checked_release_distinguishes_denial_from_uncertain_effect() {
        let f = Fixture::new();
        let handle = f.admit().await;
        f.authority
            .reserve(handle, f.receiver.clone(), f.request.binding())
            .await
            .unwrap()
            .finish(true)
            .await
            .unwrap();
        let mut revoked = f.policy.clone();
        revoked.grants.get_mut("test").unwrap().revoked = true;
        let attempted = AtomicBool::new(false);
        let denial = experimental_checked_release(
            &f,
            handle,
            || Ok(revoked),
            || {
                attempted.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert_eq!(denial.to_string(), "static grant denied before publication");
        assert!(!attempted.load(Ordering::SeqCst));
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"clean\n"
        );
        assert!(f.authority.entries.lock().await[&handle].committed);

        let failure = experimental_checked_release(
            &f,
            handle,
            || Err(anyhow!("snapshot unavailable")),
            || {
                attempted.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert_eq!(failure.to_string(), "snapshot unavailable");
        assert!(!attempted.load(Ordering::SeqCst));
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"clean\n"
        );

        let f = Fixture::new();
        let handle = f.admit().await;
        f.authority
            .reserve(handle, f.receiver.clone(), f.request.binding())
            .await
            .unwrap()
            .finish(true)
            .await
            .unwrap();
        let failure = experimental_checked_release(
            &f,
            handle,
            || Ok(f.policy.clone()),
            || {
                attempted.store(true, Ordering::SeqCst);
                Err(anyhow!("publication outcome unknown"))
            },
        )
        .await
        .unwrap_err();
        assert_eq!(failure.to_string(), "publication outcome unknown");
        assert!(attempted.load(Ordering::SeqCst));
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"dirty\n"
        );
        let retried = AtomicBool::new(false);
        assert!(
            experimental_checked_release(
                &f,
                handle,
                || Ok(f.policy.clone()),
                || {
                    retried.store(true, Ordering::SeqCst);
                    Ok(())
                }
            )
            .await
            .is_err()
        );
        assert!(!retried.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn experimental_checked_release_checks_fresh_expiry_and_success() {
        let f = Fixture::new();
        let handle = f.admit().await;
        f.authority
            .reserve(handle, f.receiver.clone(), f.request.binding())
            .await
            .unwrap()
            .finish(true)
            .await
            .unwrap();
        let mut expiring = f.policy.clone();
        expiring.grants.get_mut("test").unwrap().expires_at = 102;
        f.clock.store(102, Ordering::SeqCst);
        let attempted = AtomicBool::new(false);
        let denial = experimental_checked_release(
            &f,
            handle,
            || Ok(expiring),
            || {
                attempted.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .unwrap_err();
        assert_eq!(denial.to_string(), "static grant denied before publication");
        assert!(!attempted.load(Ordering::SeqCst));
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"clean\n"
        );
        let g = Fixture::new();
        let handle = g.admit().await;
        g.authority
            .reserve(handle, g.receiver.clone(), g.request.binding())
            .await
            .unwrap()
            .finish(true)
            .await
            .unwrap();
        experimental_checked_release(&g, handle, || Ok(g.policy.clone()), || Ok(()))
            .await
            .unwrap();
        assert!(g.authority.entries.lock().await.is_empty());
        assert_eq!(
            std::fs::read(g.dir.join("uncertainty")).unwrap(),
            b"clean\n"
        );
    }

    #[tokio::test]
    async fn experimental_static_revocation_before_release_blocks_publication() {
        // Test-only composition: production release does not yet perform this
        // snapshot check. A denial must not be interpreted as rollback of a
        // transaction that already committed.
        let f = Fixture::new();
        let handle = f.admit().await;
        f.authority
            .reserve(handle, f.receiver.clone(), f.request.binding())
            .await
            .unwrap()
            .finish(true)
            .await
            .unwrap();
        let policy = Arc::new(std::sync::Mutex::new(f.policy.clone()));
        let candidate = policy.clone();
        f.authority
            .issuer_change(async {
                let mut p = candidate.lock().unwrap();
                let grant = p.grants.get_mut("test").unwrap();
                grant.revision += 1;
                grant.revoked = true;
                Ok(())
            })
            .await
            .unwrap();
        let entered = AtomicBool::new(false);
        let published = AtomicBool::new(false);
        let result = f
            .authority
            .release(handle, &f.origin, f.request.binding(), || {
                entered.store(true, Ordering::SeqCst);
                native_memory::VerifiedRequest::validate(
                    f.request.envelope(),
                    &policy.lock().unwrap(),
                    "hotel",
                    "runner",
                    101,
                )
                .map_err(|_| anyhow!("static grant denied"))?;
                published.store(true, Ordering::SeqCst);
                Ok(())
            })
            .await;
        assert_eq!(result.unwrap_err().to_string(), "static grant denied");
        assert!(entered.load(Ordering::SeqCst));
        assert!(!published.load(Ordering::SeqCst));
        assert!(f.authority.entries.lock().await[&handle].committed);
        // The existing release callback treats any error as an uncertain
        // publication and leaves the journal dirty. A production design must
        // distinguish a pre-publication denial from an uncertain effect.
        assert_eq!(
            std::fs::read(f.dir.join("uncertainty")).unwrap(),
            b"dirty\n"
        );
    }

    #[tokio::test]
    async fn experimental_static_revocation_first_denies_later_runner_commit() {
        let f = Fixture::new();
        let handle = f.admit().await;
        let policy = Arc::new(std::sync::Mutex::new(f.policy.clone()));
        let candidate = policy.clone();
        f.authority
            .issuer_change(async {
                let mut p = candidate.lock().unwrap();
                let grant = p.grants.get_mut("test").unwrap();
                grant.revision += 1;
                grant.revoked = true;
                Ok(())
            })
            .await
            .unwrap();
        // Current hotel reservations do not themselves validate static grants;
        // the runner's existing policy check is the denial demonstrated here.
        let lease = f
            .authority
            .reserve(handle, f.receiver.clone(), f.request.binding())
            .await
            .unwrap();
        let mut runner = native_memory::Runner::open(
            &f.dir.join("grant-denial.sqlite"),
            "hotel".into(),
            "runner".into(),
        )
        .unwrap();
        let result = runner.execute_current(
            f.request.envelope(),
            || Ok((policy.lock().unwrap().clone(), 101)),
            &AtomicBool::new(false),
        );
        assert_eq!(result, Err(native_memory::Error::Denied));
        lease.finish(false).await.unwrap();
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
}
