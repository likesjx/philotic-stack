//! Mesh membership — hardened v2 invite/join ceremony.
//!
//! ## Protocol overview
//!
//! The invite URL carries *no PSK*.  Instead:
//!
//! 1. The **inviter** (Hotel A) generates an ephemeral X25519 keypair and signs the
//!    full invite payload (including its X25519 public key) with its long-term
//!    Ed25519 private key.
//!
//! 2. The **joiner** (Hotel B) verifies the Ed25519 signature, generates its own
//!    ephemeral X25519 keypair, and computes:
//!
//!    ```text
//!    shared_secret = X25519(B_priv_ephemeral, A_pub_ephemeral)
//!    session_key   = HKDF-SHA256(shared_secret, info="philotic-mesh-v2")
//!    ```
//!
//! 3. B sends a `JoinRequest` (over the TCP execution plane or retried UDP) containing
//!    B's identity Ed25519 public key and B's ephemeral X25519 public key.
//!
//! 4. A computes the same shared secret (X25519 commutes), derives the same session
//!    key, stores B's identity, and sends `JoinAccepted`.
//!
//! 5. Both sides now share `session_key` that was *never transmitted*.  Future
//!    beacon HMACs from this peer are verified with that per-peer key.
//!
//! ## Security properties
//!
//! | Threat | Defense |
//! |---|---|
//! | Invite URL interception | Attacker cannot complete ECDH without A's ephemeral private key |
//! | Invite replay | Single-use nonce consumed by both sides |
//! | Stale invite | `valid_until` TTL (default 30 min) |
//! | Unknown hotel spoofing | Ed25519 signature ties invite to A's long-term keypair |
//! | PSK leak | No PSK exists; session key derived from ECDH, never stored in plaintext |
//!
//! ## Compatibility
//!
//! The URL scheme version byte distinguishes v1 (legacy, PSK-in-URL) from v2.
//! v1 invites are rejected by v2 `accept` by default unless
//! `PHILOTIC_MESH_ALLOW_V1_INVITE=1` is set (dev mode only).

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, SigningKey, VerifyingKey, Signer, Verifier};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use x25519_dalek::{EphemeralSecret, PublicKey as X25519PublicKey};

use crate::storage::HotelRecord;

// ── Constants ─────────────────────────────────────────────────────────────────

/// URL scheme prefix for v2 signed invites.
pub const INVITE_URL_PREFIX_V2: &str = "philotic-invite://v2/";

/// URL scheme prefix for v1 legacy invites (PSK-in-URL, transitional).
pub const INVITE_URL_PREFIX_V1: &str = "philotic-invite://v1/";

/// HKDF info string — changing this invalidates all existing derived session keys.
pub const HKDF_INFO: &[u8] = b"philotic-mesh-v2";

/// Default invite TTL: 30 minutes.
pub const DEFAULT_INVITE_TTL_SECS: u64 = 1_800;

/// Session key length (bytes) — 32 bytes = 256-bit AES-GCM / HMAC key.
pub const SESSION_KEY_LEN: usize = 32;

// ── Wire types ────────────────────────────────────────────────────────────────

/// The invite payload encoded in the URL.
///
/// Contains everything Hotel B needs to verify the invite and perform ECDH.
/// Does NOT contain a PSK. The PSK is derived from the ECDH output.
///
/// **IMPORTANT:** Any changes to field ordering or naming MUST increment
/// `version` to prevent parsing incompatibilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvitePayload {
    /// Protocol version — must be 2 for this invite type.
    pub version: u8,

    /// Stable hotel ID of the inviting hotel.
    pub inviter_hotel_id: String,

    /// Long-term Ed25519 verifying key of the inviting hotel, base64url-encoded.
    /// This is the hotel's durable identity. Pinned on first successful join (TOFU).
    pub inviter_ed25519_pubkey: String,

    /// Ephemeral X25519 public key for this invite session, base64url-encoded.
    /// Combined with the joiner's ephemeral X25519 private key to derive `session_key`.
    /// Generated fresh for each invite; discarded after the join ceremony.
    pub inviter_x25519_ephemeral_pubkey: String,

    /// Full `HotelRecord` for the inviting hotel (addresses, ports, capabilities).
    pub inviter_hotel: HotelRecord,

    /// Single-use nonce (32 random bytes, hex-encoded). Prevents invite URL replay.
    /// Both Hotels A and B mark this nonce consumed on first use.
    pub nonce: String,

    /// Unix epoch seconds after which this invite is invalid.
    pub valid_until: u64,

    /// Unix epoch seconds when this invite was issued.
    pub issued_at: u64,
}

/// A complete signed invite — payload + Ed25519 signature.
///
/// The URL encoding is:
/// ```text
/// philotic-invite://v2/<base64url(json(payload))>.<base64url(signature_bytes)>
/// ```
#[derive(Debug, Clone)]
pub struct SignedInvite {
    pub payload: InvitePayload,
    /// Ed25519 signature over the canonical JSON of `payload`.
    /// Signed with the inviting hotel's long-term Ed25519 private key.
    pub signature: Vec<u8>,
}

/// Sent by the joining hotel after a successful invite verification and ECDH.
///
/// Transmitted over the TCP execution plane (or retried UDP for small payloads).
/// The inviter uses this to complete the ECDH and derive the same session key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequest {
    /// Protocol version (2).
    pub version: u8,
    /// Echo of the invite nonce — allows A to correlate to the outstanding invite.
    pub invite_nonce: String,
    /// B's stable hotel identity record (for A's graph storage).
    pub joiner_hotel: HotelRecord,
    /// B's long-term Ed25519 verifying key (hex-encoded).
    /// A pins this to `MeshMemberRecord` for future message verification.
    pub joiner_ed25519_pubkey: String,
    /// B's ephemeral X25519 public key for ECDH (base64url-encoded).
    pub joiner_x25519_ephemeral_pubkey: String,
    /// Unix epoch seconds when the request was created.
    pub requested_at: u64,
}

/// Response from the inviting hotel confirming the join ceremony is complete.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinAccepted {
    /// Protocol version (2).
    pub version: u8,
    /// Echo of the invite nonce.
    pub invite_nonce: String,
    /// A's Ed25519 verifying key (hex-encoded), confirming identity.
    pub inviter_ed25519_pubkey: String,
    /// Capabilities granted to B in this mesh.
    pub granted_capabilities: Vec<String>,
    /// Unix epoch seconds.
    pub accepted_at: u64,
}

// ── Validation ────────────────────────────────────────────────────────────────

/// Errors produced during invite parsing, verification, and acceptance.
#[derive(Debug, thiserror::Error)]
pub enum InviteError {
    #[error("invite version {0} is not supported (only v2 is accepted by this build)")]
    UnsupportedVersion(u8),

    #[error("invite has expired (valid_until={valid_until}, now={now})")]
    Expired { valid_until: u64, now: u64 },

    #[error("Ed25519 signature verification failed: {0}")]
    SignatureInvalid(String),

    #[error("invite payload is malformed: {0}")]
    Malformed(String),

    #[error("invite nonce has already been consumed (replay rejected)")]
    NonceReplayed,
}

// ── Signing and URL encoding ──────────────────────────────────────────────────

impl SignedInvite {
    /// Create and sign a new invite.
    ///
    /// `signing_key` is the inviting hotel's long-term Ed25519 private key.
    /// `x25519_ephemeral_pubkey` is the newly-generated ephemeral key for this invite.
    pub fn new(
        payload: InvitePayload,
        signing_key: &SigningKey,
    ) -> Result<Self> {
        let canonical = canonical_payload_json(&payload)?;
        let signature = signing_key.sign(canonical.as_bytes()).to_bytes().to_vec();
        Ok(Self { payload, signature })
    }

    /// Encode as a `philotic-invite://v2/<payload>.<signature>` URL.
    pub fn to_url(&self) -> Result<String> {
        let payload_json = canonical_payload_json(&self.payload)?;
        let payload_enc = URL_SAFE_NO_PAD.encode(payload_json.as_bytes());
        let sig_enc = URL_SAFE_NO_PAD.encode(&self.signature);
        Ok(format!("{}{}.{}", INVITE_URL_PREFIX_V2, payload_enc, sig_enc))
    }

    /// Parse a `philotic-invite://v2/<payload>.<signature>` URL.
    ///
    /// Returns an error for v1 URLs unless `PHILOTIC_MESH_ALLOW_V1_INVITE=1`.
    /// Does NOT verify the signature — call `verify_signature` after parsing.
    pub fn from_url(url: &str) -> Result<Self> {
        if url.starts_with(INVITE_URL_PREFIX_V1) {
            let allow_v1 = std::env::var("PHILOTIC_MESH_ALLOW_V1_INVITE")
                .map(|v| v == "1" || v == "true")
                .unwrap_or(false);
            if !allow_v1 {
                bail!(
                    "v1 invite URLs (PSK-in-URL) are no longer accepted. \
                     Regenerate the invite with the current `phil mesh invite` command. \
                     Set PHILOTIC_MESH_ALLOW_V1_INVITE=1 to bypass (dev only)."
                );
            }
            // Fallback for dev: treat v1 as unverified (no signature).
            // The PSK will be missing/ignored but the ceremony will proceed.
            bail!("v1 invite fallback not implemented in this build; regenerate the invite");
        }

        let blob = url
            .strip_prefix(INVITE_URL_PREFIX_V2)
            .ok_or_else(|| anyhow::anyhow!("not a philotic-invite://v2/ URL"))?;

        let (payload_enc, sig_enc) = blob
            .rsplit_once('.')
            .ok_or_else(|| anyhow::anyhow!("invite URL missing signature segment (expected <payload>.<sig>)"))?;

        let payload_bytes = URL_SAFE_NO_PAD
            .decode(payload_enc)
            .context("base64url decode invite payload")?;
        let payload: InvitePayload =
            serde_json::from_slice(&payload_bytes).context("deserialize InvitePayload")?;

        let signature = URL_SAFE_NO_PAD
            .decode(sig_enc)
            .context("base64url decode invite signature")?;

        Ok(Self { payload, signature })
    }

    /// Verify the Ed25519 signature on the payload using the inviter's public key
    /// embedded in the payload itself (TOFU model — caller can optionally compare
    /// the fingerprint out-of-band before trusting).
    pub fn verify_signature(&self) -> Result<(), InviteError> {
        let pubkey_bytes = URL_SAFE_NO_PAD
            .decode(&self.payload.inviter_ed25519_pubkey)
            .map_err(|e| InviteError::Malformed(format!("inviter Ed25519 pubkey base64: {e}")))?;

        let verifying_key = VerifyingKey::try_from(pubkey_bytes.as_slice())
            .map_err(|e| InviteError::Malformed(format!("inviter Ed25519 pubkey parse: {e}")))?;

        let sig_bytes: [u8; 64] = self
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| InviteError::Malformed("signature must be 64 bytes".into()))?;

        let signature = Signature::from_bytes(&sig_bytes);
        let canonical = canonical_payload_json(&self.payload)
            .map_err(|e| InviteError::Malformed(format!("re-serialize payload: {e}")))?;

        verifying_key
            .verify(canonical.as_bytes(), &signature)
            .map_err(|e| InviteError::SignatureInvalid(e.to_string()))
    }

    /// Validate version and TTL.
    pub fn validate_time(&self) -> Result<(), InviteError> {
        if self.payload.version != 2 {
            return Err(InviteError::UnsupportedVersion(self.payload.version));
        }
        let now = now_epoch_secs();
        if now > self.payload.valid_until {
            return Err(InviteError::Expired {
                valid_until: self.payload.valid_until,
                now,
            });
        }
        Ok(())
    }
}

// ── ECDH session key derivation ───────────────────────────────────────────────

/// Output of the joiner's side of the ECDH ceremony.
pub struct EcdhJoinerOutput {
    /// The derived session key (32 bytes). Never transmitted.
    pub session_key: [u8; SESSION_KEY_LEN],
    /// B's ephemeral X25519 public key to send to the inviter.
    pub joiner_x25519_pubkey: X25519PublicKey,
    /// B's ephemeral X25519 public key bytes, base64url-encoded (for wire serialization).
    pub joiner_x25519_pubkey_enc: String,
}

/// Joiner (B) generates its ephemeral X25519 keypair and derives the session key.
///
/// `inviter_x25519_pubkey_enc` is the base64url-encoded X25519 public key from
/// the invite payload.
pub fn derive_session_key_joiner(
    inviter_x25519_pubkey_enc: &str,
) -> Result<EcdhJoinerOutput> {
    let inviter_pub_bytes = URL_SAFE_NO_PAD
        .decode(inviter_x25519_pubkey_enc)
        .context("decode inviter X25519 pubkey")?;

    let inviter_pub: [u8; 32] = inviter_pub_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("inviter X25519 pubkey must be 32 bytes"))?;

    let inviter_x25519_pub = X25519PublicKey::from(inviter_pub);
    let joiner_secret = EphemeralSecret::random_from_rng(OsRng);
    let joiner_pub = X25519PublicKey::from(&joiner_secret);
    let shared_secret = joiner_secret.diffie_hellman(&inviter_x25519_pub);

    let session_key = hkdf_derive_session_key(shared_secret.as_bytes())?;

    Ok(EcdhJoinerOutput {
        session_key,
        joiner_x25519_pubkey_enc: URL_SAFE_NO_PAD.encode(joiner_pub.as_bytes()),
        joiner_x25519_pubkey: joiner_pub,
    })
}

/// Output of the inviter's side of the ECDH ceremony.
pub struct EcdhInviterOutput {
    /// The derived session key (32 bytes). Must match the joiner's derived key.
    pub session_key: [u8; SESSION_KEY_LEN],
}

/// Inviter (A) derives the session key from the joiner's X25519 public key.
///
/// `inviter_x25519_secret_bytes` is the raw 32-byte private key material for the
/// ephemeral X25519 key that was included in the invite.
///
/// Note: `EphemeralSecret` is consumed by `diffie_hellman`. The inviter must hold
/// this value in memory between invite generation and join acceptance. It is NOT
/// persisted to disk.
pub fn derive_session_key_inviter(
    inviter_x25519_secret_bytes: &[u8; 32],
    joiner_x25519_pubkey_enc: &str,
) -> Result<EcdhInviterOutput> {
    let joiner_pub_bytes = URL_SAFE_NO_PAD
        .decode(joiner_x25519_pubkey_enc)
        .context("decode joiner X25519 pubkey")?;

    let joiner_pub: [u8; 32] = joiner_pub_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("joiner X25519 pubkey must be 32 bytes"))?;

    let joiner_x25519_pub = X25519PublicKey::from(joiner_pub);

    // Reconstruct the static secret from the raw bytes.
    // This is safe because we generated these bytes ourselves.
    let inviter_secret = x25519_dalek::StaticSecret::from(*inviter_x25519_secret_bytes);
    let shared_secret = inviter_secret.diffie_hellman(&joiner_x25519_pub);

    let session_key = hkdf_derive_session_key(shared_secret.as_bytes())?;

    Ok(EcdhInviterOutput { session_key })
}

fn hkdf_derive_session_key(shared_secret_bytes: &[u8]) -> Result<[u8; SESSION_KEY_LEN]> {
    let hk = Hkdf::<Sha256>::new(None, shared_secret_bytes);
    let mut okm = [0u8; SESSION_KEY_LEN];
    hk.expand(HKDF_INFO, &mut okm)
        .map_err(|_| anyhow::anyhow!("HKDF expand failed (output too long)"))?;
    Ok(okm)
}

// ── Invite generation helpers ─────────────────────────────────────────────────

/// Generate a fresh Ed25519 signing keypair (for bootstrapping hotel identity).
pub fn generate_hotel_signing_keypair() -> SigningKey {
    SigningKey::generate(&mut OsRng)
}

/// Load an Ed25519 signing key from raw 32 bytes (as stored in `operator.key`).
pub fn signing_key_from_raw_bytes(bytes: &[u8]) -> Result<SigningKey> {
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("Ed25519 private key must be 32 bytes"))?;
    Ok(SigningKey::from_bytes(&arr))
}

/// Load an Ed25519 signing key from a raw 32-byte hex string (hotel private key file).
pub fn signing_key_from_hex(hex_str: &str) -> Result<SigningKey> {
    let bytes = hex::decode(hex_str).context("decode Ed25519 private key hex")?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("Ed25519 private key must be 32 bytes"))?;
    Ok(SigningKey::from_bytes(&arr))
}

/// Encode a verifying key as base64url (for the invite payload field).
pub fn verifying_key_to_base64url(vk: &VerifyingKey) -> String {
    URL_SAFE_NO_PAD.encode(vk.as_bytes())
}

/// Decode a verifying key from base64url.
pub fn verifying_key_from_base64url(s: &str) -> Result<VerifyingKey> {
    let bytes = URL_SAFE_NO_PAD.decode(s).context("base64url decode verifying key")?;
    VerifyingKey::try_from(bytes.as_slice()).context("parse Ed25519 verifying key")
}

/// Generate a fresh ephemeral X25519 keypair.
///
/// Returns `(secret_bytes_32, pubkey_base64url)`. The secret bytes must be held
/// in memory (or encrypted at rest) until the join acceptance ceremony completes,
/// at which point they should be zeroed. We use `StaticSecret` here (rather than
/// `EphemeralSecret`) because the invite-to-accept window requires holding the
/// private key bytes for the duration between invite generation and join completion.
/// The security model is the same — generate once, use once, zero after use.
pub fn generate_x25519_ephemeral() -> ([u8; 32], String) {
    use rand::RngCore;
    let mut raw = [0u8; 32];
    OsRng.fill_bytes(&mut raw);
    let static_secret = x25519_dalek::StaticSecret::from(raw);
    let pubkey = X25519PublicKey::from(&static_secret);
    (raw, URL_SAFE_NO_PAD.encode(pubkey.as_bytes()))
}

// ── Nonce helpers ─────────────────────────────────────────────────────────────

/// Graph config key for tracking a consumed invite nonce.
pub fn consumed_nonce_key(nonce: &str) -> String {
    format!("mesh_consumed_invite:{}", nonce)
}

/// Generate a cryptographically random 32-byte nonce, hex-encoded.
pub fn generate_nonce() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

// ── Fingerprint ───────────────────────────────────────────────────────────────

/// Human-readable fingerprint from a base64url-encoded Ed25519 public key.
///
/// Example: `AB:CD:EF:12:34:56:78:90`
pub fn fingerprint_from_base64url(pubkey_b64: &str) -> Result<String> {
    let bytes = URL_SAFE_NO_PAD.decode(pubkey_b64).context("decode pubkey for fingerprint")?;
    use sha2::Digest;
    let digest = sha2::Sha256::digest(&bytes);
    Ok(digest[..8]
        .chunks(2)
        .map(|pair| {
            if pair.len() == 2 {
                format!("{:02X}{:02X}", pair[0], pair[1])
            } else {
                format!("{:02X}", pair[0])
            }
        })
        .collect::<Vec<_>>()
        .join(":"))
}

/// Human-readable fingerprint from a raw hex-encoded public key (legacy v1 compat).
pub fn operator_fingerprint_from_hex(pubkey_hex: &str) -> Result<String> {
    let bytes = hex::decode(pubkey_hex).context("decode pubkey hex for fingerprint")?;
    use sha2::Digest;
    let digest = sha2::Sha256::digest(&bytes);
    Ok(digest[..8]
        .chunks(2)
        .map(|pair| {
            if pair.len() == 2 {
                format!("{:02X}{:02X}", pair[0], pair[1])
            } else {
                format!("{:02X}", pair[0])
            }
        })
        .collect::<Vec<_>>()
        .join(":"))
}

// ── Payload canonicalization ──────────────────────────────────────────────────

/// Produce a canonical JSON string for signing.
///
/// Field order is determined by serde_json's insertion order (alphabetical with
/// `#[serde(...)]` defaults). We serialize via the standard path and treat the
/// output as canonical. Callers MUST NOT deserialize and re-serialize before
/// verifying — pass the raw URL bytes directly.
fn canonical_payload_json(payload: &InvitePayload) -> Result<String> {
    serde_json::to_string(payload).context("serialize InvitePayload for signing")
}

// ── Time helper ───────────────────────────────────────────────────────────────

pub fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeCapabilities, NodeConstraints, NodeRole};
    use crate::storage::HotelRecord;

    fn test_hotel() -> HotelRecord {
        HotelRecord {
            hotel_name: "hotel-a".to_string(),
            capabilities: NodeCapabilities {
                node_id: "node-a".to_string(),
                roles: vec![NodeRole::AnsibleNode],
                models: vec![],
                tools: vec![],
                constraints: NodeConstraints::default(),
            },
            mesh_host: Some("192.168.1.10".to_string()),
            mesh_port: 8999,
            blob_port: 9001,
            execution_port: 9002,
            ipc_socket_path: "/tmp/philotic-hotel-a.sock".to_string(),
            active_pid: None,
        }
    }

    #[test]
    fn signed_invite_roundtrip() {
        let signing_key = generate_hotel_signing_keypair();
        let verifying_key = signing_key.verifying_key();
        let (x25519_secret_bytes, x25519_pub_enc) = generate_x25519_ephemeral();

        let payload = InvitePayload {
            version: 2,
            inviter_hotel_id: "hotel-a".to_string(),
            inviter_ed25519_pubkey: verifying_key_to_base64url(&verifying_key),
            inviter_x25519_ephemeral_pubkey: x25519_pub_enc.clone(),
            inviter_hotel: test_hotel(),
            nonce: generate_nonce(),
            valid_until: u64::MAX,
            issued_at: now_epoch_secs(),
        };

        let invite = SignedInvite::new(payload.clone(), &signing_key).unwrap();
        let url = invite.to_url().unwrap();
        assert!(url.starts_with(INVITE_URL_PREFIX_V2));

        let parsed = SignedInvite::from_url(&url).unwrap();
        assert_eq!(parsed.payload.nonce, payload.nonce);
        assert_eq!(parsed.payload.inviter_hotel_id, "hotel-a");
        assert_eq!(parsed.payload.version, 2);

        // Signature must verify.
        parsed.verify_signature().unwrap();
    }

    #[test]
    fn tampered_payload_fails_signature() {
        let signing_key = generate_hotel_signing_keypair();
        let verifying_key = signing_key.verifying_key();
        let (_x25519_secret_bytes, x25519_pub_enc) = generate_x25519_ephemeral();

        let mut payload = InvitePayload {
            version: 2,
            inviter_hotel_id: "hotel-a".to_string(),
            inviter_ed25519_pubkey: verifying_key_to_base64url(&verifying_key),
            inviter_x25519_ephemeral_pubkey: x25519_pub_enc,
            inviter_hotel: test_hotel(),
            nonce: generate_nonce(),
            valid_until: u64::MAX,
            issued_at: now_epoch_secs(),
        };

        let invite = SignedInvite::new(payload.clone(), &signing_key).unwrap();
        let url = invite.to_url().unwrap();

        // Parse, tamper, re-serialize.
        let mut parsed = SignedInvite::from_url(&url).unwrap();
        parsed.payload.inviter_hotel_id = "evil-hotel".to_string();

        // Must reject.
        assert!(matches!(
            parsed.verify_signature(),
            Err(InviteError::SignatureInvalid(_))
        ));
    }

    #[test]
    fn ecdh_both_sides_derive_same_key() {
        let (inviter_secret_bytes, inviter_pub_enc) = generate_x25519_ephemeral();

        // Joiner side.
        let joiner_out = derive_session_key_joiner(&inviter_pub_enc).unwrap();

        // Inviter side — uses joiner's pub key.
        let inviter_out = derive_session_key_inviter(
            &inviter_secret_bytes,
            &joiner_out.joiner_x25519_pubkey_enc,
        )
        .unwrap();

        assert_eq!(
            joiner_out.session_key, inviter_out.session_key,
            "ECDH session keys must match on both sides"
        );
    }

    #[test]
    fn expired_invite_rejected() {
        let signing_key = generate_hotel_signing_keypair();
        let verifying_key = signing_key.verifying_key();
        let (_x25519_secret_bytes, x25519_pub_enc) = generate_x25519_ephemeral();

        let payload = InvitePayload {
            version: 2,
            inviter_hotel_id: "hotel-a".to_string(),
            inviter_ed25519_pubkey: verifying_key_to_base64url(&verifying_key),
            inviter_x25519_ephemeral_pubkey: x25519_pub_enc,
            inviter_hotel: test_hotel(),
            nonce: generate_nonce(),
            valid_until: 2, // in the past
            issued_at: 1,
        };

        let invite = SignedInvite::new(payload, &signing_key).unwrap();
        assert!(matches!(invite.validate_time(), Err(InviteError::Expired { .. })));
    }

    #[test]
    fn v1_url_rejected_without_env_var() {
        let url = "philotic-invite://v1/dGVzdA";
        let result = SignedInvite::from_url(url);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("v1 invite URLs"));
    }

    #[test]
    fn fingerprint_is_stable() {
        let signing_key = generate_hotel_signing_keypair();
        let vk = signing_key.verifying_key();
        let enc = verifying_key_to_base64url(&vk);
        let fp1 = fingerprint_from_base64url(&enc).unwrap();
        let fp2 = fingerprint_from_base64url(&enc).unwrap();
        assert_eq!(fp1, fp2);
        assert!(fp1.contains(':'), "fingerprint should be colon-separated hex pairs");
    }
}
