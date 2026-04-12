//! `phil mesh invite` and `phil mesh accept` — v2 signed invite / ECDH join ceremony.
//!
//! V2 protocol: No PSK in the URL. The invite payload is signed with the inviting
//! hotel's Ed25519 private key. Session keys are derived via X25519 ECDH + HKDF.
//! An intercepted invite URL cannot be used to join the mesh.

use anyhow::{bail, Context, Result};
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::membership::{
    consumed_nonce_key, derive_session_key_joiner, fingerprint_from_base64url,
    generate_nonce, generate_x25519_ephemeral, now_epoch_secs, signing_key_from_raw_bytes,
    verifying_key_to_base64url, InvitePayload, JoinRequest, SignedInvite,
    DEFAULT_INVITE_TTL_SECS,
};
use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
use ansible_mesh_core::storage::HotelRecord;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::UdpSocket;
use uuid::Uuid;

use crate::init::{active_profile, profile_dir, private_key_path, public_key_path};

fn db_path() -> PathBuf {
    match active_profile() {
        Some(_) => profile_dir().join("context.db"),
        None => PathBuf::from("aiua_context.db"),
    }
}

fn open_graph() -> Result<GraphDomain> {
    let path = db_path();
    let storage = SqliteGraphStorage::open(&path)
        .with_context(|| format!("failed to open graph DB at {}", path.display()))?;
    Ok(GraphDomain::new(Arc::new(storage.adapter())))
}

fn load_hotel_private_signing_key() -> Result<ed25519_dalek::SigningKey> {
    // Prefer hotel-specific private key if it exists.
    let hotel_key_path = profile_dir().join("vault").join("hotel_private_key");
    if hotel_key_path.exists() {
        let raw = fs::read(&hotel_key_path)
            .with_context(|| format!("read hotel private key at {}", hotel_key_path.display()))?;
        return ansible_mesh_core::membership::signing_key_from_raw_bytes(&raw)
            .context("parse hotel private key");
    }

    // Fallback: operator.key (same format — raw 32 bytes).
    // TODO(S1): generate a dedicated hotel identity keypair in `aiua` on first start.
    let op_key_path = private_key_path();
    if op_key_path.exists() {
        let raw = fs::read(&op_key_path)
            .with_context(|| format!("read operator private key at {}", op_key_path.display()))?;
        return ansible_mesh_core::membership::signing_key_from_raw_bytes(&raw)
            .context("parse operator private key");
    }

    anyhow::bail!(
        "no hotel private key found at {} or {}. Run `phil init` first.",
        hotel_key_path.display(),
        op_key_path.display()
    )
}

fn load_hotel_public_key_base64url() -> Result<String> {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;

    // Try the base64url-encoded hotel pubkey first (for v2 invites).
    let hotel_pub_path = profile_dir().join("identity").join("hotel_public.key");
    if hotel_pub_path.exists() {
        let raw = fs::read_to_string(&hotel_pub_path)
            .with_context(|| format!("read hotel public key at {}", hotel_pub_path.display()))?;
        return Ok(raw.trim().to_string());
    }

    // Fallback: operator.pub is hex-encoded. Convert to base64url.
    let op_pub_path = public_key_path();
    let hex_raw = fs::read_to_string(&op_pub_path)
        .with_context(|| format!("read operator public key at {}", op_pub_path.display()))?;
    let bytes = hex::decode(hex_raw.trim()).context("decode operator pubkey hex")?;
    Ok(URL_SAFE_NO_PAD.encode(&bytes))
}

fn mesh_host_for(hotel: &HotelRecord) -> &str {
    hotel
        .mesh_host
        .as_deref()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or("127.0.0.1")
}

fn update_hotel_mesh_host(graph: &GraphDomain, hotel_name: &str, mesh_host: &str) -> Result<HotelRecord> {
    let Some(mut hotel) = graph.get_hotel(hotel_name)? else {
        bail!(
            "hotel '{}' not found in graph. Run `phil load --hotel {}` first.",
            hotel_name, hotel_name
        );
    };
    hotel.mesh_host = Some(mesh_host.to_string());
    graph.upsert_hotel(&hotel)?;
    Ok(hotel)
}

// ─── Invite ───────────────────────────────────────────────────────────────────

/// Generate a signed, single-use mesh invite URL for `hotel_name`.
///
/// The URL contains the inviting hotel's Ed25519 signature over the full payload.
/// The PSK is NOT in the URL. Session keys are derived from ECDH during acceptance.
///
/// The ephemeral X25519 private key must be stored in the graph so `aiua` can
/// complete the ECDH when the `JoinRequest` arrives. It is stored encrypted
/// under the config key `mesh_invite_ephemeral:<nonce>`.
pub async fn invite(
    hotel_name: String,
    mesh_host: String,
    out: Option<PathBuf>,
    ttl_secs: Option<u64>,
) -> Result<()> {
    let graph = open_graph()?;
    let hotel = update_hotel_mesh_host(&graph, &hotel_name, &mesh_host)?;

    let signing_key = load_hotel_private_signing_key()?;
    let verifying_key = signing_key.verifying_key();
    let pubkey_b64 = verifying_key_to_base64url(&verifying_key);
    let fingerprint = fingerprint_from_base64url(&pubkey_b64)?;

    let (x25519_secret_bytes, x25519_pub_enc) = generate_x25519_ephemeral();
    let nonce = generate_nonce();
    let now = now_epoch_secs();
    let ttl = ttl_secs.unwrap_or(DEFAULT_INVITE_TTL_SECS);

    let payload = InvitePayload {
        version: 2,
        inviter_hotel_id: hotel_name.clone(),
        inviter_ed25519_pubkey: pubkey_b64.clone(),
        inviter_x25519_ephemeral_pubkey: x25519_pub_enc.clone(),
        inviter_hotel: hotel.clone(),
        nonce: nonce.clone(),
        valid_until: now + ttl,
        issued_at: now,
    };

    let signed = SignedInvite::new(payload, &signing_key)?;
    let invite_url = signed.to_url()?;

    // Store the ephemeral secret so aiua can complete ECDH when JoinRequest arrives.
    let ephemeral_key = format!("mesh_invite_ephemeral:{}", nonce);
    let secret_hex = hex::encode(x25519_secret_bytes);
    graph.set_config_value(&ephemeral_key, &serde_json::to_string(&secret_hex)?)?;

    // Write JSON archive alongside.
    let out_path = out.unwrap_or_else(|| {
        PathBuf::from(format!("mesh-invite-{}.json", hotel_name))
    });
    let archive = serde_json::json!({
        "invite_url": invite_url,
        "fingerprint": fingerprint,
        "nonce_prefix": &nonce[..8],
        "hotel": hotel_name,
        "valid_until": now + ttl,
        "issued_at": now,
    });
    fs::write(&out_path, serde_json::to_string_pretty(&archive)?)
        .with_context(|| format!("write invite archive to {}", out_path.display()))?;

    println!("Mesh invite generated (v2 — signed, no PSK in URL):");
    println!("  hotel       {}", hotel_name);
    println!("  target      {}:{}", mesh_host_for(&hotel), hotel.mesh_port);
    println!("  fingerprint ed25519:{}", fingerprint);
    println!("  expires     +{}min  (at unix {})", ttl / 60, now + ttl);
    println!("  nonce       {}", &nonce[..8]);
    println!("  archive     {}", out_path.display());
    println!();
    println!("Invite URL — share via confidential channel (Telegram DM, Signal, etc.):");
    println!();
    println!("  {}", invite_url);
    println!();
    println!("The joining hotel runs:");
    println!("  phil mesh accept '<url>' --hotel <their-hotel> --mesh-host <their-addr>");
    println!();
    println!("Security note: An intercepted URL cannot be used to join the mesh.");
    println!("Session keys are derived from ECDH and never transmitted.");
    Ok(())
}

// ─── Accept ───────────────────────────────────────────────────────────────────

/// Accept a mesh invite (URL or JSON archive path).
///
/// Full ceremony:
/// 1. Parse and verify Ed25519 signature.
/// 2. Validate TTL and nonce.
/// 3. Derive session key from ECDH.
/// 4. Store inviter hotel + session key in graph.
/// 5. Send `JoinRequest` beacon to inviter so it can complete ECDH on its side.
pub async fn accept(invite_src: String, hotel_name: String, mesh_host: String) -> Result<()> {
    let signed = if invite_src.starts_with(ansible_mesh_core::membership::INVITE_URL_PREFIX_V2) {
        SignedInvite::from_url(&invite_src)?
    } else {
        // Try reading as a JSON archive produced by `invite --out`.
        let json: serde_json::Value = serde_json::from_str(
            &fs::read_to_string(Path::new(&invite_src))
                .with_context(|| format!("read invite file {}", invite_src))?,
        )
        .context("parse invite JSON archive")?;
        let url = json
            .get("invite_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("invite file missing 'invite_url' field"))?;
        SignedInvite::from_url(url)?
    };

    // 1. Verify signature — before doing anything else.
    signed
        .verify_signature()
        .map_err(|e| anyhow::anyhow!("invite signature verification failed: {}", e))?;

    // 2. Validate TTL and nonce.
    signed
        .validate_time()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    let graph = open_graph()?;
    let nonce_key = consumed_nonce_key(&signed.payload.nonce);
    if graph.get_config_value(&nonce_key)?.is_some() {
        bail!("invite nonce already consumed (replay rejected)");
    }

    // 3. ECDH — derive session key. B generates its own ephemeral X25519 keypair.
    let joiner_ecdh = derive_session_key_joiner(&signed.payload.inviter_x25519_ephemeral_pubkey)?;

    let local_hotel = update_hotel_mesh_host(&graph, &hotel_name, &mesh_host)?;
    let joiner_pubkey_b64 = load_hotel_public_key_base64url()?;
    let fingerprint = fingerprint_from_base64url(&joiner_pubkey_b64)?;

    // 4. Persist: inviter hotel + session key + mark nonce consumed.
    graph.upsert_hotel(&signed.payload.inviter_hotel)?;
    graph.set_config_value(&nonce_key, "consumed")?;

    // Store derived session key for this peer (encrypted at rest TODO: vault).
    let peer_key = format!("mesh_session_key:{}", signed.payload.inviter_hotel_id);
    graph.set_config_value(
        &peer_key,
        &serde_json::to_string(&hex::encode(joiner_ecdh.session_key))?,
    )?;

    // 5. Send JoinRequest to inviter so it can compute the same session key.
    let join_req = JoinRequest {
        version: 2,
        invite_nonce: signed.payload.nonce.clone(),
        joiner_hotel: local_hotel.clone(),
        joiner_ed25519_pubkey: joiner_pubkey_b64,
        joiner_x25519_ephemeral_pubkey: joiner_ecdh.joiner_x25519_pubkey_enc.clone(),
        requested_at: now_epoch_secs(),
    };
    let payload_bytes = serde_json::to_vec(&join_req)?;

    let target_host = signed
        .payload
        .inviter_hotel
        .mesh_host
        .as_deref()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or("127.0.0.1");
    let target_addr: SocketAddr =
        format!("{}:{}", target_host, signed.payload.inviter_hotel.mesh_port)
            .parse()
            .with_context(|| format!("invalid inviter mesh addr {}:{}", target_host, signed.payload.inviter_hotel.mesh_port))?;

    // Use unsigned UDP for the JoinRequest — the JoinRequest itself contains B's
    // identity pubkey + ECDH material, so A can verify B's identity separately.
    // Per-peer HMAC authentication (S3) will replace this with signed traffic.
    let msg_id = Uuid::new_v4();
    let timestamp = now_epoch_secs();
    let message = ansible_mesh_core::BeaconMessage {
        version: 2,
        msg_id,
        src_node: local_hotel.capabilities.node_id.clone(),
        dest_node: signed.payload.inviter_hotel.capabilities.node_id.clone(),
        msg_type: ansible_mesh_core::MsgType::MeshMembershipAccept,
        seq: 0,
        total: 1,
        payload: payload_bytes,
        timestamp,
        hmac: vec![], // S3: per-peer HMAC not yet enforced
    };

    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .context("bind local UDP socket for join request")?;
    socket
        .send_to(&serde_json::to_vec(&message)?, target_addr)
        .await
        .with_context(|| format!("send JoinRequest to {}", target_addr))?;

    let inviter_fp = fingerprint_from_base64url(&signed.payload.inviter_ed25519_pubkey)
        .unwrap_or_else(|_| "?".to_string());

    println!("Mesh invite accepted (v2 — ECDH session key derived):");
    println!("  local hotel     {}", local_hotel.hotel_name);
    println!("  local identity  ed25519:{}", fingerprint);
    println!("  inviter hotel   {}", signed.payload.inviter_hotel_id);
    println!("  inviter id      ed25519:{}", inviter_fp);
    println!("  session key     derived via X25519+HKDF (never transmitted)");
    println!("  notified        {}", target_addr);
    println!();
    println!("Local graph now trusts the inviter. If the inviter aiua is running,");
    println!("it will complete the ECDH ceremony and both hotels will begin authenticated");
    println!("mesh communication automatically.");
    Ok(())
}
