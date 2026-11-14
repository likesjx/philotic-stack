//! Vault master key material that is not an environment variable.
//!
//! systemd `LoadCredential=` exposes a root-installed file through
//! `$CREDENTIALS_DIRECTORY`. The hotel can also point philotic-web at a
//! mode-0400 file via `PHILOTIC_VAULT_MASTER_KEY_FILE`. Both are read once.
//! The legacy `PHILOTIC_VAULT_MASTER_KEY` environment variable remains a
//! fallback for processes that are not guests.

use anyhow::{bail, Context, Result};
use base64::Engine;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

pub const CREDENTIAL_NAME: &str = "philotic-vault-master-key";
pub const EXPLICIT_FILE_ENV: &str = "PHILOTIC_VAULT_MASTER_KEY_FILE";

/// Tests that change the process-wide key environment must hold this.
pub fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|err| err.into_inner())
}

#[derive(Debug)]
pub struct LoadedKey {
    pub source_kind: &'static str,
    pub vault_ref: String,
    pub bytes: Zeroizing<Vec<u8>>,
}

/// Credential file, then the explicit file path. Does not consult the
/// environment variable that holds the key itself.
pub fn load_non_env_root_key(account: &str) -> Option<LoadedKey> {
    if let Some(bytes) = read_systemd_credential() {
        return Some(LoadedKey {
            source_kind: "systemd-credential",
            vault_ref: format!("credential://{CREDENTIAL_NAME}/{account}"),
            bytes,
        });
    }
    if let Some(bytes) = read_explicit_file() {
        return Some(LoadedKey {
            source_kind: "credential-file",
            vault_ref: format!("file://{EXPLICIT_FILE_ENV}/{account}"),
            bytes,
        });
    }
    None
}

pub fn systemd_credential_bytes() -> Option<Vec<u8>> {
    read_systemd_credential().map(|bytes| bytes.to_vec())
}

pub fn explicit_file_bytes() -> Option<Vec<u8>> {
    read_explicit_file().map(|bytes| bytes.to_vec())
}

fn read_systemd_credential() -> Option<Zeroizing<Vec<u8>>> {
    let dir = std::env::var_os("CREDENTIALS_DIRECTORY")?;
    let path = PathBuf::from(dir).join(CREDENTIAL_NAME);
    read_private_key_file(&path).ok()
}

fn read_explicit_file() -> Option<Zeroizing<Vec<u8>>> {
    let path = std::env::var_os(EXPLICIT_FILE_ENV)?;
    if path.is_empty() {
        return None;
    }
    read_private_key_file(Path::new(&path)).ok()
}

fn read_private_key_file(path: &Path) -> Result<Zeroizing<Vec<u8>>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta =
            std::fs::symlink_metadata(path).with_context(|| format!("stat {}", path.display()))?;
        if meta.file_type().is_symlink() || !meta.is_file() {
            bail!("refusing a non-regular vault key file");
        }
        if meta.mode() & 0o077 != 0 {
            bail!("refusing a group- or world-accessible vault key file");
        }
        if meta.len() == 0 || meta.len() > 512 {
            bail!("refusing a vault key file with an unexpected size");
        }
    }
    let raw =
        Zeroizing::new(std::fs::read(path).with_context(|| format!("read {}", path.display()))?);
    let text =
        Zeroizing::new(String::from_utf8(raw.to_vec()).context("vault key file is not utf-8")?);
    decode_root_key(text.trim())
}

pub fn decode_root_key(raw: &str) -> Result<Zeroizing<Vec<u8>>> {
    let key_bytes = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .context("vault key was not base64")?;
    if key_bytes.len() != 32 {
        bail!("vault key must decode to exactly 32 bytes");
    }
    Ok(Zeroizing::new(key_bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use std::os::unix::fs::OpenOptionsExt;

    fn key(byte: u8) -> String {
        base64::engine::general_purpose::STANDARD.encode([byte; 32])
    }

    fn write_key(path: &Path, contents: &str, mode: u32) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(path)
            .unwrap();
        use std::io::Write;
        file.write_all(contents.as_bytes()).unwrap();
    }

    struct EnvGuard {
        key: &'static str,
        prev: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let prev = std::env::var_os(key);
            unsafe { std::env::set_var(key, value) };
            Self { key, prev }
        }
        fn unset(key: &'static str) -> Self {
            let prev = std::env::var_os(key);
            unsafe { std::env::remove_var(key) };
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match self.prev.take() {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    #[test]
    fn credential_file_beats_the_environment_and_loose_files_are_ignored() {
        let _lock = test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let credential = dir.path().join(CREDENTIAL_NAME);
        let loose = dir.path().join("loose");
        write_key(&credential, &key(9), 0o400);
        write_key(&loose, &key(3), 0o644);
        let _cred = EnvGuard::set("CREDENTIALS_DIRECTORY", dir.path().to_str().unwrap());
        let _env = EnvGuard::set("PHILOTIC_VAULT_MASTER_KEY", &key(7));
        let _file = EnvGuard::set(EXPLICIT_FILE_ENV, loose.to_str().unwrap());
        let loaded = load_non_env_root_key("acct").unwrap();
        assert_eq!(loaded.source_kind, "systemd-credential");
        assert_eq!(loaded.bytes.as_slice(), &[9u8; 32]);
        assert!(!loaded.vault_ref.contains(&key(9)));
    }

    #[test]
    fn explicit_private_file_is_used_when_no_credential_directory_is_set() {
        let _lock = test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("web-key");
        write_key(&path, &key(4), 0o400);
        let _cred = EnvGuard::unset("CREDENTIALS_DIRECTORY");
        let _file = EnvGuard::set(EXPLICIT_FILE_ENV, path.to_str().unwrap());
        let loaded = load_non_env_root_key("acct").unwrap();
        assert_eq!(loaded.source_kind, "credential-file");
        assert_eq!(loaded.bytes.as_slice(), &[4u8; 32]);
    }

    #[test]
    fn world_readable_explicit_file_is_refused() {
        let _lock = test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("open-key");
        write_key(&path, &key(4), 0o644);
        let _cred = EnvGuard::unset("CREDENTIALS_DIRECTORY");
        let _file = EnvGuard::set(EXPLICIT_FILE_ENV, path.to_str().unwrap());
        assert!(load_non_env_root_key("acct").is_none());
    }
}
