use crate::authz::{MeshAuth, NonceTracker};
use crate::domain::GraphDomain;
use crate::heartbeat::{
    emit_catalog_sync, emit_heartbeat, emit_hotel_state_sync, CapabilitySyncPayload,
    HeartbeatPayload, HotelStateSyncPayload, MeshCatalogSyncPayload, MeshPeerEntry,
};
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
/// A beacon packet that takes longer than this to handle is logged: the
/// receive loop is a single task, so every peer's traffic waits behind it.
const SLOW_PACKET_WARN: std::time::Duration = std::time::Duration::from_millis(250);

/// Does the node id a payload names equal the peer whose key authenticated
/// the packet?
///
/// The HMAC proves who SENT a packet, but heartbeats, capability syncs and
/// hotel-state syncs each carry the node they describe INSIDE the payload and
/// every handler acted on that value. Any enrolled peer could therefore make
/// every hotel believe it hosts an agent (roster injection), rewrite another
/// hotel's stored `mesh_port` to its own source port, or feed placement
/// gossip for another node — the same class DEF-170 closed on the event
/// plane, still open here (DEF-183).
fn claimed_node_is_sender(claimed_node_id: &str, msg: &BeaconMessage) -> bool {
    if claimed_node_id == msg.src_node {
        return true;
    }
    warn!(
        claimed = claimed_node_id,
        authenticated_sender = %msg.src_node,
        msg_type = ?msg.msg_type,
        "Packet dropped: its payload describes a node other than the peer that sent it (DEF-183)"
    );
    false
}

pub struct BeaconDaemon {
    socket: Arc<UdpSocket>,
    graph: Arc<GraphDomain>,
    registry: Arc<RwLock<NodeRegistry>>,
    local_capabilities: NodeCapabilities,
    inbox_tx: mpsc::Sender<BeaconMessage>,
    // Persistent nonce tracker — initialized once to avoid per-packet DB open overhead
    // and WAL contention on the main context.db under concurrent UDP load.
    nonce_tracker: NonceTracker,
    enable_rust_auth: bool,
    /// Where newly applied gossiped placement records are reported so the
    /// hotel can push them to local guests at once (DEF-107). `None` = no
    /// subscriber; records are still applied to the graph.
    placement_change_tx: Option<mpsc::UnboundedSender<crate::placement_sync::PlacementChange>>,
    /// Shared snapshot of this hotel's current guest+agent roster. Written by aiua
    /// whenever the roster changes; read by the beacon to include in anchor handshakes.
    pub local_hotel_state: Arc<RwLock<Option<HotelStateSyncPayload>>>,
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
        // The replay window is held in memory. The sidecar nonces.db it replaced
        // grew without bound (274-411 MB per hotel, sweep never called), so
        // retire it.
        let nonce_tracker = NonceTracker::new();
        let legacy_sidecar = std::path::Path::new(db_path)
            .parent()
            .map(|p| p.join("nonces.db").to_string_lossy().to_string())
            .unwrap_or_else(|| "nonces.db".to_string());
        if let Some(what) = NonceTracker::retire_legacy_store(&legacy_sidecar) {
            info!("{what}");
        }
        Ok(Self {
            socket: Arc::new(socket),
            graph,
            registry,
            local_capabilities,
            inbox_tx,
            nonce_tracker,
            enable_rust_auth,
            placement_change_tx: None,
            local_hotel_state: Arc::new(RwLock::new(None)),
        })
    }
    /// Report newly applied gossiped placement records (role homes,
    /// transport homes) on `tx` so the hotel can push them to local guests.
    pub fn with_placement_change_tx(
        mut self,
        tx: Option<mpsc::UnboundedSender<crate::placement_sync::PlacementChange>>,
    ) -> Self {
        self.placement_change_tx = tx;
        self
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
                    let started = std::time::Instant::now();
                    self.handle_packet(&buf[..size], src).await;
                    let took = started.elapsed();
                    if took >= SLOW_PACKET_WARN {
                        warn!(
                            took_ms = took.as_millis() as u64,
                            bytes = size,
                            %src,
                            "slow beacon packet: the single receive loop was busy this long (DEF-191)"
                        );
                    }
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

                    if let Err(e) = self.nonce_tracker.assert_and_record_nonce(&msg.msg_id) {
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
                    if !claimed_node_is_sender(&payload.capabilities.node_id, &msg) {
                        return;
                    }
                    info!(
                        "Received heartbeat from node: {} (roles: {:?})",
                        payload.capabilities.node_id, payload.capabilities.roles
                    );
                    let peer_node_id = payload.capabilities.node_id.clone();

                    // Check before updating so we can detect reconnects (stale → fresh).
                    let was_stale = {
                        let registry = self.registry.read().await;
                        registry.is_node_stale(&peer_node_id)
                    };

                    let mut registry = self.registry.write().await;
                    registry.observe_heartbeat(
                        payload.capabilities.clone(),
                        payload.execution_reachability,
                        payload.node_health,
                    );
                    drop(registry);

                    // Reconcile stored mesh_port if the heartbeat arrived from a different port.
                    // This self-heals boot-time port conflicts without manual DB edits.
                    // We do NOT update mesh_host from src.ip() — Tailscale routing makes
                    // the observed source IP unreliable vs. the stored Tailscale address.
                    let mut port_changed = false;
                    if let Ok(hotels) = self.graph.list_hotels() {
                        if let Some(mut hotel) = hotels
                            .into_iter()
                            .find(|h| h.capabilities.node_id == peer_node_id)
                        {
                            if hotel.mesh_port != src.port() {
                                info!(
                                    "Reconciling mesh_port for {}: {} → {}",
                                    hotel.hotel_name,
                                    hotel.mesh_port,
                                    src.port()
                                );
                                hotel.mesh_port = src.port();
                                let _ = self.graph.upsert_hotel(&hotel);
                                port_changed = true;
                            }
                        }
                    }

                    // Reply with a heartbeat + peer catalog when a peer reconnects (was stale or
                    // changed port). The heartbeat gives the reconnecting node our current address;
                    // the catalog lets it fix stale entries for ALL other peers in one shot.
                    // This is the anchor handshake: even if mac-jane doesn't know mbp-jane's new
                    // port, it can learn it from vps-jane's catalog the moment it re-announces.
                    if port_changed || was_stale {
                        if let Ok(Some(auth_key)) = self.auth_key_for_node(&peer_node_id) {
                            if let Err(e) = emit_heartbeat(
                                &self.socket,
                                src,
                                &self.local_capabilities,
                                None,
                                &auth_key,
                                None,
                            )
                            .await
                            {
                                warn!("Failed to send reconnect reply heartbeat to {}: {}", src, e);
                            } else {
                                info!(
                                    "Sent reconnect reply heartbeat to {} at {}",
                                    peer_node_id, src
                                );
                            }

                            // Send our peer directory so the reconnecting node can update any
                            // stale peer records in a single round-trip (anchor handshake).
                            if let Ok(hotels) = self.graph.list_hotels() {
                                let peers: Vec<MeshPeerEntry> = hotels
                                    .into_iter()
                                    .filter(|h| {
                                        h.capabilities.node_id != self.local_capabilities.node_id
                                            && h.capabilities.node_id != peer_node_id
                                    })
                                    .map(|h| MeshPeerEntry {
                                        node_id: h.capabilities.node_id.clone(),
                                        hotel_name: h.hotel_name.clone(),
                                        mesh_host: h.mesh_host.clone(),
                                        mesh_port: h.mesh_port,
                                    })
                                    .collect();
                                if !peers.is_empty() {
                                    if let Err(e) = emit_catalog_sync(
                                        &self.socket,
                                        src,
                                        &self.local_capabilities,
                                        peers,
                                        &auth_key,
                                    )
                                    .await
                                    {
                                        warn!(
                                            "Failed to send reconnect catalog sync to {}: {}",
                                            src, e
                                        );
                                    }
                                }
                            }

                            // Send our hotel roster so the reconnecting peer can immediately
                            // route to our agents without waiting for the next HotelStateSync.
                            if let Some(state) = self.local_hotel_state.read().await.clone() {
                                if let Err(e) = emit_hotel_state_sync(
                                    &self.socket,
                                    src,
                                    &self.local_capabilities,
                                    state,
                                    &auth_key,
                                )
                                .await
                                {
                                    warn!(
                                        "Failed to send hotel state sync to {} at {}: {}",
                                        peer_node_id, src, e
                                    );
                                }
                            }
                        }
                    }
                }
                let _ = self.inbox_tx.send(msg).await;
            }
            MsgType::CapabilitySync => {
                if let Ok(payload) = serde_json::from_slice::<CapabilitySyncPayload>(&msg.payload) {
                    if !claimed_node_is_sender(&payload.capabilities.node_id, &msg) {
                        return;
                    }
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
            MsgType::MeshCatalogSync => {
                // Anchor handshake reply: update stale peer records from the sender's directory.
                // Only updates mesh_port (port changes are the main drift vector). mesh_host is
                // only filled in if empty — we trust stored Tailscale IPs over relayed values.
                if let Ok(payload) = serde_json::from_slice::<MeshCatalogSyncPayload>(&msg.payload)
                {
                    if let Ok(hotels) = self.graph.list_hotels() {
                        for peer in &payload.peers {
                            if peer.node_id == self.local_capabilities.node_id {
                                continue;
                            }
                            if let Some(mut hotel) = hotels
                                .iter()
                                .find(|h| h.capabilities.node_id == peer.node_id)
                                .cloned()
                            {
                                let mut changed = false;
                                if hotel.mesh_port != peer.mesh_port {
                                    info!(
                                        "Catalog sync from {}: updating mesh_port for {} {} → {}",
                                        msg.src_node,
                                        hotel.hotel_name,
                                        hotel.mesh_port,
                                        peer.mesh_port
                                    );
                                    hotel.mesh_port = peer.mesh_port;
                                    changed = true;
                                }
                                if hotel.mesh_host.as_deref().unwrap_or("").is_empty() {
                                    if let Some(ref host) = peer.mesh_host {
                                        if !host.is_empty() {
                                            info!(
                                                "Catalog sync from {}: setting mesh_host for {} to {}",
                                                msg.src_node, hotel.hotel_name, host
                                            );
                                            hotel.mesh_host = Some(host.clone());
                                            changed = true;
                                        }
                                    }
                                }
                                if changed {
                                    let _ = self.graph.upsert_hotel(&hotel);
                                }
                            }
                        }
                    }
                }
                let _ = self.inbox_tx.send(msg).await;
            }
            MsgType::HotelStateSync => {
                if let Ok(payload) = serde_json::from_slice::<HotelStateSyncPayload>(&msg.payload) {
                    if !claimed_node_is_sender(&payload.node_id, &msg) {
                        return;
                    }
                    if payload.node_id != self.local_capabilities.node_id {
                        let t0 = std::time::Instant::now();
                        // Everything that touches SQLite happens BEFORE the
                        // registry lock is taken. It used to run inside it, so
                        // heartbeats and message routing — every registry
                        // reader — queued behind synchronous graph writes.
                        let mut replicated_profiles = 0usize;
                        for profile in payload
                            .model_profiles
                            .iter()
                            .filter(|profile| profile.node_id == payload.node_id)
                        {
                            if let Err(err) = self.graph.upsert_model_profile(profile) {
                                warn!(
                                    "Hotel state sync from {}: failed to upsert model profile {}@{}: {}",
                                    payload.node_id, profile.model_ref, profile.node_id, err
                                );
                            } else {
                                replicated_profiles += 1;
                            }
                        }
                        let profiles_ms = t0.elapsed().as_millis() as u64;
                        // Placement is graph truth every hotel must agree on
                        // (DEF-107): apply newer role/transport homes, LWW.
                        let t1 = std::time::Instant::now();
                        let applied = crate::placement_sync::apply_remote_placement(
                            &self.graph,
                            &payload.node_id,
                            &payload.role_homes,
                            &payload.transport_homes,
                        );
                        let placement_ms = t1.elapsed().as_millis() as u64;
                        if !applied.is_empty() {
                            info!(
                                "Hotel state sync from {}: applied {} role home(s), {} transport home(s)",
                                payload.node_id,
                                applied.role_homes.len(),
                                applied.transport_homes.len()
                            );
                            if let Some(tx) = &self.placement_change_tx {
                                for change in applied.into_changes() {
                                    let _ = tx.send(change);
                                }
                            }
                        }
                        let (guest_count, agent_count) =
                            (payload.guests.len(), payload.agents.len());
                        let (node_id, hotel_name) =
                            (payload.node_id.clone(), payload.hotel_name.clone());
                        let t2 = std::time::Instant::now();
                        let mut registry = self.registry.write().await;
                        let lock_wait_ms = t2.elapsed().as_millis() as u64;
                        registry.observe_hotel_state(
                            payload.node_id,
                            payload.hotel_name,
                            payload.guests,
                            payload.agents,
                        );
                        drop(registry);
                        info!(
                            "Hotel state sync from {} ({}): {} guests, {} agents, {} model profiles",
                            hotel_name, node_id, guest_count, agent_count, replicated_profiles,
                        );
                        let total_ms = t0.elapsed().as_millis() as u64;
                        if total_ms >= SLOW_PACKET_WARN.as_millis() as u64 {
                            warn!(
                                total_ms,
                                profiles_ms,
                                placement_ms,
                                registry_lock_wait_ms = lock_wait_ms,
                                "slow hotel-state sync from {node_id} (DEF-191)"
                            );
                        }
                    }
                }
            }
            MsgType::MeshEventBatch
            | MsgType::MeshEventAck
            | MsgType::MeshMembershipAccept
            | MsgType::MeshMembershipSync
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
            build_version: String::new(),
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
            payload: serde_json::to_vec(&payload).unwrap().into(),
            timestamp: 0,
            hmac: crate::BeaconPayload::default(),
        }
    }

    #[tokio::test]
    async fn heartbeat_reconciles_mesh_port() {
        let storage = SqliteGraphStorage::open_in_memory().unwrap();
        let graph = Arc::new(GraphDomain::new(Arc::new(storage.adapter())));

        // Peer stored at port 9100 (stale, pre-conflict-resolution value)
        graph
            .upsert_hotel(&test_hotel("mbp-jane", "mbp-jane-aiua-01", 9100))
            .unwrap();

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
        daemon
            .dispatch_message(heartbeat_msg("mbp-jane-aiua-01"), src)
            .await;

        let hotel = graph.get_hotel("mbp-jane").unwrap().unwrap();
        assert_eq!(
            hotel.mesh_port, 9106,
            "mesh_port should be reconciled to the observed source port"
        );
    }

    #[tokio::test]
    async fn heartbeat_no_write_on_port_match() {
        let storage = SqliteGraphStorage::open_in_memory().unwrap();
        let graph = Arc::new(GraphDomain::new(Arc::new(storage.adapter())));

        graph
            .upsert_hotel(&test_hotel("mbp-jane", "mbp-jane-aiua-01", 9106))
            .unwrap();

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
        daemon
            .dispatch_message(heartbeat_msg("mbp-jane-aiua-01"), src)
            .await;

        let hotel = graph.get_hotel("mbp-jane").unwrap().unwrap();
        assert_eq!(hotel.mesh_port, 9106, "mesh_port should remain unchanged");
    }

    #[tokio::test]
    async fn heartbeat_stale_detection_returns_true_for_new_node() {
        let storage = SqliteGraphStorage::open_in_memory().unwrap();
        let graph = Arc::new(GraphDomain::new(Arc::new(storage.adapter())));

        let (inbox_tx, _inbox_rx) = tokio::sync::mpsc::channel(8);
        let registry = Arc::new(RwLock::new(NodeRegistry::new()));
        let daemon = BeaconDaemon::bind_with_registry(
            "127.0.0.1:0",
            test_caps("mac-jane-aiua-01"),
            inbox_tx,
            graph.clone(),
            "",
            false,
            registry.clone(),
        )
        .await
        .unwrap();

        // Before any heartbeat, the node is stale (unknown).
        assert!(
            registry.read().await.is_node_stale("mbp-jane-aiua-01"),
            "unknown node should be stale"
        );

        let src: SocketAddr = "100.79.239.64:9106".parse().unwrap();
        daemon
            .dispatch_message(heartbeat_msg("mbp-jane-aiua-01"), src)
            .await;

        // After observing a fresh heartbeat, the node is no longer stale.
        assert!(
            !registry.read().await.is_node_stale("mbp-jane-aiua-01"),
            "node should be fresh after heartbeat"
        );
    }

    async fn daemon_with_registry(
        graph: Arc<GraphDomain>,
    ) -> (BeaconDaemon, Arc<RwLock<NodeRegistry>>) {
        let registry = Arc::new(RwLock::new(NodeRegistry::new()));
        let (inbox_tx, _inbox_rx) = tokio::sync::mpsc::channel(8);
        let daemon = BeaconDaemon::bind_with_registry(
            "127.0.0.1:0",
            test_caps("mac-jane-aiua-01"),
            inbox_tx,
            graph,
            "",
            false,
            registry.clone(),
        )
        .await
        .unwrap();
        (daemon, registry)
    }

    /// DEF-183: a heartbeat sent by mbp-jane that CLAIMS to describe vps-jane
    /// must not create vps-jane's registry entry or move its stored port.
    #[tokio::test]
    async fn a_heartbeat_describing_another_node_is_dropped() {
        let storage = SqliteGraphStorage::open_in_memory().unwrap();
        let graph = Arc::new(GraphDomain::new(Arc::new(storage.adapter())));
        graph
            .upsert_hotel(&test_hotel("vps-jane", "vps-jane-aiua-01", 9200))
            .unwrap();
        let (daemon, registry) = daemon_with_registry(graph.clone()).await;

        let mut forged = heartbeat_msg("vps-jane-aiua-01");
        forged.src_node = "mbp-jane-aiua-01".to_string();
        let attacker_src: SocketAddr = "100.79.239.64:6666".parse().unwrap();
        daemon.dispatch_message(forged, attacker_src).await;

        assert!(
            registry.read().await.get_node("vps-jane-aiua-01").is_none(),
            "a forged heartbeat must not create the described node"
        );
        assert_eq!(
            graph.get_hotel("vps-jane").unwrap().unwrap().mesh_port,
            9200,
            "and must not rewrite its stored port to the sender's"
        );
    }

    #[tokio::test]
    async fn a_roster_describing_another_node_is_dropped() {
        let storage = SqliteGraphStorage::open_in_memory().unwrap();
        let graph = Arc::new(GraphDomain::new(Arc::new(storage.adapter())));
        let (daemon, registry) = daemon_with_registry(graph).await;

        let payload = HotelStateSyncPayload {
            node_id: "vps-jane-aiua-01".into(),
            hotel_name: "vps-jane".into(),
            guests: vec![],
            agents: vec![crate::heartbeat::HotelStateSyncAgent {
                agent_id: "agent-beacon".into(),
                persona_name: "Beacon".into(),
            }],
            model_profiles: vec![],
            role_homes: vec![],
            transport_homes: vec![],
        };
        let mut msg = heartbeat_msg("mbp-jane-aiua-01");
        msg.msg_type = MsgType::HotelStateSync;
        msg.payload = serde_json::to_vec(&payload).unwrap().into();
        daemon
            .dispatch_message(msg.clone(), "100.79.239.64:1".parse().unwrap())
            .await;
        assert_eq!(
            registry.read().await.remote_hotel_states().count(),
            0,
            "mbp-jane may not inject a roster for vps-jane"
        );

        // The same roster from the hotel it describes is accepted.
        msg.src_node = "vps-jane-aiua-01".to_string();
        daemon
            .dispatch_message(msg, "100.64.212.8:1".parse().unwrap())
            .await;
        assert_eq!(registry.read().await.remote_hotel_states().count(), 1);
    }

    #[tokio::test]
    async fn a_capability_sync_describing_another_node_is_dropped() {
        let storage = SqliteGraphStorage::open_in_memory().unwrap();
        let graph = Arc::new(GraphDomain::new(Arc::new(storage.adapter())));
        let (daemon, registry) = daemon_with_registry(graph).await;

        let payload = CapabilitySyncPayload {
            capabilities: test_caps("vps-jane-aiua-01"),
            execution_reachability: None,
            advertisements: vec![],
            sync_id: Uuid::new_v4(),
            chunk_index: 0,
            chunk_total: 1,
        };
        let mut msg = heartbeat_msg("mbp-jane-aiua-01");
        msg.msg_type = MsgType::CapabilitySync;
        msg.payload = serde_json::to_vec(&payload).unwrap().into();
        daemon
            .dispatch_message(msg.clone(), "100.79.239.64:1".parse().unwrap())
            .await;
        assert!(registry.read().await.get_node("vps-jane-aiua-01").is_none());
    }
}
