//! Shared durable uncertainty fence for hotel-owned issuer and native execution.
//! Extracted from the existing read owner; no credentials or lifecycle owner.
use anyhow::{anyhow, bail, Result};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use tokio::sync::OwnedMutexGuard;
// Durable uncertainty marker, not a policy/grant database. The operator must
// preprovision a private clean marker and explicitly reconcile dirty recovery.
// File lock prevents two daemons from owning this journal concurrently.
pub struct Journal {
    file: File,
    path: PathBuf,
    uid: u32,
    device: u64,
    inode: u64,
    poisoned: bool,
}
impl Journal {
    pub fn open(path: &Path, uid: u32) -> Result<Self> {
        let before = std::fs::symlink_metadata(path)?;
        let parent =
            std::fs::symlink_metadata(path.parent().ok_or_else(|| anyhow!("owner unavailable"))?)?;
        if !path.is_absolute()
            || !before.is_file()
            || before.uid() != uid
            || before.mode() & 0o077 != 0
            || !parent.is_dir()
            || parent.uid() != uid
            || parent.mode() & 0o077 != 0
        {
            bail!("owner unavailable");
        }
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        file.try_lock().map_err(|_| anyhow!("owner unavailable"))?;
        let after = file.metadata()?;
        if after.dev() != before.dev() || after.ino() != before.ino() {
            bail!("owner unavailable");
        }
        let mut value = Vec::new();
        (&mut file).take(7).read_to_end(&mut value)?;
        if value != b"clean\n" {
            bail!("owner recovery required");
        }
        Ok(Self {
            file,
            path: path.into(),
            uid,
            device: after.dev(),
            inode: after.ino(),
            poisoned: false,
        })
    }
    pub fn validate(&self) -> Result<()> {
        let s = std::fs::symlink_metadata(&self.path)?;
        if self.poisoned
            || !s.is_file()
            || s.uid() != self.uid
            || s.mode() & 0o077 != 0
            || s.dev() != self.device
            || s.ino() != self.inode
        {
            bail!("owner unavailable");
        }
        Ok(())
    }
    pub fn mark(&mut self, clean: bool) -> Result<()> {
        self.validate()?;
        let result = (|| -> Result<()> {
            self.file.seek(SeekFrom::Start(0))?;
            self.file
                .write_all(if clean { b"clean\n" } else { b"dirty\n" })?;
            self.file.set_len(6)?;
            self.file.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            self.poison();
        }
        result
    }
    pub fn poison(&mut self) {
        self.poisoned = true;
    } // dirty marker remains durable
}

// An aborted/panicking task cannot bypass async cleanup and silently reopen
// admission in the same daemon. Drop is deliberately conservative; only an
// acknowledged clean completion can disarm the durable uncertainty guard.
pub struct Fence {
    journal: OwnedMutexGuard<Journal>,
    complete: bool,
}
impl std::ops::Deref for Fence {
    type Target = Journal;
    fn deref(&self) -> &Journal {
        &self.journal
    }
}
impl std::ops::DerefMut for Fence {
    fn deref_mut(&mut self) -> &mut Journal {
        &mut self.journal
    }
}
impl Drop for Fence {
    fn drop(&mut self) {
        if !self.complete {
            self.journal.poison();
        }
    }
}

impl Fence {
    pub fn new(journal: OwnedMutexGuard<Journal>) -> Self {
        Self {
            journal,
            complete: false,
        }
    }
    pub fn finish(&mut self) -> Result<()> {
        self.mark(true)?;
        self.complete = true;
        Ok(())
    }
}
impl Journal {
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    fn fixture() -> (tempfile::TempDir, PathBuf, u32) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = dir.path().join("fence");
        std::fs::write(&path, b"clean\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let uid = std::fs::metadata(&path).unwrap().uid();
        (dir, path, uid)
    }

    #[tokio::test]
    async fn uncertain_completion_blocks_reopen_after_owner_is_gone() {
        let (_dir, path, uid) = fixture();
        let journal = Arc::new(Mutex::new(Journal::open(&path, uid).unwrap()));
        let mut fence = Fence::new(journal.clone().lock_owned().await);
        fence.mark(false).unwrap();
        drop(fence);
        assert!(journal.lock().await.validate().is_err());
        drop(journal); // Release the OS lock: rejection must be durable, not lock contention.
        assert_eq!(std::fs::read(&path).unwrap(), b"dirty\n");
        assert!(Journal::open(&path, uid).is_err());
    }

    #[tokio::test]
    async fn known_completion_allows_reopen_but_not_a_concurrent_owner() {
        let (_dir, path, uid) = fixture();
        let journal = Arc::new(Mutex::new(Journal::open(&path, uid).unwrap()));
        assert!(Journal::open(&path, uid).is_err());
        let mut fence = Fence::new(journal.clone().lock_owned().await);
        fence.mark(false).unwrap();
        fence.finish().unwrap();
        drop(fence);
        drop(journal);
        assert_eq!(std::fs::read(&path).unwrap(), b"clean\n");
        assert!(Journal::open(&path, uid).is_ok());
    }
}
