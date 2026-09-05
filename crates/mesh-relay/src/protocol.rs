//! Wire protocol and authentication primitives for the mesh relay.
//!
//! ## Why a relay exists
//!
//! The Philotic mesh addresses each peer by a single reachable IP (in practice a
//! Tailscale `100.64/10` address). When Tailscale is logged out the mesh has no
//! second path and goes dark. mac-jane roams behind CGNAT and can never be
//! reached inbound, so the only node with a stable inbound address is vps-jane.
//! The relay therefore runs on vps and every hotel dials it **outbound** (which
//! works through CGNAT/NAT) over QUIC/TLS. Frames destined for an unreachable
//! peer are handed to the relay, which forwards them down that peer's own
//! outbound connection — the DERP model.
//!
//! ## Trust model
//!
//! The relay forwards **opaque, already-HMAC-signed** mesh frames; it never holds
//! peer signing keys and cannot forge a frame — receiving hotels validate the
//! HMAC exactly as they do for a directly delivered frame. QUIC/TLS supplies the
//! wire confidentiality the mesh itself lacks (the mesh is signed, not
//! encrypted), so relayed frames are not cleartext on the public internet.
//!
//! The one risk the relay must close itself is **impersonation-to-receive**: a
//! connection claiming to be node X would otherwise be handed X's inbound
//! traffic. The mesh's per-pair HMAC keys are symmetric (every hotel holds its
//! peers' keys), so they cannot prove identity. Instead we challenge the
//! connection to sign a fresh nonce with node X's **ed25519 member identity
//! key** — the private half of which only X holds — and verify against the
//! published member public key. See [`sign_challenge`] / [`verify_challenge`].

use ansible_mesh_core::membership::verifying_key_from_base64url;
use anyhow::{Result, anyhow};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

/// Domain-separation tag mixed into every relay-auth signature so it can never
/// be replayed as, or confused with, any other ed25519 signature the same mesh
/// identity key produces (mesh invites, membership accepts, …). Bump the suffix
/// if the challenge payload shape ever changes.
pub const RELAY_AUTH_DOMAIN: &str = "philotic-mesh-relay-auth-v1";

/// Which mesh plane a relayed frame belongs to, so the receiving hotel injects
/// it into the correct inbox. Beacons carry liveness / `HotelStateSync`;
/// execution carries the durable event ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Plane {
    Beacon,
    Execution,
}

/// Messages a hotel (client) sends to the relay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    /// First frame: announce which node this connection claims to be. The relay
    /// does NOT trust this — it resolves the node's published member public key
    /// from its own store and challenges the connection to prove possession.
    Hello { node_id: String },
    /// Answer to a [`ServerMsg::Challenge`]: the nonce signed with the node's
    /// ed25519 member identity key.
    Auth { signature_hex: String },
    /// Forward `inner` (opaque, already-signed mesh frame bytes, base64) to
    /// `to_node_id` on `plane`.
    Relay {
        to_node_id: String,
        plane: Plane,
        inner_b64: String,
    },
}

/// Messages the relay sends to a hotel (client).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    /// Prove you are `Hello.node_id` by signing this nonce.
    Challenge { nonce: String },
    /// Authentication succeeded; the connection is now registered for its node.
    AuthOk,
    /// Authentication failed; the relay will close the connection.
    AuthFailed { reason: String },
    /// A frame forwarded from another hotel, for local injection.
    Deliver {
        from_node_id: String,
        plane: Plane,
        inner_b64: String,
    },
    /// The target of a [`ClientMsg::Relay`] has no live connection right now.
    Undeliverable { to_node_id: String },
}

/// Canonical bytes a hotel signs to prove it holds `node_id`'s member identity
/// key. Length-delimited so distinct field boundaries can never collide
/// (`("a","bc")` must not hash the same as `("ab","c")`).
pub fn challenge_signing_payload(node_id: &str, nonce: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    for part in [RELAY_AUTH_DOMAIN, node_id, nonce] {
        buf.extend_from_slice(&(part.len() as u32).to_be_bytes());
        buf.extend_from_slice(part.as_bytes());
    }
    buf
}

/// Sign a relay challenge with this hotel's ed25519 member identity key.
/// Returns the signature as lowercase hex.
pub fn sign_challenge(signing_key: &SigningKey, node_id: &str, nonce: &str) -> String {
    let sig = signing_key.sign(&challenge_signing_payload(node_id, nonce));
    hex::encode(sig.to_bytes())
}

/// Verify a relay challenge response against the node's published member public
/// key (base64url, as stored in `config:mesh_member_public_key:<hotel>`).
///
/// Returns `Ok(())` only when `signature_hex` is a valid signature by
/// `member_public_key_b64` over exactly this `(node_id, nonce)` challenge.
pub fn verify_challenge(
    member_public_key_b64: &str,
    node_id: &str,
    nonce: &str,
    signature_hex: &str,
) -> Result<()> {
    let verifying_key: VerifyingKey = verifying_key_from_base64url(member_public_key_b64)?;
    let raw =
        hex::decode(signature_hex).map_err(|e| anyhow!("relay auth: bad signature hex: {e}"))?;
    let bytes: [u8; 64] = raw
        .as_slice()
        .try_into()
        .map_err(|_| anyhow!("relay auth: signature must be 64 bytes, got {}", raw.len()))?;
    let sig = Signature::from_bytes(&bytes);
    verifying_key
        .verify(&challenge_signing_payload(node_id, nonce), &sig)
        .map_err(|e| anyhow!("relay auth: signature verification failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::membership::verifying_key_to_base64url;
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    fn keypair() -> (SigningKey, String) {
        let sk = SigningKey::generate(&mut OsRng);
        let pk_b64 = verifying_key_to_base64url(&sk.verifying_key());
        (sk, pk_b64)
    }

    #[test]
    fn valid_challenge_response_verifies() {
        let (sk, pk) = keypair();
        let sig = sign_challenge(&sk, "mac-jane-aiua-01", "nonce-abc");
        assert!(verify_challenge(&pk, "mac-jane-aiua-01", "nonce-abc", &sig).is_ok());
    }

    #[test]
    fn wrong_key_is_rejected() {
        // The impersonation-to-receive case: a mesh peer that does NOT hold
        // mac's identity key cannot answer mac's challenge, even though under
        // the old symmetric-HMAC scheme it would hold mac's per-pair key.
        let (attacker_sk, _) = keypair();
        let (_, victim_pk) = keypair();
        let sig = sign_challenge(&attacker_sk, "mac-jane-aiua-01", "nonce-abc");
        assert!(verify_challenge(&victim_pk, "mac-jane-aiua-01", "nonce-abc", &sig).is_err());
    }

    #[test]
    fn tampered_nonce_is_rejected() {
        let (sk, pk) = keypair();
        let sig = sign_challenge(&sk, "mac-jane-aiua-01", "nonce-abc");
        assert!(verify_challenge(&pk, "mac-jane-aiua-01", "nonce-DIFFERENT", &sig).is_err());
    }

    #[test]
    fn swapped_node_id_is_rejected() {
        // A signature minted for one node must not authenticate a connection
        // claiming to be a different node, even with the same nonce.
        let (sk, pk) = keypair();
        let sig = sign_challenge(&sk, "mac-jane-aiua-01", "nonce-abc");
        assert!(verify_challenge(&pk, "mbp-jane-aiua-01", "nonce-abc", &sig).is_err());
    }

    #[test]
    fn payload_is_length_delimited_no_boundary_collision() {
        // ("ab","c") must not equal ("a","bc") — proves the length prefixes work.
        assert_ne!(
            challenge_signing_payload("ab", "c"),
            challenge_signing_payload("a", "bc")
        );
    }

    #[test]
    fn client_msg_roundtrips_json() {
        let msg = ClientMsg::Relay {
            to_node_id: "vps-jane-aiua-01".into(),
            plane: Plane::Execution,
            inner_b64: "ZGVhZGJlZWY=".into(),
        };
        let wire = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<ClientMsg>(&wire).unwrap(), msg);
    }

    #[test]
    fn server_msg_roundtrips_json() {
        let msg = ServerMsg::Deliver {
            from_node_id: "mac-jane-aiua-01".into(),
            plane: Plane::Beacon,
            inner_b64: "ZGVhZGJlZWY=".into(),
        };
        let wire = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<ServerMsg>(&wire).unwrap(), msg);
    }
}
