//! Sealed secret transfer — Relocation Ceremony R7.
//!
//! When a role moves hotels together with its transport, the secret that
//! transport needs (a Telegram bot token) must reach the target without an
//! operator re-provisioning it, and without ever crossing the mesh in the
//! clear. The mesh is signed, not encrypted, and every event it carries
//! sits in the sender's ledger until acked — so the secret is sealed to the
//! one hotel that should read it:
//!
//! 1. At STANDBY the target mints a single-use X25519 key for this move and
//!    returns its public half. It keeps the private half in memory only, and
//!    drops it once used or expired.
//! 2. The origin seals with a fresh ephemeral X25519 key of its own. The
//!    AEAD key is HKDF-SHA256 over the X25519 shared secret, salted with the
//!    origin↔target per-pair mesh key, with the whole [`SealContext`] as
//!    `info` — so only the holder of both the target's single-use private key
//!    and the pair key can derive it, and it is distinct from every HMAC key
//!    and vault master key.
//! 3. AES-256-GCM seals the plaintext with the same context as AAD: a sealed
//!    secret cannot be replayed into another ceremony, re-addressed to
//!    another hotel, relabelled as another secret, or widened to other roles.
//!
//! Ciphertext left behind in a ledger or retransmitted later is useless once
//! the target's single-use key is gone.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use hkdf::Hkdf;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey, StaticSecret};
use zeroize::Zeroizing;

const SEAL_DOMAIN: &[u8] = b"philotic-sealed-secret-v1\n";

/// Everything a sealed secret is bound to. Serialized in field order as the
/// KDF `info` and the AEAD associated data; any change breaks decryption.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealContext {
    pub ceremony_id: String,
    /// The STANDBY `MaterializeRequest` id the target minted its key under.
    pub standby_request_id: String,
    pub origin_node_id: String,
    pub target_node_id: String,
    pub agent_id: String,
    pub role_name: String,
    /// The config key that names the secret on both hotels.
    pub config_key: String,
    pub secret_kind: String,
    /// Who may read it on the target; never empty.
    pub allowed_roles: Vec<String>,
    pub expires_at_unix: u64,
}

impl SealContext {
    fn bound_bytes(&self) -> Result<Vec<u8>> {
        let mut bytes = SEAL_DOMAIN.to_vec();
        bytes.extend(serde_json::to_vec(self).context("serialize seal context")?);
        Ok(bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedSecret {
    pub context: SealContext,
    pub origin_ephemeral_public_b64: String,
    pub nonce_b64: String,
    pub ciphertext_b64: String,
}

/// The target's single-use key for one move.
pub struct SealKey {
    secret: StaticSecret,
}

impl SealKey {
    pub fn generate() -> Self {
        Self {
            secret: StaticSecret::random_from_rng(OsRng),
        }
    }

    pub fn public_b64(&self) -> String {
        BASE64.encode(PublicKey::from(&self.secret).as_bytes())
    }
}

fn decode_public(b64: &str) -> Result<PublicKey> {
    let bytes: [u8; 32] = BASE64
        .decode(b64)
        .context("decode X25519 public key")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("X25519 public key must be 32 bytes"))?;
    Ok(PublicKey::from(bytes))
}

fn aead_key(shared: &[u8; 32], pair_key: &str, context: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    if pair_key.trim().is_empty() {
        bail!("no per-pair mesh key with the peer; refusing to seal or open");
    }
    let hk = Hkdf::<Sha256>::new(Some(pair_key.as_bytes()), shared);
    let mut key = Zeroizing::new([0u8; 32]);
    hk.expand(context, key.as_mut())
        .map_err(|_| anyhow::anyhow!("HKDF expand failed"))?;
    Ok(key)
}

/// Origin side: seal `plaintext` to the target's single-use public key.
pub fn seal(
    plaintext: &str,
    context: SealContext,
    target_public_b64: &str,
    pair_key: &str,
) -> Result<SealedSecret> {
    if context.allowed_roles.is_empty() {
        bail!("refusing to seal a secret with an empty role ACL");
    }
    let target_public = decode_public(target_public_b64)?;
    let ephemeral = EphemeralSecret::random_from_rng(OsRng);
    let ephemeral_public = PublicKey::from(&ephemeral);
    let shared = ephemeral.diffie_hellman(&target_public);
    if !shared.was_contributory() {
        bail!("target public key is a low-order point");
    }
    let bound = context.bound_bytes()?;
    let key = aead_key(shared.as_bytes(), pair_key, &bound)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.as_ref()));
    let mut nonce = [0u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: plaintext.as_bytes(),
                aad: &bound,
            },
        )
        .map_err(|_| anyhow::anyhow!("seal failed"))?;
    Ok(SealedSecret {
        context,
        origin_ephemeral_public_b64: BASE64.encode(ephemeral_public.as_bytes()),
        nonce_b64: BASE64.encode(nonce),
        ciphertext_b64: BASE64.encode(ciphertext),
    })
}

/// Target side: open a sealed secret with this move's single-use key.
/// The caller must still check the context names this hotel, this ceremony
/// and the secret it expects.
pub fn open(
    sealed: &SealedSecret,
    key: &SealKey,
    pair_key: &str,
    now_unix: u64,
) -> Result<Zeroizing<String>> {
    if now_unix > sealed.context.expires_at_unix {
        bail!("sealed secret expired");
    }
    if sealed.context.allowed_roles.is_empty() {
        bail!("refusing a sealed secret with an empty role ACL");
    }
    let origin_public = decode_public(&sealed.origin_ephemeral_public_b64)?;
    let shared = key.secret.diffie_hellman(&origin_public);
    if !shared.was_contributory() {
        bail!("origin ephemeral key is a low-order point");
    }
    let bound = sealed.context.bound_bytes()?;
    let aead = aead_key(shared.as_bytes(), pair_key, &bound)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(aead.as_ref()));
    let nonce: [u8; 12] = BASE64
        .decode(&sealed.nonce_b64)
        .context("decode nonce")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("nonce must be 12 bytes"))?;
    let ciphertext = BASE64
        .decode(&sealed.ciphertext_b64)
        .context("decode ciphertext")?;
    let plaintext = Zeroizing::new(
        cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &bound,
                },
            )
            .map_err(|_| anyhow::anyhow!("sealed secret failed authentication"))?,
    );
    let text = std::str::from_utf8(&plaintext).context("sealed secret is not UTF-8")?;
    Ok(Zeroizing::new(text.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAIR: &str = "pair-key-mac-vps";

    fn context() -> SealContext {
        SealContext {
            ceremony_id: "relocate-1".into(),
            standby_request_id: "req-1".into(),
            origin_node_id: "mac-jane-aiua-01".into(),
            target_node_id: "vps-jane-aiua-01".into(),
            agent_id: "agent-bjork-01".into(),
            role_name: "orchestrator".into(),
            config_key: "telegram_bot_token_bjork".into(),
            secret_kind: "telegram_bot_token".into(),
            allowed_roles: vec!["membrane".into()],
            expires_at_unix: 2_000,
        }
    }

    #[test]
    fn the_target_opens_what_the_origin_sealed() {
        let key = SealKey::generate();
        let sealed = seal("123:secret", context(), &key.public_b64(), PAIR).unwrap();
        assert!(!sealed.ciphertext_b64.contains("secret"));
        assert_eq!(
            open(&sealed, &key, PAIR, 1_000).unwrap().as_str(),
            "123:secret"
        );
    }

    #[test]
    fn another_key_or_pair_key_cannot_open_it() {
        let key = SealKey::generate();
        let sealed = seal("123:secret", context(), &key.public_b64(), PAIR).unwrap();
        assert!(open(&sealed, &SealKey::generate(), PAIR, 1_000).is_err());
        assert!(open(&sealed, &key, "another-pair", 1_000).is_err());
    }

    #[test]
    fn any_change_to_the_context_breaks_it() {
        let key = SealKey::generate();
        let sealed = seal("123:secret", context(), &key.public_b64(), PAIR).unwrap();
        let mut widened = sealed.clone();
        widened.context.allowed_roles.push("agent".into());
        assert!(open(&widened, &key, PAIR, 1_000).is_err());
        let mut readdressed = sealed.clone();
        readdressed.context.target_node_id = "mbp-jane-aiua-01".into();
        assert!(open(&readdressed, &key, PAIR, 1_000).is_err());
        let mut replayed = sealed;
        replayed.context.ceremony_id = "relocate-2".into();
        assert!(open(&replayed, &key, PAIR, 1_000).is_err());
    }

    #[test]
    fn an_expired_seal_or_an_empty_acl_is_refused() {
        let key = SealKey::generate();
        let sealed = seal("123:secret", context(), &key.public_b64(), PAIR).unwrap();
        assert!(open(&sealed, &key, PAIR, 2_001).is_err());
        let mut open_to_all = context();
        open_to_all.allowed_roles.clear();
        assert!(seal("123:secret", open_to_all, &key.public_b64(), PAIR).is_err());
        assert!(seal("123:secret", context(), &key.public_b64(), "  ").is_err());
    }

    #[test]
    fn a_low_order_target_key_is_refused() {
        let zero = BASE64.encode([0u8; 32]);
        assert!(seal("123:secret", context(), &zero, PAIR).is_err());
    }
}
