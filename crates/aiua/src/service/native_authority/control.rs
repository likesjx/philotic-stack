//! Private control adapters owned by the hotel, not a second supervisor.
use super::*;
use native_memory::authority_control::{Reply, Request, read, write};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};

impl NativeAuthority {
    pub async fn serve_control(
        self: Arc<Self>,
        listener: UnixListener,
        policy: Arc<dyn Fn() -> Result<native_memory::Policy> + Send + Sync>,
    ) -> Result<()> {
        let capacity = Arc::new(tokio::sync::Semaphore::new(32));
        loop {
            let slot = capacity.clone().acquire_owned().await?;
            let (stream, _) = listener.accept().await?;
            let owner = self.clone();
            let policy = policy.clone();
            tokio::spawn(async move {
                let _slot = slot;
                let outcome = owner.control(stream, policy).await;
                #[cfg(test)]
                eprintln!("synthetic native control completed: {}", outcome.is_ok());
                let _ = outcome;
            });
        }
    }
    pub(super) async fn control(
        self: Arc<Self>,
        stream: UnixStream,
        policy: Arc<dyn Fn() -> Result<native_memory::Policy> + Send + Sync>,
    ) -> Result<()> {
        self.control_with_publisher_uid(stream, policy, 0).await
    }
    #[cfg(test)]
    pub(super) async fn test_control_with_publisher_uid(
        self: Arc<Self>,
        stream: UnixStream,
        policy: Arc<dyn Fn() -> Result<native_memory::Policy> + Send + Sync>,
        uid: u32,
    ) -> Result<()> {
        self.control_with_publisher_uid(stream, policy, uid).await
    }
    // The production caller always passes UID 0. Tests may inject their own
    // UID to exercise framing without pretending to prove privilege separation.
    async fn control_with_publisher_uid(
        self: Arc<Self>,
        mut stream: UnixStream,
        policy: Arc<dyn Fn() -> Result<native_memory::Policy> + Send + Sync>,
        publisher_uid: u32,
    ) -> Result<()> {
        let first: Request = read(&mut stream).await?;
        let guest = match &first {
            Request::Admit { guest, .. }
            | Request::Reserve { guest, .. }
            | Request::Release { guest, .. }
            | Request::Cancel { guest, .. }
            | Request::SnapshotReady { guest, .. }
            | Request::BeginSnapshot { guest, .. } => guest,
            _ => return Err(anyhow!("invalid native control start")),
        };
        if matches!(&first, Request::BeginSnapshot { .. }) {
            ensure!(
                stream.peer_cred()?.uid() == publisher_uid
                    && self
                        .publisher_plan
                        .as_ref()
                        .is_some_and(|plan| plan.guest == *guest),
                "unknown publisher peer"
            );
        } else {
            ensure!(
                [self.membrane.as_str(), self.runner.as_str()].contains(&guest.as_str()),
                "unknown native peer"
            );
        }
        let peer = self.launches.authenticate(&self.hotel, &stream, guest)?;
        match first {
            Request::BeginSnapshot { operation_id, .. } => {
                let plan = self
                    .publisher_plan
                    .as_ref()
                    .ok_or_else(|| anyhow!("publisher disabled"))?;
                let started = (self.clock)();
                ensure!(
                    Uuid::parse_str(&operation_id)? == plan.operation_id
                        && started < plan.deadline
                        && peer.consumer() == plan.consumer,
                    "publisher operation rejected"
                );
                peer.principal()?;
                let mut fence = self.fence().await?;
                let after_wait = (self.clock)();
                if after_wait < started || after_wait >= plan.deadline {
                    fence.finish()?;
                    return Err(anyhow!("publisher deadline elapsed before permission"));
                }
                // Missing/inconsistent storage evidence retains dirty state.
                let source = self
                    .snapshot_source
                    .as_ref()
                    .ok_or_else(|| anyhow!("snapshot guard disabled"))?;
                let before = source()?;
                self.verify_snapshot_binding(&before)?;
                if before.digest() == plan.new_digest
                    && (plan.receipt)(plan.operation_id, plan.new_digest)?
                {
                    fence.finish()?;
                    return Err(anyhow!("publisher operation already completed"));
                }
                ensure!(
                    before.digest() == plan.old_digest,
                    "publisher starting snapshot mismatch"
                );
                let previous = before.snapshot().generation;
                tokio::time::timeout(Duration::from_secs(5),async {
                    peer.principal()?;ensure!(peer.consumer()==plan.consumer,"publisher generation changed");
                    let ready_time=(self.clock)();
                    if ready_time<after_wait || ready_time>=plan.deadline {fence.finish()?;return Err(anyhow!("publisher deadline elapsed before permission"));}
                    write(&mut stream,&Reply::Ready).await?;
                    let finish:Request=read(&mut stream).await?;
                    ensure!(matches!(finish,Request::FinishSnapshot {ref operation_id,digest} if Uuid::parse_str(operation_id).ok()==Some(plan.operation_id) && digest==plan.new_digest),"invalid publisher completion");
                    let finish_time=(self.clock)();ensure!(finish_time>=ready_time && finish_time<plan.deadline,"publisher deadline elapsed");
                    peer.principal()?;let after=source()?;self.verify_snapshot_binding(&after)?;
                    ensure!(after.digest()==plan.new_digest && after.snapshot().generation==previous.checked_add(1).ok_or_else(||anyhow!("generation overflow"))?,"publisher resulting snapshot mismatch");
                    ensure!((plan.receipt)(plan.operation_id,plan.new_digest)?,"publisher completion receipt missing");
                    peer.principal()?;ensure!(peer.consumer()==plan.consumer,"publisher generation changed");
                    let final_time=(self.clock)();ensure!(final_time>=finish_time && final_time<plan.deadline,"publisher deadline elapsed");
                    // Completion is durable before the final ACK. Losing the
                    // ACK after this point does not dirty the journal or retry.
                    fence.finish()?;write(&mut stream,&Reply::Finished).await?;
                    Ok::<(),anyhow::Error>(())
                }).await??;
            }
            Request::SnapshotReady { digest, .. } => {
                let mut fence = self.fence().await?;
                let outcome = (|| {
                    peer.principal()?;
                    ensure!(
                        peer.consumer() == self.launches.target(&self.hotel, &self.runner)?,
                        "wrong snapshot peer"
                    );
                    let source = self
                        .snapshot_source
                        .as_ref()
                        .ok_or_else(|| anyhow!("snapshot guard disabled"))?;
                    ensure!(source()?.digest() == digest, "snapshot digest mismatch");
                    peer.principal()?;
                    Ok::<(), anyhow::Error>(())
                })();
                fence.finish()?;
                outcome?;
                write(&mut stream, &Reply::SnapshotReady).await?;
            }
            Request::Admit {
                token, envelope, ..
            } => {
                #[cfg(test)]
                eprintln!("synthetic native control: admit");
                self.origin(&peer)?;
                let verified = native_memory::VerifiedRequest::validate(
                    &envelope,
                    &policy()?,
                    &self.hotel,
                    &self.runner,
                    (self.clock)(),
                )?;
                let handle = self.admit(&peer, token, &verified).await?;
                write(
                    &mut stream,
                    &Reply::Permit {
                        handle: handle.to_string(),
                    },
                )
                .await?;
            }
            Request::Cancel { handle, .. } => {
                let committed = self.cancel(Uuid::parse_str(&handle)?, &peer).await?;
                write(&mut stream, &Reply::Cancelled { committed }).await?;
            }
            request @ (Request::Reserve { .. } | Request::Release { .. }) => {
                let (handle, digest, release) = match request {
                    Request::Reserve { handle, digest, .. } => (handle, digest, false),
                    Request::Release { handle, digest, .. } => (handle, digest, true),
                    _ => unreachable!(),
                };
                #[cfg(test)]
                eprintln!("synthetic native control: reservation release={release}");
                let lease = self
                    .reserve_kind(Uuid::parse_str(&handle)?, peer, digest, release)
                    .await?;
                // Timeout/drop preserves the same journal's dirty state. Never
                // acknowledge clean recovery just because a client disconnected.
                tokio::time::timeout(Duration::from_secs(5), async {
                    write(&mut stream, &Reply::Ready).await?;
                    let mut validations = 0;
                    loop {
                        match read::<Request>(&mut stream).await? {
                            Request::Validate { digest: requested } => {
                                #[cfg(test)]
                                eprintln!("synthetic native control: validate");
                                ensure!(
                                    requested == digest && validations < 2,
                                    "invalid native validation"
                                );
                                validations += 1;
                                lease.validate_before_commit(digest).await?;
                                write(&mut stream, &Reply::Ready).await?;
                            }
                            Request::Finish { committed } => {
                                #[cfg(test)]
                                eprintln!("synthetic native control: finish committed={committed}");
                                ensure!(
                                    release || !committed || validations > 0,
                                    "precommit validation required"
                                );
                                lease.finish(committed).await?;
                                write(&mut stream, &Reply::Finished).await?;
                                return Ok::<(), anyhow::Error>(());
                            }
                            _ => return Err(anyhow!("invalid native reservation frame")),
                        }
                    }
                })
                .await??;
            }
            _ => return Err(anyhow!("invalid native request")),
        }
        Ok(())
    }

    /// Existing Node authority protocol, same journal as native commits. The
    /// issuer guest is fixed by hotel configuration, never supplied by the caller.
    pub async fn serve_issuer(
        self: Arc<Self>,
        listener: UnixListener,
        issuer_guest: String,
        owner_id: String,
    ) -> Result<()> {
        let capacity = Arc::new(tokio::sync::Semaphore::new(16));
        loop {
            let slot = capacity.clone().acquire_owned().await?;
            let (mut stream, _) = listener.accept().await?;
            let owner = self.clone();
            let guest = issuer_guest.clone();
            let id = owner_id.clone();
            tokio::spawn(async move {
                let _slot = slot;
                let _=async {
                    let peer=owner.launches.authenticate(&owner.hotel,&stream,&guest)?;
                    let begin:IssuerFrame=read(&mut stream).await?;
                    ensure!(begin.version==1 && begin.owner==id && begin.sequence==1 && Uuid::parse_str(&begin.session).is_ok() && begin.operation=="begin_issuer_change" && begin.parameters==serde_json::json!({}),"invalid issuer start");
                    let mut fence=owner.fence().await?;
                    tokio::time::timeout(Duration::from_secs(5),async {
                        peer.principal()?;
                        write(&mut stream,&serde_json::json!({"owner":id,"session":begin.session,"sequence":1,"ok":true,"result":true})).await?;
                        let end:IssuerFrame=read(&mut stream).await?;
                        ensure!(end.version==1 && end.owner==id && end.session==begin.session && end.sequence==2 && end.operation=="end_issuer_change" && end.parameters==serde_json::json!({}),"invalid issuer finish");
                        peer.principal()?;fence.finish()?;
                        write(&mut stream,&serde_json::json!({"owner":id,"session":begin.session,"sequence":2,"ok":true,"result":true})).await?;
                        Ok::<(),anyhow::Error>(())
                    }).await??;
                    Ok::<(),anyhow::Error>(())
                }.await;
            });
        }
    }
}

// A local experiment only. It is deliberately not wired into `serve_control`:
// the complete bounded protocol, authenticated request binding, root-owned
// snapshot publisher and crash reconciliation are still unimplemented.
#[cfg(test)]
mod root_admin_candidate {
    use anyhow::{Result, ensure};
    use serde::{Deserialize, Serialize};
    use tokio::net::UnixStream;

    // Test-only request shape. Neither a matching frame nor a claimed UID is
    // authorization; the peer UID must come from the kernel. This does not
    // authenticate the hotel back to the publisher or establish a PID generation.
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct CandidateRequest {
        version: u8,
        operation: String,
        expected_digest: [u8; 32],
        replacement_digest: [u8; 32],
    }

    async fn candidate_begin(
        stream: &mut UnixStream,
        allowed_uid: u32,
        current: [u8; 32],
    ) -> Result<()> {
        ensure!(
            stream.peer_cred()?.uid() == allowed_uid,
            "unauthorized kernel peer"
        );
        let frame: CandidateRequest = native_memory::authority_control::read(stream).await?;
        ensure!(
            frame.version == 1
                && frame.operation == "replace_grant_snapshot"
                && frame.expected_digest == current
                && frame.replacement_digest != current,
            "invalid or stale mutation"
        );
        Ok(())
    }

    fn verify_kernel_root_peer(stream: &UnixStream) -> Result<()> {
        let credentials = stream.peer_cred()?;
        ensure!(credentials.uid() == 0, "root peer required");
        Ok(())
    }

    fn verify_expected_hotel_peer(
        stream: &UnixStream,
        expected_pid: u32,
        expected_uid: u32,
    ) -> Result<()> {
        let credentials = stream.peer_cred()?;
        ensure!(credentials.uid() == expected_uid, "hotel peer UID mismatch");
        ensure!(
            credentials.pid().and_then(|pid| u32::try_from(pid).ok()) == Some(expected_pid),
            "hotel peer PID mismatch"
        );
        Ok(())
    }

    #[tokio::test]
    async fn caller_claiming_root_cannot_replace_kernel_peer_identity() {
        // A root-run test cannot establish this negative; Linux UID-separation
        // coverage is still required before using this idea in production.
        let uid = unsafe { libc::geteuid() };
        assert_ne!(uid, 0, "this negative test requires a non-root process");
        let (mut claimed, actual) = UnixStream::pair().unwrap();
        use tokio::io::AsyncWriteExt;
        claimed
            .write_all(br#"{"uid":0,"role":"root"}"#)
            .await
            .unwrap();
        assert_eq!(actual.peer_cred().unwrap().uid(), uid);
        assert!(verify_kernel_root_peer(&actual).is_err());
    }

    #[tokio::test]
    async fn publisher_must_check_kernel_peer_against_trusted_hotel_pid() {
        let (_hotel, publisher) = UnixStream::pair().unwrap();
        let uid = unsafe { libc::geteuid() };
        let pid = std::process::id();
        assert!(verify_expected_hotel_peer(&publisher, pid, uid).is_ok());
        assert!(verify_expected_hotel_peer(&publisher, pid.wrapping_add(1), uid).is_err());
        assert!(verify_expected_hotel_peer(&publisher, pid, uid.wrapping_add(1)).is_err());
        // Expected PID provenance, process generation and a real root/service
        // socket remain unimplemented; caller-supplied IDs are not authority.
    }

    #[tokio::test]
    async fn candidate_binds_kernel_uid_and_expected_digest() {
        let uid = unsafe { libc::geteuid() };
        for (allowed, expected, accepted) in [
            (uid, [1; 32], true),
            (uid.wrapping_add(1), [1; 32], false),
            (uid, [9; 32], false),
        ] {
            let (mut sender, mut receiver) = UnixStream::pair().unwrap();
            let frame = CandidateRequest {
                version: 1,
                operation: "replace_grant_snapshot".into(),
                expected_digest: expected,
                replacement_digest: [2; 32],
            };
            native_memory::authority_control::write(&mut sender, &frame)
                .await
                .unwrap();
            assert_eq!(
                candidate_begin(&mut receiver, allowed, [1; 32])
                    .await
                    .is_ok(),
                accepted
            );
        }
        // The positive fixture uses this test process UID, not root. Deployable
        // root-only authentication and root-to-hotel mutual identity need Linux.
    }

    #[tokio::test]
    async fn candidate_rejects_malformed_and_unscoped_frames() {
        let uid = unsafe { libc::geteuid() };
        let base = serde_json::json!({"version":1,"operation":"replace_grant_snapshot",
            "expected_digest":vec![1;32],"replacement_digest":vec![2;32]});
        let mut frames = Vec::new();
        for (key, value) in [
            ("version", serde_json::json!(2)),
            ("operation", serde_json::json!("delete")),
            ("replacement_digest", serde_json::json!(vec![1; 32])),
            ("extra", serde_json::json!(true)),
        ] {
            let mut frame = base.clone();
            frame[key] = value;
            frames.push(frame);
        }
        for frame in frames {
            let (mut sender, mut receiver) = UnixStream::pair().unwrap();
            native_memory::authority_control::write(&mut sender, &frame)
                .await
                .unwrap();
            assert!(candidate_begin(&mut receiver, uid, [1; 32]).await.is_err());
        }
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        use tokio::io::AsyncWriteExt;
        sender.write_all(&[0, 0, 0, 1, b'{']).await.unwrap();
        assert!(candidate_begin(&mut receiver, uid, [1; 32]).await.is_err());
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IssuerFrame {
    version: u32,
    owner: String,
    session: String,
    sequence: u64,
    operation: String,
    parameters: serde_json::Value,
}
