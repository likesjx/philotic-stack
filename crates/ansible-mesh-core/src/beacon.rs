use crate::authz::{MeshAuth, NonceTracker};
use crate::heartbeat::HeartbeatPayload;
use crate::registry::NodeRegistry;
use crate::{BeaconMessage, MsgType, NodeCapabilities};
use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

/// A lightweight beacon daemon that binds to a UDP port and listens
/// for incoming mesh control messages.
pub struct BeaconDaemon {
    socket: Arc<UdpSocket>,
    registry: Arc<RwLock<NodeRegistry>>,
    local_capabilities: NodeCapabilities,
    inbox_tx: mpsc::Sender<BeaconMessage>,
    auth: Arc<MeshAuth>,
    nonce_tracker: Arc<NonceTracker>,
    enable_rust_auth: bool,
}

impl BeaconDaemon {
    /// Bind the daemon to a specific UDP address (e.g., "0.0.0.0:1234" or a WireGuard IP).
    pub async fn bind(
        addr: &str,
        local_capabilities: NodeCapabilities,
        inbox_tx: mpsc::Sender<BeaconMessage>,
        psk: &str,
        db_path: &str,
        enable_rust_auth: bool,
    ) -> Result<Self> {
        let socket = UdpSocket::bind(addr)
            .await
            .context(format!("Failed to bind UDP socket to {}", addr))?;

        let auth = Arc::new(MeshAuth::new(psk));
        let nonce_tracker = Arc::new(NonceTracker::open(db_path)?);

        info!("Beacon daemon listening on {}", socket.local_addr()?);
        Ok(Self {
            socket: Arc::new(socket),
            registry: Arc::new(RwLock::new(NodeRegistry::new())),
            local_capabilities,
            inbox_tx,
            auth,
            nonce_tracker,
            enable_rust_auth,
        })
    }
    pub fn socket(&self) -> Arc<UdpSocket> {
        self.socket.clone()
    }
    pub fn registry(&self) -> Arc<RwLock<NodeRegistry>> {
        self.registry.clone()
    }
    pub fn inbox_tx(&self) -> mpsc::Sender<BeaconMessage> {
        self.inbox_tx.clone()
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
                debug!(
                    "Received message {} from {} type {:?}",
                    msg.msg_id, src, msg.msg_type
                );

                // 1. Time-Window & HMAC Cryptographic Validation
                if self.enable_rust_auth {
                    if let Err(e) = self.auth.validate(
                        &msg.msg_id,
                        msg.seq as u64,
                        &msg.payload,
                        msg.timestamp,
                        &msg.hmac,
                    ) {
                        warn!(
                            "Packet dropped: Auth validation failed for {} from {}: {}",
                            msg.msg_id, src, e
                        );
                        return;
                    }

                    // 2. Replay Guard Nonce Check
                    if let Err(e) = self.nonce_tracker.assert_and_record_nonce(&msg.msg_id) {
                        warn!("Packet dropped: {}", e);
                        return;
                    }
                } else {
                    debug!(
                        "Rust Auth is disabled. Bypassing HMAC and Replay Guard for [{}]",
                        msg.msg_id
                    );
                }

                // Discard messages from ourselves
                if msg.src_node == self.local_capabilities.node_id {
                    return;
                }

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
                    info!(
                        "Received heartbeat from node: {} (roles: {:?})",
                        payload.capabilities.node_id, payload.capabilities.roles
                    );
                    let mut registry = self.registry.write().await;
                    registry.update_node(payload.capabilities, payload.advertisements);
                }
            }
            MsgType::MeshEventBatch | MsgType::MeshEventAck | MsgType::WebRtcSignal => {
                let _ = self.inbox_tx.send(msg).await;
            }
            _ => {
                // Placeholder for other routing paths (Agent, Tool, Model)
                debug!("Dispatching message: {:?}", msg.msg_type);
            }
        }
    }
}
