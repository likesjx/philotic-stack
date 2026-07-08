//! Mesh runtime activation — beacon bind, outbound dispatcher, execution-plane
//! server, heartbeat + capability-sync loops, and the mesh inbound loop.
//!
//! Extracted verbatim from `main.rs`; no behavior change.

use ansible_mesh_core::NodeCapabilities;
use ansible_mesh_core::beacon::BeaconDaemon;
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::event::EventEnvelope;
use ansible_mesh_core::heartbeat::{
    CapabilitySyncPayload, HeartbeatPayload, emit_capability_sync, emit_heartbeat,
};
use ansible_mesh_core::registry::NodeRegistry;
use ansible_mesh_core::storage::{CursorStorage, EventStorage, HotelRecord};
use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock, mpsc};
use tracing::{debug, error, info, warn};

use crate::service::ipc::IpcServer;
use crate::{
    LedgerCommand, capability_sync_fingerprint, execution_reachability_for_hotel,
    handle_cron_fired_broadcast, handle_cron_job_sync, handle_mesh_membership_accept,
    handle_projected_user_identity_sync, local_capability_advertisements, mesh_auth_key_for_node,
    mesh_target_addr_for_node, mesh_targets_for_graph, reconcile_peer_execution_reachability,
    sample_node_health,
};

type BeaconInboxReceiver = Arc<Mutex<Option<mpsc::Receiver<ansible_mesh_core::BeaconMessage>>>>;
type WebRtcSignalReceiver =
    Arc<Mutex<Option<mpsc::Receiver<ansible_mesh_core::webrtc::WebRtcSignalMessage>>>>;

#[derive(Clone)]
pub(crate) struct MeshRuntimeContext {
    pub(crate) hotel_name: String,
    pub(crate) hotel: HotelRecord,
    pub(crate) caps: NodeCapabilities,
    pub(crate) mesh_addr: String,
    pub(crate) execution_addr: String,
    pub(crate) db_path: String,
    pub(crate) enable_rust_auth: bool,
    pub(crate) enable_rust_dispatcher: bool,
    pub(crate) graph_domain: Arc<GraphDomain>,
    pub(crate) registry: Arc<RwLock<NodeRegistry>>,
    pub(crate) ledger: Arc<dyn EventStorage>,
    pub(crate) tracker: Arc<dyn CursorStorage>,
    pub(crate) dispatcher_tx: mpsc::Sender<LedgerCommand>,
    pub(crate) ipc_inboxes: crate::service::ipc::InboxRegistry,
    pub(crate) ipc_parked_inbound: crate::service::ipc::ParkedInboundRegistry,
    /// Hotel-wide single-delivery claim set shared between `CronTicker::fire` and the
    /// mesh inbound consumer — gives every `TaskInvoke` exactly one delivery owner.
    pub(crate) ipc_delivery_claims: crate::service::ipc::DeliveryClaimRegistry,
    pub(crate) ipc_materialization_requester:
        Option<std::sync::Arc<dyn crate::service::guest_manager::GuestMaterializationRequester>>,
    pub(crate) shutdown_tx: tokio::sync::broadcast::Sender<()>,
    pub(crate) inbox_tx: mpsc::Sender<ansible_mesh_core::BeaconMessage>,
    pub(crate) inbox_rx: BeaconInboxReceiver,
    pub(crate) webrtc_signal_tx: mpsc::Sender<ansible_mesh_core::webrtc::WebRtcSignalMessage>,
    pub(crate) webrtc_signal_rx: WebRtcSignalReceiver,
    pub(crate) perimeter_svc: Arc<crate::service::perimeter::HotelPerimeterService>,
    pub(crate) ipc_operator_surface_tx: Option<mpsc::Sender<String>>,
    /// Shared hotel roster snapshot used by BeaconDaemon for anchor handshakes.
    pub(crate) local_hotel_state:
        Arc<RwLock<Option<ansible_mesh_core::heartbeat::HotelStateSyncPayload>>>,
}

pub(crate) async fn activate_mesh_runtime(ctx: MeshRuntimeContext) -> Result<()> {
    let daemon = Arc::new(
        BeaconDaemon::bind_with_registry(
            &ctx.mesh_addr,
            ctx.caps.clone(),
            ctx.inbox_tx.clone(),
            ctx.graph_domain.clone(),
            &ctx.db_path,
            ctx.enable_rust_auth,
            ctx.registry.clone(),
        )
        .await?,
    );
    *daemon.local_hotel_state.write().await = ctx.local_hotel_state.read().await.clone();
    // Keep beacon's snapshot in sync: whenever the shared Arc is updated, propagate here.
    {
        let beacon_state = daemon.local_hotel_state.clone();
        let source_state = ctx.local_hotel_state.clone();
        let mut shutdown_rx = ctx.shutdown_tx.subscribe();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let snap = source_state.read().await.clone();
                        *beacon_state.write().await = snap;
                    }
                    _ = shutdown_rx.recv() => break,
                }
            }
        });
    }

    // Proactive hotel-state broadcast: push our roster to all backbone peers every 30s
    // so they can route to our guests without waiting for an anchor handshake.
    {
        use ansible_mesh_core::heartbeat::emit_hotel_state_sync;
        let hs_socket = daemon.socket();
        let hs_state = ctx.local_hotel_state.clone();
        let hs_graph = ctx.graph_domain.clone();
        let hs_caps = ctx.caps.clone();
        let mut hs_shutdown = ctx.shutdown_tx.subscribe();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let Some(payload) = hs_state.read().await.clone() else { continue };
                        let targets = match mesh_targets_for_graph(hs_graph.as_ref(), &hs_caps.node_id) {
                            Ok(t) => t,
                            Err(e) => { warn!("hotel-state broadcast: failed to resolve targets: {e}"); continue }
                        };
                        for (target_node_id, target_addr) in targets {
                            let Ok(target) = target_addr.parse::<SocketAddr>() else { continue };
                            let Some(auth_key) = mesh_auth_key_for_node(hs_graph.as_ref(), &target_node_id).ok().flatten() else { continue };
                            if let Err(e) = emit_hotel_state_sync(&hs_socket, target, &hs_caps, payload.clone(), &auth_key).await {
                                warn!("hotel-state broadcast to {}: {e}", target_addr);
                            }
                        }
                    }
                    _ = hs_shutdown.recv() => break,
                }
            }
        });
    }

    let mut inbox_rx = {
        let mut guard = ctx.inbox_rx.lock().await;
        guard.take()
    }
    .context("mesh runtime activation missing beacon inbox receiver")?;

    let mut webrtc_signal_rx = {
        let mut guard = ctx.webrtc_signal_rx.lock().await;
        guard.take()
    }
    .context("mesh runtime activation missing WebRTC signal receiver")?;

    info!(
        hotel = %ctx.hotel_name,
        "Mesh transport antenna extended"
    );

    if ctx.enable_rust_dispatcher {
        tokio::spawn(crate::service::mesh_dispatcher::outbound_dispatcher(
            ctx.ledger.clone(),
            ctx.tracker.clone(),
            daemon.socket(),
            ctx.graph_domain.clone(),
            ctx.registry.clone(),
            ctx.caps.node_id.clone(),
            ctx.shutdown_tx.subscribe(),
        ));
    }

    {
        let execution_inbox_tx = daemon.inbox_tx();
        let execution_caps = ctx.caps.clone();
        let execution_db_path = ctx.db_path.clone();
        let execution_graph = ctx.graph_domain.clone();
        let execution_addr = ctx.execution_addr.clone();
        let execution_enable_rust_auth = ctx.enable_rust_auth;
        tokio::spawn(async move {
            if let Err(e) = crate::service::execution_transport::serve_execution_plane(
                &execution_addr,
                execution_caps,
                execution_inbox_tx,
                execution_graph,
                &execution_db_path,
                execution_enable_rust_auth,
            )
            .await
            {
                error!("Hotel execution transport failed: {}", e);
            }
        });
    }

    {
        let heartbeat_socket = daemon.socket();
        let heartbeat_graph = ctx.graph_domain.clone();
        let heartbeat_hotel = ctx.hotel.clone();
        let heartbeat_caps = ctx.caps.clone();
        let heartbeat_perimeter = ctx.perimeter_svc.clone();
        let heartbeat_registry = ctx.registry.clone();
        let mut heartbeat_shutdown = ctx.shutdown_tx.subscribe();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(3));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let execution_reachability =
                            execution_reachability_for_hotel(heartbeat_graph.as_ref(), &heartbeat_hotel);
                        let node_health =
                            sample_node_health(heartbeat_graph.as_ref(), &heartbeat_hotel.hotel_name, &heartbeat_perimeter);

                        // Self-observe: keep the LOCAL node fresh in the registry so
                        // operator-target views (desktop membrane, edge clients) always
                        // include a local target — even on an isolated hotel with no
                        // backbone peers. Peers discard our beacons, so nothing else
                        // ever inserts the local node.
                        heartbeat_registry.write().await.observe_heartbeat(
                            heartbeat_caps.clone(),
                            Some(execution_reachability.clone()),
                            Some(node_health.clone()),
                        );

                        let targets = match mesh_targets_for_graph(heartbeat_graph.as_ref(), &heartbeat_caps.node_id) {
                            Ok(targets) => targets,
                            Err(err) => {
                                warn!("Failed to resolve mesh heartbeat targets: {}", err);
                                continue;
                            }
                        };
                        if targets.is_empty() {
                            continue;
                        }
                        for (_target_node_id, target_addr) in targets {
                            let Ok(target) = target_addr.parse::<SocketAddr>() else {
                                warn!("Skipping invalid heartbeat target address {}", target_addr);
                                continue;
                            };
                            let Some(auth_key) = mesh_auth_key_for_node(heartbeat_graph.as_ref(), &_target_node_id).ok().flatten() else {
                                debug!("Skipping heartbeat to {} until mesh auth key exists", _target_node_id);
                                continue;
                            };
                            if let Err(err) = emit_heartbeat(
                                &heartbeat_socket,
                                target,
                                &heartbeat_caps,
                                Some(execution_reachability.clone()),
                                &auth_key,
                                Some(node_health.clone()),
                            )
                            .await
                            {
                                warn!("Failed to emit heartbeat to {}: {}", target_addr, err);
                            }
                        }
                    }
                    _ = heartbeat_shutdown.recv() => break,
                }
            }
        });
    }

    {
        let capability_socket = daemon.socket();
        let capability_graph = ctx.graph_domain.clone();
        let capability_hotel = ctx.hotel.clone();
        let capability_caps = ctx.caps.clone();
        let mut capability_shutdown = ctx.shutdown_tx.subscribe();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
            let mut last_sync_fingerprint: Option<String> = None;
            let mut last_full_sync =
                std::time::Instant::now() - std::time::Duration::from_secs(3600);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let targets = match mesh_targets_for_graph(capability_graph.as_ref(), &capability_caps.node_id) {
                            Ok(targets) => targets,
                            Err(err) => {
                                warn!("Failed to resolve mesh capability-sync targets: {}", err);
                                continue;
                            }
                        };
                        if targets.is_empty() {
                            continue;
                        }

                        let advertisements = match local_capability_advertisements(capability_graph.as_ref(), &capability_hotel) {
                            Ok(advertisements) => advertisements,
                            Err(err) => {
                                warn!("Failed to build local capability advertisements: {}", err);
                                continue;
                            }
                        };
                        let execution_reachability =
                            execution_reachability_for_hotel(capability_graph.as_ref(), &capability_hotel);
                        let sync_fingerprint = capability_sync_fingerprint(&advertisements, &execution_reachability);
                        let should_sync =
                            last_sync_fingerprint.as_ref() != Some(&sync_fingerprint)
                            || last_full_sync.elapsed() >= std::time::Duration::from_secs(3600);
                        if !should_sync {
                            continue;
                        }

                        for (target_node_id, target_addr) in targets {
                            let Ok(target) = target_addr.parse::<SocketAddr>() else {
                                warn!("Skipping invalid capability-sync target address {}", target_addr);
                                continue;
                            };
                            let Some(auth_key) = mesh_auth_key_for_node(capability_graph.as_ref(), &target_node_id).ok().flatten() else {
                                debug!("Skipping capability sync to {} until mesh auth key exists", target_node_id);
                                continue;
                            };
                            if let Err(err) = emit_capability_sync(
                                &capability_socket,
                                target,
                                &capability_caps,
                                &advertisements,
                                Some(execution_reachability.clone()),
                                &auth_key,
                            )
                            .await
                            {
                                warn!("Failed to emit capability sync to {}: {}", target_addr, err);
                            }
                        }

                        last_sync_fingerprint = Some(sync_fingerprint);
                        last_full_sync = std::time::Instant::now();
                    }
                    _ = capability_shutdown.recv() => break,
                }
            }
        });
    }

    {
        let dispatcher_inbound_tx = ctx.dispatcher_tx.clone();
        let inbound_graph = ctx.graph_domain.clone();
        let inbound_inboxes = ctx.ipc_inboxes.clone();
        let inbound_parked = ctx.ipc_parked_inbound.clone();
        let inbound_delivery_claims = ctx.ipc_delivery_claims.clone();
        let inbound_mat_req = ctx.ipc_materialization_requester.clone();
        let inbound_local_node_id = ctx.caps.node_id.clone();
        let webrtc_signal_tx_inbound = ctx.webrtc_signal_tx.clone();
        let local_node_id_webrtc_inbound = ctx.caps.node_id.clone();
        let inbound_operator_surface_tx = ctx.ipc_operator_surface_tx.clone();
        tokio::spawn(async move {
            while let Some(msg) = inbox_rx.recv().await {
                match msg.msg_type {
                    ansible_mesh_core::MsgType::Heartbeat => {
                        if let Ok(payload) =
                            serde_json::from_slice::<HeartbeatPayload>(&msg.payload)
                        {
                            reconcile_peer_execution_reachability(
                                inbound_graph.as_ref(),
                                &payload.capabilities,
                                payload.execution_reachability.as_ref(),
                            );
                        }
                    }
                    ansible_mesh_core::MsgType::CapabilitySync => {
                        if let Ok(payload) =
                            serde_json::from_slice::<CapabilitySyncPayload>(&msg.payload)
                        {
                            reconcile_peer_execution_reachability(
                                inbound_graph.as_ref(),
                                &payload.capabilities,
                                payload.execution_reachability.as_ref(),
                            );
                        }
                    }
                    ansible_mesh_core::MsgType::MeshEventBatch
                    | ansible_mesh_core::MsgType::ExecutionEventBatch => {
                        if let Ok(events) =
                            serde_json::from_slice::<Vec<EventEnvelope>>(&msg.payload)
                        {
                            if !events.is_empty() {
                                let max_seq = events.iter().map(|e| e.seq).max().unwrap_or(0);
                                for event in &events {
                                    IpcServer::deliver_event_envelope_or_park(
                                        &inbound_inboxes,
                                        event,
                                        inbound_operator_surface_tx.as_ref(),
                                        inbound_graph.as_ref(),
                                        &inbound_local_node_id,
                                        &inbound_parked,
                                        inbound_mat_req.as_deref(),
                                        &inbound_delivery_claims,
                                    )
                                    .await;
                                    // Cron control-plane broadcasts.
                                    match &event.kind {
                                        ansible_mesh_core::event::EventKind::CronFired => {
                                            if let ansible_mesh_core::event::EventPayload::Inline {
                                                data,
                                            } = &event.payload
                                            {
                                                handle_cron_fired_broadcast(
                                                    inbound_graph.as_ref(),
                                                    data,
                                                );
                                            }
                                        }
                                        ansible_mesh_core::event::EventKind::CronJobSync => {
                                            if let ansible_mesh_core::event::EventPayload::Inline {
                                                data,
                                            } = &event.payload
                                            {
                                                handle_cron_job_sync(
                                                    inbound_graph.as_ref(),
                                                    data,
                                                );
                                            }
                                        }
                                        ansible_mesh_core::event::EventKind::ProjectedUserIdentitySync => {
                                            if let ansible_mesh_core::event::EventPayload::Inline {
                                                data,
                                            } = &event.payload
                                            {
                                                handle_projected_user_identity_sync(
                                                    inbound_graph.as_ref(),
                                                    data,
                                                );
                                            }
                                        }
                                        ansible_mesh_core::event::EventKind::SessionControl => {
                                            if let ansible_mesh_core::event::EventPayload::Inline {
                                                data,
                                            } = &event.payload
                                            {
                                                if let Ok(v) = serde_json::from_str::<
                                                    serde_json::Value,
                                                >(data)
                                                {
                                                    if v.get("action")
                                                        .and_then(|a| a.as_str())
                                                        == Some("session.handoff")
                                                    {
                                                        let graph = inbound_graph.clone();
                                                        let inboxes = inbound_inboxes.clone();
                                                        let parked = inbound_parked.clone();
                                                        let mat_req = inbound_mat_req.clone();
                                                        let node_id =
                                                            inbound_local_node_id.clone();
                                                        let data = data.clone();
                                                        tokio::spawn(async move {
                                                            IpcServer::handle_remote_role_handoff(
                                                                graph.as_ref(),
                                                                &inboxes,
                                                                &parked,
                                                                mat_req,
                                                                &node_id,
                                                                &data,
                                                            )
                                                            .await;
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                let _ = dispatcher_inbound_tx
                                    .send(LedgerCommand::CommitInboundBatch {
                                        events,
                                        source_node: msg.src_node.clone(),
                                    })
                                    .await;

                                let ack_payload =
                                    serde_json::json!({ "acked_seq": max_seq }).to_string();
                                if let Some(target_addr) =
                                    mesh_target_addr_for_node(inbound_graph.as_ref(), &msg.src_node)
                                        .ok()
                                        .flatten()
                                {
                                    let Some(auth_key) = mesh_auth_key_for_node(
                                        inbound_graph.as_ref(),
                                        &msg.src_node,
                                    )
                                    .ok()
                                    .flatten() else {
                                        warn!(
                                            "No mesh auth key found for ACK destination {}",
                                            msg.src_node
                                        );
                                        continue;
                                    };
                                    let msg_id = uuid::Uuid::new_v4();
                                    let seq = 0;
                                    let timestamp = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs();
                                    let payload = ack_payload.into_bytes();
                                    let hmac = ansible_mesh_core::authz::MeshAuth::new(auth_key)
                                        .sign(&msg_id, seq as u64, &payload, timestamp);
                                    let ack = ansible_mesh_core::BeaconMessage {
                                        version: 1,
                                        msg_id,
                                        src_node: inbound_local_node_id.clone(),
                                        dest_node: msg.src_node.clone(),
                                        msg_type: ansible_mesh_core::MsgType::ExecutionEventAck,
                                        seq,
                                        total: 1,
                                        payload: payload.into(),
                                        timestamp,
                                        hmac: hmac.into(),
                                    };
                                    if let Err(err) =
                                        crate::service::execution_transport::send_execution_message(
                                            &target_addr,
                                            &ack,
                                        )
                                        .await
                                    {
                                        warn!(
                                            "Failed to return execution ACK to {} at {}: {}",
                                            msg.src_node, target_addr, err
                                        );
                                    }
                                } else {
                                    warn!(
                                        "No mesh target address found for ACK destination {}",
                                        msg.src_node
                                    );
                                }
                            }
                        }
                    }
                    ansible_mesh_core::MsgType::MeshEventAck
                    | ansible_mesh_core::MsgType::ExecutionEventAck => {
                        debug!("Received MeshEventAck from {}", msg.src_node);
                        if let Ok(ack_payload) =
                            serde_json::from_slice::<serde_json::Value>(&msg.payload)
                        {
                            if let Some(acked_seq) =
                                ack_payload.get("acked_seq").and_then(|v| v.as_u64())
                            {
                                let _ = dispatcher_inbound_tx
                                    .send(LedgerCommand::ProcessAck {
                                        consumer_node_id: msg.src_node.clone(),
                                        acked_seq,
                                    })
                                    .await;
                            }
                        }
                    }
                    ansible_mesh_core::MsgType::MeshMembershipAccept => {
                        if let Ok(payload_json) = String::from_utf8(msg.payload.to_vec()) {
                            handle_mesh_membership_accept(inbound_graph.as_ref(), &payload_json);
                        } else {
                            warn!(
                                "Received mesh membership acceptance from {} with non-UTF8 payload",
                                msg.src_node
                            );
                        }
                    }
                    ansible_mesh_core::MsgType::WebRtcSignal => {
                        info!("Received WebRTC Signaling Payload from {}", msg.src_node);
                        if let Ok(signal_msg) = serde_json::from_slice::<
                            ansible_mesh_core::webrtc::WebRtcSignalMessage,
                        >(&msg.payload)
                        {
                            let webrtc_signal_tx = webrtc_signal_tx_inbound.clone();
                            let local_node_id = local_node_id_webrtc_inbound.clone();
                            tokio::spawn(async move {
                                match signal_msg.signal {
                                    ansible_mesh_core::webrtc::SignalPayload::Offer(sdp) => {
                                        let guest =
                                            crate::service::webrtc_guest::WebRtcGuest::new(
                                                signal_msg.session_id,
                                                local_node_id,
                                                msg.src_node,
                                                signal_msg.target_guest_id,
                                                signal_msg.sender_guest_id,
                                                webrtc_signal_tx,
                                            );
                                        if let Err(e) = guest.run_answering(sdp).await {
                                            error!("WebRTC Transceiver Guest failed: {}", e);
                                        }
                                    }
                                    ansible_mesh_core::webrtc::SignalPayload::Answer(sdp) => {
                                        match crate::service::webrtc_guest::WebRtcGuest::apply_answer(
                                            &signal_msg.session_id,
                                            sdp,
                                        )
                                        .await
                                        {
                                            Ok(true) => {}
                                            Ok(false) => debug!(
                                                "Received WebRTC answer for unknown session {}",
                                                signal_msg.session_id
                                            ),
                                            Err(e) => error!(
                                                "Failed to apply WebRTC answer for session {}: {}",
                                                signal_msg.session_id, e
                                            ),
                                        }
                                    }
                                    ansible_mesh_core::webrtc::SignalPayload::IceCandidate(candidate) => {
                                        debug!(
                                            "Received WebRTC ICE candidate for session {} but trickle ICE is not wired yet: {} bytes",
                                            signal_msg.session_id,
                                            candidate.len()
                                        );
                                    }
                                    ansible_mesh_core::webrtc::SignalPayload::SessionEnded => {
                                        match crate::service::webrtc_guest::WebRtcGuest::close_session(
                                            &signal_msg.session_id,
                                        )
                                        .await
                                        {
                                            Ok(true) => {}
                                            Ok(false) => debug!(
                                                "Received WebRTC session end for unknown session {}",
                                                signal_msg.session_id
                                            ),
                                            Err(e) => error!(
                                                "Failed to close WebRTC session {}: {}",
                                                signal_msg.session_id, e
                                            ),
                                        }
                                    }
                                }
                            });
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    {
        let socket_webrtc = daemon.socket();
        let webrtc_graph = ctx.graph_domain.clone();
        let local_node_id_webrtc = ctx.caps.node_id.clone();
        tokio::spawn(async move {
            while let Some(signal) = webrtc_signal_rx.recv().await {
                if let Ok(payload_bytes) = serde_json::to_vec(&signal) {
                    let Some(auth_key) =
                        mesh_auth_key_for_node(webrtc_graph.as_ref(), &signal.target_node_id)
                            .ok()
                            .flatten()
                    else {
                        debug!(
                            "Skipping WebRTC signal for {} until mesh auth key exists",
                            signal.target_node_id
                        );
                        continue;
                    };
                    let Some(target_addr) =
                        mesh_target_addr_for_node(webrtc_graph.as_ref(), &signal.target_node_id)
                            .ok()
                            .flatten()
                    else {
                        debug!(
                            "Skipping WebRTC signal for {} until mesh reachability exists",
                            signal.target_node_id
                        );
                        continue;
                    };
                    let msg_id = uuid::Uuid::new_v4();
                    let seq = 0;
                    let timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();

                    let hmac = ansible_mesh_core::authz::MeshAuth::new(auth_key).sign(
                        &msg_id,
                        seq as u64,
                        &payload_bytes,
                        timestamp,
                    );

                    let msg = ansible_mesh_core::BeaconMessage {
                        version: 1,
                        msg_id,
                        src_node: local_node_id_webrtc.clone(),
                        dest_node: signal.target_node_id.clone(),
                        msg_type: ansible_mesh_core::MsgType::WebRtcSignal,
                        seq,
                        total: 1,
                        timestamp,
                        payload: payload_bytes.into(),
                        hmac: hmac.into(),
                    };

                    if let Ok(packet) = serde_json::to_vec(&msg) {
                        if let Err(e) = socket_webrtc.send_to(&packet, &target_addr).await {
                            tracing::error!("UDP WebRTC Signal send failed: {}", e);
                        }
                    }
                }
            }
        });
    }

    {
        let daemon = daemon.clone();
        tokio::spawn(async move {
            if let Err(e) = daemon.run_loop().await {
                error!("Beacon Daemon error: {}", e);
            }
        });
    }

    Ok(())
}
