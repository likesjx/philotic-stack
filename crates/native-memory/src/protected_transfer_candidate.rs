//! Experimental protected transfer, unwired and not production qualified.
//! Trusted operation metadata and binding must be loaded by the receiving hotel;
//! neither can be supplied by the sender. The caller must hold the hotel fence
//! and reconcile operation receipts independently before acting on a snapshot.
use crate::{
    Error, Result,
    grant_snapshot_candidate::{ExpectedBinding, LoadedSnapshot, validate_snapshot_bytes},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
// JSON encodes each byte as up to three digits plus a comma. This bound leaves
// ample room for frame metadata under the 131072-byte packet limit.
pub(crate) const MAX_TRANSFER_SNAPSHOT: usize = 16_000;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    version: u8,
    operation_id: String,
    generation: u64,
    snapshot: Vec<u8>,
}

/// Loaded from trusted, durable hotel operation state, never from the frame.
pub struct ExpectedTransfer<'a> {
    pub operation_id: uuid::Uuid,
    pub generation: u64,
    pub sha256: [u8; 32],
    pub binding: &'a ExpectedBinding<'a>,
}

fn validate_frame(raw: &[u8], expected: &ExpectedTransfer<'_>) -> Result<LoadedSnapshot> {
    if raw.is_empty() || raw.len() > 131_072 {
        return Err(Error::Invalid);
    }
    let frame: Frame = serde_json::from_slice(raw).map_err(|_| Error::Invalid)?;
    if frame.snapshot.len() > MAX_TRANSFER_SNAPSHOT {
        return Err(Error::Invalid);
    }
    if frame.version != 1
        || frame.operation_id != expected.operation_id.to_string()
        || frame.generation != expected.generation
        || <[u8; 32]>::from(Sha256::digest(&frame.snapshot)) != expected.sha256
    {
        return Err(Error::Denied);
    }
    let snapshot = validate_snapshot_bytes(&frame.snapshot, expected.binding)?;
    if snapshot.snapshot().generation != expected.generation {
        return Err(Error::Denied);
    }
    Ok(snapshot)
}

/// Receive exactly one packet on an already-established AF_UNIX SOCK_SEQPACKET
/// with SO_PASSCRED enabled by a trusted listener. Linux sends SCM_CREDENTIALS
/// for each message, so a delegated socket cannot silently send as the original
/// process. This candidate is synchronous and MUST run off the async executor.
/// Root remains trusted: privileged credential forgery is outside this boundary.
/// Manager-confirmed incarnation, operation authorization and fencing are still
/// prerequisites; this is not a complete production protocol.
#[cfg(target_os = "linux")]
pub fn receive_packet(
    stream: &std::os::unix::net::UnixStream,
    peer: &crate::protected_peer_candidate::ManagerPinnedProcess,
    expected: &ExpectedTransfer<'_>,
) -> Result<LoadedSnapshot> {
    use std::os::fd::AsRawFd;
    let fd = stream.as_raw_fd();
    let get_opt = |name| -> Result<i32> {
        let mut value: i32 = 0;
        let mut len = std::mem::size_of::<i32>() as libc::socklen_t;
        let result = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                name,
                (&mut value as *mut i32).cast(),
                &mut len,
            )
        };
        if result != 0 || len as usize != std::mem::size_of::<i32>() {
            return Err(Error::Unavailable);
        }
        Ok(value)
    };
    if get_opt(libc::SO_TYPE)? != libc::SOCK_SEQPACKET || get_opt(libc::SO_PASSCRED)? != 1 {
        return Err(Error::Denied);
    }
    // Check the connection peer separately from the sender of this packet.
    let mut original: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut original as *mut libc::ucred).cast(),
            &mut len,
        )
    } != 0
        || len as usize != std::mem::size_of::<libc::ucred>()
    {
        return Err(Error::Unavailable);
    }
    peer.verify_sender(original.uid, u32::try_from(original.pid).ok())?;
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    if unsafe { libc::poll(&mut poll, 1, 1000) } != 1 || poll.revents & libc::POLLIN == 0 {
        return Err(Error::Unavailable);
    }
    let mut data = vec![0u8; 131_072];
    // usize elements ensure cmsghdr alignment. Reject all ancillary data except
    // exactly one credentials record; close any received descriptors first.
    let mut control = [0usize; 64];
    let mut iov = libc::iovec {
        iov_base: data.as_mut_ptr().cast(),
        iov_len: data.len(),
    };
    let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = std::mem::size_of_val(&control);
    let n = unsafe { libc::recvmsg(fd, &mut msg, libc::MSG_CMSG_CLOEXEC | libc::MSG_DONTWAIT) };
    if n < 0 {
        return Err(Error::Unavailable);
    }
    if msg.msg_controllen > std::mem::size_of_val(&control) {
        return Err(Error::Denied);
    }
    let mut credentials = None;
    let mut unexpected = false;
    unsafe {
        let begin = msg.msg_control as usize;
        let Some(end) = begin.checked_add(msg.msg_controllen) else {
            return Err(Error::Denied);
        };
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
                && credentials.is_none()
            {
                credentials = Some(std::ptr::read_unaligned(
                    libc::CMSG_DATA(c).cast::<libc::ucred>(),
                ));
            } else {
                unexpected = true;
                if h.cmsg_level == libc::SOL_SOCKET && h.cmsg_type == libc::SCM_RIGHTS {
                    let header = libc::CMSG_LEN(0) as usize;
                    if h.cmsg_len >= header {
                        let count = (h.cmsg_len - header) / std::mem::size_of::<libc::c_int>();
                        let ptr = libc::CMSG_DATA(c).cast::<libc::c_int>();
                        for i in 0..count {
                            libc::close(std::ptr::read_unaligned(ptr.add(i)));
                        }
                    }
                }
            }
            c = libc::CMSG_NXTHDR(&msg, c);
        }
    }
    if msg.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 {
        return Err(Error::Invalid);
    }
    if unexpected || n == 0 {
        return Err(Error::Denied);
    }
    let sender = credentials.ok_or(Error::Denied)?;
    peer.verify_sender(sender.uid, u32::try_from(sender.pid).ok())?;
    data.truncate(n as usize);
    let result = validate_frame(&data, expected)?;
    peer.verify_sender(sender.uid, u32::try_from(sender.pid).ok())?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    struct FixedManager(crate::protected_peer_candidate::ManagerIdentity);
    #[cfg(target_os = "linux")]
    impl crate::protected_peer_candidate::ManagerEvidence for FixedManager {
        fn current(&self) -> Result<crate::protected_peer_candidate::ManagerIdentity> {
            Ok(self.0.clone())
        }
    }
    #[test]
    fn transfer_rejects_operation_substitution_and_wrong_generation() {
        let snapshot = crate::grant_snapshot_candidate::tests::sample();
        let bytes = serde_json::to_vec(&snapshot).unwrap();
        let id = uuid::Uuid::new_v4();
        let binding = crate::grant_snapshot_candidate::tests::expected();
        let expected = ExpectedTransfer {
            operation_id: id,
            generation: snapshot.generation,
            sha256: Sha256::digest(&bytes).into(),
            binding: &binding,
        };
        let frame = |id: uuid::Uuid, generation, snapshot| {
            serde_json::to_vec(&Frame {
                version: 1,
                operation_id: id.to_string(),
                generation,
                snapshot,
            })
            .unwrap()
        };
        assert!(validate_frame(&frame(id, snapshot.generation, bytes.clone()), &expected).is_ok());
        assert!(
            validate_frame(
                &frame(uuid::Uuid::new_v4(), snapshot.generation, bytes.clone()),
                &expected
            )
            .is_err()
        );
        assert!(
            validate_frame(
                &frame(id, snapshot.generation + 1, bytes.clone()),
                &expected
            )
            .is_err()
        );
        let mut changed = bytes;
        changed.push(b' ');
        assert!(validate_frame(&frame(id, snapshot.generation, changed), &expected).is_err());
    }
    #[test]
    fn maximum_snapshot_fits_wire_and_oversize_is_rejected() {
        let snapshot = crate::grant_snapshot_candidate::tests::sample();
        let mut bytes = serde_json::to_vec(&snapshot).unwrap();
        bytes.resize(MAX_TRANSFER_SNAPSHOT, b' ');
        let id = uuid::Uuid::new_v4();
        let binding = crate::grant_snapshot_candidate::tests::expected();
        let expected = ExpectedTransfer {
            operation_id: id,
            generation: snapshot.generation,
            sha256: Sha256::digest(&bytes).into(),
            binding: &binding,
        };
        let frame = Frame {
            version: 1,
            operation_id: id.to_string(),
            generation: snapshot.generation,
            snapshot: bytes.clone(),
        };
        let raw = serde_json::to_vec(&frame).unwrap();
        assert!(raw.len() < 131_072);
        assert!(
            serde_json::to_vec(&Frame {
                snapshot: vec![255; MAX_TRANSFER_SNAPSHOT],
                ..frame.clone()
            })
            .unwrap()
            .len()
                < 131_072
        );
        assert!(validate_frame(&raw, &expected).is_ok());
        bytes.push(b' ');
        let too_large = Frame {
            snapshot: bytes,
            ..frame
        };
        assert!(validate_frame(&serde_json::to_vec(&too_large).unwrap(), &expected).is_err());
    }

    // Requires a Linux kernel with pidfd and SO_PASSCRED. No root privileges,
    // network or production resources are used. A distinct-UID test requires
    // a disposable privileged CI job and is deliberately not claimed here.
    #[cfg(target_os = "linux")]
    #[test]
    fn real_linux_packet_rejects_delegated_same_uid_sender_and_dead_incarnation() {
        use std::io::Write;
        use std::os::fd::{AsRawFd, FromRawFd};
        fn pair() -> (
            std::os::unix::net::UnixStream,
            std::os::unix::net::UnixStream,
        ) {
            let mut fds = [-1; 2];
            assert_eq!(
                unsafe {
                    libc::socketpair(
                        libc::AF_UNIX,
                        libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                        0,
                        fds.as_mut_ptr(),
                    )
                },
                0
            );
            let send = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fds[0]) };
            let recv = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fds[1]) };
            let value: libc::c_int = 1;
            assert_eq!(
                unsafe {
                    libc::setsockopt(
                        recv.as_raw_fd(),
                        libc::SOL_SOCKET,
                        libc::SO_PASSCRED,
                        (&value as *const libc::c_int).cast(),
                        std::mem::size_of_val(&value) as libc::socklen_t,
                    )
                },
                0
            );
            (send, recv)
        }
        let snapshot = crate::grant_snapshot_candidate::tests::sample();
        let bytes = serde_json::to_vec(&snapshot).unwrap();
        let id = uuid::Uuid::new_v4();
        let binding = crate::grant_snapshot_candidate::tests::expected();
        let expected = ExpectedTransfer {
            operation_id: id,
            generation: snapshot.generation,
            sha256: Sha256::digest(&bytes).into(),
            binding: &binding,
        };
        let raw = serde_json::to_vec(&Frame {
            version: 1,
            operation_id: id.to_string(),
            generation: snapshot.generation,
            snapshot: bytes,
        })
        .unwrap();
        let uid = unsafe { libc::geteuid() };
        let pid = unsafe { libc::getpid() } as u32;
        let pinned =
            crate::protected_peer_candidate::ManagerPinnedProcess::pin(std::sync::Arc::new(
                FixedManager(crate::protected_peer_candidate::ManagerIdentity {
                    pid,
                    uid,
                    invocation: [1; 16],
                }),
            ))
            .unwrap();
        let (mut sender, receiver) = pair();
        sender.write_all(&raw).unwrap();
        assert!(receive_packet(&receiver, &pinned, &expected).is_ok());
        let (sender, receiver) = pair();
        // Child uses only async-signal-safe syscalls before _exit.
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            let sent = unsafe { libc::send(sender.as_raw_fd(), raw.as_ptr().cast(), raw.len(), 0) };
            unsafe { libc::_exit(if sent == raw.len() as isize { 0 } else { 1 }) };
        }
        assert!(matches!(
            receive_packet(&receiver, &pinned, &expected),
            Err(Error::Denied)
        ));
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pinned_child =
            crate::protected_peer_candidate::PinnedProcess::pin_manager_process(child.id(), uid)
                .unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(pinned_child.verify_sender(uid, Some(child.id())).is_err());
    }

    /// Explicitly run only inside a disposable Linux VM/container as root.
    /// This test forks and drops the child's UID to 65534; it never touches
    /// network, services, credentials, or persistent host state.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires explicitly provisioned disposable Linux root environment"]
    fn real_linux_root_receiver_rejects_dropped_uid_child() {
        use std::os::fd::{AsRawFd, FromRawFd};
        assert_eq!(
            unsafe { libc::geteuid() },
            0,
            "run only as root in disposable CI"
        );
        let mut fds = [-1; 2];
        assert_eq!(
            unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                    0,
                    fds.as_mut_ptr(),
                )
            },
            0
        );
        let sender = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fds[0]) };
        let receiver = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fds[1]) };
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
        let snapshot = crate::grant_snapshot_candidate::tests::sample();
        let bytes = serde_json::to_vec(&snapshot).unwrap();
        let id = uuid::Uuid::new_v4();
        let binding = crate::grant_snapshot_candidate::tests::expected();
        let expected = ExpectedTransfer {
            operation_id: id,
            generation: snapshot.generation,
            sha256: Sha256::digest(&bytes).into(),
            binding: &binding,
        };
        let raw = serde_json::to_vec(&Frame {
            version: 1,
            operation_id: id.to_string(),
            generation: snapshot.generation,
            snapshot: bytes,
        })
        .unwrap();
        let peer = crate::protected_peer_candidate::ManagerPinnedProcess::pin(std::sync::Arc::new(
            FixedManager(crate::protected_peer_candidate::ManagerIdentity {
                pid: unsafe { libc::getpid() } as u32,
                uid: 0,
                invocation: [1; 16],
            }),
        ))
        .unwrap();
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            if unsafe { libc::setuid(65534) } != 0 {
                unsafe { libc::_exit(2) }
            }
            let sent = unsafe { libc::send(sender.as_raw_fd(), raw.as_ptr().cast(), raw.len(), 0) };
            unsafe { libc::_exit(if sent == raw.len() as isize { 0 } else { 1 }) };
        }
        assert!(matches!(
            receive_packet(&receiver, &peer, &expected),
            Err(Error::Denied)
        ));
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
    }

    /// Run alone in disposable Linux CI: process-wide FD counting is not stable
    /// when unrelated tests run concurrently. No file contents are read.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires isolated Linux run with --test-threads=1"]
    fn linux_isolated_rejects_rights_and_truncation_without_fd_leaks() {
        use std::os::fd::{AsRawFd, FromRawFd};
        fn fd_count() -> usize {
            std::fs::read_dir("/proc/self/fd").unwrap().count()
        }
        let snapshot = crate::grant_snapshot_candidate::tests::sample();
        let bytes = serde_json::to_vec(&snapshot).unwrap();
        let id = uuid::Uuid::new_v4();
        let binding = crate::grant_snapshot_candidate::tests::expected();
        let expected = ExpectedTransfer {
            operation_id: id,
            generation: snapshot.generation,
            sha256: Sha256::digest(&bytes).into(),
            binding: &binding,
        };
        let raw = serde_json::to_vec(&Frame {
            version: 1,
            operation_id: id.to_string(),
            generation: snapshot.generation,
            snapshot: bytes,
        })
        .unwrap();
        let uid = unsafe { libc::geteuid() };
        let pid = unsafe { libc::getpid() } as u32;
        let peer = crate::protected_peer_candidate::ManagerPinnedProcess::pin(std::sync::Arc::new(
            FixedManager(crate::protected_peer_candidate::ManagerIdentity {
                pid,
                uid,
                invocation: [1; 16],
            }),
        ))
        .unwrap();
        for count in [1usize, 200] {
            let mut fds = [-1; 2];
            assert_eq!(
                unsafe {
                    libc::socketpair(
                        libc::AF_UNIX,
                        libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                        0,
                        fds.as_mut_ptr(),
                    )
                },
                0
            );
            let sender = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fds[0]) };
            let receiver = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fds[1]) };
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
            let source = std::fs::File::open("/dev/null").unwrap();
            let rights = vec![source.as_raw_fd(); count];
            let space =
                unsafe { libc::CMSG_SPACE((count * std::mem::size_of::<libc::c_int>()) as u32) }
                    as usize;
            let mut control = vec![0usize; space.div_ceil(std::mem::size_of::<usize>())];
            let mut data = raw.clone();
            let mut iov = libc::iovec {
                iov_base: data.as_mut_ptr().cast(),
                iov_len: data.len(),
            };
            let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
            msg.msg_iov = &mut iov;
            msg.msg_iovlen = 1;
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = space;
            unsafe {
                let c = libc::CMSG_FIRSTHDR(&msg);
                assert!(!c.is_null());
                (*c).cmsg_level = libc::SOL_SOCKET;
                (*c).cmsg_type = libc::SCM_RIGHTS;
                (*c).cmsg_len =
                    libc::CMSG_LEN((count * std::mem::size_of::<libc::c_int>()) as u32) as usize;
                std::ptr::copy_nonoverlapping(
                    rights.as_ptr(),
                    libc::CMSG_DATA(c).cast::<libc::c_int>(),
                    count,
                );
                assert_eq!(
                    libc::sendmsg(sender.as_raw_fd(), &msg, 0),
                    raw.len() as isize
                );
            }
            let before = fd_count();
            let result = receive_packet(&receiver, &peer, &expected);
            if count == 1 {
                assert!(matches!(result, Err(Error::Denied)));
            } else {
                assert!(
                    matches!(result, Err(Error::Invalid)),
                    "oversized control data must be reported as truncated"
                );
            }
            assert_eq!(fd_count(), before, "received SCM_RIGHTS descriptors leaked");
        }
        let mut fds = [-1; 2];
        assert_eq!(
            unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                    0,
                    fds.as_mut_ptr(),
                )
            },
            0
        );
        let sender = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fds[0]) };
        let receiver = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fds[1]) };
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
        // A complete valid frame plus whitespace: without MSG_TRUNC detection,
        // the received 131072-byte prefix would still parse successfully.
        let mut oversized = raw.clone();
        oversized.resize(131_073, b' ');
        assert!(validate_frame(&oversized[..131_072], &expected).is_ok());
        assert_eq!(
            unsafe {
                libc::send(
                    sender.as_raw_fd(),
                    oversized.as_ptr().cast(),
                    oversized.len(),
                    libc::MSG_DONTWAIT,
                )
            },
            oversized.len() as isize
        );
        assert!(
            matches!(
                receive_packet(&receiver, &peer, &expected),
                Err(Error::Invalid)
            ),
            "oversized payload must be reported as truncated"
        );
    }
}
