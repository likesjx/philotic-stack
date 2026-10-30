//! Shared verified artifact reader used by supervised read and native workers.
use anyhow::{bail, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::PathBuf,
};
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub path: PathBuf,
    pub sha256: String,
}
pub fn validate_artifact(a: &Artifact) -> Result<()> {
    if !a.path.is_absolute()
        || a.path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        bail!("artifact path invalid");
    }
    if a.sha256.len() != 64
        || !a
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        bail!("artifact digest invalid");
    }
    Ok(())
}
pub fn verified_artifact(a: &Artifact, executable: bool, uid: u32) -> Result<File> {
    let before = std::fs::symlink_metadata(&a.path)?;
    if !before.is_file()
        || (before.uid() != uid && before.uid() != 0)
        || before.mode() & 0o022 != 0
        || (before.uid() == uid && before.mode() & 0o222 != 0)
        || before.len() > 536870912
        || (executable && (before.mode() & 0o111 == 0 || before.mode() & 0o6000 != 0))
    {
        bail!("artifact unavailable");
    }
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&a.path)?;
    let m = f.metadata()?;
    if m.dev() != before.dev() || m.ino() != before.ino() {
        bail!("artifact changed");
    }
    let mut hash = Sha256::new();
    let mut buf = [0u8; 65536];
    let mut first = true;
    let mut read = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        read += n as u64;
        if read > 536870912 {
            bail!("artifact size exceeded");
        }
        if first && executable && (n < 4 || &buf[..4] != b"\x7fELF") {
            bail!("ELF executable required");
        }
        first = false;
        hash.update(&buf[..n]);
    }
    let after = f.metadata()?;
    if first
        || format!("{:x}", hash.finalize()) != a.sha256
        || m.len() != after.len()
        || m.mtime() != after.mtime()
        || m.mtime_nsec() != after.mtime_nsec()
        || m.ctime() != after.ctime()
        || m.ctime_nsec() != after.ctime_nsec()
    {
        bail!("artifact verification failed");
    }
    Ok(f)
}
