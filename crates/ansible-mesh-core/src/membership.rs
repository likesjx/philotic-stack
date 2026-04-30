use crate::graph::{AbstractSkillRecord, AbstractToolRecord, ToolsetProfileRecord};
use crate::NodeCapabilities;
use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

pub const MESH_INVITE_VERSION: u8 = 2;
pub const DEFAULT_INVITE_TTL_SECS: u64 = 1_800;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshInvitePayload {
    pub version: u8,
    pub hotel_name: String,
    pub capabilities: NodeCapabilities,
    pub mesh_host: String,
    pub mesh_port: u16,
    pub blob_port: u16,
    pub execution_port: u16,
    pub inviter_pubkey_b64: String,
    pub inviter_fingerprint: String,
    pub inviter_transport_pubkey_b64: String,
    pub nonce: String,
    pub created_at: u64,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshInvite {
    pub payload: MeshInvitePayload,
    pub signature_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshJoinRequestPayload {
    pub version: u8,
    pub invite_nonce: String,
    pub hotel_name: String,
    pub capabilities: NodeCapabilities,
    pub mesh_host: String,
    pub mesh_port: u16,
    pub blob_port: u16,
    pub execution_port: u16,
    pub joiner_pubkey_b64: String,
    pub joiner_fingerprint: String,
    pub joiner_transport_pubkey_b64: String,
    pub requested_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshMembershipAcceptPayload {
    pub payload: MeshJoinRequestPayload,
    pub signature_b64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshMemberRecord {
    pub hotel_name: String,
    pub capabilities: NodeCapabilities,
    pub mesh_host: String,
    pub mesh_port: u16,
    pub blob_port: u16,
    pub execution_port: u16,
    pub member_pubkey_b64: String,
    pub member_fingerprint: String,
    pub member_transport_pubkey_b64: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admitted_via: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admitted_at: Option<u64>,
    pub membership_state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshMembershipSyncPayload {
    pub mesh_id: String,
    pub issued_at: u64,
    pub records: Vec<MeshMemberRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshCatalogSyncPayload {
    pub mesh_id: String,
    pub issued_at: u64,
    #[serde(default)]
    pub abstract_tools: Vec<AbstractToolRecord>,
    #[serde(default)]
    pub abstract_skills: Vec<AbstractSkillRecord>,
    #[serde(default)]
    pub toolset_profiles: Vec<ToolsetProfileRecord>,
}

pub fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn signing_key_from_raw_bytes(bytes: &[u8]) -> Result<SigningKey> {
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("Ed25519 private key must be 32 bytes"))?;
    Ok(SigningKey::from_bytes(&arr))
}

pub fn signing_key_from_hex(value: &str) -> Result<SigningKey> {
    let bytes = hex::decode(value).context("decode Ed25519 private key hex")?;
    signing_key_from_raw_bytes(&bytes)
}

pub fn verifying_key_to_base64url(key: &VerifyingKey) -> String {
    URL_SAFE_NO_PAD.encode(key.to_bytes())
}

pub fn verifying_key_from_base64url(value: &str) -> Result<VerifyingKey> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .context("decode Ed25519 public key from base64url")?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("Ed25519 public key must be 32 bytes"))?;
    VerifyingKey::from_bytes(&arr).context("parse Ed25519 public key")
}

pub fn fingerprint_from_base64url(value: &str) -> Result<String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .context("decode public key for fingerprint")?;
    let digest = Sha256::digest(bytes);
    Ok(digest[..8]
        .chunks(2)
        .map(|chunk| chunk.iter().map(|b| format!("{b:02x}")).collect::<String>())
        .collect::<Vec<_>>()
        .join(":"))
}

pub fn generate_nonce() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

pub fn generate_transport_keypair() -> (String, String) {
    let secret = StaticSecret::random_from_rng(OsRng);
    let public = X25519PublicKey::from(&secret);
    (
        hex::encode(secret.to_bytes()),
        URL_SAFE_NO_PAD.encode(public.as_bytes()),
    )
}

fn transport_secret_from_hex(value: &str) -> Result<StaticSecret> {
    let bytes = hex::decode(value).context("decode X25519 private key hex")?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("X25519 private key must be 32 bytes"))?;
    Ok(StaticSecret::from(arr))
}

fn transport_public_from_base64url(value: &str) -> Result<X25519PublicKey> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .context("decode X25519 public key from base64url")?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("X25519 public key must be 32 bytes"))?;
    Ok(X25519PublicKey::from(arr))
}

pub fn derive_transport_session_key(
    invite_nonce: &str,
    private_key_hex: &str,
    peer_public_key_b64: &str,
) -> Result<String> {
    derive_transport_shared_key(invite_nonce, private_key_hex, peer_public_key_b64)
}

pub fn derive_transport_shared_key(
    context_salt: &str,
    private_key_hex: &str,
    peer_public_key_b64: &str,
) -> Result<String> {
    let local_secret = transport_secret_from_hex(private_key_hex)?;
    let peer_public = transport_public_from_base64url(peer_public_key_b64)?;
    let shared_secret = local_secret.diffie_hellman(&peer_public);
    let mut output = [0u8; 32];
    Hkdf::<Sha256>::new(Some(context_salt.as_bytes()), shared_secret.as_bytes())
        .expand(b"philotic-mesh-peer-auth-v1", &mut output)
        .map_err(|_| anyhow::anyhow!("derive peer mesh auth key from X25519 shared secret"))?;
    Ok(hex::encode(output))
}

fn sign_canonical<T: Serialize>(value: &T, signing_key: &SigningKey) -> Result<String> {
    let canonical = serde_json::to_vec(value).context("serialize signed mesh payload")?;
    let signature = signing_key.sign(&canonical);
    Ok(URL_SAFE_NO_PAD.encode(signature.to_bytes()))
}

fn verify_canonical<T: Serialize>(
    value: &T,
    signature_b64: &str,
    public_key_b64: &str,
) -> Result<()> {
    let canonical = serde_json::to_vec(value).context("serialize signed mesh payload")?;
    let signature_bytes = URL_SAFE_NO_PAD
        .decode(signature_b64)
        .context("decode mesh signature")?;
    let signature_arr: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("mesh signature must be 64 bytes"))?;
    let signature = Signature::from_bytes(&signature_arr);
    let verifying_key = verifying_key_from_base64url(public_key_b64)?;
    verifying_key
        .verify(&canonical, &signature)
        .context("verify signed mesh payload")
}

pub fn sign_invite(payload: MeshInvitePayload, signing_key: &SigningKey) -> Result<MeshInvite> {
    let signature_b64 = sign_canonical(&payload, signing_key)?;
    Ok(MeshInvite {
        payload,
        signature_b64,
    })
}

pub fn verify_invite(invite: &MeshInvite, now: u64) -> Result<()> {
    if invite.payload.version != MESH_INVITE_VERSION {
        bail!(
            "unsupported mesh invite version {} (expected {})",
            invite.payload.version,
            MESH_INVITE_VERSION
        );
    }
    if now > invite.payload.expires_at {
        bail!(
            "mesh invite expired at {} (now {})",
            invite.payload.expires_at,
            now
        );
    }
    verify_canonical(
        &invite.payload,
        &invite.signature_b64,
        &invite.payload.inviter_pubkey_b64,
    )
}

pub fn sign_join_request(
    payload: MeshJoinRequestPayload,
    signing_key: &SigningKey,
) -> Result<MeshMembershipAcceptPayload> {
    let signature_b64 = sign_canonical(&payload, signing_key)?;
    Ok(MeshMembershipAcceptPayload {
        payload,
        signature_b64,
    })
}

pub fn verify_join_request(request: &MeshMembershipAcceptPayload) -> Result<()> {
    if request.payload.version != MESH_INVITE_VERSION {
        bail!(
            "unsupported mesh join version {} (expected {})",
            request.payload.version,
            MESH_INVITE_VERSION
        );
    }
    verify_canonical(
        &request.payload,
        &request.signature_b64,
        &request.payload.joiner_pubkey_b64,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeConstraints, NodeRole};

    fn caps(node_id: &str) -> NodeCapabilities {
        NodeCapabilities {
            node_id: node_id.to_string(),
            roles: vec![NodeRole::AnsibleNode],
            models: vec![],
            tools: vec![],
            constraints: NodeConstraints {
                max_concurrent_jobs: Some(2),
                latency_hint_ms: Some(20),
                trust_level: Some("trusted".into()),
            },
        }
    }

    #[test]
    fn mesh_invite_round_trips() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let public_key_b64 = verifying_key_to_base64url(&signing_key.verifying_key());
        let invite = sign_invite(
            MeshInvitePayload {
                version: MESH_INVITE_VERSION,
                hotel_name: "alpha".into(),
                capabilities: caps("alpha-aiua-01"),
                mesh_host: "alpha.example".into(),
                mesh_port: 9100,
                blob_port: 9101,
                execution_port: 9102,
                inviter_pubkey_b64: public_key_b64.clone(),
                inviter_fingerprint: fingerprint_from_base64url(&public_key_b64).unwrap(),
                inviter_transport_pubkey_b64: generate_transport_keypair().1,
                nonce: generate_nonce(),
                created_at: 123,
                expires_at: 456,
            },
            &signing_key,
        )
        .expect("invite should sign");

        let encoded = serde_json::to_vec(&invite).expect("invite should encode");
        let decoded: MeshInvite = serde_json::from_slice(&encoded).expect("invite should decode");
        verify_invite(&decoded, 200).expect("invite should verify");
        assert_eq!(decoded.payload.hotel_name, "alpha");
        assert_eq!(decoded.payload.capabilities.node_id, "alpha-aiua-01");
        assert_eq!(decoded.payload.mesh_host, "alpha.example");
    }

    #[test]
    fn mesh_accept_round_trips() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let public_key_b64 = verifying_key_to_base64url(&signing_key.verifying_key());
        let payload = sign_join_request(
            MeshJoinRequestPayload {
                version: MESH_INVITE_VERSION,
                invite_nonce: generate_nonce(),
                hotel_name: "beta".into(),
                capabilities: caps("beta-aiua-01"),
                mesh_host: "beta.example".into(),
                mesh_port: 9200,
                blob_port: 9201,
                execution_port: 9202,
                joiner_pubkey_b64: public_key_b64.clone(),
                joiner_fingerprint: fingerprint_from_base64url(&public_key_b64).unwrap(),
                joiner_transport_pubkey_b64: generate_transport_keypair().1,
                requested_at: 456,
            },
            &signing_key,
        )
        .expect("join request should sign");

        let encoded = serde_json::to_vec(&payload).expect("payload should encode");
        let decoded: MeshMembershipAcceptPayload =
            serde_json::from_slice(&encoded).expect("payload should decode");
        verify_join_request(&decoded).expect("join request should verify");
        assert_eq!(decoded.payload.hotel_name, "beta");
        assert_eq!(decoded.payload.capabilities.node_id, "beta-aiua-01");
        assert_eq!(decoded.payload.mesh_port, 9200);
    }

    #[test]
    fn tampered_join_request_fails_verification() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let public_key_b64 = verifying_key_to_base64url(&signing_key.verifying_key());
        let mut payload = sign_join_request(
            MeshJoinRequestPayload {
                version: MESH_INVITE_VERSION,
                invite_nonce: generate_nonce(),
                hotel_name: "beta".into(),
                capabilities: caps("beta-aiua-01"),
                mesh_host: "beta.example".into(),
                mesh_port: 9200,
                blob_port: 9201,
                execution_port: 9202,
                joiner_pubkey_b64: public_key_b64.clone(),
                joiner_fingerprint: fingerprint_from_base64url(&public_key_b64).unwrap(),
                joiner_transport_pubkey_b64: generate_transport_keypair().1,
                requested_at: 456,
            },
            &signing_key,
        )
        .expect("join request should sign");

        payload.payload.hotel_name = "mallory".into();
        assert!(verify_join_request(&payload).is_err());
    }

    #[test]
    fn expired_invite_fails_verification() {
        let signing_key = SigningKey::generate(&mut OsRng);
        let public_key_b64 = verifying_key_to_base64url(&signing_key.verifying_key());
        let invite = sign_invite(
            MeshInvitePayload {
                version: MESH_INVITE_VERSION,
                hotel_name: "alpha".into(),
                capabilities: caps("alpha-aiua-01"),
                mesh_host: "alpha.example".into(),
                mesh_port: 9100,
                blob_port: 9101,
                execution_port: 9102,
                inviter_pubkey_b64: public_key_b64.clone(),
                inviter_fingerprint: fingerprint_from_base64url(&public_key_b64).unwrap(),
                inviter_transport_pubkey_b64: generate_transport_keypair().1,
                nonce: generate_nonce(),
                created_at: 100,
                expires_at: 101,
            },
            &signing_key,
        )
        .expect("invite should sign");

        assert!(verify_invite(&invite, 102).is_err());
    }

    #[test]
    fn transport_session_key_matches_for_both_hotels() {
        let nonce = generate_nonce();
        let (inviter_private_hex, inviter_public_b64) = generate_transport_keypair();
        let (joiner_private_hex, joiner_public_b64) = generate_transport_keypair();

        let inviter_key =
            derive_transport_session_key(&nonce, &inviter_private_hex, &joiner_public_b64)
                .expect("inviter should derive auth key");
        let joiner_key =
            derive_transport_session_key(&nonce, &joiner_private_hex, &inviter_public_b64)
                .expect("joiner should derive auth key");

        assert_eq!(inviter_key, joiner_key);
    }

    #[test]
    fn deterministic_transport_shared_key_matches_for_both_peers() {
        let (alpha_private, alpha_public) = generate_transport_keypair();
        let (beta_private, beta_public) = generate_transport_keypair();
        let context = "philotic-mesh-peer-v2:alpha-aiua-01:beta-aiua-01";

        let alpha_key = derive_transport_shared_key(context, &alpha_private, &beta_public).unwrap();
        let beta_key = derive_transport_shared_key(context, &beta_private, &alpha_public).unwrap();

        assert_eq!(alpha_key, beta_key);
    }

    #[test]
    fn mesh_catalog_sync_round_trips_shared_catalog_records() {
        let payload = MeshCatalogSyncPayload {
            mesh_id: "default".into(),
            issued_at: 123,
            abstract_tools: vec![AbstractToolRecord {
                tool_name: "hotel.best_place_to_run".into(),
                description: "placement helper".into(),
                input_schema: serde_json::json!({"type": "object"}),
                class: "config".into(),
                tool_markers: vec![],
            }],
            abstract_skills: vec![AbstractSkillRecord {
                skill_name: "role.governance".into(),
                description: "govern roles".into(),
                implied_tools: vec!["hotel.best_place_to_run".into()],
                ..Default::default()
            }],
            toolset_profiles: vec![ToolsetProfileRecord {
                profile_name: "admin".into(),
                allowed_tools: vec!["hotel.best_place_to_run".into()],
                allowed_classes: vec!["config".into()],
                allowed_skills: vec!["role.governance".into()],
                description: Some("admin defaults".into()),
            }],
        };

        let encoded = serde_json::to_vec(&payload).expect("payload should encode");
        let decoded: MeshCatalogSyncPayload =
            serde_json::from_slice(&encoded).expect("payload should decode");
        assert_eq!(decoded.mesh_id, "default");
        assert_eq!(decoded.abstract_tools[0].tool_name, "hotel.best_place_to_run");
        assert_eq!(decoded.abstract_skills[0].skill_name, "role.governance");
        assert_eq!(decoded.toolset_profiles[0].profile_name, "admin");
    }
}
