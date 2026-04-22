use crate::registry::{CapabilityAdvertisement, ExecutionReachability};
use crate::authz::MeshAuth;
use crate::{BeaconMessage, MsgType, NodeCapabilities};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tokio::net::UdpSocket;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatPayload {
    pub capabilities: NodeCapabilities,
    #[serde(default)]
    pub advertisements: Vec<CapabilityAdvertisement>,
    #[serde(default)]
    pub execution_reachability: Option<ExecutionReachability>,
}

/// Emits heartbeat messages over the given UDP socket to a target address.
pub async fn emit_heartbeat(
    socket: &UdpSocket,
    target: SocketAddr,
    capabilities: &NodeCapabilities,
    advertisements: &[CapabilityAdvertisement],
    execution_reachability: Option<ExecutionReachability>,
    auth_key: &str,
) -> Result<()> {
    let payload = HeartbeatPayload {
        capabilities: capabilities.clone(),
        advertisements: advertisements.to_vec(),
        execution_reachability,
    };

    let msg_id = Uuid::new_v4();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let payload_bytes = serde_json::to_vec(&payload)?;
    let auth = MeshAuth::new(auth_key);
    let hmac = auth.sign(&msg_id, 0, &payload_bytes, timestamp);

    let msg = BeaconMessage {
        version: 1,
        msg_id,
        src_node: capabilities.node_id.clone(),
        dest_node: "broadcast".to_string(), // In MVP 2, this could be a known orchestrator IP or subnet broadcast
        msg_type: MsgType::Heartbeat,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NodeCapabilities, NodeConstraints, NodeRole};

    #[test]
    fn heartbeat_payload_round_trips_with_advertisements() {
        let payload = HeartbeatPayload {
            capabilities: NodeCapabilities {
                node_id: "aria-node".into(),
                roles: vec![NodeRole::AnsibleNode],
                models: vec![],
                tools: vec![],
                constraints: NodeConstraints {
                    max_concurrent_jobs: Some(4),
                    latency_hint_ms: Some(12),
                    trust_level: None,
                },
            },
            advertisements: vec![CapabilityAdvertisement {
                hotel_id: "aria-architect-hotel".into(),
                node_id: "aria-node".into(),
                incarnation_id: "aria-architect-hotel:model-controller-gemini".into(),
                target_role: "model".into(),
                availability_state: "live".into(),
                selection_hint: Some("remote_fallback".into()),
                latency_hint_ms: Some(12),
                max_concurrent_jobs: Some(4),
                active_jobs: 1,
                queue_depth: 0,
            }],
            execution_reachability: Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "aria-vps".into(),
                port: 9002,
            }),
        };

        let encoded = serde_json::to_vec(&payload).expect("payload should encode");
        let decoded: HeartbeatPayload =
            serde_json::from_slice(&encoded).expect("payload should decode");
        assert_eq!(decoded.advertisements.len(), 1);
        assert_eq!(
            decoded.advertisements[0].incarnation_id,
            "aria-architect-hotel:model-controller-gemini"
        );
        assert_eq!(
            decoded
                .execution_reachability
                .as_ref()
                .map(|value| value.host.as_str()),
            Some("aria-vps")
        );
    }
}
