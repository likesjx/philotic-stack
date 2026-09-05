use crate::authz::MeshAuth;
use crate::graph::{MembraneTransportHomeRecord, ModelProfileRecord};
use crate::registry::{CapabilityAdvertisement, ExecutionReachability};
use crate::{BeaconMessage, MsgType, NodeCapabilities, NodeHealthSnapshot};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tokio::net::UdpSocket;
use uuid::Uuid;

const MAX_SYNC_PAYLOAD_BYTES: usize = 900;

/// Wire-size budget for a single `HotelStateSync` datagram, measured on the
/// FULL signed `BeaconMessage` encoding (not the inner payload).
///
/// The binding ceiling is NOT the ~65507-byte UDP maximum: macOS caps
/// datagrams at `net.inet.udp.maxdgram` = **9216 bytes by default**, and Mac
/// hotels are first-class mesh senders. `BeaconMessage.payload` is a `Vec<u8>`
/// that serde_json encodes as a JSON integer array (`[123,34,...]`), a
/// payload-dependent 2.5–4× blowup, so inner-byte proxies chronically
/// mis-budget — the earlier 12 KB inner budget wrapped to ~48 KB on the wire
/// and every Mac-side broadcast failed EMSGSIZE ("Message too long", os error
/// 40), silently dropping hotel-state sync (DEF-032 recurrence). Chunking now
/// measures [`hotel_state_wire_len`] directly against this ceiling, using the
/// ACTIVE [`crate::WireEncoding`] — once senders flip to base64 (see
/// `PHILOTIC_BEACON_PAYLOAD_B64`) the same budget fits ~3-4x more state per
/// datagram.
const MAX_HOTEL_STATE_WIRE_BYTES: usize = 9_000;

/// A single peer record included in a [`MeshCatalogSyncPayload`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshPeerEntry {
    pub node_id: String,
    pub hotel_name: String,
    pub mesh_host: Option<String>,
    pub mesh_port: u16,
}

/// Payload for `MsgType::MeshCatalogSync` sent on reconnect.
/// Carries the sender's current known-good peer directory so the receiver
/// can update stale hotel records without waiting for each peer's next heartbeat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshCatalogSyncPayload {
    pub peers: Vec<MeshPeerEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatPayload {
    pub capabilities: NodeCapabilities,
    #[serde(default)]
    pub execution_reachability: Option<ExecutionReachability>,
    /// Environment vitals sampled at emit time; absent on older nodes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_health: Option<NodeHealthSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitySyncPayload {
    pub capabilities: NodeCapabilities,
    #[serde(default)]
    pub execution_reachability: Option<ExecutionReachability>,
    #[serde(default)]
    pub advertisements: Vec<CapabilityAdvertisement>,
    pub sync_id: Uuid,
    pub chunk_index: u32,
    pub chunk_total: u32,
}

/// One guest entry in a [`HotelStateSyncPayload`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotelStateSyncGuest {
    pub guest_id: String,
    pub role: String,
    pub active: bool,
}

/// One agent identity entry in a [`HotelStateSyncPayload`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotelStateSyncAgent {
    pub agent_id: String,
    pub persona_name: String,
}

/// One runtime-placed role home in a [`HotelStateSyncPayload`]. Only roles
/// with a non-zero `placement_updated_unix` are gossiped; receivers apply
/// last-writer-wins onto role records they already hold (`placement_sync`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HotelStateSyncRoleHome {
    pub agent_id: String,
    pub role_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_node: Option<String>,
    pub placement_updated_unix: u64,
}

/// Payload for `MsgType::HotelStateSync`.
/// Broadcast whenever a hotel's routable state changes so every peer on the mesh
/// can route to any agent/model without querying the authority hotel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotelStateSyncPayload {
    pub node_id: String,
    pub hotel_name: String,
    pub guests: Vec<HotelStateSyncGuest>,
    pub agents: Vec<HotelStateSyncAgent>,
    #[serde(default)]
    pub model_profiles: Vec<ModelProfileRecord>,
    /// Runtime role placements (graph truth), so every hotel agrees who is
    /// home for a role. Rides every chunk like the roster.
    #[serde(default)]
    pub role_homes: Vec<HotelStateSyncRoleHome>,
    /// Runtime transport placements (Telegram poll authority etc.). Rides
    /// every chunk like the roster.
    #[serde(default)]
    pub transport_homes: Vec<MembraneTransportHomeRecord>,
}

/// Emits a `HotelStateSync` to a target carrying the sender's current routable state.
///
/// The small guests/agents roster rides every datagram while `model_profiles`
/// is split across as many datagrams as needed to keep each under the UDP
/// ceiling (see [`chunk_hotel_state_payloads`]). The receiver upserts each
/// profile idempotently and replaces the roster per datagram, so no reassembly
/// is required and dropped chunks self-heal on the next 30s broadcast.
pub async fn emit_hotel_state_sync(
    socket: &UdpSocket,
    target: SocketAddr,
    capabilities: &NodeCapabilities,
    payload: HotelStateSyncPayload,
    auth_key: &str,
) -> Result<()> {
    for chunk in chunk_hotel_state_payloads(&payload)? {
        emit_signed_message(
            socket,
            target,
            capabilities,
            MsgType::HotelStateSync,
            serde_json::to_vec(&chunk)?,
            auth_key,
        )
        .await?;
    }
    Ok(())
}

/// Conservatively measures the signed wire size a payload will occupy.
///
/// Mirrors `emit_signed_message` framing with worst-case envelope fields
/// (64-byte HMAC, broadcast destination, max-width timestamp) so the estimate
/// upper-bounds the real datagram regardless of key or clock.
fn hotel_state_wire_len(payload: &HotelStateSyncPayload) -> Result<usize> {
    let payload_bytes = serde_json::to_vec(payload)?;
    let msg = BeaconMessage {
        version: 1,
        msg_id: Uuid::nil(),
        src_node: payload.node_id.clone(),
        dest_node: "broadcast".to_string(),
        msg_type: MsgType::HotelStateSync,
        seq: u32::MAX,
        total: u32::MAX,
        timestamp: u64::MAX,
        payload: payload_bytes.into(),
        hmac: vec![255u8; 64].into(),
    };
    Ok(serde_json::to_vec(&msg)?.len())
}

/// Splits a [`HotelStateSyncPayload`] into datagram-sized payloads.
///
/// Guests and agents are cloned onto every chunk (the receiver replaces the
/// roster per datagram, so roster-free chunks would wipe it on older peers);
/// `model_profiles` are packed greedily so each chunk's FULL signed wire size
/// stays within [`MAX_HOTEL_STATE_WIRE_BYTES`]. A profile that cannot fit in
/// any datagram alongside the roster is skipped with a warning — an oversized
/// datagram is guaranteed to fail EMSGSIZE on macOS senders, which would lose
/// the roster too. The union of emitted profiles equals the input minus
/// skipped ones, order preserved. Returns at least one chunk.
fn chunk_hotel_state_payloads(
    payload: &HotelStateSyncPayload,
) -> Result<Vec<HotelStateSyncPayload>> {
    // Fast path: the whole payload already fits in one datagram.
    if hotel_state_wire_len(payload)? <= MAX_HOTEL_STATE_WIRE_BYTES {
        return Ok(vec![payload.clone()]);
    }

    let with_profiles = |model_profiles: Vec<ModelProfileRecord>| HotelStateSyncPayload {
        node_id: payload.node_id.clone(),
        hotel_name: payload.hotel_name.clone(),
        guests: payload.guests.clone(),
        agents: payload.agents.clone(),
        model_profiles,
        role_homes: payload.role_homes.clone(),
        transport_homes: payload.transport_homes.clone(),
    };

    let base_len = hotel_state_wire_len(&with_profiles(Vec::new()))?;
    if base_len > MAX_HOTEL_STATE_WIRE_BYTES {
        // The roster alone exceeds the ceiling; nothing we pack will send.
        // Ship it anyway so the failure stays visible at the send site rather
        // than silently dropping state sync here.
        tracing::warn!(
            wire_bytes = base_len,
            ceiling = MAX_HOTEL_STATE_WIRE_BYTES,
            "hotel-state roster alone exceeds the UDP wire ceiling; broadcast will likely fail"
        );
        return Ok(vec![payload.clone()]);
    }

    let mut chunks: Vec<HotelStateSyncPayload> = Vec::new();
    let mut current: Vec<ModelProfileRecord> = Vec::new();

    for profile in &payload.model_profiles {
        let mut candidate = current.clone();
        candidate.push(profile.clone());
        let over_budget =
            hotel_state_wire_len(&with_profiles(candidate.clone()))? > MAX_HOTEL_STATE_WIRE_BYTES;
        if !over_budget {
            current = candidate;
        } else if current.is_empty() {
            // Roster + this single profile can never fit: sending it would
            // EMSGSIZE and lose the roster broadcast too. Skip the profile.
            tracing::warn!(
                model_ref = %profile.model_ref,
                "model profile too large for one hotel-state datagram; skipping from mesh sync"
            );
        } else {
            chunks.push(with_profiles(std::mem::take(&mut current)));
            current.push(profile.clone());
            // Re-check the new singleton batch on the next iteration boundary:
            // if it alone exceeds the ceiling, the branch above will catch it
            // when the following profile arrives — but if it is the LAST
            // profile we must validate it before the final flush below.
            if hotel_state_wire_len(&with_profiles(current.clone()))? > MAX_HOTEL_STATE_WIRE_BYTES {
                let dropped = current.pop();
                if let Some(p) = dropped {
                    tracing::warn!(
                        model_ref = %p.model_ref,
                        "model profile too large for one hotel-state datagram; skipping from mesh sync"
                    );
                }
            }
        }
    }

    if !current.is_empty() || chunks.is_empty() {
        chunks.push(with_profiles(current));
    }

    Ok(chunks)
}

/// Emits compact heartbeat messages over the given UDP socket to a target address.
pub async fn emit_heartbeat(
    socket: &UdpSocket,
    target: SocketAddr,
    capabilities: &NodeCapabilities,
    execution_reachability: Option<ExecutionReachability>,
    auth_key: &str,
    node_health: Option<NodeHealthSnapshot>,
) -> Result<()> {
    let payload = HeartbeatPayload {
        capabilities: capabilities.clone(),
        execution_reachability,
        node_health,
    };

    emit_signed_message(
        socket,
        target,
        capabilities,
        MsgType::Heartbeat,
        serde_json::to_vec(&payload)?,
        auth_key,
    )
    .await
}

/// Emits a chunked capability sync for rich advertisements outside the frequent heartbeat path.
pub async fn emit_capability_sync(
    socket: &UdpSocket,
    target: SocketAddr,
    capabilities: &NodeCapabilities,
    advertisements: &[CapabilityAdvertisement],
    execution_reachability: Option<ExecutionReachability>,
    auth_key: &str,
) -> Result<()> {
    let sync_id = Uuid::new_v4();
    let chunks =
        chunk_advertisements(capabilities, advertisements, execution_reachability.clone())?;
    let chunk_total = chunks.len() as u32;

    for (chunk_index, chunk_advertisements) in chunks.into_iter().enumerate() {
        let payload = CapabilitySyncPayload {
            capabilities: capabilities.clone(),
            execution_reachability: execution_reachability.clone(),
            advertisements: chunk_advertisements,
            sync_id,
            chunk_index: chunk_index as u32,
            chunk_total,
        };

        emit_signed_message(
            socket,
            target,
            capabilities,
            MsgType::CapabilitySync,
            serde_json::to_vec(&payload)?,
            auth_key,
        )
        .await?;
    }

    Ok(())
}

/// Emits a `MeshCatalogSync` to a target, carrying the sender's known peer directory.
/// Used as part of the reconnect handshake so the receiver can fix stale peer addresses.
pub async fn emit_catalog_sync(
    socket: &UdpSocket,
    target: SocketAddr,
    capabilities: &NodeCapabilities,
    peers: Vec<MeshPeerEntry>,
    auth_key: &str,
) -> Result<()> {
    let payload = MeshCatalogSyncPayload { peers };
    emit_signed_message(
        socket,
        target,
        capabilities,
        MsgType::MeshCatalogSync,
        serde_json::to_vec(&payload)?,
        auth_key,
    )
    .await
}

async fn emit_signed_message(
    socket: &UdpSocket,
    target: SocketAddr,
    capabilities: &NodeCapabilities,
    msg_type: MsgType,
    payload_bytes: Vec<u8>,
    auth_key: &str,
) -> Result<()> {
    let msg_id = Uuid::new_v4();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let auth = MeshAuth::new(auth_key);
    let hmac = auth.sign(&msg_id, 0, &payload_bytes, timestamp);

    let msg = BeaconMessage {
        version: 1,
        msg_id,
        src_node: capabilities.node_id.clone(),
        dest_node: "broadcast".to_string(),
        msg_type,
        seq: 0,
        total: 1,
        timestamp,
        payload: payload_bytes.into(),
        hmac: hmac.into(),
    };

    let data = serde_json::to_vec(&msg)?;
    socket.send_to(&data, target).await?;

    Ok(())
}

fn chunk_advertisements(
    capabilities: &NodeCapabilities,
    advertisements: &[CapabilityAdvertisement],
    execution_reachability: Option<ExecutionReachability>,
) -> Result<Vec<Vec<CapabilityAdvertisement>>> {
    if advertisements.is_empty() {
        return Ok(vec![Vec::new()]);
    }

    let mut chunks = Vec::new();
    let mut current = Vec::new();

    for advertisement in advertisements {
        let mut candidate = current.clone();
        candidate.push(advertisement.clone());
        let payload = CapabilitySyncPayload {
            capabilities: capabilities.clone(),
            execution_reachability: execution_reachability.clone(),
            advertisements: candidate.clone(),
            sync_id: Uuid::nil(),
            chunk_index: 0,
            chunk_total: 1,
        };

        if !current.is_empty() && serde_json::to_vec(&payload)?.len() > MAX_SYNC_PAYLOAD_BYTES {
            chunks.push(current);
            current = vec![advertisement.clone()];
        } else {
            current = candidate;
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    Ok(chunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BeaconPayload, NodeConstraints, NodeRole, WireEncoding};
    use std::sync::Mutex;

    /// The beacon wire-encoding mode is process-wide, so tests that pin it
    /// (all wire-size assertions below) must not interleave with each other.
    static ENCODING_GUARD: Mutex<()> = Mutex::new(());

    fn with_wire_encoding<T>(mode: WireEncoding, run: impl FnOnce() -> T) -> T {
        let _guard = ENCODING_GUARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        BeaconPayload::set_wire_encoding(mode);
        let out = run();
        BeaconPayload::set_wire_encoding(WireEncoding::LegacyIntArray);
        out
    }

    fn capabilities() -> NodeCapabilities {
        NodeCapabilities {
            node_id: "aria-node".into(),
            roles: vec![NodeRole::AnsibleNode],
            models: vec![],
            tools: vec![],
            constraints: NodeConstraints {
                max_concurrent_jobs: Some(4),
                latency_hint_ms: Some(12),
                trust_level: None,
            },
        }
    }

    fn advertisement(ix: usize) -> CapabilityAdvertisement {
        CapabilityAdvertisement {
            hotel_id: "aria-architect-hotel".into(),
            node_id: "aria-node".into(),
            incarnation_id: format!("aria-architect-hotel:model-controller-gemini-{ix}"),
            target_role: "model".into(),
            availability_state: "live".into(),
            selection_hint: Some("remote_fallback".into()),
            latency_hint_ms: Some(12),
            max_concurrent_jobs: Some(4),
            active_jobs: 1,
            queue_depth: 0,
        }
    }

    #[test]
    fn heartbeat_payload_round_trips_without_advertisements() {
        let payload = HeartbeatPayload {
            capabilities: capabilities(),
            execution_reachability: Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "aria-vps".into(),
                port: 9002,
            }),
            node_health: Some(NodeHealthSnapshot {
                guest_count: Some(5),
                disk_free_pct: Some(72.5),
                mem_free_pct: Some(61.0),
                load_avg_1m: Some(0.42),
                perimeter: None,
            }),
        };

        let encoded = serde_json::to_vec(&payload).expect("payload should encode");
        let decoded: HeartbeatPayload =
            serde_json::from_slice(&encoded).expect("payload should decode");
        assert_eq!(decoded.capabilities.node_id, "aria-node");
        assert_eq!(
            decoded
                .node_health
                .as_ref()
                .and_then(|value| value.guest_count),
            Some(5)
        );
        assert_eq!(
            decoded
                .execution_reachability
                .as_ref()
                .map(|value| value.host.as_str()),
            Some("aria-vps")
        );
    }

    #[test]
    fn hotel_state_sync_round_trips_model_profiles_and_defaults_old_payloads() {
        let payload = HotelStateSyncPayload {
            node_id: "aria-node".into(),
            hotel_name: "aria".into(),
            guests: Vec::new(),
            agents: Vec::new(),
            role_homes: Vec::new(),
            transport_homes: Vec::new(),
            model_profiles: vec![ModelProfileRecord {
                model_ref: "openrouter".into(),
                node_id: "aria-node".into(),
                provider: "openrouter".into(),
                task_kinds: vec!["text.generate".into()],
                trust_tier: "remote_cloud".into(),
                max_context_tokens: 0,
                latency_p50_ms: 123,
                error_rate: 0.0,
                status: "healthy".into(),
                last_healthy_secs: 1,
                updated_secs: 2,
                ..Default::default()
            }],
        };

        let encoded = serde_json::to_vec(&payload).expect("payload should encode");
        let decoded: HotelStateSyncPayload =
            serde_json::from_slice(&encoded).expect("payload should decode");
        assert_eq!(decoded.model_profiles.len(), 1);
        assert_eq!(decoded.model_profiles[0].provider, "openrouter");

        let legacy = serde_json::json!({
            "node_id": "legacy-node",
            "hotel_name": "legacy",
            "guests": [],
            "agents": []
        });
        let decoded_legacy: HotelStateSyncPayload =
            serde_json::from_value(legacy).expect("legacy payload should decode");
        assert!(decoded_legacy.model_profiles.is_empty());
    }

    #[test]
    fn capability_sync_chunking_splits_large_advertisement_sets() {
        let advertisements = (0..16).map(advertisement).collect::<Vec<_>>();
        let chunks = chunk_advertisements(
            &capabilities(),
            &advertisements,
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "aria-vps".into(),
                port: 9002,
            }),
        )
        .expect("chunking should succeed");

        assert!(chunks.len() > 1);
        let flattened = chunks.into_iter().flatten().collect::<Vec<_>>();
        assert_eq!(flattened.len(), advertisements.len());
    }

    fn model_profile(ix: usize) -> ModelProfileRecord {
        ModelProfileRecord {
            model_ref: format!("openrouter/vendor-{ix}/model-name-{ix}-instruct-v3"),
            node_id: "mbp-jane-aiua-01".into(),
            provider: "openrouter".into(),
            task_kinds: vec!["text.generate".into(), "media.analyze".into()],
            trust_tier: "remote_cloud".into(),
            max_context_tokens: 131_072,
            latency_p50_ms: 850,
            error_rate: 0.01,
            status: "healthy".into(),
            last_healthy_secs: 1_720_000_000,
            updated_secs: 1_720_000_100,
            ..Default::default()
        }
    }

    fn hotel_state_payload(profile_count: usize) -> HotelStateSyncPayload {
        HotelStateSyncPayload {
            node_id: "mbp-jane-aiua-01".into(),
            hotel_name: "mbp-jane".into(),
            role_homes: Vec::new(),
            transport_homes: Vec::new(),
            guests: (0..20)
                .map(|ix| HotelStateSyncGuest {
                    guest_id: format!("mbp-jane:model-controller-openrouter-{ix}"),
                    role: "model".into(),
                    active: true,
                })
                .collect(),
            agents: (0..6)
                .map(|ix| HotelStateSyncAgent {
                    agent_id: format!("agent-persona-{ix}"),
                    persona_name: format!("Persona {ix}"),
                })
                .collect(),
            model_profiles: (0..profile_count).map(model_profile).collect(),
        }
    }

    /// Serialized size of the on-the-wire datagram for a single chunk, matching
    /// how `emit_signed_message` frames the payload inside a `BeaconMessage`.
    fn datagram_len(chunk: &HotelStateSyncPayload) -> usize {
        let payload = serde_json::to_vec(chunk).expect("chunk should encode");
        let msg = BeaconMessage {
            version: 1,
            msg_id: Uuid::nil(),
            src_node: "mbp-jane-aiua-01".into(),
            dest_node: "broadcast".into(),
            msg_type: MsgType::HotelStateSync,
            seq: 0,
            total: 1,
            timestamp: 1_720_000_000,
            payload: payload.into(),
            // Conservative HMAC width; encoded per the active wire mode.
            hmac: vec![0u8; 64].into(),
        };
        serde_json::to_vec(&msg)
            .expect("message should encode")
            .len()
    }

    /// The binding ceiling is macOS's default `net.inet.udp.maxdgram`, not the
    /// theoretical 65507-byte UDP maximum — Mac hotels send with EMSGSIZE above it.
    const UDP_DATAGRAM_MAX: usize = 9_216;

    #[test]
    fn hotel_state_single_datagram_when_small() {
        with_wire_encoding(WireEncoding::Base64, || {
            let payload = hotel_state_payload(1);
            let chunks = chunk_hotel_state_payloads(&payload).expect("chunking should succeed");
            assert_eq!(chunks.len(), 1);
            assert!(datagram_len(&chunks[0]) < UDP_DATAGRAM_MAX);
        });
    }

    /// The realistic 20-guest/6-agent roster plus a handful of model profiles
    /// must fit ONE datagram under the macOS ceiling once senders flip to
    /// base64 — this exact fixture is what overflowed the legacy encoding.
    #[test]
    fn hotel_state_roster_and_profiles_fit_one_base64_datagram() {
        with_wire_encoding(WireEncoding::Base64, || {
            let payload = hotel_state_payload(4);
            let wire = hotel_state_wire_len(&payload).expect("encode");
            assert!(
                wire < UDP_DATAGRAM_MAX,
                "roster + 4 profiles is {wire} wire bytes; must clear {UDP_DATAGRAM_MAX}"
            );
            let chunks = chunk_hotel_state_payloads(&payload).expect("chunking should succeed");
            assert_eq!(chunks.len(), 1);
            assert!(datagram_len(&chunks[0]) < UDP_DATAGRAM_MAX);
        });
    }

    /// Documents why the base64 flip is required at all: in the legacy
    /// int-array encoding the SAME roster fixture (~8.2 KB wire by itself)
    /// leaves no room for even a single model profile under the ceiling —
    /// wire-measured chunking can only drop every profile, so no amount of
    /// chunking recovers profile sync without the encoding change.
    #[test]
    fn hotel_state_legacy_encoding_cannot_carry_profiles_alongside_roster() {
        with_wire_encoding(WireEncoding::LegacyIntArray, || {
            let payload = hotel_state_payload(1);
            let wire = hotel_state_wire_len(&payload).expect("encode");
            assert!(
                wire > MAX_HOTEL_STATE_WIRE_BYTES,
                "roster + one profile should exceed the legacy ceiling (got {wire} bytes)"
            );
            let chunks = chunk_hotel_state_payloads(&payload).expect("chunking should succeed");
            assert_eq!(chunks.len(), 1);
            assert!(
                chunks[0].model_profiles.is_empty(),
                "legacy encoding must be forced to drop the profile to keep the roster sendable"
            );
        });
    }

    #[test]
    fn hotel_state_chunks_stay_under_udp_ceiling_and_preserve_profiles() {
        with_wire_encoding(WireEncoding::Base64, || {
            // A large OpenRouter-style catalog that far exceeds one datagram.
            let payload = hotel_state_payload(600);
            assert!(
                hotel_state_wire_len(&payload).expect("encode") > MAX_HOTEL_STATE_WIRE_BYTES,
                "test fixture must actually require chunking"
            );

            let chunks = chunk_hotel_state_payloads(&payload).expect("chunking should succeed");
            assert!(
                chunks.len() > 1,
                "large catalog must split into multiple chunks"
            );

            // (a) Every emitted datagram fits under the UDP ceiling, with headroom.
            for chunk in &chunks {
                let len = datagram_len(chunk);
                assert!(
                    len < UDP_DATAGRAM_MAX,
                    "datagram {len} bytes exceeds UDP ceiling {UDP_DATAGRAM_MAX}"
                );
            }

            // (b) Guests and agents ride every chunk so each is independently useful.
            for chunk in &chunks {
                assert_eq!(chunk.guests.len(), payload.guests.len());
                assert_eq!(chunk.agents.len(), payload.agents.len());
            }

            // (c) The union of profiles across chunks equals the input, in order —
            //     no profile is lost or duplicated by chunking.
            let flattened: Vec<_> = chunks
                .iter()
                .flat_map(|c| c.model_profiles.iter().map(|p| p.model_ref.clone()))
                .collect();
            let expected: Vec<_> = payload
                .model_profiles
                .iter()
                .map(|p| p.model_ref.clone())
                .collect();
            assert_eq!(flattened, expected);
        });
    }

    #[test]
    fn hotel_state_chunking_skips_profiles_that_can_never_fit() {
        with_wire_encoding(WireEncoding::Base64, || {
            let mut payload = hotel_state_payload(3);
            // Middle profile carries a pathological blob that cannot share a
            // datagram with the roster at any packing.
            payload.model_profiles[1].trust_tier = "x".repeat(MAX_HOTEL_STATE_WIRE_BYTES * 2);

            let chunks = chunk_hotel_state_payloads(&payload).expect("chunking should succeed");

            // Every emitted datagram must still clear the macOS ceiling…
            for chunk in &chunks {
                assert!(datagram_len(chunk) < UDP_DATAGRAM_MAX);
            }
            // …and the oversized profile is skipped while its siblings survive.
            let flattened: Vec<_> = chunks
                .iter()
                .flat_map(|c| c.model_profiles.iter().map(|p| p.model_ref.clone()))
                .collect();
            assert!(!flattened.contains(&payload.model_profiles[1].model_ref));
            assert!(flattened.contains(&payload.model_profiles[0].model_ref));
            assert!(flattened.contains(&payload.model_profiles[2].model_ref));
        });
    }
}
