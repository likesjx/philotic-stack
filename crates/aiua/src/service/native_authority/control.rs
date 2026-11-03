//! Private control adapters owned by the hotel, not a second supervisor.
use super::*;
use native_memory::authority_control::{Reply, Request, read, write};
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};

// Candidate-only credential-aware read after a fresh challenge. Legacy control
// framing remains unchanged. Missing or delegated credentials fail closed.
#[cfg(target_os = "linux")]
fn enable_candidate_credentials(stream: &UnixStream) -> Result<()> {
    use std::os::fd::AsRawFd;
    let one: libc::c_int = 1;
    let rc = unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PASSCRED,
            (&one as *const libc::c_int).cast(),
            std::mem::size_of_val(&one) as libc::socklen_t,
        )
    };
    ensure!(rc == 0, "candidate peer credentials unavailable");
    Ok(())
}

#[cfg(target_os = "linux")]
fn candidate_fragment(
    fd: std::os::fd::RawFd,
    bytes: &mut [u8],
    uid: u32,
    pid: u32,
) -> std::io::Result<usize> {
    use std::io::{Error, ErrorKind};
    let mut control = [0usize; 64];
    let mut iov = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = std::mem::size_of_val(&control);
    let n = unsafe { libc::recvmsg(fd, &mut msg, libc::MSG_CMSG_CLOEXEC | libc::MSG_DONTWAIT) };
    if n < 0 {
        return Err(Error::last_os_error());
    }
    if n == 0 {
        return Err(Error::new(
            ErrorKind::UnexpectedEof,
            "candidate stream closed",
        ));
    }
    if msg.msg_controllen > std::mem::size_of_val(&control) {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "candidate ancillary overflow",
        ));
    }
    let mut credential = None;
    let mut unexpected = false;
    unsafe {
        let begin = msg.msg_control as usize;
        let end = begin
            .checked_add(msg.msg_controllen)
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "candidate ancillary overflow"))?;
        let mut c = libc::CMSG_FIRSTHDR(&msg);
        while !c.is_null() {
            let at = c as usize;
            if at < begin
                || at
                    .checked_add(std::mem::size_of::<libc::cmsghdr>())
                    .is_none_or(|v| v > end)
            {
                unexpected = true;
                break;
            }
            let h = &*c;
            if h.cmsg_len < libc::CMSG_LEN(0) as usize
                || at.checked_add(h.cmsg_len).is_none_or(|v| v > end)
            {
                unexpected = true;
                break;
            }
            if h.cmsg_level == libc::SOL_SOCKET
                && h.cmsg_type == libc::SCM_CREDENTIALS
                && h.cmsg_len == libc::CMSG_LEN(std::mem::size_of::<libc::ucred>() as u32) as usize
                && credential.is_none()
            {
                credential = Some(std::ptr::read_unaligned(
                    libc::CMSG_DATA(c).cast::<libc::ucred>(),
                ));
            } else {
                unexpected = true;
                if h.cmsg_level == libc::SOL_SOCKET && h.cmsg_type == libc::SCM_RIGHTS {
                    let count = (h.cmsg_len - libc::CMSG_LEN(0) as usize)
                        / std::mem::size_of::<libc::c_int>();
                    let ptr = libc::CMSG_DATA(c).cast::<libc::c_int>();
                    for i in 0..count {
                        libc::close(std::ptr::read_unaligned(ptr.add(i)));
                    }
                }
            }
            c = libc::CMSG_NXTHDR(&msg, c);
        }
    }
    if unexpected || msg.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "unexpected candidate ancillary data",
        ));
    }
    let credential = credential
        .ok_or_else(|| Error::new(ErrorKind::PermissionDenied, "candidate credentials missing"))?;
    if credential.uid != uid || u32::try_from(credential.pid).ok() != Some(pid) {
        return Err(Error::new(
            ErrorKind::PermissionDenied,
            "candidate sender changed",
        ));
    }
    Ok(n as usize)
}

#[cfg(target_os = "linux")]
async fn read_candidate<T: serde::de::DeserializeOwned>(
    stream: &UnixStream,
    peer: &VerifiedLocalSession,
) -> Result<T> {
    let cred = stream.peer_cred()?;
    let pid = u32::try_from(
        cred.pid()
            .ok_or_else(|| anyhow!("candidate PID unavailable"))?,
    )?;
    let uid = cred.uid();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    async fn exact(
        stream: &UnixStream,
        peer: &VerifiedLocalSession,
        out: &mut [u8],
        uid: u32,
        pid: u32,
        deadline: tokio::time::Instant,
    ) -> Result<()> {
        use std::os::fd::AsRawFd;
        let mut offset = 0;
        while offset < out.len() {
            let count = tokio::time::timeout_at(
                deadline,
                stream.async_io(tokio::io::Interest::READABLE, || {
                    candidate_fragment(stream.as_raw_fd(), &mut out[offset..], uid, pid)
                }),
            )
            .await??;
            peer.principal()?;
            offset += count;
        }
        Ok(())
    }
    let mut header = [0u8; 4];
    exact(stream, peer, &mut header, uid, pid, deadline).await?;
    let size = u32::from_be_bytes(header) as usize;
    ensure!(size > 0 && size <= 131_072, "invalid candidate frame size");
    let mut bytes = vec![0; size];
    exact(stream, peer, &mut bytes, uid, pid, deadline).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

// Synthetic tests on other hosts exercise control flow only, never Linux peer
// authentication. Non-Linux production does not enable candidate publication.
#[cfg(all(not(target_os = "linux"), test))]
fn enable_candidate_credentials(_: &UnixStream) -> Result<()> {
    Ok(())
}
#[cfg(all(not(target_os = "linux"), test))]
async fn read_candidate<T: serde::de::DeserializeOwned>(
    stream: &mut UnixStream,
    peer: &VerifiedLocalSession,
) -> Result<T> {
    peer.principal()?;
    Ok(read(stream).await?)
}
#[cfg(all(not(target_os = "linux"), not(test)))]
fn enable_candidate_credentials(_: &UnixStream) -> Result<()> {
    Err(anyhow!("candidate credentials unsupported"))
}
#[cfg(all(not(target_os = "linux"), not(test)))]
async fn read_candidate<T: serde::de::DeserializeOwned>(
    _: &mut UnixStream,
    _: &VerifiedLocalSession,
) -> Result<T> {
    Err(anyhow!("candidate credentials unsupported"))
}

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
            | Request::BeginSnapshot { guest, .. }
            | Request::SnapshotStatus { guest, .. } => guest,
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
        if let Request::BeginSnapshot { operation_id, .. }
        | Request::SnapshotStatus { operation_id, .. } = &first
        {
            // The first frame is only a request to challenge; no fence or
            // authority effect occurs until the actual sender confirms.
            let id = Uuid::parse_str(operation_id)?;
            enable_candidate_credentials(&stream)?;
            let nonce = *Uuid::new_v4().as_bytes();
            write(&mut stream, &Reply::SnapshotChallenge { nonce }).await?;
            let confirm: Request = read_candidate(&mut stream, &peer).await?;
            ensure!(
                matches!(confirm, Request::ConfirmSnapshot { operation_id, nonce: got }
                if Uuid::parse_str(&operation_id).ok() == Some(id) && got == nonce),
                "candidate confirmation denied"
            );
        }
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
                    // A receipt alone does not prove this or any older
                    // publisher is quiescent; require explicit recovery.
                    return Err(anyhow!("publisher operation already completed"));
                }
                ensure!(
                    before.digest() == plan.old_digest,
                    "publisher starting snapshot mismatch"
                );
                let previous = before.snapshot().generation;
                // Status is volatile and deliberately conservative after restart.
                *self
                    .publisher_status
                    .lock()
                    .map_err(|_| anyhow!("publisher status unavailable"))? =
                    PublisherStatus::Pending;
                let _status_guard = PublisherStatusGuard(self.publisher_status.clone());
                let request_result = async {
                    peer.principal()?;
                    ensure!(peer.consumer() == plan.consumer, "publisher generation changed");
                    let ready_time = (self.clock)();
                    ensure!(ready_time >= after_wait && ready_time < plan.deadline, "publisher deadline elapsed");
                    write(&mut stream, &Reply::Ready).await?;
                    let finish: Request = read_candidate(&mut stream, &peer).await?;
                    ensure!(matches!(finish, Request::FinishSnapshot { ref operation_id, digest }
                        if Uuid::parse_str(operation_id).ok() == Some(plan.operation_id) && digest == plan.new_digest),
                        "invalid publisher completion");
                    peer.principal()?;
                    ensure!(peer.consumer() == plan.consumer, "publisher generation changed");
                    let finish_time = (self.clock)();
                    ensure!(finish_time >= ready_time && finish_time < plan.deadline, "publisher deadline elapsed");
                    Ok::<u64, anyhow::Error>(finish_time)
                }.await;
                // The publisher closes its stream after submission and stays
                // alive until the trusted lifecycle owner stops it.
                // Retain the actual fence until the trusted lifecycle owner has
                // confirmed quiescence. Missing proof holds indefinitely; task
                // cancellation/shutdown leaves the durable journal dirty.
                if (plan.quiesce)().await.is_err() {
                    *self
                        .publisher_status
                        .lock()
                        .map_err(|_| anyhow!("publisher status unavailable"))? =
                        PublisherStatus::RecoveryRequired;
                    std::future::pending::<()>().await;
                    unreachable!();
                }
                let finish_time = match request_result {
                    Ok(value) => value,
                    Err(error) => {
                        *self
                            .publisher_status
                            .lock()
                            .map_err(|_| anyhow!("publisher status unavailable"))? =
                            PublisherStatus::RecoveryRequired;
                        return Err(error);
                    }
                };
                let verified = (|| -> Result<()> {
                    let after = source()?;
                    self.verify_snapshot_binding(&after)?;
                    ensure!(
                        after.digest() == plan.new_digest
                            && after.snapshot().generation
                                == previous
                                    .checked_add(1)
                                    .ok_or_else(|| anyhow!("generation overflow"))?,
                        "publisher resulting snapshot mismatch"
                    );
                    ensure!(
                        (plan.receipt)(plan.operation_id, plan.new_digest)?,
                        "publisher completion receipt missing"
                    );
                    let final_time = (self.clock)();
                    ensure!(
                        final_time >= finish_time && final_time < plan.deadline,
                        "publisher deadline elapsed"
                    );
                    Ok(())
                })();
                if let Err(error) = verified {
                    *self
                        .publisher_status
                        .lock()
                        .map_err(|_| anyhow!("publisher status unavailable"))? =
                        PublisherStatus::RecoveryRequired;
                    return Err(error);
                }
                fence.finish()?;
                *self
                    .publisher_status
                    .lock()
                    .map_err(|_| anyhow!("publisher status unavailable"))? =
                    PublisherStatus::Complete;
                // One-way completion: do not wait for an ACK from an exited
                // publisher. A separate authenticated consumer queries status.
            }
            Request::SnapshotStatus { operation_id, .. } => {
                self.origin(&peer)?;
                let plan = self
                    .publisher_plan
                    .as_ref()
                    .ok_or_else(|| anyhow!("publisher disabled"))?;
                ensure!(
                    Uuid::parse_str(&operation_id)? == plan.operation_id,
                    "unknown publisher operation"
                );
                peer.principal()?;
                let status = *self
                    .publisher_status
                    .lock()
                    .map_err(|_| anyhow!("publisher status unavailable"))?;
                let reply = match status {
                    PublisherStatus::Unknown => Reply::SnapshotUnknown,
                    PublisherStatus::Pending => Reply::SnapshotPending,
                    PublisherStatus::Complete => Reply::SnapshotComplete,
                    PublisherStatus::RecoveryRequired => Reply::SnapshotRecoveryRequired,
                };
                write(&mut stream, &reply).await?;
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

#[cfg(all(test, target_os = "linux"))]
mod candidate_ancillary_tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn candidate_fragment_without_kernel_credentials_is_denied() {
        use std::io::Write as _;
        let (mut sender, receiver) = std::os::unix::net::UnixStream::pair().unwrap();
        // Deliberately do not enable SO_PASSCRED on the receiving socket.
        sender.write_all(&[7]).unwrap();
        let mut out = [0u8; 1];
        let error = candidate_fragment(
            receiver.as_raw_fd(),
            &mut out,
            unsafe { libc::geteuid() },
            std::process::id(),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    }

    /// Run in a single-threaded disposable Linux process: /proc FD counts are
    /// process-wide and unrelated concurrent tests would make them unreliable.
    #[test]
    #[ignore = "requires isolated Linux test process"]
    fn candidate_truncated_rights_are_closed_before_rejection() {
        use std::os::unix::net::UnixStream as StdUnixStream;
        let (sender, receiver) = StdUnixStream::pair().unwrap();
        let one: libc::c_int = 1;
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    receiver.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_PASSCRED,
                    (&one as *const libc::c_int).cast(),
                    std::mem::size_of_val(&one) as libc::socklen_t,
                )
            },
            0
        );
        let file = std::fs::File::open("/dev/null").unwrap();
        let fds = vec![file.as_raw_fd(); 200];
        let bytes = fds.len() * std::mem::size_of::<libc::c_int>();
        let space = unsafe { libc::CMSG_SPACE(bytes as u32) } as usize;
        let mut control = vec![0usize; space.div_ceil(std::mem::size_of::<usize>())];
        let mut payload = [7u8];
        let mut iov = libc::iovec {
            iov_base: payload.as_mut_ptr().cast(),
            iov_len: 1,
        };
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = space;
        unsafe {
            let h = libc::CMSG_FIRSTHDR(&msg);
            assert!(!h.is_null());
            (*h).cmsg_level = libc::SOL_SOCKET;
            (*h).cmsg_type = libc::SCM_RIGHTS;
            (*h).cmsg_len = libc::CMSG_LEN(bytes as u32) as usize;
            std::ptr::copy_nonoverlapping(
                fds.as_ptr(),
                libc::CMSG_DATA(h).cast::<libc::c_int>(),
                fds.len(),
            );
            assert_eq!(libc::sendmsg(sender.as_raw_fd(), &msg, 0), 1);
        }
        let count = || std::fs::read_dir("/proc/self/fd").unwrap().count();
        let before = count();
        let mut received = [0u8; 1];
        assert!(
            candidate_fragment(
                receiver.as_raw_fd(),
                &mut received,
                unsafe { libc::geteuid() },
                std::process::id()
            )
            .is_err()
        );
        assert_eq!(
            count(),
            before,
            "received descriptors leaked on ancillary truncation"
        );
    }
}
