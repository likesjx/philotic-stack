//! Root-only exec transition: never exec the key-bearing helper as dumpable UID999.
use anyhow::Result;
#[cfg(target_os = "linux")] use anyhow::ensure;
#[cfg(target_os = "linux")]
pub fn enter() -> Result<()> { enter_with_probe(|| Ok(())) }
#[cfg(target_os = "linux")]
pub fn enter_with_probe(probe: impl FnOnce() -> Result<()>) -> Result<()> {
    // Read only this non-secret kernel setting. Never weaken or change it.
    ensure!(unsafe { libc::getuid() == 0 && libc::geteuid() == 0 }, "root start required");
    ensure!(matches!(std::fs::read_to_string("/proc/sys/fs/suid_dumpable")?.trim(), "0" | "2"), "unsafe kernel dump policy");
    unsafe {
        let account = libc::getpwuid(999);
        ensure!(!account.is_null(), "runtime identity absent");
        let gid = (*account).pw_gid;
        let limit = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        ensure!(libc::setrlimit(libc::RLIMIT_CORE, &limit) == 0, "core limit");
        ensure!(libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) == 0, "dump protection");
        ensure!(libc::prctl(libc::PR_SET_KEEPCAPS, 0, 0, 0, 0) == 0, "capability policy");
        ensure!(libc::prctl(libc::PR_CAP_AMBIENT, libc::PR_CAP_AMBIENT_CLEAR_ALL, 0, 0, 0) == 0, "ambient capabilities");
        ensure!(libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0, "privilege policy");
        ensure!(libc::setgroups(0, std::ptr::null()) == 0, "groups");
        ensure!(libc::setresgid(gid, gid, gid) == 0, "gid");
        // Kernel ptrace access denies dumpability 0 and 2 to unprivileged peers.
        // Policy1 is rejected: an attacker could attach before the final prctl.
        ensure!(libc::setresuid(999, 999, 999) == 0, "uid");
        ensure!(matches!(libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0), 0 | 2), "unsafe transition state");
        // Fixture-only callback makes the exact pre-protection window observable.
        // Production passes a no-op. No key FD or key bytes exist in this process yet.
        probe()?;
        ensure!(libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) == 0, "final dump protection");
        ensure!(libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) == 0, "final dump state");
        let (mut real, mut effective, mut saved) = (0, 0, 0);
        ensure!(libc::getresuid(&mut real, &mut effective, &mut saved) == 0 && (real,effective,saved)==(999,999,999), "uid state");
        ensure!(libc::getgroups(0, std::ptr::null_mut()) == 0, "supplementary groups");
        let (mut real_gid,mut effective_gid,mut saved_gid)=(0,0,0);
        ensure!(libc::getresgid(&mut real_gid,&mut effective_gid,&mut saved_gid)==0 && (real_gid,effective_gid,saved_gid)==(gid,gid,gid),"gid state");
        ensure!(libc::prctl(libc::PR_GET_NO_NEW_PRIVS,0,0,0,0)==1,"no-new-privileges state");
    }
    let status=std::fs::read_to_string("/proc/self/status")?;
    for field in ["CapInh:", "CapPrm:", "CapEff:", "CapAmb:"] {
        let value=status.lines().find_map(|line|line.strip_prefix(field)).ok_or_else(||anyhow::anyhow!("capability evidence absent"))?;
        ensure!(u64::from_str_radix(value.trim(),16)?==0,"capabilities retained");
    }
    // No exec after this point. A subsequent normal exec would reset dumpability.
    Ok(())
}
#[cfg(not(target_os = "linux"))]
pub fn enter() -> Result<()> { anyhow::bail!("Linux required") }

#[cfg(not(target_os = "linux"))]
pub fn enter_with_probe(_probe: impl FnOnce() -> Result<()>) -> Result<()> { anyhow::bail!("Linux required") }

#[cfg(target_os = "linux")]
pub fn receive_key(fd: i32) -> Result<()> {
    use std::io::{Read, Write};
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use base64::Engine;
    ensure!(fd >= 3 && unsafe { libc::geteuid()==999 && libc::prctl(libc::PR_GET_DUMPABLE,0,0,0,0)==0 }, "protected delivery required");
    let mut channel=unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
    channel.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
    channel.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
    channel.write_all(b"READY\n")?;
    let mut data=Vec::new();(&mut channel).take(130).read_to_end(&mut data)?;
    let line=std::str::from_utf8(&data)?;
    let encoded=line.strip_prefix("PHILOTIC_VAULT_MASTER_KEY=").and_then(|s|s.strip_suffix('\n')).ok_or_else(||anyhow::anyhow!("private input format"))?;
    let decoded=base64::engine::general_purpose::STANDARD.decode(encoded)?;
    ensure!(decoded.len()==32 && base64::engine::general_purpose::STANDARD.encode(&decoded)==encoded,"private input format");
    let path=std::path::PathBuf::from(std::env::var("HOME")?).join(".philotic/vault-master-key.env");
    let mut file=std::fs::OpenOptions::new().write(true).create_new(true).mode(0o400).custom_flags(libc::O_NOFOLLOW|libc::O_CLOEXEC).open(path)?;
    file.write_all(&data)?;
    Ok(())
}
#[cfg(not(target_os = "linux"))]
pub fn receive_key(_fd: i32) -> Result<()> { anyhow::bail!("Linux required") }
