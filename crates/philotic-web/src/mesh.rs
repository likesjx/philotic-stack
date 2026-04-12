use anyhow::{bail, Context, Result};
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::membership::{
    consumed_nonce_key, generate_nonce, now_epoch_secs, operator_fingerprint_from_hex,
    MeshInvite, MeshMembershipAcceptPayload, DEFAULT_INVITE_TTL_SECS,
};
use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
use ansible_mesh_core::storage::HotelRecord;
use ansible_mesh_core::{BeaconMessage, MsgType};
use rand::RngCore;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::UdpSocket;
use uuid::Uuid;

use crate::init::{active_profile, profile_dir, public_key_path};

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

fn random_mesh_psk() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn load_operator_pubkey_hex() -> Result<String> {
    let path = public_key_path();
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("failed to read operator public key at {}", path.display()))?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("operator public key is empty; run `phil init` first");
    }
    Ok(trimmed.to_string())
}

fn mesh_psk_from_graph(graph: &GraphDomain) -> Result<Option<String>> {
    Ok(graph
        .get_config_value("mesh_psk")?
        .and_then(|raw| serde_json::from_str::<String>(&raw).ok().or(Some(raw))))
}

fn upsert_mesh_psk(graph: &GraphDomain, mesh_psk: &str) -> Result<()> {
    graph.set_config_value("mesh_psk", &serde_json::to_string(mesh_psk)?)
}

fn update_hotel_mesh_host(graph: &GraphDomain, hotel_name: &str, mesh_host: &str) -> Result<HotelRecord> {
    let Some(mut hotel) = graph.get_hotel(hotel_name)? else {
        bail!(
            "hotel [{}] is not seeded in the graph yet; run `phil load --hotel {}` first",
            hotel_name,
            hotel_name
        );
    };
    hotel.mesh_host = Some(mesh_host.to_string());
    graph.upsert_hotel(&hotel)?;
    Ok(hotel)
}

/// Generate a mesh invite URL for `hotel_name` and print it to stdout.
///
/// Also writes a JSON file alongside for archival. The URL is the canonical
/// delivery artifact — share it via a confidential channel (Telegram DM, Signal,
/// etc). It expire after `ttl_secs` (default 30 minutes) and can only be accepted
/// once (nonce replay protection).
pub async fn invite(
    hotel_name: String,
    mesh_host: String,
    out: Option<PathBuf>,
    ttl_secs: Option<u64>,
) -> Result<()> {
    let graph = open_graph()?;
    let hotel = update_hotel_mesh_host(&graph, &hotel_name, &mesh_host)?;
    let mesh_psk = match mesh_psk_from_graph(&graph)? {
        Some(existing) => existing,
        None => {
            let generated = random_mesh_psk();
            upsert_mesh_psk(&graph, &generated)?;
            generated
        }
    };

    let operator_pubkey_hex = load_operator_pubkey_hex()?;
    let operator_fingerprint = operator_fingerprint_from_hex(&operator_pubkey_hex)?;

    let now = now_epoch_secs();
    let ttl = ttl_secs.unwrap_or(DEFAULT_INVITE_TTL_SECS);
    let nonce = generate_nonce();

    let invite = MeshInvite {
        version: 1,
        inviter_hotel: hotel,
        mesh_psk,
        operator_pubkey_hex: operator_pubkey_hex.clone(),
        operator_fingerprint: operator_fingerprint.clone(),
        issued_at: now,
        valid_until: now + ttl,
        nonce: nonce.clone(),
    };

    // Encode as URL — the primary delivery artifact.
    let invite_url = invite.to_url()?;

    // Also write a JSON file for archival / debugging.
    let out_path = out.unwrap_or_else(|| {
        PathBuf::from(format!("mesh-invite-{}.json", invite.inviter_hotel.hotel_name))
    });
    fs::write(&out_path, serde_json::to_string_pretty(&invite)?)
        .with_context(|| format!("failed to write invite to {}", out_path.display()))?;

    println!("Mesh invite generated:");
    println!("  hotel       {}", invite.inviter_hotel.hotel_name);
    println!(
        "  target      {}:{}",
        invite.inviter_hotel.mesh_host.as_deref().unwrap_or("127.0.0.1"),
        invite.inviter_hotel.mesh_port
    );
    println!("  fingerprint ed25519:{}", operator_fingerprint);
    println!(
        "  expires     +{}min  (at unix {})",
        ttl / 60,
        now + ttl
    );
    println!("  nonce       {}", &nonce[..8]);
    println!("  json file   {}", out_path.display());
    println!();
    println!("Invite URL (share via confidential channel):");
    println!();
    println!("  {}", invite_url);
    println!();
    println!("On the joining hotel run:");
    println!("  philotic-web mesh accept '<url>'");
    Ok(())
}

fn read_invite_from_path(path: &Path) -> Result<MeshInvite> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read invite file {}", path.display()))?;
    serde_json::from_str(&raw).context("invite file is not valid mesh invite JSON")
}

/// Accept a mesh invite delivered as either:
/// - a `philotic-invite://v1/<blob>` URL string
/// - a path to a `.json` invite file
///
/// Validates TTL and checks nonce has not been consumed. On success: stores the
/// inviter's hotel record, writes the mesh PSK to the graph, marks the nonce
/// consumed, and fires a `MeshMembershipAccept` beacon back to the inviter.
pub async fn accept(invite_src: String, hotel_name: String, mesh_host: String) -> Result<()> {
    // Parse invite from URL or file path.
    let invite = if invite_src.starts_with(ansible_mesh_core::membership::INVITE_URL_PREFIX) {
        MeshInvite::from_url(&invite_src)?
    } else {
        read_invite_from_path(Path::new(&invite_src))?
    };

    // Validate version and TTL.
    invite
        .validate_time()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    let graph = open_graph()?;

    // Nonce replay check — reject if already consumed.
    let nonce_key = consumed_nonce_key(&invite.nonce);
    if graph.get_config_value(&nonce_key)?.is_some() {
        bail!("invite nonce has already been consumed (replay rejected)");
    }

    // Store peer hotel, PSK, and mark nonce consumed — all before sending the
    // acceptance beacon so we don't emit a beacon and then fail to persist.
    let local_hotel = update_hotel_mesh_host(&graph, &hotel_name, &mesh_host)?;
    upsert_mesh_psk(&graph, &invite.mesh_psk)?;
    graph.upsert_hotel(&invite.inviter_hotel)?;
    graph.set_config_value(&nonce_key, "consumed")?;

    // Fire acceptance beacon to inviter.
    let payload = MeshMembershipAcceptPayload {
        version: 1,
        hotel: local_hotel.clone(),
        invite_nonce: invite.nonce.clone(),
        accepted_at: now_epoch_secs(),
    };
    let payload_bytes = serde_json::to_vec(&payload)?;

    let msg_id = Uuid::new_v4();
    let timestamp = now_epoch_secs();
    let auth = ansible_mesh_core::authz::MeshAuth::new(invite.mesh_psk.clone());
    let hmac = auth.sign(&msg_id, 0, &payload_bytes, timestamp);

    let target_host = invite
        .inviter_hotel
        .mesh_host
        .as_deref()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or("127.0.0.1");
    let target_addr: SocketAddr = format!("{}:{}", target_host, invite.inviter_hotel.mesh_port)
        .parse()
        .with_context(|| {
            format!(
                "invalid inviter mesh address {}:{}",
                target_host, invite.inviter_hotel.mesh_port
            )
        })?;

    let message = BeaconMessage {
        version: 1,
        msg_id,
        src_node: local_hotel.capabilities.node_id.clone(),
        dest_node: invite.inviter_hotel.capabilities.node_id.clone(),
        msg_type: MsgType::MeshMembershipAccept,
        seq: 0,
        total: 1,
        payload: payload_bytes,
        timestamp,
        hmac,
    };

    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .context("failed to bind local UDP socket for mesh acceptance")?;
    socket
        .send_to(&serde_json::to_vec(&message)?, target_addr)
        .await
        .with_context(|| format!("failed to send mesh acceptance to {}", target_addr))?;

    println!("Mesh invite accepted:");
    println!("  local hotel   {}", local_hotel.hotel_name);
    println!("  inviter hotel {}", invite.inviter_hotel.hotel_name);
    println!("  notified      {}", target_addr);
    println!();
    println!("Local graph now trusts the inviter. If the inviter aiua is running");
    println!("and listening on its mesh port, it will persist this hotel and begin");
    println!("mesh discovery automatically.");
    Ok(())
}
