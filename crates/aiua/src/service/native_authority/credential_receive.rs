//! Disabled Linux credential-aware receive path; not wired to publication.
use super::*;
use std::time::Duration;
use tokio::net::UnixStream;

// Disabled factory: SO_PASSCRED is set before this socket can be bound or
// connected. The returned owned descriptor is not yet a listener; a future
// adapter must retain exclusive ownership and preserve this ordering.
#[allow(dead_code)]
fn new_credential_socket() -> Result<std::os::fd::OwnedFd> {
    use std::os::fd::{AsRawFd, FromRawFd};
    let raw = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    ensure!(raw >= 0, "candidate socket unavailable");
    let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw) };
    let one: libc::c_int = 1;
    ensure!(
        unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PASSCRED,
                (&one as *const libc::c_int).cast(),
                std::mem::size_of_val(&one) as libc::socklen_t,
            )
        } == 0,
        "candidate peer credentials unavailable"
    );
    Ok(fd)
}

// Only this module constructs the receive wrapper. The listener is created
// credential-enabled before bind/listen, and accepted streams inherit it.
struct CredentialListener(tokio::net::UnixListener);
struct CredentialStream(UnixStream);
impl CredentialListener {
    #[allow(dead_code)]
    fn bind(path: &std::path::Path) -> Result<Self> {
        use std::os::{
            fd::{AsRawFd, FromRawFd, IntoRawFd},
            unix::ffi::OsStrExt,
        };
        let name = path.as_os_str().as_bytes();
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        ensure!(
            path.is_absolute()
                && !name.is_empty()
                && name.len() < address.sun_path.len()
                && !name.contains(&0),
            "invalid credential socket path"
        );
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (dst, src) in address.sun_path.iter_mut().zip(name) {
            *dst = *src as libc::c_char;
        }
        let fd = new_credential_socket()?;
        let length =
            (std::mem::offset_of!(libc::sockaddr_un, sun_path) + name.len() + 1) as libc::socklen_t;
        ensure!(
            unsafe {
                libc::bind(
                    fd.as_raw_fd(),
                    (&address as *const libc::sockaddr_un).cast(),
                    length,
                )
            } == 0,
            "credential bind failed"
        );
        ensure!(
            unsafe { libc::listen(fd.as_raw_fd(), 16) } == 0,
            "credential listen failed"
        );
        let listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(fd.into_raw_fd()) };
        Ok(Self(tokio::net::UnixListener::from_std(listener)?))
    }
    #[allow(dead_code)]
    async fn accept(&self) -> Result<CredentialStream> {
        let (stream, _) = self.0.accept().await?;
        Ok(CredentialStream(stream))
    }
}

#[cfg(test)]
fn enable_candidate_credentials(stream: &UnixStream) -> Result<()> {
    use std::os::fd::AsRawFd;
    let one: libc::c_int = 1;
    ensure!(
        unsafe {
            libc::setsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PASSCRED,
                (&one as *const libc::c_int).cast(),
                std::mem::size_of_val(&one) as libc::socklen_t,
            )
        } == 0,
        "candidate peer credentials unavailable"
    );
    Ok(())
}

#[allow(dead_code)]
fn candidate_fragment(
    fd: std::os::fd::RawFd,
    bytes: &mut [u8],
    uid: u32,
    pid: u32,
) -> std::io::Result<usize> {
    candidate_fragment_with_control_limit(fd, bytes, uid, pid, std::mem::size_of::<[usize; 64]>())
}

fn candidate_fragment_with_control_limit(
    fd: std::os::fd::RawFd,
    bytes: &mut [u8],
    uid: u32,
    pid: u32,
    control_limit: usize,
) -> std::io::Result<usize> {
    use std::io::{Error, ErrorKind};
    let mut control = [0usize; 64];
    if control_limit > std::mem::size_of_val(&control) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "invalid ancillary bound",
        ));
    }
    let mut iov = libc::iovec {
        iov_base: bytes.as_mut_ptr().cast(),
        iov_len: bytes.len(),
    };
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = control_limit;
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

#[cfg(test)]
static HEADER_OBSERVED: std::sync::OnceLock<
    std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
> = std::sync::OnceLock::new();

#[allow(dead_code)]
async fn read_candidate<T: serde::de::DeserializeOwned>(
    protected: &CredentialStream,
    peer: &VerifiedLocalSession,
) -> Result<T> {
    let stream = &protected.0;
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
            peer.verify_message_sender(uid, pid)?;
            offset += count;
        }
        Ok(())
    }
    let mut header = [0u8; 4];
    exact(stream, peer, &mut header, uid, pid, deadline).await?;
    #[cfg(test)]
    if let Some(sender) = HEADER_OBSERVED
        .get_or_init(|| std::sync::Mutex::new(None))
        .lock()
        .unwrap()
        .take()
    {
        let _ = sender.send(());
    }
    let size = u32::from_be_bytes(header) as usize;
    ensure!(size > 0 && size <= 131_072, "invalid candidate frame size");
    let mut bytes = vec![0; size];
    exact(stream, peer, &mut bytes, uid, pid, deadline).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    // Only selected in a disposable Linux job. Child blocks before sending any
    // bytes until the parent has enabled credentials and verified its identity.
    #[test]
    #[ignore = "subprocess fixture, explicitly selected by parent"]
    fn credential_owned_child_fixture() {
        use std::io::{Read, Write};
        let path = std::env::var_os("PHILOTIC_CREDENTIAL_FIXTURE_SOCKET").expect("fixture path");
        let mut stream = std::os::unix::net::UnixStream::connect(path).unwrap();
        let mut command = [0u8; 1];
        std::io::stdin().read_exact(&mut command).unwrap();
        if command[0] == b'D' {
            let child = unsafe { libc::fork() };
            assert!(child >= 0);
            if child == 0 {
                let frame = b"\0\0\0\x07{\"x\":1}";
                let n = unsafe {
                    libc::send(stream.as_raw_fd(), frame.as_ptr().cast(), frame.len(), 0)
                };
                unsafe { libc::_exit(if n == frame.len() as isize { 0 } else { 1 }) };
            }
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
            assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
            // The registered process stays alive until its parent finishes reading.
            std::io::stdin().read_exact(&mut command).unwrap();
            return;
        }
        assert_eq!(command[0], b'H');
        stream.write_all(&7u32.to_be_bytes()).unwrap();
        std::io::stdin().read_exact(&mut command).unwrap();
        assert_eq!(command[0], b'B');
        stream.write_all(br#"{"x":1}"#).unwrap();
        // Stay alive until the parent observes and validates the denial.
        std::io::stdin().read_exact(&mut command).unwrap();
    }

    #[tokio::test]
    #[ignore = "private socket and owned child; disposable Linux only"]
    async fn owned_child_fragmented_frame_rejects_replaced_launch() {
        use ansible_mesh_core::privacy_local::{LaunchPrincipal, LocalLaunchRegistry};
        use std::{
            collections::BTreeSet,
            io::Write,
            process::{Command, Stdio},
            sync::{Arc, Mutex},
        };
        struct Cleanup(Arc<Mutex<std::process::Child>>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Ok(mut child) = self.0.lock() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("credential.sock");
        let listener = CredentialListener::bind(&socket).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "service::native_authority::credential_receive::tests::credential_owned_child_fixture", "--ignored", "--nocapture"])
            .env("PHILOTIC_CREDENTIAL_FIXTURE_SOCKET", &socket)
            .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let owned = Arc::new(Mutex::new(child));
        let _cleanup = Cleanup(owned.clone());
        let receiver = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let registry = Arc::new(LocalLaunchRegistry::default());
        let principal = || LaunchPrincipal {
            stable_agent_id: "fixture".into(),
            roles: BTreeSet::new(),
        };
        let generation = registry
            .attach_new(
                "fixture",
                unsafe { libc::geteuid() },
                principal(),
                owned.clone(),
            )
            .unwrap();
        let peer = registry
            .authenticate("hotel", &receiver.0, "fixture")
            .unwrap();
        let (header_tx, header_rx) = tokio::sync::oneshot::channel();
        *HEADER_OBSERVED
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap() = Some(header_tx);
        let read =
            tokio::spawn(
                async move { read_candidate::<serde_json::Value>(&receiver, &peer).await },
            );
        owned
            .lock()
            .unwrap()
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"H")
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), header_rx)
            .await
            .unwrap()
            .unwrap();
        registry.retire("fixture", generation).unwrap();
        let replacement = Arc::new(Mutex::new(
            Command::new("/bin/sleep").arg("30").spawn().unwrap(),
        ));
        let _replacement_cleanup = Cleanup(replacement.clone());
        registry
            .attach_new(
                "fixture",
                unsafe { libc::geteuid() },
                principal(),
                replacement,
            )
            .unwrap();
        owned
            .lock()
            .unwrap()
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"B")
            .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(5), read)
            .await
            .unwrap()
            .unwrap()
            .expect_err("replaced launch must be denied");
        assert!(
            error.chain().any(|cause| cause
                .to_string()
                .contains("unverified or replaced local session")),
            "unexpected denial: {error:#}"
        );
        assert!(
            owned.lock().unwrap().try_wait().unwrap().is_none(),
            "original child exited before denial"
        );
    }

    #[tokio::test]
    #[ignore = "private socket and owned child; disposable Linux only"]
    async fn registered_child_delegated_sender_is_denied() {
        use ansible_mesh_core::privacy_local::{LaunchPrincipal, LocalLaunchRegistry};
        use std::{
            collections::BTreeSet,
            io::Write,
            process::{Command, Stdio},
            sync::{Arc, Mutex},
        };
        struct Cleanup(Arc<Mutex<std::process::Child>>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Ok(mut child) = self.0.lock() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("credential.sock");
        let listener = CredentialListener::bind(&socket).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "service::native_authority::credential_receive::tests::credential_owned_child_fixture", "--ignored", "--nocapture"])
            .env("PHILOTIC_CREDENTIAL_FIXTURE_SOCKET", &socket)
            .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let owned = Arc::new(Mutex::new(child));
        let _cleanup = Cleanup(owned.clone());
        let receiver = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let registry = Arc::new(LocalLaunchRegistry::default());
        let principal = || LaunchPrincipal {
            stable_agent_id: "fixture".into(),
            roles: BTreeSet::new(),
        };
        let _generation = registry
            .attach_new(
                "fixture",
                unsafe { libc::geteuid() },
                principal(),
                owned.clone(),
            )
            .unwrap();
        let peer = registry
            .authenticate("hotel", &receiver.0, "fixture")
            .unwrap();
        owned
            .lock()
            .unwrap()
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"D")
            .unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            read_candidate::<serde_json::Value>(&receiver, &peer),
        )
        .await
        .unwrap();
        let error = result.expect_err("delegated sender must be denied");
        assert!(
            error
                .chain()
                .any(|cause| cause.to_string().contains("candidate sender changed")),
            "unexpected denial: {error:#}"
        );
        assert!(
            owned.lock().unwrap().try_wait().unwrap().is_none(),
            "registered child exited before denial"
        );
    }

    #[tokio::test]
    async fn candidate_checks_live_registry_generation_per_frame() {
        use ansible_mesh_core::privacy_local::{
            LaunchPrincipal, LocalLaunchRegistry, SupervisedProcess,
        };
        use std::collections::BTreeSet;
        use tokio::io::AsyncWriteExt;
        struct ThisProcess;
        impl SupervisedProcess for ThisProcess {
            fn pid(&self) -> Result<u32> {
                Ok(std::process::id())
            }
            fn alive(&self) -> Result<bool> {
                Ok(true)
            }
        }
        let registry = Arc::new(LocalLaunchRegistry::default());
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        let generation = registry
            .attach_new(
                "fixture",
                unsafe { libc::geteuid() },
                LaunchPrincipal {
                    stable_agent_id: "fixture".into(),
                    roles: BTreeSet::new(),
                },
                Arc::new(ThisProcess),
            )
            .unwrap();
        let peer = registry
            .authenticate("hotel", &receiver, "fixture")
            .unwrap();
        enable_candidate_credentials(&receiver).unwrap();
        let receiver = CredentialStream(receiver);
        let bytes = br#"{"value":7}"#;
        sender
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .await
            .unwrap();
        sender.write_all(bytes).await.unwrap();
        let value: serde_json::Value = read_candidate(&receiver, &peer).await.unwrap();
        assert_eq!(value["value"], 7);
        // Read the first fragment while the generation is still live; then
        // retire it before accepting the rest of this frame.
        sender
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .await
            .unwrap();
        let mut header = [0u8; 4];
        let count = candidate_fragment(
            receiver.0.as_raw_fd(),
            &mut header,
            unsafe { libc::geteuid() },
            std::process::id(),
        )
        .unwrap();
        assert_eq!(count, header.len());
        peer.verify_message_sender(unsafe { libc::geteuid() }, std::process::id())
            .unwrap();
        registry.retire("fixture", generation).unwrap();
        sender.write_all(bytes).await.unwrap();
        let mut body = vec![0; bytes.len()];
        let count = candidate_fragment(
            receiver.0.as_raw_fd(),
            &mut body,
            unsafe { libc::geteuid() },
            std::process::id(),
        )
        .unwrap();
        assert_eq!(count, bytes.len());
        assert!(
            peer.verify_message_sender(unsafe { libc::geteuid() }, std::process::id())
                .is_err()
        );
    }

    #[test]
    fn delegated_same_uid_sender_is_rejected() {
        let (sender, receiver) = std::os::unix::net::UnixStream::pair().unwrap();
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
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            let value = [7u8];
            let n = unsafe { libc::send(sender.as_raw_fd(), value.as_ptr().cast(), 1, 0) };
            unsafe { libc::_exit(if n == 1 { 0 } else { 1 }) };
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
        let mut data = [0u8; 1];
        assert!(
            candidate_fragment(
                receiver.as_raw_fd(),
                &mut data,
                unsafe { libc::geteuid() },
                std::process::id()
            )
            .is_err()
        );
        let _ = sender;
    }

    #[test]
    fn truncated_kernel_credentials_are_denied() {
        use std::io::Write;
        let (mut sender, receiver) = std::os::unix::net::UnixStream::pair().unwrap();
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
        sender.write_all(&[7]).unwrap();
        let mut out = [0u8; 1];
        let err = candidate_fragment_with_control_limit(
            receiver.as_raw_fd(),
            &mut out,
            unsafe { libc::geteuid() },
            std::process::id(),
            1,
        )
        .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

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
