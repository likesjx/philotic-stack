use crate::{BeaconMessage, MsgType, NodeCapabilities};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use tokio::net::UdpSocket;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatPayload {
    pub capabilities: NodeCapabilities,
}

/// Emits heartbeat messages over the given UDP socket to a target address.
pub async fn emit_heartbeat(
    socket: &UdpSocket,
    target: SocketAddr,
    capabilities: &NodeCapabilities,
) -> Result<()> {
    let payload = HeartbeatPayload {
        capabilities: capabilities.clone(),
    };

    let msg = BeaconMessage {
        version: 1,
        msg_id: Uuid::new_v4(),
        src_node: capabilities.node_id.clone(),
        dest_node: "broadcast".to_string(), // In MVP 2, this could be a known orchestrator IP or subnet broadcast
        msg_type: MsgType::Heartbeat,
        seq: 0,
        total: 1,
        payload: serde_json::to_vec(&payload)?,
        hmac: vec![], // MVP 1/2 ignores signature
    };

    let data = serde_json::to_vec(&msg)?;
    socket.send_to(&data, target).await?;

    Ok(())
}
