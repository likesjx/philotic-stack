use crate::authz::{MeshAuth, NonceTracker};
use crate::domain::GraphDomain;
use crate::heartbeat::{CapabilitySyncPayload, HeartbeatPayload};
use crate::registry::NodeRegistry;
use crate::{BeaconMessage, MsgType, NodeCapabilities};
use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{debug, error, info, warn};

/// A lightweight beacon daemon that binds to a UDP port and listens
/// for incoming mesh control messages.
pub struct BeaconDaemon {
    socket: Arc<UdpSocket>,
    graph: Arc<GraphDomain>,
    registry: Arc<RwLock<NodeRegistry>>,
    local_capabilities: NodeCapabilities,
    inbox_tx: mpsc::Sender<BeaconMessage>,
    // Persistent nonce tracker — initialized once to avoid per-packet DB open overhead
    // and WAL contention on the main context.db under concurrent UDP load.
    nonce_tracker: Option<Mutex<NonceTracker>>,
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
        // Derive a sidecar nonces.db path alongside the main context DB.
        // Using a dedicated file avoids WAL write contention with the hotel's main DB
        // on every incoming beacon packet.
        let nonce_tracker = if enable_rust_auth {
            let nonce_path = std::path::Path::new(db_path)
                .parent()
                .map(|p| p.join("nonces.db").to_string_lossy().to_string())
                .unwrap_or_else(|| "nonces.db".to_string());
            match NonceTracker::open(&nonce_path) {
                Ok(t) => Some(Mutex::new(t)),
                Err(e) => {
                    warn!("Failed to initialize beacon nonce tracker (replay protection disabled): {e}");
                    None
                }
            }
        } else {
            None
        };
        Ok(Self {
            socket: Arc::new(socket),
            graph,
            registry,
            local_capabilities,
            inbox_tx,
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

                    if let Some(ref tracker_mutex) = self.nonce_tracker {
                        let tracker = tracker_mutex.lock().await;
                        if let Err(e) = tracker.assert_and_record_nonce(&msg.msg_id) {
                            warn!("Packet dropped: {}", e);
                            return;
                        }
                    } else {
                        warn!(
                            "Packet dropped: nonce tracker unavailable for {}",
                            msg.msg_id
                        );
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

                self.dispatch_message(msg, src).await;
            }
            Err(e) => {
                error!("Failed to decode BeaconMessage from {}: {}", src, e);
            }
        }
    }

    async fn dispatch_message(&self, msg: BeaconMessage, src: SocketAddr) {
        match msg.msg_type {
            MsgType::Heartbeat => {
                if let Ok(payload) = serde_json::from_slice::<HeartbeatPayload>(&msg.payload) {
                    info!(
                        "Received heartbeat from node: {} (roles: {:?})",
                        payload.capabilities.node_id, payload.capabilities.roles
                    );
                    let mut registry = self.registry.write().await;
                    registry.observe_heartbeat(
                        payload.capabilities.clone(),
                        payload.execution_reachability,
                        payload.node_health,
                    );
                    // Reconcile stored mesh_port if the heartbeat arrived from a different port.
                    // This self-heals boot-time port conflicts without manual DB edits.
                    // We do NOT update mesh_host from src.ip() — Tailscale routing makes
                    // the observed source IP unreliable vs. the stored Tailscale address.
                    if let Ok(hotels) = self.graph.list_hotels() {
                        if let Some(mut hotel) = hotels
                            .into_iter()
                            .find(|h| h.capabilities.node_id == payload.capabilities.node_id)
                        {
                            if hotel.mesh_port != src.port() {
                                info!(
                                    "Reconciling mesh_port for {}: {} → {}",
                                    hotel.hotel_name, hotel.mesh_port, src.port()
                                );
                                hotel.mesh_port = src.port();
                                let _ = self.graph.upsert_hotel(&hotel);
                            }
                        }
                    }
                }
                let _ = self.inbox_tx.send(msg).await;
            }
            MsgType::CapabilitySync => {
                if let Ok(payload) = serde_json::from_slice::<CapabilitySyncPayload>(&msg.payload) {
                    let mut registry = self.registry.write().await;
                    registry.observe_capability_sync_chunk(
                        payload.capabilities,
                        payload.execution_reachability,
                        None,
                        payload.sync_id,
                        payload.chunk_index,
                        payload.chunk_total,
                        payload.advertisements,
                    );
                }
                let _ = self.inbox_tx.send(msg).await;
            }
            MsgType::MeshEventBatch
            | MsgType::MeshEventAck
            | MsgType::MeshMembershipAccept
            | MsgType::MeshMembershipSync
            | MsgType::MeshCatalogSync
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::GraphDomain;
    use crate::heartbeat::HeartbeatPayload;
    use crate::registry::NodeRegistry;
    use crate::sqlite_storage::SqliteGraphStorage;
    use crate::storage::HotelRecord;
    use crate::{BeaconMessage, MsgType, NodeCapabilities, NodeConstraints};
    use std::net::SocketAddr;
    use std::sync::Arc;
    use tokio::sync::RwLock;
    use uuid::Uuid;

    fn test_caps(node_id: &str) -> NodeCapabilities {
        NodeCapabilities {
            node_id: node_id.to_string(),
            roles: vec![],
            models: vec![],
            tools: vec![],
            constraints: NodeConstraints::default(),
        }
    }

    fn test_hotel(hotel_name: &str, node_id: &str, mesh_port: u16) -> HotelRecord {
        HotelRecord {
            hotel_name: hotel_name.to_string(),
            capabilities: test_caps(node_id),
            mesh_host: Some("100.79.239.64".to_string()),
            mesh_port,
            blob_port: mesh_port + 1,
            execution_port: mesh_port + 2,
            ipc_socket_path: "/tmp/philotic-aiua.sock".to_string(),
            active_pid: None,
        }
    }

    fn heartbeat_msg(node_id: &str) -> BeaconMessage {
        let payload = HeartbeatPayload {
            capabilities: test_caps(node_id),
            execution_reachability: None,
            node_health: None,
        };
        BeaconMessage {
            version: 1,
            msg_id: Uuid::new_v4(),
            src_node: node_id.to_string(),
            dest_node: "broadcast".to_string(),
            msg_type: MsgType::Heartbeat,
            seq: 0,
            total: 1,
            payload: serde_json::to_vec(&payload).unwrap(),
            timestamp: 0,
            hmac: vec![],
        }
    }

    #[tokio::test]
    async fn heartbeat_reconciles_mesh_port() {
        let storage = SqliteGraphStorage::open_in_memory().unwrap();
        let graph = Arc::new(GraphDomain::new(Arc::new(storage.adapter())));

        // Peer stored at port 9100 (stale, pre-conflict-resolution value)
        graph.upsert_hotel(&test_hotel("mbp-jane", "mbp-jane-aiua-01", 9100)).unwrap();

        let (inbox_tx, _inbox_rx) = tokio::sync::mpsc::channel(8);
        let daemon = BeaconDaemon::bind_with_registry(
            "127.0.0.1:0",
            test_caps("mac-jane-aiua-01"),
            inbox_tx,
            graph.clone(),
            "",
            false,
            Arc::new(RwLock::new(NodeRegistry::new())),
        )
        .await
        .unwrap();

        // Heartbeat arrives from source port 9106 (the actual resolved port)
        let src: SocketAddr = "100.79.239.64:9106".parse().unwrap();
        daemon.dispatch_message(heartbeat_msg("mbp-jane-aiua-01"), src).await;

        let hotel = graph.get_hotel("mbp-jane").unwrap().unwrap();
        assert_eq!(hotel.mesh_port, 9106, "mesh_port should be reconciled to the observed source port");
    }

    #[tokio::test]
    async fn heartbeat_no_write_on_port_match() {
        let storage = SqliteGraphStorage::open_in_memory().unwrap();
        let graph = Arc::new(GraphDomain::new(Arc::new(storage.adapter())));

        graph.upsert_hotel(&test_hotel("mbp-jane", "mbp-jane-aiua-01", 9106)).unwrap();

        let (inbox_tx, _inbox_rx) = tokio::sync::mpsc::channel(8);
        let daemon = BeaconDaemon::bind_with_registry(
            "127.0.0.1:0",
            test_caps("mac-jane-aiua-01"),
            inbox_tx,
            graph.clone(),
            "",
            false,
            Arc::new(RwLock::new(NodeRegistry::new())),
        )
        .await
        .unwrap();

        // Heartbeat arrives from the already-correct port
        let src: SocketAddr = "100.79.239.64:9106".parse().unwrap();
        daemon.dispatch_message(heartbeat_msg("mbp-jane-aiua-01"), src).await;

        let hotel = graph.get_hotel("mbp-jane").unwrap().unwrap();
        assert_eq!(hotel.mesh_port, 9106, "mesh_port should remain unchanged");
    }
}
