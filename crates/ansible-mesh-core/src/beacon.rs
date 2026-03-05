use crate::{BeaconMessage, MsgType, NodeCapabilities};
use crate::heartbeat::HeartbeatPayload;
use crate::registry::NodeRegistry;
use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::RwLock;
use tracing::{error, info, debug};

/// A lightweight beacon daemon that binds to a UDP port and listens
/// for incoming mesh control messages.
pub struct BeaconDaemon {
    socket: Arc<UdpSocket>,
    registry: Arc<RwLock<NodeRegistry>>,
    local_capabilities: NodeCapabilities,
}

impl BeaconDaemon {
    /// Bind the daemon to a specific UDP address (e.g., "0.0.0.0:1234" or a WireGuard IP).
    pub async fn bind(addr: &str, local_capabilities: NodeCapabilities) -> Result<Self> {
        let socket = UdpSocket::bind(addr)
            .await
            .context(format!("Failed to bind UDP socket to {}", addr))?;
        info!("Beacon daemon listening on {}", socket.local_addr()?);
        Ok(Self { 
            socket: Arc::new(socket),
            registry: Arc::new(RwLock::new(NodeRegistry::new())),
            local_capabilities,
        })
    }

    /// Run the daemon loop, receiving UDP packets and decoding them into `BeaconMessage` envelopes.
    pub async fn run_loop(&self) -> Result<()> {
        let mut buf = vec![0u8; 65535]; // Max UDP packet size

        // In a real implementation we would spawn a heartbeat emitter loop here
        // targetting known peers or a broadcast address.

        loop {
            match self.socket.recv_from(&mut buf).await {
                Ok((size, src)) => {
                    self.handle_packet(&buf[..size], src).await;
                }
                Err(e) => {
                    error!("UDP receive error: {}", e);
                }
            }
        }
    }

    async fn handle_packet(&self, data: &[u8], src: SocketAddr) {
        // Decode the outer envelope (assuming CBOR or JSON for MVP).
        // For MVP 1, we will use JSON for simplicity and debuggability.
        match serde_json::from_slice::<BeaconMessage>(data) {
            Ok(msg) => {
                debug!("Received message {} from {} type {:?}", msg.msg_id, src, msg.msg_type);
                self.dispatch_message(msg).await;
            }
            Err(e) => {
                error!("Failed to decode BeaconMessage from {}: {}", src, e);
            }
        }
    }

    async fn dispatch_message(&self, msg: BeaconMessage) {
        match msg.msg_type {
            MsgType::Heartbeat => {
                if let Ok(payload) = serde_json::from_slice::<HeartbeatPayload>(&msg.payload) {
                    info!("Received heartbeat from node: {} (roles: {:?})", payload.capabilities.node_id, payload.capabilities.roles);
                    let mut registry = self.registry.write().await;
                    registry.update_node(payload.capabilities);
                }
            }
            _ => {
                // Placeholder for other routing paths (Agent, Tool, Model)
                debug!("Dispatching message: {:?}", msg.msg_type);
            }
        }
    }
}
