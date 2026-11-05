//! Unwired Linux root-publisher client for the existing hotel stream protocol.
//!
//! This does not create a listener, authorize an operation, establish trusted
//! manager provenance or install a publisher. A caller must pin the actual
//! hotel incarnation via a trusted manager before constructing this client.
//! Kernel credentials are checked on every received fragment; root remains TCB.
use crate::{Error, Result};

/// A frame was handed to the local socket; this is not a commit or success.
#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PublicationSubmitted;

/// Historical operation state, not a statement of current grant validity.
#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PublicationStatus {
    Unknown,
    Pending,
    Complete,
    RecoveryRequired,
}

#[cfg(target_os = "linux")]
pub(crate) struct PublisherClient {
    stream: std::os::unix::net::UnixStream,
    hotel: crate::protected_peer_candidate::ManagerPinnedProcess,
    spent: bool,
}

#[cfg(target_os = "linux")]
impl PublisherClient {
    /// Runs on a dedicated blocking thread. The caller supplies a preconnected
    /// stream and manager-pinned hotel; the stream must not be shared.
    pub(crate) fn new(
        stream: std::os::unix::net::UnixStream,
        hotel: crate::protected_peer_candidate::ManagerPinnedProcess,
    ) -> Result<Self> {
        use std::os::fd::AsRawFd;
        let fd = stream.as_raw_fd();
        let mut kind: libc::c_int = 0;
        let mut len = std::mem::size_of_val(&kind) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&mut kind as *mut libc::c_int).cast(),
                &mut len,
            )
        } != 0
            || len as usize != std::mem::size_of_val(&kind)
            || kind != libc::SOCK_STREAM
        {
            return Err(Error::Denied);
        }
        let one: libc::c_int = 1;
        if unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PASSCRED,
                (&one as *const libc::c_int).cast(),
                std::mem::size_of_val(&one) as libc::socklen_t,
            )
        } != 0
        {
            return Err(Error::Unavailable);
        }
        let mut peer: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of_val(&peer) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut peer as *mut libc::ucred).cast(),
                &mut len,
            )
        } != 0
            || len as usize != std::mem::size_of_val(&peer)
        {
            return Err(Error::Unavailable);
        }
        hotel.verify_sender(peer.uid, u32::try_from(peer.pid).ok())?;
        stream
            .set_write_timeout(Some(std::time::Duration::from_secs(5)))
            .map_err(|_| Error::Unavailable)?;
        Ok(Self {
            stream,
            hotel,
            spent: false,
        })
    }

    fn send(&mut self, frame: &crate::authority_control::Request) -> Result<()> {
        use std::io::Write;
        let bytes = serde_json::to_vec(frame).map_err(|_| Error::Invalid)?;
        if bytes.is_empty() || bytes.len() > 131_072 {
            return Err(Error::Invalid);
        }
        self.stream
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .and_then(|_| self.stream.write_all(&bytes))
            .map_err(|_| Error::Unavailable)
    }
    fn recv(&self) -> Result<crate::authority_control::Reply> {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut header = [0u8; 4];
        recv_exact_authenticated(&self.stream, &self.hotel, &mut header, until)?;
        let size = u32::from_be_bytes(header) as usize;
        if size == 0 || size > 131_072 {
            return Err(Error::Invalid);
        }
        let mut bytes = vec![0; size];
        recv_exact_authenticated(&self.stream, &self.hotel, &mut bytes, until)?;
        serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)
    }
    fn poison<T>(&mut self) -> Result<T> {
        self.spent = true;
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        Err(Error::Uncertain)
    }

    /// A separate authenticated status consumer connects independently and
    /// pins the hotel incarnation. Never retries or mutates a publication.
    pub(crate) fn status(&mut self, guest: &str, id: uuid::Uuid) -> Result<PublicationStatus> {
        use crate::authority_control::{Reply, Request};
        if self.spent {
            return Err(Error::Uncertain);
        }
        self.spent = true;
        if self
            .send(&Request::SnapshotStatus {
                guest: guest.into(),
                operation_id: id.to_string(),
            })
            .is_err()
        {
            return self.poison();
        }
        let nonce = match self.recv() {
            Ok(Reply::SnapshotChallenge { nonce }) => nonce,
            _ => return self.poison(),
        };
        if self
            .send(&Request::ConfirmSnapshot {
                operation_id: id.to_string(),
                nonce,
            })
            .is_err()
        {
            return self.poison();
        }
        match self.recv() {
            Ok(Reply::SnapshotUnknown) => Ok(PublicationStatus::Unknown),
            Ok(Reply::SnapshotPending) => Ok(PublicationStatus::Pending),
            Ok(Reply::SnapshotComplete) => Ok(PublicationStatus::Complete),
            Ok(Reply::SnapshotRecoveryRequired) => Ok(PublicationStatus::RecoveryRequired),
            _ => self.poison(),
        }
    }

    /// Experimental one-way submission. Return only means the frame reached
    /// the local socket, never that publication succeeded. The blocking install
    /// closure is not bounded; this remains unwired until the hotel owns and
    /// verifies the publisher's complete lifecycle and recovery.
    pub(crate) fn publish(
        &mut self,
        guest: &str,
        id: uuid::Uuid,
        expected: [u8; 32],
        install: impl FnOnce() -> Result<[u8; 32]>,
    ) -> Result<PublicationSubmitted> {
        use crate::authority_control::{Reply, Request};
        if self.spent {
            return Err(Error::Uncertain);
        }
        self.spent = true;
        if self
            .send(&Request::BeginSnapshot {
                guest: guest.into(),
                operation_id: id.to_string(),
            })
            .is_err()
        {
            return self.poison();
        }
        let nonce = match self.recv() {
            Ok(Reply::SnapshotChallenge { nonce }) => nonce,
            _ => return self.poison(),
        };
        if self
            .send(&Request::ConfirmSnapshot {
                operation_id: id.to_string(),
                nonce,
            })
            .is_err()
            || !matches!(self.recv(), Ok(Reply::Ready))
        {
            return self.poison();
        }
        let actual = match install() {
            Ok(digest) if digest == expected => digest,
            _ => return self.poison(),
        };
        if self
            .send(&Request::FinishSnapshot {
                operation_id: id.to_string(),
                digest: actual,
            })
            .is_err()
        {
            return self.poison();
        }
        self.stream
            .shutdown(std::net::Shutdown::Both)
            .map_err(|_| Error::Uncertain)?;
        Ok(PublicationSubmitted)
    }
}

/// Reads exactly the requested bytes. Each Linux recvmsg result must carry one
/// matching SCM_CREDENTIALS record. Reject ancillary rights after closing any
/// received descriptors, and reject truncated or missing credentials.
#[cfg(target_os = "linux")]
fn recv_exact_authenticated(
    stream: &std::os::unix::net::UnixStream,
    peer: &crate::protected_peer_candidate::ManagerPinnedProcess,
    out: &mut [u8],
    until: std::time::Instant,
) -> Result<()> {
    use std::os::fd::AsRawFd;
    let fd = stream.as_raw_fd();
    let mut offset = 0;
    while offset < out.len() {
        let remaining = until
            .checked_duration_since(std::time::Instant::now())
            .ok_or(Error::Unavailable)?;
        let millis = remaining.as_millis().min(i32::MAX as u128) as i32;
        let mut poll = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut poll, 1, millis) } != 1 || poll.revents & libc::POLLIN == 0 {
            return Err(Error::Unavailable);
        }
        let mut control = [0usize; 64];
        let mut iov = libc::iovec {
            iov_base: out[offset..].as_mut_ptr().cast(),
            iov_len: out.len() - offset,
        };
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = std::mem::size_of_val(&control);
        let n = unsafe { libc::recvmsg(fd, &mut msg, libc::MSG_CMSG_CLOEXEC | libc::MSG_DONTWAIT) };
        if n <= 0 {
            return Err(Error::Unavailable);
        }
        if msg.msg_controllen > std::mem::size_of_val(&control) {
            return Err(Error::Denied);
        }
        let mut credentials = None;
        let mut unexpected = false;
        unsafe {
            let begin = msg.msg_control as usize;
            let end = begin.checked_add(msg.msg_controllen).ok_or(Error::Denied)?;
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
                    && h.cmsg_len
                        == libc::CMSG_LEN(std::mem::size_of::<libc::ucred>() as u32) as usize
                    && credentials.is_none()
                {
                    credentials = Some(std::ptr::read_unaligned(
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
            return Err(Error::Denied);
        }
        let sender = credentials.ok_or(Error::Denied)?;
        peer.verify_sender(sender.uid, u32::try_from(sender.pid).ok())?;
        if std::time::Instant::now() >= until {
            return Err(Error::Unavailable);
        }
        offset += n as usize;
    }
    Ok(())
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;
    struct FixedManager(crate::protected_peer_candidate::ManagerIdentity);
    impl crate::protected_peer_candidate::ManagerEvidence for FixedManager {
        fn current(&self) -> Result<crate::protected_peer_candidate::ManagerIdentity> {
            Ok(self.0.clone())
        }
    }
    fn pin_self() -> crate::protected_peer_candidate::ManagerPinnedProcess {
        let identity = crate::protected_peer_candidate::ManagerIdentity {
            pid: unsafe { libc::getpid() } as u32,
            uid: unsafe { libc::geteuid() },
            invocation: [1; 16],
        };
        crate::protected_peer_candidate::ManagerPinnedProcess::pin(std::sync::Arc::new(
            FixedManager(identity),
        ))
        .unwrap()
    }
    fn receive_request(
        stream: &mut std::os::unix::net::UnixStream,
    ) -> crate::authority_control::Request {
        let mut header = [0; 4];
        stream.read_exact(&mut header).unwrap();
        let mut bytes = vec![0; u32::from_be_bytes(header) as usize];
        stream.read_exact(&mut bytes).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }
    fn reply_fragmented(
        stream: &mut std::os::unix::net::UnixStream,
        reply: &crate::authority_control::Reply,
    ) {
        let bytes = serde_json::to_vec(reply).unwrap();
        let header = (bytes.len() as u32).to_be_bytes();
        for byte in header {
            stream.write_all(&[byte]).unwrap();
        }
        for chunk in bytes.chunks(2) {
            stream.write_all(chunk).unwrap();
        }
    }
    #[test]
    fn linux_stream_lifecycle_accepts_fragmented_same_process_replies() {
        use crate::authority_control::{Reply, Request};
        let (client, mut server) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut publisher = PublisherClient::new(client, pin_self()).unwrap();
        let id = uuid::Uuid::new_v4();
        let expected = [7; 32];
        let worker = std::thread::spawn(move || {
            assert!(
                matches!(receive_request(&mut server),Request::BeginSnapshot {operation_id,..} if operation_id==id.to_string())
            );
            let nonce = [4; 16];
            reply_fragmented(&mut server, &Reply::SnapshotChallenge { nonce });
            assert!(
                matches!(receive_request(&mut server),Request::ConfirmSnapshot { operation_id, nonce: got } if operation_id==id.to_string() && got==nonce)
            );
            reply_fragmented(&mut server, &Reply::Ready);
            assert!(
                matches!(receive_request(&mut server),Request::FinishSnapshot {operation_id,digest} if operation_id==id.to_string() && digest==expected)
            );
            let mut byte = [0u8; 1];
            assert_eq!(server.read(&mut byte).unwrap(), 0);
        });
        assert!(
            publisher
                .publish("publisher", id, expected, || Ok(expected))
                .is_ok()
        );
        worker.join().unwrap();
        assert!(matches!(
            publisher.publish("publisher", id, expected, || Ok(expected)),
            Err(Error::Uncertain)
        ));
    }
    #[test]
    fn linux_status_client_confirms_challenge_and_reports_historical_outcome() {
        use crate::authority_control::{Reply, Request};
        let (client, mut server) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut client = PublisherClient::new(client, pin_self()).unwrap();
        let id = uuid::Uuid::new_v4();
        let worker = std::thread::spawn(move || {
            assert!(
                matches!(receive_request(&mut server), Request::SnapshotStatus { guest, operation_id } if guest == "membrane" && operation_id == id.to_string())
            );
            let nonce = [9; 16];
            reply_fragmented(&mut server, &Reply::SnapshotChallenge { nonce });
            assert!(
                matches!(receive_request(&mut server), Request::ConfirmSnapshot { operation_id, nonce: got } if operation_id == id.to_string() && got == nonce)
            );
            reply_fragmented(&mut server, &Reply::SnapshotComplete);
        });
        assert_eq!(
            client.status("membrane", id).unwrap(),
            PublicationStatus::Complete
        );
        assert!(matches!(
            client.status("membrane", id),
            Err(Error::Uncertain)
        ));
        worker.join().unwrap();
    }

    #[test]
    fn linux_stream_rejects_delegated_socket_sender_mid_frame() {
        use crate::authority_control::Reply;
        let (client, mut sender) = std::os::unix::net::UnixStream::pair().unwrap();
        let publisher = PublisherClient::new(client, pin_self()).unwrap();
        let payload = serde_json::to_vec(&Reply::Ready).unwrap();
        sender
            .write_all(&(payload.len() as u32).to_be_bytes())
            .unwrap();
        let child = unsafe { libc::fork() };
        assert!(child >= 0);
        if child == 0 {
            let sent = unsafe {
                libc::send(
                    sender.as_raw_fd(),
                    payload.as_ptr().cast(),
                    payload.len(),
                    0,
                )
            };
            unsafe { libc::_exit(if sent == payload.len() as isize { 0 } else { 1 }) };
        }
        assert!(matches!(publisher.recv(), Err(Error::Denied)));
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
    }
}
