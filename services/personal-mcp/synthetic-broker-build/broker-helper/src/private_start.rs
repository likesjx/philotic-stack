//! Root-only exec transition: never exec the key-bearing helper as dumpable UID999.
use anyhow::Result;
#[cfg(target_os = "linux")] use anyhow::ensure;
#[cfg(target_os = "linux")]
pub fn enter() -> Result<()> {
    // Read only this non-secret kernel setting. Never weaken or change it.
    ensure!(unsafe { libc::getuid() == 0 && libc::geteuid() == 0 }, "root start required");
    ensure!(std::fs::read_to_string("/proc/sys/fs/suid_dumpable")?.trim() == "0", "unsafe kernel dump policy");
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
        // suid_dumpable==0 also protects the kernel transition itself, before the next instruction.
        ensure!(libc::setresuid(999, 999, 999) == 0, "uid");
        ensure!(libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) == 0, "dump state changed");
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
