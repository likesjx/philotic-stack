use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::keychain;
use ansible_mesh_core::storage::SecretRecord;
use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const VAULT_ENV_KEY: &str = "PHILOTIC_VAULT_MASTER_KEY";
const VAULT_KEY_ID_ENV_KEY: &str = "PHILOTIC_VAULT_KEY_ID";
const VAULT_KEYCHAIN_SERVICE: &str = "ai.philotic.hotel-vault";
const VAULT_KEYCHAIN_DEFAULT_ACCOUNT: &str = "default-root-key";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretAccess {
    pub role: String,
    pub guest_id: String,
}

#[derive(Debug, Clone)]
pub struct SecretInput {
    pub secret_kind: String,
    pub scope: String,
    pub allowed_roles: Vec<String>,
    pub allowed_guests: Vec<String>,
    pub plaintext: String,
}

pub fn store_secret(graph: &GraphDomain, input: SecretInput) -> Result<String> {
    let (ciphertext_b64, nonce_b64) = encrypt(&input.plaintext)?;
    let now = now_secs();
    let secret_ref = format!(
        "secret://hotel/default/{}/{}",
        input.secret_kind,
        Uuid::new_v4().simple()
    );

    graph.upsert_secret(&SecretRecord {
        secret_ref: secret_ref.clone(),
        secret_kind: input.secret_kind,
        scope: input.scope,
        allowed_roles: input.allowed_roles,
        allowed_guests: input.allowed_guests,
        ciphertext_b64,
        nonce_b64,
        created_at: now,
        updated_at: now,
    })?;

    Ok(secret_ref)
}

/// Re-encrypt an existing vault secret in place with new plaintext.
/// The secret_ref, scope, allowed_roles, and allowed_guests are preserved.
pub fn rotate_secret(graph: &GraphDomain, secret_ref: &str, plaintext: &str) -> Result<()> {
    let Some(mut record) = graph.get_secret(secret_ref)? else {
        anyhow::bail!("vault secret not found: {}", secret_ref);
    };
    let (ciphertext_b64, nonce_b64) = encrypt(plaintext)?;
    record.ciphertext_b64 = ciphertext_b64;
    record.nonce_b64 = nonce_b64;
    record.updated_at = now_secs();
    graph.upsert_secret(&record)
}

pub fn resolve_secret(
    graph: &GraphDomain,
    secret_ref: &str,
    access: &SecretAccess,
) -> Result<Option<String>> {
    let Some(secret) = graph.get_secret(secret_ref)? else {
        return Ok(None);
    };

    if !secret.allowed_roles.is_empty()
        && !secret.allowed_roles.iter().any(|role| role == &access.role)
    {
        bail!(
            "secret [{}] is not accessible to role [{}]",
            secret_ref,
            access.role
        );
    }

    if !secret.allowed_guests.is_empty()
        && !secret
            .allowed_guests
            .iter()
            .any(|guest_id| guest_id == &access.guest_id)
    {
        bail!(
            "secret [{}] is not accessible to guest [{}]",
            secret_ref,
            access.guest_id
        );
    }

    Ok(Some(decrypt(&secret)?))
}

/// Read and decrypt a vault secret without ACL checks.
/// Only for hotel-internal operations (e.g. migration bundle building).
pub(crate) fn export_secret_plaintext(
    graph: &GraphDomain,
    secret_ref: &str,
) -> Result<Option<String>> {
    let Some(secret) = graph.get_secret(secret_ref)? else {
        return Ok(None);
    };
    Ok(Some(decrypt(&secret)?))
}

fn encrypt(plaintext: &str) -> Result<(String, String)> {
    let cipher = cipher()?;
    let nonce_bytes = random_nonce();
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .context("failed to encrypt vault secret")?;
    Ok((
        BASE64_STANDARD.encode(ciphertext),
        BASE64_STANDARD.encode(nonce_bytes),
    ))
}

fn decrypt(secret: &SecretRecord) -> Result<String> {
    let cipher = cipher()?;
    let nonce_bytes = BASE64_STANDARD
        .decode(&secret.nonce_b64)
        .context("failed to decode vault nonce")?;
    let ciphertext = BASE64_STANDARD
        .decode(&secret.ciphertext_b64)
        .context("failed to decode vault ciphertext")?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, ciphertext.as_ref())
        .context("failed to decrypt vault secret")?;
    String::from_utf8(plaintext).context("vault secret plaintext was not utf-8")
}

fn cipher() -> Result<Aes256Gcm> {
    let key_bytes = load_or_create_root_key()?;

    Ok(Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes)))
}

/// Resolve the vault root key deterministically: explicit env var first, then the
/// operator-provisioned key file, and only then the macOS Keychain.
///
/// The Keychain must NOT take precedence over env/file: the keychain item is stored
/// with an empty trusted-app ACL, so headless contexts (launchd, ssh) cannot read it
/// ("User interaction is not allowed" -> treated as absent) while GUI shells can.
/// With keychain-first, a hand-run hotel and a launchd-supervised hotel on the same
/// machine silently resolve DIFFERENT keys, making secrets encrypted by one
/// undecryptable by the other (mbp-jane provider-secret incidents, 2026-07-04 and
/// 2026-07-08). Env -> file resolves identically in every execution context, so an
/// operator-provisioned key always wins. The Keychain remains the zero-config
/// bootstrap path when no explicit key exists.
fn load_or_create_root_key() -> Result<Vec<u8>> {
    if let Ok(from_env) = load_env_root_key() {
        return Ok(from_env);
    }

    if let Ok(from_file) = load_env_file_root_key() {
        return Ok(from_file);
    }

    if keychain::enabled() {
        if let Some(existing) = load_keychain_root_key()? {
            return Ok(existing);
        }

        let generated = random_root_key();
        store_keychain_root_key(&generated)?;
        return Ok(generated);
    }

    bail!(
        "{} must be set to a base64-encoded 32-byte key, or ~/.philotic/vault-master-key.env must exist, before using the hotel vault here.\n\
         The macOS Keychain backend is not in use on this host (non-macOS, {}=0, or a detected CI environment). \
         The Keychain is deliberately skipped where there is no unlocked login keychain, because the `security` CLI blocks indefinitely rather than failing.",
        VAULT_ENV_KEY,
        keychain::KEYCHAIN_ENABLED_ENV
    )
}

fn load_env_root_key() -> Result<Vec<u8>> {
    let raw = std::env::var(VAULT_ENV_KEY)?;
    decode_root_key(raw.trim(), VAULT_ENV_KEY)
}

fn load_env_file_root_key() -> Result<Vec<u8>> {
    let path = vault_master_key_env_path()?;
    let content =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            if key.trim() == VAULT_ENV_KEY {
                return decode_root_key(value.trim(), &path.display().to_string());
            }
        }
    }
    bail!("{} did not contain {}", path.display(), VAULT_ENV_KEY)
}

fn vault_master_key_env_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join(".philotic")
        .join("vault-master-key.env"))
}

fn load_keychain_root_key() -> Result<Option<Vec<u8>>> {
    let output = keychain::run_security(
        &[
            "find-generic-password",
            "-s",
            VAULT_KEYCHAIN_SERVICE,
            "-a",
            &vault_key_account(),
            "-w",
        ],
        "reading the Philotic vault root key",
    )?;

    if output.status.success() {
        let raw = String::from_utf8(output.stdout)
            .context("keychain root-key output was not valid utf-8")?;
        return decode_root_key(raw.trim(), "macOS Keychain")
            .map(Some)
            .context("stored Keychain root key is invalid");
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.code() == Some(36)
        || stderr.contains("could not be found")
        || stderr.contains("The specified item could not be found")
        || stderr.contains("User interaction is not allowed")
    {
        return Ok(None);
    }

    bail!(
        "failed to read Philotic vault root key from macOS Keychain: {}",
        stderr.trim()
    )
}

fn store_keychain_root_key(key_bytes: &[u8]) -> Result<()> {
    let encoded = BASE64_STANDARD.encode(key_bytes);
    let output = keychain::run_security(
        &[
            "add-generic-password",
            "-U",
            "-s",
            VAULT_KEYCHAIN_SERVICE,
            "-a",
            &vault_key_account(),
            "-w",
            &encoded,
            "-T",
            "",
        ],
        "storing the Philotic vault root key",
    )?;

    // The read path already tolerates a locked/non-interactive keychain by
    // returning Ok(None); the write path used to hard-fail on it, which turned a
    // recoverable "no keychain here" into an error after the caller had already
    // generated a key.
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() && stderr.contains("User interaction is not allowed") {
        bail!(
            "the login Keychain is locked or unavailable, so the generated vault root key could \
             not be stored. Set {}=0 and provide {} or ~/.philotic/vault-master-key.env instead.",
            keychain::KEYCHAIN_ENABLED_ENV,
            VAULT_ENV_KEY
        );
    }

    if !output.status.success() {
        bail!(
            "failed to store Philotic vault root key in macOS Keychain: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(())
}

fn decode_root_key(raw: &str, source: &str) -> Result<Vec<u8>> {
    let key_bytes = BASE64_STANDARD
        .decode(raw)
        .with_context(|| format!("failed to decode {} as base64", source))?;
    if key_bytes.len() != 32 {
        bail!("{} must decode to exactly 32 bytes", source);
    }
    Ok(key_bytes)
}

fn random_root_key() -> Vec<u8> {
    let left = Uuid::new_v4();
    let right = Uuid::new_v4();
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(left.as_bytes());
    bytes.extend_from_slice(right.as_bytes());
    bytes
}

fn vault_key_account() -> String {
    std::env::var(VAULT_KEY_ID_ENV_KEY)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| VAULT_KEYCHAIN_DEFAULT_ACCOUNT.to_string())
}

fn random_nonce() -> [u8; 12] {
    let uuid = Uuid::new_v4();
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&uuid.as_bytes()[..12]);
    nonce
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::{
        SecretAccess, SecretInput, decode_root_key, load_or_create_root_key, resolve_secret,
        store_secret, vault_key_account,
    };
    use ansible_mesh_core::domain::GraphDomain;
    use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
    use base64::Engine;
    use std::sync::Arc;

    fn set_test_key() {
        let key = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
        unsafe {
            std::env::set_var("PHILOTIC_VAULT_MASTER_KEY", key);
        }
    }

    #[test]
    fn vault_round_trips_secret_with_role_policy() {
        set_test_key();
        let storage = SqliteGraphStorage::open(":memory:").unwrap();
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        let secret_ref = store_secret(
            &graph,
            SecretInput {
                secret_kind: "gemini-access-token".into(),
                scope: "hotel".into(),
                allowed_roles: vec!["model".into()],
                allowed_guests: Vec::new(),
                plaintext: "shh".into(),
            },
        )
        .unwrap();

        let secret = resolve_secret(
            &graph,
            &secret_ref,
            &SecretAccess {
                role: "model".into(),
                guest_id: "guest-1".into(),
            },
        )
        .unwrap();

        assert_eq!(secret.as_deref(), Some("shh"));
    }

    /// The explicit env key must win over the key file (and, implicitly, over the
    /// macOS Keychain, which is only consulted after both explicit sources): key-source
    /// resolution has to be identical for GUI shells and launchd/ssh contexts, or
    /// secrets encrypted in one context become undecryptable in the other.
    #[test]
    fn env_key_wins_over_file_key() {
        set_test_key();

        let dir = std::env::temp_dir().join(format!(
            "philotic-vault-test-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let philotic_dir = dir.join(".philotic");
        std::fs::create_dir_all(&philotic_dir).unwrap();
        let file_key = base64::engine::general_purpose::STANDARD.encode([9u8; 32]);
        std::fs::write(
            philotic_dir.join("vault-master-key.env"),
            format!("PHILOTIC_VAULT_MASTER_KEY={file_key}\n"),
        )
        .unwrap();

        let old_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", &dir);
        }
        let resolved = load_or_create_root_key();
        unsafe {
            match &old_home {
                Some(home) => std::env::set_var("HOME", home),
                None => std::env::remove_var("HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(resolved.unwrap(), vec![7u8; 32]);
    }

    #[test]
    fn decode_root_key_accepts_32_byte_base64() {
        let encoded = base64::engine::general_purpose::STANDARD.encode([9u8; 32]);
        let decoded = decode_root_key(&encoded, "test").unwrap();
        assert_eq!(decoded.len(), 32);
    }

    #[test]
    fn decode_root_key_rejects_wrong_length() {
        let encoded = base64::engine::general_purpose::STANDARD.encode([1u8; 16]);
        let err = decode_root_key(&encoded, "test").unwrap_err().to_string();
        assert!(err.contains("exactly 32 bytes"));
    }

    /// Serializes tests that mutate PHILOTIC_VAULT_KEY_ID; without this the two
    /// vault_key_account tests race each other under the parallel test runner.
    fn key_id_env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    #[test]
    fn vault_key_account_defaults_when_unset() {
        let _guard = key_id_env_lock();
        unsafe {
            std::env::remove_var("PHILOTIC_VAULT_KEY_ID");
        }
        assert_eq!(vault_key_account(), "default-root-key");
    }

    #[test]
    fn vault_key_account_uses_override_when_present() {
        let _guard = key_id_env_lock();
        unsafe {
            std::env::set_var("PHILOTIC_VAULT_KEY_ID", "hotel-alpha");
        }
        assert_eq!(vault_key_account(), "hotel-alpha");
        unsafe {
            std::env::remove_var("PHILOTIC_VAULT_KEY_ID");
        }
    }
}
