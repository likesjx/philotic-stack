use crate::authz::MeshAuth;
use crate::registry::{CapabilityAdvertisement, ExecutionReachability};
use crate::{BeaconMessage, MsgType, NodeCapabilities, NodeHealthSnapshot};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tokio::net::UdpSocket;
use uuid::Uuid;

const MAX_SYNC_PAYLOAD_BYTES: usize = 900;

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
        payload: payload_bytes,
        hmac,
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
    use crate::{NodeConstraints, NodeRole};

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
}
