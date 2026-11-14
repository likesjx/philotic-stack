use crate::runtime::AgentInput;
use crate::{AgentId, BeaconMessage, BeaconPayload, MsgType, NodeId, ToolRef};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::SocketAddr;
use tokio::net::UdpSocket;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallPayload {
    pub tool: ToolRef,
    pub args: Value,
    pub context: Value,
}

/// A client adapter that ZeroClaw (or any orchestrator) uses to bridge
/// local intents to the remote mesh network.
pub struct MeshAdapter {
    local_socket: UdpSocket,
    beacon_addr: SocketAddr,
    client_node_id: NodeId,
}

impl MeshAdapter {
    /// Bind an ephemeral UDP socket and connect it conceptually to the local beacon daemon.
    pub async fn connect(beacon_addr: SocketAddr, client_node_id: NodeId) -> Result<Self> {
        // Bind to any available local port
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        Ok(Self {
            local_socket: socket,
            beacon_addr,
            client_node_id,
        })
    }

    async fn send_message(&self, msg_type: MsgType, payload: Value) -> Result<Uuid> {
        let msg_id = Uuid::new_v4();
        let msg = BeaconMessage {
            version: 1,
            msg_id,
            src_node: self.client_node_id.clone(),
            dest_node: "broadcast".to_string(), // For MVP 2, we just emit to the beacon
            msg_type,
            seq: 0,
            total: 1,
            timestamp: 0,
            payload: serde_json::to_vec(&payload)?.into(),
            hmac: BeaconPayload::default(),
        };

        let data = serde_json::to_vec(&msg)?;
        self.local_socket.send_to(&data, self.beacon_addr).await?;
        Ok(msg_id)
    }

    /// Ask the mesh to run a tool, relying on the beacon's ToolInvoker layer to catch it.
    pub async fn tool_call(&self, tool_ref: ToolRef, args: Value) -> Result<Value> {
        let payload = serde_json::to_value(ToolCallPayload {
            tool: tool_ref.clone(),
            args: args.clone(),
            context: serde_json::json!({
                "session_id": "ephemeral-client-session"
            }),
        })?;

        let _msg_id = self.send_message(MsgType::ToolCall, payload).await?;

        // For MVP 2, we simulate async wait/response routing
        // In a real implementation we would wait on the UDP socket for the `Result` msg_type matching `msg_id`
        Ok(serde_json::json!({
            "status": "dispatched",
            "message": format!("Dispatched tool call {} to mesh", tool_ref)
        }))
    }

    /// Deploy an agent bundle to the mesh for remote execution
    pub async fn run_agent(&self, agent_id: AgentId, input: AgentInput) -> Result<Value> {
        // Converts AgentInput to JSON and sends a RunAgent packet
        let payload = serde_json::json!({
            "agent_id": agent_id,
            "input": {
                "message": input.message,
                "session_id": input.session_id,
                "context": input.context
            }
        });

        let _msg_id = self.send_message(MsgType::RunAgent, payload).await?;
        Ok(serde_json::json!({"status": "agent_execution_started"}))
    }
}
