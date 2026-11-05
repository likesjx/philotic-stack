//! Experimental peer identity building block; not wired into any control path.
//!
//! The expected PID must be obtained from a trusted service manager, pinned
//! before connection, and revalidated with that manager. Kernel PID/UID and
//! pidfd liveness alone do not authorize operations, establish executable
//! identity, prevent descriptor delegation, or replace launch-generation fencing.
use crate::{Error, Result};

/// Evidence must be read from a trusted service manager, never a caller frame
/// or user-writable pidfile. The systemd provider below is unqualified and
/// must not be treated as trusted until its host integration is verified.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagerIdentity {
    pub pid: u32,
    pub uid: u32,
    pub invocation: [u8; 16],
}
pub trait ManagerEvidence: Send + Sync {
    fn current(&self) -> Result<ManagerIdentity>;
}

// Parse only the two non-secret systemd properties used for identity. The UID
// is pinned separately by trusted configuration and checked against the kernel.
fn parse_manager_properties(output: &str, uid: u32) -> Result<ManagerIdentity> {
    let mut pid = None;
    let mut invocation = None;
    for line in output.lines() {
        let (key, value) = line.split_once('=').ok_or(Error::Invalid)?;
        match key {
            "MainPID" if pid.is_none() => {
                pid = Some(value.parse::<u32>().map_err(|_| Error::Invalid)?);
            }
            "InvocationID" if invocation.is_none() => {
                if value.len() != 32
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(Error::Invalid);
                }
                let mut bytes = [0u8; 16];
                for (i, slot) in bytes.iter_mut().enumerate() {
                    *slot = u8::from_str_radix(&value[i * 2..i * 2 + 2], 16)
                        .map_err(|_| Error::Invalid)?;
                }
                invocation = Some(bytes);
            }
            _ => return Err(Error::Invalid),
        }
    }
    let identity = ManagerIdentity {
        pid: pid.ok_or(Error::Invalid)?,
        uid,
        invocation: invocation.ok_or(Error::Invalid)?,
    };
    same_incarnation(&identity, &identity)?;
    Ok(identity)
}

/// Unwired Linux candidate: fixed service, fixed systemctl path and fixed
/// system bus environment. It trusts root-owned systemd and filesystem state.
/// It has not been qualified against the live service, process substitution,
/// or manager restart, and must run outside an async executor thread.
#[cfg(target_os = "linux")]
pub struct SystemdHotelManager {
    pub pinned_uid: u32,
}

#[cfg(target_os = "linux")]
impl ManagerEvidence for SystemdHotelManager {
    fn current(&self) -> Result<ManagerIdentity> {
        use std::{
            io::{ErrorKind, Read},
            os::fd::AsRawFd,
            os::unix::fs::MetadataExt,
            process::{Command, Stdio},
            time::{Duration, Instant},
        };
        let binary = std::path::Path::new("/usr/bin/systemctl");
        let mut path = Some(binary);
        while let Some(p) = path {
            let m = std::fs::symlink_metadata(p).map_err(|_| Error::Unavailable)?;
            if m.uid() != 0
                || m.mode() & 0o022 != 0
                || (p == binary && !m.is_file())
                || (p != binary && !m.is_dir())
            {
                return Err(Error::Denied);
            }
            path = p.parent();
        }
        let mut child = Command::new(binary)
            .args([
                "show",
                "philotic-hotel.service",
                "--property=MainPID",
                "--property=InvocationID",
                "--no-pager",
            ])
            .env_clear()
            .env("LC_ALL", "C")
            .env("SYSTEMD_PAGER", "cat")
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| Error::Unavailable)?;
        let mut stdout = match child.stdout.take() {
            Some(stdout) => stdout,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Unavailable);
            }
        };
        let flags = unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_GETFL) };
        if flags < 0
            || unsafe { libc::fcntl(stdout.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                < 0
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Unavailable);
        }
        let until = Instant::now() + Duration::from_secs(2);
        let mut output = Vec::new();
        let mut status = None;
        let mut eof = false;
        loop {
            if Instant::now() >= until {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Unavailable);
            }
            let mut buf = [0u8; 256];
            if !eof {
                match stdout.read(&mut buf) {
                    Ok(0) => eof = true,
                    Ok(n) => {
                        output.extend_from_slice(&buf[..n]);
                        if output.len() > 1024 {
                            let _ = child.kill();
                            let _ = child.wait();
                            return Err(Error::Unavailable);
                        }
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                    Err(_) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(Error::Unavailable);
                    }
                }
            }
            if status.is_none() {
                match child.try_wait() {
                    Ok(value) => status = value,
                    Err(_) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(Error::Unavailable);
                    }
                }
            }
            if eof && status.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !status.ok_or(Error::Unavailable)?.success() {
            return Err(Error::Unavailable);
        }
        parse_manager_properties(
            std::str::from_utf8(&output).map_err(|_| Error::Invalid)?,
            self.pinned_uid,
        )
    }
}

fn same_incarnation(expected: &ManagerIdentity, current: &ManagerIdentity) -> Result<()> {
    if expected.pid == 0 || expected.invocation == [0; 16] || expected != current {
        return Err(Error::Denied);
    }
    Ok(())
}

fn check_observation(
    expected_uid: u32,
    expected_pid: u32,
    peer_uid: u32,
    peer_pid: Option<u32>,
    alive: bool,
) -> Result<()> {
    if expected_pid == 0 || !alive || peer_uid != expected_uid || peer_pid != Some(expected_pid) {
        return Err(Error::Denied);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(crate) struct PinnedProcess {
    fd: std::os::fd::OwnedFd,
    uid: u32,
    pid: u32,
}

#[cfg(target_os = "linux")]
impl PinnedProcess {
    /// `pid` must come from a trusted manager, not a socket frame or a
    /// caller-controlled pidfile. The manager must confirm the same live
    /// incarnation after this call and before each effect. This method alone
    /// cannot prove the original PID lookup was free from a reuse race.
    pub(crate) fn pin_manager_process(pid: u32, uid: u32) -> Result<Self> {
        use std::os::fd::FromRawFd;
        if pid == 0 || pid > i32::MAX as u32 {
            return Err(Error::Invalid);
        }
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
        if fd < 0 {
            return Err(Error::Unavailable);
        }
        let this = Self {
            fd: unsafe { std::os::fd::OwnedFd::from_raw_fd(fd as i32) },
            uid,
            pid,
        };
        if !this.alive()? {
            return Err(Error::Denied);
        }
        Ok(this)
    }
    fn alive(&self) -> Result<bool> {
        use std::os::fd::AsRawFd;
        let mut poll = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut poll, 1, 0) };
        if result < 0 {
            return Err(Error::Unavailable);
        }
        Ok(result == 0 && poll.revents == 0)
    }
    pub(crate) fn verify_sender(&self, uid: u32, pid: Option<u32>) -> Result<()> {
        check_observation(self.uid, self.pid, uid, pid, self.alive()?)
    }
    /// Check on the same established stream immediately before each effect.
    /// A live peer can delegate the socket; this check does not prevent that.
    pub fn verify_stream(&self, stream: &tokio::net::UnixStream) -> Result<()> {
        let peer = stream.peer_cred().map_err(|_| Error::Unavailable)?;
        let pid = peer.pid().and_then(|p| u32::try_from(p).ok());
        check_observation(self.uid, self.pid, peer.uid(), pid, self.alive()?)
    }
}

/// Manager observations bracket pidfd creation and every use. This candidate
/// does not supply a trusted manager provider, prevent manager rollback, or
/// establish that a claimed invocation ID is authentic.
#[cfg(target_os = "linux")]
pub struct ManagerPinnedProcess {
    manager: std::sync::Arc<dyn ManagerEvidence>,
    expected: ManagerIdentity,
    process: PinnedProcess,
}

#[cfg(target_os = "linux")]
impl ManagerPinnedProcess {
    pub fn pin(manager: std::sync::Arc<dyn ManagerEvidence>) -> Result<Self> {
        let expected = manager.current()?;
        same_incarnation(&expected, &expected)?;
        let process = PinnedProcess::pin_manager_process(expected.pid, expected.uid)?;
        same_incarnation(&expected, &manager.current()?)?;
        if !process.alive()? {
            return Err(Error::Denied);
        }
        Ok(Self {
            manager,
            expected,
            process,
        })
    }
    pub(crate) fn verify_sender(&self, uid: u32, pid: Option<u32>) -> Result<()> {
        same_incarnation(&self.expected, &self.manager.current()?)?;
        self.process.verify_sender(uid, pid)?;
        same_incarnation(&self.expected, &self.manager.current()?)?;
        if !self.process.alive()? {
            return Err(Error::Denied);
        }
        Ok(())
    }
    /// Observation only. It does not lock manager lifecycle after return.
    pub(crate) fn verify_manager(&self) -> Result<()> {
        same_incarnation(&self.expected, &self.manager.current()?)?;
        if !self.process.alive()? {
            return Err(Error::Denied);
        }
        same_incarnation(&self.expected, &self.manager.current()?)?;
        if !self.process.alive()? {
            return Err(Error::Denied);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn synthetic_peer_policy_rejects_same_uid_impostor_and_retired_pid() {
        assert!(check_observation(999, 42, 999, Some(42), true).is_ok());
        assert_eq!(
            check_observation(999, 42, 999, Some(43), true),
            Err(Error::Denied)
        );
        assert_eq!(
            check_observation(999, 42, 0, Some(42), true),
            Err(Error::Denied)
        );
        assert_eq!(
            check_observation(999, 42, 999, Some(42), false),
            Err(Error::Denied)
        );
        assert_eq!(
            check_observation(999, 42, 999, None, true),
            Err(Error::Denied)
        );
        assert_eq!(
            check_observation(999, 0, 999, Some(0), true),
            Err(Error::Denied)
        );
    }
    #[test]
    fn manager_generation_change_and_missing_evidence_fail_closed() {
        let expected = ManagerIdentity {
            pid: 42,
            uid: 999,
            invocation: [1; 16],
        };
        assert!(same_incarnation(&expected, &expected).is_ok());
        let mut current = expected.clone();
        current.invocation = [2; 16];
        assert_eq!(same_incarnation(&expected, &current), Err(Error::Denied));
        current = expected.clone();
        current.pid = 43;
        assert_eq!(same_incarnation(&expected, &current), Err(Error::Denied));
        current = expected.clone();
        current.uid = 0;
        assert_eq!(same_incarnation(&expected, &current), Err(Error::Denied));
        current = expected.clone();
        current.invocation = [0; 16];
        assert_eq!(same_incarnation(&current, &current), Err(Error::Denied));
    }
    #[test]
    fn systemd_property_parser_rejects_missing_duplicate_or_invalid_identity() {
        let good = "MainPID=42\nInvocationID=01010101010101010101010101010101\n";
        assert_eq!(
            parse_manager_properties(good, 999).unwrap(),
            ManagerIdentity {
                pid: 42,
                uid: 999,
                invocation: [1; 16]
            }
        );
        for bad in [
            "MainPID=0\nInvocationID=01010101010101010101010101010101\n",
            "MainPID=42\n",
            "MainPID=42\nMainPID=43\nInvocationID=01010101010101010101010101010101\n",
            "MainPID=42\nInvocationID=00000000000000000000000000000000\n",
            "MainPID=42\nInvocationID=zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\n",
            "MainPID=42\nInvocationID=01010101010101010101010101010101\nExtra=x\n",
        ] {
            assert!(parse_manager_properties(bad, 999).is_err());
        }
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn live_pidfd_rejects_changed_manager_generation() {
        struct MutableManager(std::sync::Mutex<ManagerIdentity>);
        impl ManagerEvidence for MutableManager {
            fn current(&self) -> Result<ManagerIdentity> {
                self.0
                    .lock()
                    .map(|v| v.clone())
                    .map_err(|_| Error::Unavailable)
            }
        }
        let identity = ManagerIdentity {
            pid: unsafe { libc::getpid() } as u32,
            uid: unsafe { libc::geteuid() },
            invocation: [1; 16],
        };
        let manager = std::sync::Arc::new(MutableManager(std::sync::Mutex::new(identity.clone())));
        let pinned = ManagerPinnedProcess::pin(manager.clone()).unwrap();
        assert!(
            pinned
                .verify_sender(identity.uid, Some(identity.pid))
                .is_ok()
        );
        manager.0.lock().unwrap().invocation = [2; 16];
        assert_eq!(
            pinned.verify_sender(identity.uid, Some(identity.pid)),
            Err(Error::Denied)
        );
    }
}
