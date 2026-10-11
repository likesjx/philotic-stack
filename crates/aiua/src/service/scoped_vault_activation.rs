//! Opt-in named supervisor descriptor adoption. Never bind or chmod a socket.
use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd};
use tokio::net::UnixListener;

#[cfg(any(target_os = "linux", test))]
const NAME: &str = "percival-scoped-vault";
fn denied() -> io::Error {
    io::Error::other("scoped vault activation unavailable")
}

#[cfg(target_os = "linux")]
fn scoped_descriptors() -> io::Result<Vec<i32>> {
    let mut descriptors = Vec::new();
    for entry in std::fs::read_dir("/proc/self/fd")? {
        let entry = entry?;
        let Some(fd) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        // SAFETY: getsockname writes into a correctly sized zeroed address;
        // it borrows the descriptor and cannot close it or reveal credential data.
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of_val(&address) as libc::socklen_t;
        if unsafe {
            libc::getsockname(
                fd,
                (&mut address as *mut libc::sockaddr_un).cast(),
                &mut len,
            )
        } == 0
            && address.sun_family as i32 == libc::AF_UNIX
        {
            let path: Vec<u8> = address
                .sun_path
                .iter()
                .take_while(|byte| **byte != 0)
                .map(|byte| *byte as u8)
                .collect();
            if path == b"/run/percival-personal-vault-broker.sock" {
                descriptors.push(fd);
            }
        }
    }
    Ok(descriptors)
}

#[cfg(any(target_os = "linux", test))]
fn descriptor(pid: &str, count: &str, names: &str, own_pid: u32) -> io::Result<i32> {
    if pid.parse::<u32>().ok() != Some(own_pid) {
        return Err(denied());
    }
    let count: usize = count.parse().map_err(|_| denied())?;
    if count == 0 || count > 64 {
        return Err(denied());
    }
    let names: Vec<_> = names.split(':').collect();
    if names.len() != count {
        return Err(denied());
    }
    let matches: Vec<_> = names
        .iter()
        .enumerate()
        .filter(|(_, name)| **name == NAME)
        .collect();
    if matches.len() != 1 {
        return Err(denied());
    }
    Ok(3 + matches[0].0 as i32)
}

/// Duplicate only a kernel-validated listening Unix stream FD, close-on-exec.
/// The inherited original is also close-on-exec so materialized guests cannot
/// inherit credential endpoint authority. Policy/path/owner validation follows
/// in serve(), before any connection is accepted.
#[cfg(target_os = "linux")]
pub fn adopt_named(pid: &str, count: &str, names: &str) -> io::Result<UnixListener> {
    let fd = descriptor(pid, count, names, std::process::id())?;
    // No second advertised or hidden alias may reach guest exec. Reject before
    // adopting the listener; only the unique selected original is allowed.
    if scoped_descriptors()?
        .iter()
        .any(|candidate| *candidate != fd)
    {
        return Err(denied());
    }
    for (option, expected) in [
        (libc::SO_DOMAIN, libc::AF_UNIX),
        (libc::SO_TYPE, libc::SOCK_STREAM),
        (libc::SO_ACCEPTCONN, 1),
    ] {
        let mut value: libc::c_int = 0;
        let mut len = std::mem::size_of_val(&value) as libc::socklen_t;
        // SAFETY: getsockopt writes to this valid integer buffer; fd is not owned.
        if unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                option,
                (&mut value as *mut libc::c_int).cast(),
                &mut len,
            )
        } != 0
            || len as usize != std::mem::size_of_val(&value)
            || value != expected
        {
            return Err(denied());
        }
    }
    // SAFETY: checked live descriptor; fcntl does not transfer ownership.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(denied());
    }
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate < 0 {
        return Err(denied());
    }
    // SAFETY: duplicate is newly owned and proven to be a Unix listening socket.
    let listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(duplicate) };
    listener.set_nonblocking(true)?;
    debug_assert_ne!(listener.as_raw_fd(), fd);
    UnixListener::from_std(listener)
}
#[cfg(not(target_os = "linux"))]
pub fn adopt_named(_pid: &str, _count: &str, _names: &str) -> io::Result<UnixListener> {
    Err(denied())
}

pub fn optional() -> io::Result<Option<UnixListener>> {
    // A disabled endpoint must not silently retain supervisor descriptors before
    // materializing guests. Reject *all* unexpected activation metadata: even a
    // wrong/missing name or PID must not let an inherited scoped FD escape.
    let disabled = || {
        #[cfg(target_os = "linux")]
        if !scoped_descriptors()?.is_empty() {
            return Err(denied());
        }
        if ["LISTEN_PID", "LISTEN_FDS", "LISTEN_FDNAMES"]
            .iter()
            .any(|key| std::env::var_os(key).is_some())
        {
            Err(denied())
        } else {
            Ok(None)
        }
    };
    match std::env::var("PHILOTIC_SCOPED_VAULT_ENABLED") {
        Err(std::env::VarError::NotPresent) => disabled(),
        Ok(value) if value == "0" => disabled(),
        Ok(value) if value == "1" => {
            let get = |key| std::env::var(key).map_err(|_| denied());
            adopt_named(
                &get("LISTEN_PID")?,
                &get("LISTEN_FDS")?,
                &get("LISTEN_FDNAMES")?,
            )
            .map(Some)
        }
        _ => Err(denied()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_pid_count_and_unique_name_required() {
        assert_eq!(
            descriptor("42", "2", "general:percival-scoped-vault", 42).unwrap(),
            4
        );
        for (pid, count, names) in [
            ("41", "1", NAME),
            ("42", "0", ""),
            ("42", "65", NAME),
            ("42", "2", NAME),
            ("42", "1", "general"),
            ("42", "2", "percival-scoped-vault:percival-scoped-vault"),
        ] {
            assert!(descriptor(pid, count, names, 42).is_err());
        }
    }
}
