use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::keychain;
use ansible_mesh_core::storage::SecretRecord;
use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use sha2::Digest;
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

/// What [`rotate_master_key`] found and did.
#[derive(Debug, Clone)]
pub struct MasterKeyRotationReport {
    pub total: usize,
    pub migrated: usize,
    pub already_on_new_key: usize,
    pub old_key_fingerprint: String,
    pub new_key_fingerprint: String,
}

/// Re-encrypt every secret this hotel's vault holds under `new_key_bytes`.
///
/// Resumable, not transactional: each secret is decrypted and re-encrypted
/// independently and written as soon as it's done, rather than staged into
/// one big commit. A secret that decrypts under NEITHER the current root key
/// nor `new_key_bytes` stops the run immediately without writing anything
/// for it or any secret after it in the listing — already-migrated secrets
/// from earlier in this run (or a previous run) stay migrated. Investigate
/// that secret, then re-run with the same `new_key_bytes`: already-migrated
/// records are detected (they decrypt under `new_key_bytes` already) and
/// skipped, so the run converges instead of needing a rollback.
///
/// Does NOT persist `new_key_bytes` anywhere (env file, Keychain, ansible
/// vault) — applying it is a separate, host-specific step. In particular, on
/// a host where `PHILOTIC_VAULT_MASTER_KEY` is set via a systemd
/// EnvironmentFile (vps-jane today), writing `~/.philotic/vault-master-key.env`
/// here would do nothing, because the env var wins over the file in
/// [`load_or_create_root_key`].
pub fn rotate_master_key(
    graph: &GraphDomain,
    new_key_bytes: &[u8; 32],
    dry_run: bool,
) -> Result<MasterKeyRotationReport> {
    let old_key_bytes = load_or_create_root_key()?;
    let secrets = graph.list_secrets()?;
    let mut migrated = 0usize;
    let mut already_on_new_key = 0usize;

    for secret in &secrets {
        if decrypt_with_key(secret, new_key_bytes).is_ok() {
            already_on_new_key += 1;
            continue;
        }

        let plaintext = decrypt_with_key(secret, &old_key_bytes).with_context(|| {
            format!(
                "secret [{}] decrypts under neither the current root key nor the new key; \
                 stopping now with nothing written for it or any secret after it — \
                 investigate this one, then re-run with the same new key",
                secret.secret_ref
            )
        })?;

        if dry_run {
            migrated += 1;
            continue;
        }

        let (ciphertext_b64, nonce_b64) = encrypt_with_key(&plaintext, new_key_bytes)?;
        let mut record = secret.clone();
        record.ciphertext_b64 = ciphertext_b64;
        record.nonce_b64 = nonce_b64;

        // Round-trip proof before this record is written: a cipher/key
        // mistake fails loud right here instead of silently bricking the
        // secret once the old plaintext is out of scope.
        let verify = decrypt_with_key(&record, new_key_bytes).with_context(|| {
            format!(
                "post-encrypt round-trip check failed for [{}]; not written",
                secret.secret_ref
            )
        })?;
        if verify != plaintext {
            bail!(
                "post-encrypt round-trip mismatch for [{}]; not written",
                secret.secret_ref
            );
        }

        record.updated_at = now_secs();
        graph.upsert_secret(&record)?;
        migrated += 1;
    }

    Ok(MasterKeyRotationReport {
        total: secrets.len(),
        migrated,
        already_on_new_key,
        old_key_fingerprint: key_fingerprint(&old_key_bytes),
        new_key_fingerprint: key_fingerprint(new_key_bytes),
    })
}

/// A non-secret fingerprint for operator-facing rotation confirmations and
/// audit trails: sha256 of the key, first 8 bytes as hex. Never enough to
/// reconstruct the key. Mirrors `philotic-web`'s `root_key_fingerprint`.
pub fn key_fingerprint(key_bytes: &[u8]) -> String {
    let digest = sha2::Sha256::digest(key_bytes);
    format!("sha256:{}", hex::encode(&digest[..8]))
}

/// Generate a fresh 32-byte root key, base64-encoded, for an operator about
/// to rotate the master key. Nothing in this crate stores the result —
/// whoever calls this must capture it before doing anything else with it.
pub fn generate_root_key_b64() -> String {
    BASE64_STANDARD.encode(random_root_key())
}

/// Decode a base64-encoded 32-byte master key an operator is about to make
/// active (e.g. `aiua auth rotate-master-key --new-key`).
pub fn decode_master_key_b64(raw: &str) -> Result<[u8; 32]> {
    let bytes = decode_root_key(raw.trim(), "--new-key")?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("--new-key must decode to exactly 32 bytes"))
}

/// Where the currently-active root key is actually coming from, so an
/// operator applying a new one knows which source to change. Re-resolves via
/// the same precedence as [`load_or_create_root_key`] without exposing the
/// key material.
pub fn describe_root_key_source() -> &'static str {
    if load_env_root_key().is_ok() {
        "the PHILOTIC_VAULT_MASTER_KEY environment variable — a systemd EnvironmentFile or \
         shell export keeps winning over any file you write, so update that source, not a file"
    } else if load_env_file_root_key().is_ok() {
        "~/.philotic/vault-master-key.env — safe to overwrite directly"
    } else {
        "the macOS Keychain (ai.philotic.hotel-vault) — use `security add-generic-password` \
         to overwrite it, or delete the item and let it regenerate"
    }
}

/// What [`sync_secret_roles`] found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleSync {
    pub before: Vec<String>,
    pub after: Vec<String>,
    /// Roles added (empty when the entry already listed them all).
    pub added: Vec<String>,
    /// Whether the record was written (false for a dry run or nothing to add).
    pub written: bool,
}

/// Add `desired` roles to an existing vault secret's `allowed_roles`, without
/// decrypting it or touching the ciphertext, so an entry sealed before a role was
/// added to its `ProviderKeySpec` can be opened by that role without re-entering
/// the key. It only ADDS: existing roles are kept in order.
///
/// Two cases are refused rather than "fixed", because a grant would be misleading:
/// - an empty `allowed_roles` means *any role* may read it, so adding a role would
///   RESTRICT it;
/// - a non-empty `allowed_guests` keeps denying other guests whatever roles it lists.
pub fn sync_secret_roles(
    graph: &GraphDomain,
    secret_ref: &str,
    desired: &[&str],
    dry_run: bool,
) -> Result<RoleSync> {
    let Some(mut record) = graph.get_secret(secret_ref)? else {
        bail!("vault secret not found: {secret_ref}");
    };
    if record.allowed_roles.is_empty() {
        bail!(
            "secret [{secret_ref}] has an empty allowed_roles, which means any role may read it; \
             adding a role would restrict it, so nothing was changed"
        );
    }
    if !record.allowed_guests.is_empty() {
        bail!(
            "secret [{secret_ref}] is also restricted to guests {:?}; a role grant alone would \
             not open it, so nothing was changed",
            record.allowed_guests
        );
    }

    let before = record.allowed_roles.clone();
    let added: Vec<String> = desired
        .iter()
        .filter(|role| !before.iter().any(|existing| existing == *role))
        .map(|role| role.to_string())
        .collect();
    let mut after = before.clone();
    after.extend(added.iter().cloned());

    let written = !dry_run && !added.is_empty();
    if written {
        record.allowed_roles = after.clone();
        record.updated_at = now_secs();
        graph.upsert_secret(&record)?;
    }
    Ok(RoleSync {
        before,
        after,
        added,
        written,
    })
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
    encrypt_with_key(plaintext, &load_or_create_root_key()?)
}

fn decrypt(secret: &SecretRecord) -> Result<String> {
    decrypt_with_key(secret, &load_or_create_root_key()?)
}

fn encrypt_with_key(plaintext: &str, key_bytes: &[u8]) -> Result<(String, String)> {
    let cipher = cipher_for_key(key_bytes)?;
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

fn decrypt_with_key(secret: &SecretRecord, key_bytes: &[u8]) -> Result<String> {
    let cipher = cipher_for_key(key_bytes)?;
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

fn cipher_for_key(key_bytes: &[u8]) -> Result<Aes256Gcm> {
    Ok(Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key_bytes)))
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
        SecretAccess, SecretInput, decode_root_key, decrypt_with_key, encrypt_with_key,
        generate_root_key_b64, key_fingerprint, load_or_create_root_key, resolve_secret,
        rotate_master_key, store_secret, sync_secret_roles, vault_key_account,
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

    fn access(role: &str) -> SecretAccess {
        SecretAccess {
            role: role.into(),
            guest_id: "any-guest".into(),
        }
    }

    fn stored(graph: &GraphDomain, roles: &[&str], guests: &[&str]) -> String {
        store_secret(
            graph,
            SecretInput {
                secret_kind: "openrouter_api_key".into(),
                scope: "hotel".into(),
                allowed_roles: roles.iter().map(|r| r.to_string()).collect(),
                allowed_guests: guests.iter().map(|g| g.to_string()).collect(),
                plaintext: "sk-or-test-plaintext".into(),
            },
        )
        .unwrap()
    }

    #[test]
    fn sync_roles_opens_an_existing_entry_to_new_roles_without_touching_the_key() {
        set_test_key();
        let graph = GraphDomain::new(Arc::new(
            SqliteGraphStorage::open(":memory:").unwrap().adapter(),
        ));
        // An entry sealed before the decisions roles existed.
        let secret_ref = stored(&graph, &["model", "model.openrouter"], &[]);
        let before = graph.get_secret(&secret_ref).unwrap().unwrap();
        assert!(resolve_secret(&graph, &secret_ref, &access("heal-dispatcher")).is_err());

        let dry = sync_secret_roles(
            &graph,
            &secret_ref,
            &["model.decisions", "heal-dispatcher"],
            true,
        )
        .unwrap();
        assert_eq!(dry.added, ["model.decisions", "heal-dispatcher"]);
        assert!(!dry.written, "a dry run writes nothing");
        assert!(resolve_secret(&graph, &secret_ref, &access("heal-dispatcher")).is_err());

        let done = sync_secret_roles(
            &graph,
            &secret_ref,
            &[
                "model",
                "model.openrouter",
                "model.decisions",
                "heal-dispatcher",
            ],
            false,
        )
        .unwrap();
        assert!(done.written);
        assert_eq!(
            done.after,
            [
                "model",
                "model.openrouter",
                "model.decisions",
                "heal-dispatcher"
            ],
            "existing roles keep their order and nothing is duplicated"
        );

        // Now readable by the new roles, and still by the old ones.
        for role in [
            "heal-dispatcher",
            "model.decisions",
            "model",
            "model.openrouter",
        ] {
            assert_eq!(
                resolve_secret(&graph, &secret_ref, &access(role))
                    .unwrap()
                    .as_deref(),
                Some("sk-or-test-plaintext"),
                "{role}"
            );
        }
        // Other roles are still refused.
        assert!(resolve_secret(&graph, &secret_ref, &access("membrane")).is_err());

        // The key itself was never decrypted or re-encrypted.
        let after = graph.get_secret(&secret_ref).unwrap().unwrap();
        assert_eq!(after.ciphertext_b64, before.ciphertext_b64);
        assert_eq!(after.nonce_b64, before.nonce_b64);
    }

    #[test]
    fn sync_roles_is_idempotent() {
        set_test_key();
        let graph = GraphDomain::new(Arc::new(
            SqliteGraphStorage::open(":memory:").unwrap().adapter(),
        ));
        let secret_ref = stored(&graph, &["model", "heal-dispatcher"], &[]);
        let result =
            sync_secret_roles(&graph, &secret_ref, &["model", "heal-dispatcher"], false).unwrap();
        assert!(result.added.is_empty() && !result.written);
        assert_eq!(result.before, result.after);
    }

    #[test]
    fn sync_roles_refuses_the_cases_where_a_grant_would_mislead() {
        set_test_key();
        let graph = GraphDomain::new(Arc::new(
            SqliteGraphStorage::open(":memory:").unwrap().adapter(),
        ));

        // Empty roles means ANY role may read it; adding one would restrict it.
        let open = stored(&graph, &[], &[]);
        let err = sync_secret_roles(&graph, &open, &["heal-dispatcher"], false).unwrap_err();
        assert!(err.to_string().contains("empty allowed_roles"), "{err}");
        assert!(
            graph
                .get_secret(&open)
                .unwrap()
                .unwrap()
                .allowed_roles
                .is_empty()
        );

        // A guest restriction keeps denying whatever roles say.
        let guarded = stored(&graph, &["model"], &["only-this-guest"]);
        let err = sync_secret_roles(&graph, &guarded, &["heal-dispatcher"], false).unwrap_err();
        assert!(err.to_string().contains("restricted to guests"), "{err}");
        assert_eq!(
            graph.get_secret(&guarded).unwrap().unwrap().allowed_roles,
            ["model"]
        );

        assert!(sync_secret_roles(&graph, "secret://nope", &["x"], false).is_err());
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

    #[test]
    fn rotate_master_key_migrates_every_secret_and_the_old_key_no_longer_decrypts() {
        set_test_key();
        let graph = GraphDomain::new(Arc::new(
            SqliteGraphStorage::open(":memory:").unwrap().adapter(),
        ));
        let ref_a = stored(&graph, &["model"], &[]);
        let ref_b = stored(&graph, &["heal-dispatcher"], &[]);
        let old_key = load_or_create_root_key().unwrap();
        let new_key = [9u8; 32];

        let report = rotate_master_key(&graph, &new_key, false).unwrap();
        assert_eq!(report.total, 2);
        assert_eq!(report.migrated, 2);
        assert_eq!(report.already_on_new_key, 0);
        assert_eq!(report.old_key_fingerprint, key_fingerprint(&old_key));
        assert_eq!(report.new_key_fingerprint, key_fingerprint(&new_key));

        for secret_ref in [&ref_a, &ref_b] {
            let record = graph.get_secret(secret_ref).unwrap().unwrap();
            assert_eq!(
                decrypt_with_key(&record, &new_key).unwrap(),
                "sk-or-test-plaintext"
            );
            assert!(
                decrypt_with_key(&record, &old_key).is_err(),
                "the old key must no longer open a migrated secret"
            );
        }
    }

    #[test]
    fn rotate_master_key_is_resumable_when_a_secret_is_already_on_the_new_key() {
        set_test_key();
        let graph = GraphDomain::new(Arc::new(
            SqliteGraphStorage::open(":memory:").unwrap().adapter(),
        ));
        let ref_a = stored(&graph, &["model"], &[]);
        let ref_b = stored(&graph, &["heal-dispatcher"], &[]);
        let new_key = [9u8; 32];

        // Simulate a prior run that migrated ref_a and then stopped.
        let mut record_a = graph.get_secret(&ref_a).unwrap().unwrap();
        let (ciphertext_b64, nonce_b64) =
            encrypt_with_key("sk-or-test-plaintext", &new_key).unwrap();
        record_a.ciphertext_b64 = ciphertext_b64;
        record_a.nonce_b64 = nonce_b64;
        graph.upsert_secret(&record_a).unwrap();

        let report = rotate_master_key(&graph, &new_key, false).unwrap();
        assert_eq!(report.total, 2);
        assert_eq!(
            report.already_on_new_key, 1,
            "the already-migrated secret must be detected, not re-encrypted"
        );
        assert_eq!(report.migrated, 1);

        for secret_ref in [&ref_a, &ref_b] {
            let record = graph.get_secret(secret_ref).unwrap().unwrap();
            assert_eq!(
                decrypt_with_key(&record, &new_key).unwrap(),
                "sk-or-test-plaintext"
            );
        }
    }

    #[test]
    fn rotate_master_key_stops_without_corrupting_anything_when_a_secret_is_unreadable() {
        set_test_key();
        let graph = GraphDomain::new(Arc::new(
            SqliteGraphStorage::open(":memory:").unwrap().adapter(),
        ));
        let good_ref = stored(&graph, &["model"], &[]);
        let old_key = load_or_create_root_key().unwrap();
        let new_key = [9u8; 32];

        // A secret whose ciphertext decrypts under neither key.
        let mut bad = graph.get_secret(&good_ref).unwrap().unwrap();
        bad.secret_ref = "secret://hotel/default/corrupted/deadbeef".into();
        bad.ciphertext_b64 =
            base64::engine::general_purpose::STANDARD.encode(b"not gcm ciphertext");
        graph.upsert_secret(&bad).unwrap();

        let err = rotate_master_key(&graph, &new_key, false).unwrap_err();
        assert!(err.to_string().contains("decrypts under neither"), "{err}");

        // The good secret must still be readable under some key — a failure
        // on one secret must never cost another one its readability.
        let good_after = graph.get_secret(&good_ref).unwrap().unwrap();
        let still_readable = decrypt_with_key(&good_after, &old_key).is_ok()
            || decrypt_with_key(&good_after, &new_key).is_ok();
        assert!(still_readable);
    }

    #[test]
    fn rotate_master_key_dry_run_writes_nothing() {
        set_test_key();
        let graph = GraphDomain::new(Arc::new(
            SqliteGraphStorage::open(":memory:").unwrap().adapter(),
        ));
        let secret_ref = stored(&graph, &["model"], &[]);
        let before = graph.get_secret(&secret_ref).unwrap().unwrap();
        let new_key = [9u8; 32];

        let report = rotate_master_key(&graph, &new_key, true).unwrap();
        assert_eq!(report.total, 1);
        assert_eq!(report.migrated, 1);

        let after = graph.get_secret(&secret_ref).unwrap().unwrap();
        assert_eq!(before.ciphertext_b64, after.ciphertext_b64);
        assert_eq!(before.nonce_b64, after.nonce_b64);
    }

    #[test]
    fn key_fingerprint_is_deterministic_and_distinguishes_keys() {
        let a = key_fingerprint(&[1u8; 32]);
        let b = key_fingerprint(&[1u8; 32]);
        let c = key_fingerprint(&[2u8; 32]);
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("sha256:"));
    }

    #[test]
    fn generate_root_key_b64_produces_a_valid_32_byte_key() {
        let encoded = generate_root_key_b64();
        let decoded = decode_root_key(&encoded, "test").unwrap();
        assert_eq!(decoded.len(), 32);
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
