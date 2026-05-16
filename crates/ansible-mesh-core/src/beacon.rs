use crate::authz::{MeshAuth, NonceTracker};
use crate::domain::GraphDomain;
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
    graph: Arc<GraphDomain>,
    registry: Arc<RwLock<NodeRegistry>>,
    local_capabilities: NodeCapabilities,
    inbox_tx: mpsc::Sender<BeaconMessage>,
    nonce_db_path: String,
    enable_rust_auth: bool,
}

impl BeaconDaemon {
    /// Bind the daemon to a specific UDP address (e.g., "0.0.0.0:1234" or a WireGuard IP).
    pub async fn bind(
        addr: &str,
        local_capabilities: NodeCapabilities,
        inbox_tx: mpsc::Sender<BeaconMessage>,
        graph: Arc<GraphDomain>,
        db_path: &str,
        enable_rust_auth: bool,
    ) -> Result<Self> {
        Self::bind_with_registry(
            addr,
            local_capabilities,
            inbox_tx,
            graph,
            db_path,
            enable_rust_auth,
            Arc::new(RwLock::new(NodeRegistry::new())),
        )
        .await
    }

    pub async fn bind_with_registry(
        addr: &str,
        local_capabilities: NodeCapabilities,
        inbox_tx: mpsc::Sender<BeaconMessage>,
        graph: Arc<GraphDomain>,
        db_path: &str,
        enable_rust_auth: bool,
        registry: Arc<RwLock<NodeRegistry>>,
    ) -> Result<Self> {
        let socket = UdpSocket::bind(addr)
            .await
            .context(format!("Failed to bind UDP socket to {}", addr))?;

        info!("Beacon daemon listening on {}", socket.local_addr()?);
        Ok(Self {
            socket: Arc::new(socket),
            graph,
            registry,
            local_capabilities,
            inbox_tx,
            nonce_db_path: db_path.to_string(),
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
                if self.enable_rust_auth && msg.msg_type != MsgType::MeshMembershipAccept {
                    let auth_key = match self.auth_key_for_node(&msg.src_node) {
                        Ok(Some(value)) => value,
                        Ok(None) => {
                            warn!(
                                "Packet dropped: no mesh auth key for node {} type {:?}",
                                msg.src_node, msg.msg_type
                            );
                            return;
                        }
                        Err(e) => {
                            warn!(
                                "Packet dropped: failed to resolve auth key for {}: {}",
                                msg.src_node, e
                            );
                            return;
                        }
                    };
                    let auth = MeshAuth::new(auth_key);
                    if let Err(e) = auth.validate(
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

                    let nonce_tracker = match NonceTracker::open(&self.nonce_db_path) {
                        Ok(tracker) => tracker,
                        Err(e) => {
                            warn!(
                                "Packet dropped: failed to open nonce tracker for {}: {}",
                                msg.msg_id, e
                            );
                            return;
                        }
                    };
                    if let Err(e) = nonce_tracker.assert_and_record_nonce(&msg.msg_id) {
                        warn!("Packet dropped: {}", e);
                        return;
                    }
                } else {
                    debug!(
                        "Bypassing beacon HMAC validation for [{}] type {:?}",
                        msg.msg_id, msg.msg_type
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
                    registry.update_node(
                        payload.capabilities,
                        payload.advertisements,
                        payload.execution_reachability,
                        payload.node_health,
                    );
                }
            }
            MsgType::MeshEventBatch
            | MsgType::MeshEventAck
            | MsgType::MeshMembershipAccept
            | MsgType::WebRtcSignal => {
                let _ = self.inbox_tx.send(msg).await;
            }
            _ => {
                // Placeholder for other routing paths (Agent, Tool, Model)
                debug!("Dispatching message: {:?}", msg.msg_type);
            }
        }
    }

    fn auth_key_for_node(&self, node_id: &str) -> Result<Option<String>> {
        let key = format!("mesh_auth_key:{node_id}");
        Ok(self
            .graph
            .get_config_value(&key)?
            .and_then(|value| serde_json::from_str::<String>(&value).ok().or(Some(value)))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()))
    }
}
