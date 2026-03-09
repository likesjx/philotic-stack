use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use ansible_mesh_core::storage::{GraphStorage, SecretRecord};
use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const VAULT_ENV_KEY: &str = "PHILOTIC_VAULT_MASTER_KEY";

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

pub fn store_secret(graph: &dyn GraphStorage, input: SecretInput) -> Result<String> {
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

pub fn resolve_secret(
    graph: &dyn GraphStorage,
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
    let raw = std::env::var(VAULT_ENV_KEY).with_context(|| {
        format!(
            "{} must be set to a base64-encoded 32-byte key before using the hotel vault",
            VAULT_ENV_KEY
        )
    })?;
    let key_bytes = BASE64_STANDARD
        .decode(raw.trim())
        .context("failed to decode PHILOTIC_VAULT_MASTER_KEY as base64")?;
    if key_bytes.len() != 32 {
        bail!("PHILOTIC_VAULT_MASTER_KEY must decode to exactly 32 bytes");
    }

    Ok(Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key_bytes)))
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
    use super::{SecretAccess, SecretInput, resolve_secret, store_secret};
    use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
    use base64::Engine;

    fn set_test_key() {
        let key = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
        unsafe {
            std::env::set_var("PHILOTIC_VAULT_MASTER_KEY", key);
        }
    }

    #[test]
    fn vault_round_trips_secret_with_role_policy() {
        set_test_key();
        let graph = SqliteGraphStorage::open(":memory:").unwrap();
        let secret_ref = store_secret(
            &graph,
            SecretInput {
                secret_kind: "gemini-access-token".into(),
                scope: "hotel".into(),
                allowed_roles: vec!["model.gemini".into()],
                allowed_guests: Vec::new(),
                plaintext: "shh".into(),
            },
        )
        .unwrap();

        let secret = resolve_secret(
            &graph,
            &secret_ref,
            &SecretAccess {
                role: "model.gemini".into(),
                guest_id: "guest-1".into(),
            },
        )
        .unwrap();

        assert_eq!(secret.as_deref(), Some("shh"));
    }
}
