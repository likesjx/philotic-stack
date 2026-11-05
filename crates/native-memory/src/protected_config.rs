//! Descriptor-based reader for non-secret protected configuration.
//! Extracted from the existing snapshot candidate; not connected to runtime.
//! Mode and ownership checks do not establish exclusive custody when ACLs or
//! other access mechanisms permit additional writers. Deployment must verify
//! those separately; this reader is not evidence that custody is exclusive.
use crate::Error;
use std::{
    ffi::CString,
    fs::File,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
};
const LIMIT: u64 = 65536;
const MODE_MASK: u32 = 0o7777;

fn checked_dir(fd: &File, uid: u32) -> Result<(), Error> {
    let m = fd.metadata().map_err(|_| Error::Unavailable)?;
    if !m.is_dir() || ![0, uid].contains(&m.uid()) || m.mode() & 0o022 != 0 {
        return Err(Error::Denied);
    }
    Ok(())
}

pub(crate) fn read_with_owner(path: &Path, uid: u32, gid: u32) -> Result<Vec<u8>, Error> {
    read_with_owner_inner(path, uid, gid, |_| Ok(()))
}

fn read_with_owner_inner(
    path: &Path,
    uid: u32,
    gid: u32,
    mut stage: impl FnMut(&str) -> Result<(), Error>,
) -> Result<Vec<u8>, Error> {
    if !path.is_absolute() {
        return Err(Error::Invalid);
    }
    let mut parts = path.components();
    if parts.next() != Some(Component::RootDir) {
        return Err(Error::Invalid);
    }
    let mut components = Vec::new();
    for part in parts {
        let Component::Normal(name) = part else {
            return Err(Error::Invalid);
        };
        components.push(CString::new(name.as_bytes()).map_err(|_| Error::Invalid)?);
    }
    let (name, parents) = components.split_last().ok_or(Error::Invalid)?;
    let slash = CString::new("/").map_err(|_| Error::Invalid)?;
    let raw = unsafe {
        libc::open(
            slash.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(Error::Denied);
    }
    let mut dir = unsafe { File::from_raw_fd(raw) };
    checked_dir(&dir, uid)?;
    for component in parents {
        let raw = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                component.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if raw < 0 {
            return Err(Error::Denied);
        }
        dir = unsafe { File::from_raw_fd(raw) };
        checked_dir(&dir, uid)?;
    }
    let parent = dir.metadata().map_err(|_| Error::Unavailable)?;
    if parent.uid() != uid || parent.gid() != gid || parent.mode() & MODE_MASK != 0o750 {
        return Err(Error::Denied);
    }
    let raw = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(Error::Denied);
    }
    let mut file = unsafe { File::from_raw_fd(raw) };
    let before = file.metadata().map_err(|_| Error::Unavailable)?;
    if !before.is_file()
        || before.uid() != uid
        || before.gid() != gid
        || before.mode() & MODE_MASK != 0o640
        || before.nlink() != 1
        || before.len() > LIMIT
    {
        return Err(Error::Denied);
    }
    stage("opened")?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Unavailable)?;
    stage("read")?;
    let after = file.metadata().map_err(|_| Error::Unavailable)?;
    if bytes.len() as u64 != before.len()
        || before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(Error::Denied);
    }
    stage("metadata_validated")?;
    // Reopen relative to the already checked parent descriptor to reject a
    // replacement of the final pathname while the original fd was read.
    // This is a point-in-time check, not a lock against a later replacement.
    let raw = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(Error::Denied);
    }
    let current = unsafe { File::from_raw_fd(raw) };
    let current = current.metadata().map_err(|_| Error::Unavailable)?;
    if !current.is_file()
        || current.dev() != before.dev()
        || current.ino() != before.ino()
        || current.uid() != uid
        || current.gid() != gid
        || current.mode() & MODE_MASK != 0o640
        || current.nlink() != 1
    {
        return Err(Error::Denied);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt, time::SystemTime};

    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, u32, u32) {
        let dir = tempfile::Builder::new()
            .prefix("protected-config-race-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o750)).unwrap();
        let path = dir.path().join("binding.json");
        fs::write(&path, b"first").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        let meta = fs::metadata(&path).unwrap();
        (dir, path, meta.uid(), meta.gid())
    }

    #[test]
    fn rejects_final_path_replacement_after_open_even_with_identical_bytes() {
        let (dir, path, uid, gid) = fixture();
        let mut reached_reopen_boundary = false;
        let result = read_with_owner_inner(&path, uid, gid, |stage| {
            if stage == "metadata_validated" {
                reached_reopen_boundary = true;
                fs::rename(&path, dir.path().join("original")).unwrap();
                fs::write(&path, b"first").unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
            }
            Ok(())
        });
        assert!(reached_reopen_boundary);
        assert!(matches!(result, Err(Error::Denied)));
    }

    #[test]
    fn unchanged_path_passes_the_same_reopen_boundary() {
        let (_dir, path, uid, gid) = fixture();
        let mut reached_reopen_boundary = false;
        let result = read_with_owner_inner(&path, uid, gid, |stage| {
            reached_reopen_boundary |= stage == "metadata_validated";
            Ok(())
        });
        assert!(reached_reopen_boundary);
        assert_eq!(result.unwrap(), b"first");
    }

    #[test]
    fn rejects_same_length_in_place_mutation_before_final_check() {
        let (_dir, path, uid, gid) = fixture();
        let result = read_with_owner_inner(&path, uid, gid, |stage| {
            if stage == "read" {
                fs::write(&path, b"other").unwrap();
                let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
                file.set_times(fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
                    .unwrap();
            }
            Ok(())
        });
        assert!(result.is_err());
    }
}
