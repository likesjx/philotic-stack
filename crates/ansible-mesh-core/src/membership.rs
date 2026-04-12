//! Mesh invite / join protocol — v1 (PSK + URL + nonce + TTL).
//!
//! # Trust Model (v1 — transitional)
//!
//! The invite URL embeds the mesh PSK. The operator is the trust anchor — they
//! control the delivery channel (Telegram DM, Signal, shared clipboard). Anyone
//! who receives the URL gets mesh-level access until the PSK rotates, so the
//! delivery channel must be confidential.
//!
//! A consumed-nonce registry prevents URL reuse. The `valid_until` field prevents
//! stale invites from being accepted after the TTL expires.
//!
//! # Transitional note
//!
//! v1 is intentionally PSK-based to get the join ceremony working with minimal
//! new dependencies. v2 will migrate to per-hotel Ed25519 identity keypairs so
//! that an invite leak cannot compromise hotels that were not yet members, and
//! membership can be revoked per-hotel without rotating a shared secret.
//! Track that work under seam `hotel-identity-keypair`.

use crate::storage::HotelRecord;
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};

// ── Default TTL ───────────────────────────────────────────────────────────────

/// Default invite TTL: 30 minutes.
pub const DEFAULT_INVITE_TTL_SECS: u64 = 30 * 60;

/// URL scheme prefix for mesh invites.
pub const INVITE_URL_PREFIX: &str = "philotic-invite://v1/";

// ── Wire types ────────────────────────────────────────────────────────────────

/// A one-time, time-bounded mesh invite.
///
/// Serialised to JSON, base64url-encoded, and embedded in a `philotic-invite://v1/<blob>`
/// URL for delivery. The consuming hotel validates `nonce` (not seen before) and
/// `valid_until` (not in the past) before accepting.
///
/// **Security note (v1 transitional):** `mesh_psk` is present in plaintext inside
/// this payload. The invite URL must be treated as a secret and delivered through a
/// confidential out-of-band channel. This field is replaced by a per-hotel ECDH
/// exchange in v2 (seam: `hotel-identity-keypair`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshInvite {
    /// Protocol version — must be 1.
    pub version: u8,
    /// Hotel record of the inviting hotel, including its mesh address.
    pub inviter_hotel: HotelRecord,
    /// Mesh PSK. **Transitional v1 only** — replaced by ECDH in v2.
    pub mesh_psk: String,
    /// Operator public key hex, loaded from the operator's key file.
    pub operator_pubkey_hex: String,
    /// Short fingerprint of `operator_pubkey_hex` for human display.
    pub operator_fingerprint: String,
    /// Unix epoch seconds when this invite was issued.
    pub issued_at: u64,
    /// Unix epoch seconds after which this invite is no longer valid.
    pub valid_until: u64,
    /// Random single-use nonce (hex). Prevents URL replay.
    pub nonce: String,
}

/// Payload sent by the joining hotel after accepting a `MeshInvite`.
/// Transmitted as the body of a `MsgType::MeshMembershipAccept` BeaconMessage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshMembershipAcceptPayload {
    /// Protocol version — must be 1.
    pub version: u8,
    /// The joining hotel's record (populated from local graph + accepted invite).
    pub hotel: HotelRecord,
    /// Echo of the invite nonce so the inviter can correlate and mark it consumed.
    pub invite_nonce: String,
    /// Unix epoch seconds when the join was accepted.
    pub accepted_at: u64,
}

// ── Validation ────────────────────────────────────────────────────────────────

/// Errors that can occur during invite acceptance.
#[derive(Debug, thiserror::Error)]
pub enum InviteError {
    #[error("invite version {0} is not supported (expected 1)")]
    UnsupportedVersion(u8),
    #[error("invite has expired (valid_until={valid_until}, now={now})")]
    Expired { valid_until: u64, now: u64 },
    #[error("invite nonce has already been consumed (replay rejected)")]
    NonceReplayed,
    #[error("invite payload is malformed: {0}")]
    Malformed(String),
}

impl MeshInvite {
    /// Validate version and TTL. Does NOT check the nonce — that requires
    /// a graph lookup and must be done by the caller via `mark_nonce_consumed`.
    pub fn validate_time(&self) -> Result<(), InviteError> {
        if self.version != 1 {
            return Err(InviteError::UnsupportedVersion(self.version));
        }
        let now = now_epoch_secs();
        if now > self.valid_until {
            return Err(InviteError::Expired {
                valid_until: self.valid_until,
                now,
            });
        }
        Ok(())
    }
}

// ── URL encode / decode ───────────────────────────────────────────────────────

impl MeshInvite {
    /// Encode this invite as a `philotic-invite://v1/<base64url_payload>` URL.
    ///
    /// The URL is safe to paste into Telegram, email, or a terminal.
    pub fn to_url(&self) -> Result<String> {
        let json = serde_json::to_string(self).context("serialize MeshInvite")?;
        let encoded = URL_SAFE_NO_PAD.encode(json.as_bytes());
        Ok(format!("{}{}", INVITE_URL_PREFIX, encoded))
    }

    /// Decode a `philotic-invite://v1/<blob>` URL back into a `MeshInvite`.
    pub fn from_url(url: &str) -> Result<Self> {
        let blob = url
            .strip_prefix(INVITE_URL_PREFIX)
            .ok_or_else(|| anyhow::anyhow!("not a philotic-invite URL: {}", url))?;
        let json_bytes = URL_SAFE_NO_PAD
            .decode(blob)
            .context("base64url decode invite URL")?;
        serde_json::from_slice(&json_bytes).context("deserialize MeshInvite from URL")
    }
}

// ── Nonce tracking helpers (graph-layer) ─────────────────────────────────────

/// Graph config key for a consumed nonce. One key per nonce; presence = consumed.
pub fn consumed_nonce_key(nonce: &str) -> String {
    format!("mesh_consumed_invite:{}", nonce)
}

// ── Fingerprint helper ────────────────────────────────────────────────────────

/// Produce a short human-readable fingerprint from a hex-encoded public key.
pub fn operator_fingerprint_from_hex(pubkey_hex: &str) -> Result<String> {
    let bytes = hex::decode(pubkey_hex).context("operator public key is not valid hex")?;
    use sha2::Digest;
    let digest = sha2::Sha256::digest(&bytes);
    Ok(digest[..8]
        .chunks(2)
        .map(|pair| {
            if pair.len() == 2 {
                format!("{:02x}{:02x}", pair[0], pair[1])
            } else {
                format!("{:02x}", pair[0])
            }
        })
        .collect::<Vec<_>>()
        .join(":"))
}

// ── Time helper ───────────────────────────────────────────────────────────────

pub fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

// ── Nonce generation ──────────────────────────────────────────────────────────

/// Generate a cryptographically random 32-byte nonce, hex-encoded.
pub fn generate_nonce() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
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
    fn url_roundtrip() {
        let invite = MeshInvite {
            version: 1,
            inviter_hotel: test_hotel(),
            mesh_psk: "abc123".to_string(),
            operator_pubkey_hex: "deadbeef".to_string(),
            operator_fingerprint: "de:ad:be:ef".to_string(),
            issued_at: 1_000_000,
            valid_until: 1_002_000,
            nonce: "cafebabe".to_string(),
        };
        let url = invite.to_url().unwrap();
        assert!(url.starts_with(INVITE_URL_PREFIX));
        let decoded = MeshInvite::from_url(&url).unwrap();
        // Compare fields individually — HotelRecord doesn't derive PartialEq.
        assert_eq!(decoded.version, invite.version);
        assert_eq!(decoded.nonce, invite.nonce);
        assert_eq!(decoded.mesh_psk, invite.mesh_psk);
        assert_eq!(decoded.valid_until, invite.valid_until);
        assert_eq!(decoded.inviter_hotel.hotel_name, invite.inviter_hotel.hotel_name);
        assert_eq!(decoded.inviter_hotel.mesh_port, invite.inviter_hotel.mesh_port);
    }

    #[test]
    fn expired_invite_rejected() {
        let invite = MeshInvite {
            version: 1,
            inviter_hotel: test_hotel(),
            mesh_psk: "abc123".to_string(),
            operator_pubkey_hex: "deadbeef".to_string(),
            operator_fingerprint: "de:ad:be:ef".to_string(),
            issued_at: 1,
            valid_until: 2, // way in the past
            nonce: "cafebabe".to_string(),
        };
        assert!(matches!(
            invite.validate_time(),
            Err(InviteError::Expired { .. })
        ));
    }

    #[test]
    fn wrong_version_rejected() {
        let invite = MeshInvite {
            version: 99,
            inviter_hotel: test_hotel(),
            mesh_psk: "abc123".to_string(),
            operator_pubkey_hex: "deadbeef".to_string(),
            operator_fingerprint: "de:ad:be:ef".to_string(),
            issued_at: 1_000_000,
            valid_until: u64::MAX,
            nonce: "cafebabe".to_string(),
        };
        assert!(matches!(
            invite.validate_time(),
            Err(InviteError::UnsupportedVersion(99))
        ));
    }
}
