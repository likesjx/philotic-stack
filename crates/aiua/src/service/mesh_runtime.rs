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
use crate::service::role_materialization::{DeliveryOutcome, DropReason};
use crate::{
    LedgerCommand, capability_sync_fingerprint, execution_reachability_for_hotel,
    handle_mesh_membership_accept, handle_projected_user_identity_sync,
    local_capability_advertisements, mesh_auth_key_for_node, mesh_target_addr_for_node,
    mesh_targets_for_graph, reconcile_peer_execution_reachability, sample_node_health,
};

/// Does an inbound event's claimed source match the peer whose per-pair
/// HMAC key authenticated the batch?
///
/// Handlers act on `event.source_node_id` — as the origin of a relocation,
/// the hotel to reply to, the peer a secret may be released to — but that
/// field sits inside the payload, and the batch's HMAC only proves who
/// *sent* it. Without this check any authenticated peer could speak as any
/// other hotel (DEF-170). Every hotel builds its envelopes with its own node
/// id as the source and sends them directly, so for an event addressed here
/// the two must agree. An event addressed to another node is left alone:
/// delivery ignores it anyway.
pub(crate) fn event_source_is_authenticated_sender(
    event: &EventEnvelope,
    authenticated_sender: &str,
    local_node_id: &str,
) -> bool {
    match event.target_node_id.as_deref() {
        Some(target) if target != local_node_id => true,
        _ => event.source_node_id == authenticated_sender,
    }
}

/// Is this event addressed to a node other than this one? (An event with no
/// target is a broadcast and is ours.)
pub(crate) fn event_is_addressed_elsewhere(event: &EventEnvelope, local_node_id: &str) -> bool {
    event
        .target_node_id
        .as_deref()
        .is_some_and(|target| target != local_node_id)
}

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
    /// Newly applied gossiped placement records flow here so the hotel can
    /// push `TransportHomeChanged` to local guests at once (DEF-107).
    pub(crate) placement_change_tx:
        Option<mpsc::UnboundedSender<ansible_mesh_core::placement_sync::PlacementChange>>,
    /// Alarm bus for inbound mesh traffic that could not be decoded or
    /// delivered (MESH_DELIVERY_GUARANTEES L1). `None` = log only.
    pub(crate) heal_queue: Option<Arc<dyn ansible_mesh_core::heal_queue::HealQueueStorage>>,
}

/// One element of an inbound mesh event batch that did not decode as an
/// [`EventEnvelope`] (or the whole payload, when it is not a JSON array).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UndecodableEvent {
    /// Raw `kind` field, when the element had one.
    pub(crate) kind: Option<String>,
    pub(crate) event_id: Option<String>,
    pub(crate) seq: Option<u64>,
    pub(crate) error: String,
}

/// Decode an inbound `MeshEventBatch` / `ExecutionEventBatch` payload element by
/// element (DEF-182). A batch used to be parsed as one `Vec<EventEnvelope>`, so a
/// single element this build cannot decode (an unknown `EventKind` from a newer
/// peer) silently dropped every event in the batch.
pub(crate) fn decode_inbound_batch(payload: &[u8]) -> (Vec<EventEnvelope>, Vec<UndecodableEvent>) {
    let values = match serde_json::from_slice::<Vec<serde_json::Value>>(payload) {
        Ok(values) => values,
        Err(err) => {
            return (
                Vec::new(),
                vec![UndecodableEvent {
                    kind: None,
                    event_id: None,
                    seq: None,
                    error: format!("batch payload is not a JSON array: {err}"),
                }],
            );
        }
    };
    let mut events = Vec::with_capacity(values.len());
    let mut undecodable = Vec::new();
    for value in values {
        let kind = value.get("kind").map(|kind| match kind {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        });
        let event_id = value
            .get("event_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let seq = value.get("seq").and_then(serde_json::Value::as_u64);
        match serde_json::from_value::<EventEnvelope>(value) {
            Ok(event) => events.push(event),
            Err(err) => undecodable.push(UndecodableEvent {
                kind,
                event_id,
                seq,
                error: err.to_string(),
            }),
        }
    }
    (events, undecodable)
}

/// Short operator-facing description of a drop.
fn drop_reason_message(reason: &DropReason) -> String {
    match reason {
        DropReason::NoSubscriber { target_role } => {
            format!("no guest on this hotel serves role '{target_role}' and none could be revived")
        }
        DropReason::MemoryWriteForwardFailed { error } => {
            format!("forwarded memory write failed to apply on the cluster primary: {error}")
        }
        DropReason::OperatorHandoffRefused { reason } => {
            format!("operator-surface handoff refused: {reason}")
        }
    }
}

/// Actions that are themselves replies. A dropped reply is never answered,
/// or two hotels could bounce error replies at each other forever.
fn is_reply_action(action: &str) -> bool {
    action.ends_with("_response")
        || matches!(
            action,
            "tool_result" | "send_reply" | "partial_reply" | "send_error" | "turn_event"
        )
}

/// The error reply owed to the originator of a cross-hotel task this hotel
/// accepted and dropped (MESH_DELIVERY_GUARANTEES L1), so the caller's turn
/// fails now instead of at its deadline. `None` when the task is itself a
/// reply, carries no JSON, or names no route back.
///
/// The reply shape follows what the waiting philote consumes for the dropped
/// request: `tool_result` for `execute_tool`, `model_response` for a model
/// request, `datasource_response` for a dotted capability (`graph.query`, …),
/// and otherwise a user-facing `send_reply` to the turn's `final_reply_*`
/// membrane. Philote parses `error` as a `TaskErrorPayload` object, never a
/// bare string.
pub(crate) fn dropped_task_error_reply(
    event: &EventEnvelope,
    message: &str,
    local_node_id: &str,
) -> Option<EventEnvelope> {
    use ansible_mesh_core::event::{EventKind, EventPayload};
    if event.kind != EventKind::TaskInvoke {
        return None;
    }
    let EventPayload::Inline { data } = &event.payload else {
        return None;
    };
    let task = serde_json::from_str::<serde_json::Value>(data).ok()?;
    let action = task
        .get("action")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if is_reply_action(action) {
        return None;
    }
    let target_role = event.target_agent_id.as_deref().unwrap_or("");
    let echo = |field: &str| task.get(field).cloned().unwrap_or(serde_json::Value::Null);
    let error = serde_json::json!({
        "kind": "delivery_failure",
        "message": message,
        "code": "TARGET_ROLE_UNSERVED",
        "retryable": false,
    });
    let is_model = action == "generate_text" || target_role.starts_with("model");
    let is_request = action == "execute_tool" || is_model || action.contains('.');
    let (node, role, guest, reply) = if is_request {
        let route = philotic_client::ReturnRoute::from_task(&task, &event.source_node_id, "agent");
        let reply = if action == "execute_tool" {
            serde_json::json!({
                "action": "tool_result",
                "tool_name": task.get("tool_name").or_else(|| task.get("tool")).cloned()
                    .unwrap_or(serde_json::Value::Null),
                "content": format!("Tool call failed: {message}"),
                "error": error,
            })
        } else if is_model {
            serde_json::json!({
                "action": "model_response",
                "content": "",
                "agent_action": {
                    "kind": "fail",
                    "message": message,
                    "model_result": { "error": error.clone() },
                },
                "error": error,
            })
        } else {
            serde_json::json!({
                "action": "datasource_response",
                "capability": action,
                "error": error,
            })
        };
        // A request reply addressed only by role would reach whichever
        // philote the receiving hotel picks; only answer a known caller.
        route.guest_id.as_ref()?;
        (route.node, route.role, route.guest_id, reply)
    } else {
        let node = task
            .get("final_reply_to")
            .and_then(serde_json::Value::as_str)?
            .to_string();
        let role = task
            .get("final_reply_role")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("membrane")
            .to_string();
        let guest = task
            .get("final_reply_guest_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        // EmitTask stamps the emitting agent on membrane replies so only that
        // agent's seat posts it (DEF-166). This reply skips EmitTask, so stamp
        // the agent the dropped turn was for. With neither a pinned guest nor
        // an owner, every seat on the hotel would post it: send nothing.
        let owner = task
            .get("agent_id")
            .and_then(serde_json::Value::as_str)
            .filter(|agent| !agent.trim().is_empty());
        if guest.is_none() && owner.is_none() {
            return None;
        }
        let mut reply = serde_json::json!({
            "action": "send_reply",
            "content": format!("⚠️ Delivery failed: {message}"),
        });
        if let Some(owner) = owner {
            reply[crate::service::ipc::REPLY_OWNER_AGENT_ID_FIELD] =
                serde_json::Value::String(owner.to_string());
        }
        (node, role, guest, reply)
    };
    // A reply addressed to this hotel would sit in the outbound ledger; the
    // local caller's own timeout already covers it.
    if node == local_node_id {
        return None;
    }
    let mut reply = reply;
    for field in ["session_id", "turn_id", "chat_id"] {
        reply[field] = echo(field);
    }
    if let Some(guest) = guest {
        reply["delivery_target_guest_id"] = serde_json::Value::String(guest);
    }
    Some(EventEnvelope {
        event_id: uuid::Uuid::new_v4(),
        seq: 0,
        source_node_id: local_node_id.to_string(),
        target_node_id: Some(node),
        source_agent_id: "unknown".into(),
        target_agent_id: Some(role),
        kind: EventKind::TaskInvoke,
        corr_id: format!("dropped:{}", event.event_id),
        attempt: 0,
        created_at: 0,
        expires_at: None,
        payload: EventPayload::Inline {
            data: reply.to_string(),
        },
        trace: vec![],
    })
}

/// File a dropped cross-hotel task (heal tag `mesh_task_dropped`) and send
/// its originator an error reply when one is owed.
async fn report_dropped_task(
    heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
    dispatcher_tx: &mpsc::Sender<LedgerCommand>,
    local_node_id: &str,
    event: &EventEnvelope,
    reason: &DropReason,
) {
    use ansible_mesh_core::mesh_alarm::{MESH_ALARM_SOURCE, MESH_TASK_DROPPED_TAG};
    let message = drop_reason_message(reason);
    if let Some(hq) = heal_queue {
        let text = format!(
            "[{MESH_TASK_DROPPED_TAG}] task {} from {} for role [{}] dropped: {message}",
            event.event_id,
            event.source_node_id,
            event.target_agent_id.as_deref().unwrap_or("-"),
        );
        if let Err(err) =
            hq.push_classified(MESH_ALARM_SOURCE, &text, "high", MESH_TASK_DROPPED_TAG)
        {
            warn!(error = %err, "Failed to push dropped-task alarm to heal queue");
        }
    }
    // A refused operator handoff is a policy decision, and a forwarded memory
    // write is fire-and-forget: neither has a waiting turn to fail.
    if !matches!(reason, DropReason::NoSubscriber { .. }) {
        return;
    }
    if let Some(reply) = dropped_task_error_reply(event, &message, local_node_id) {
        info!(
            event_id = %event.event_id,
            reply_to = reply.target_node_id.as_deref().unwrap_or("-"),
            reply_role = reply.target_agent_id.as_deref().unwrap_or("-"),
            "Dropped cross-hotel task: error reply sent to its originator"
        );
        let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(reply)).await;
    }
}

/// Log and file an inbound batch element that did not decode.
///
/// Throttled per (peer, event id), falling back to kind: an undecodable event
/// that stays unacked is re-sent by the peer on every dispatch tick until L4
/// dead-letters it, and must not flood `aiua.log`.
fn report_undecodable_event(
    alarm: &mut ansible_mesh_core::mesh_alarm::GossipParseAlarm,
    heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
    src_node: &str,
    bad: &UndecodableEvent,
) {
    use ansible_mesh_core::mesh_alarm::{MESH_ALARM_SOURCE, MESH_EVENT_UNDECODABLE_TAG};
    let key = format!(
        "event:{}",
        bad.event_id
            .as_deref()
            .or(bad.kind.as_deref())
            .unwrap_or("batch")
    );
    if !alarm.should_report(src_node, &key, std::time::Instant::now()) {
        return;
    }
    warn!(
        src_node,
        kind = bad.kind.as_deref().unwrap_or("-"),
        event_id = bad.event_id.as_deref().unwrap_or("-"),
        seq = bad.seq,
        error = %bad.error,
        "Undecodable mesh event from peer: not delivered (DEF-182)"
    );
    if let Some(hq) = heal_queue {
        let message = format!(
            "[{MESH_EVENT_UNDECODABLE_TAG}] event {} (kind {}, seq {}) from peer {src_node} did not \
             decode and was not delivered: {}. The peer may run an incompatible build.",
            bad.event_id.as_deref().unwrap_or("?"),
            bad.kind.as_deref().unwrap_or("?"),
            bad.seq.map(|s| s.to_string()).unwrap_or_else(|| "?".into()),
            bad.error
        );
        if let Err(err) = hq.push_classified(
            MESH_ALARM_SOURCE,
            &message,
            "high",
            MESH_EVENT_UNDECODABLE_TAG,
        ) {
            warn!(error = %err, "Failed to push undecodable-event alarm to heal queue");
        }
    }
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
        .await?
        .with_placement_change_tx(ctx.placement_change_tx.clone())
        .with_heal_queue(ctx.heal_queue.clone()),
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

    // Re-broadcast this hotel's agents' command manifests so a peer that
    // restarted or joined since the last publish catches up (DEF-180). The
    // first pass waits for rosters to arrive; a peer only caches a manifest
    // from the hotel its roster says runs the agent.
    {
        let cm_graph = ctx.graph_domain.clone();
        let cm_tx = ctx.dispatcher_tx.clone();
        let cm_node = ctx.caps.node_id.clone();
        let cm_hotel = ctx.hotel_name.clone();
        let mut cm_shutdown = ctx.shutdown_tx.subscribe();
        tokio::spawn(async move {
            let mut delay = tokio::time::Duration::from_secs(45);
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {
                        crate::service::command_manifest::rebroadcast_local_manifests(
                            cm_graph.as_ref(), &cm_tx, &cm_node, &cm_hotel,
                        ).await;
                        delay = tokio::time::Duration::from_secs(120);
                    }
                    _ = cm_shutdown.recv() => break,
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
        let inbound_hotel = ctx.hotel_name.clone();
        let inbound_registry = ctx.registry.clone();
        let inbound_inboxes = ctx.ipc_inboxes.clone();
        let inbound_parked = ctx.ipc_parked_inbound.clone();
        let inbound_delivery_claims = ctx.ipc_delivery_claims.clone();
        let inbound_mat_req = ctx.ipc_materialization_requester.clone();
        let inbound_local_node_id = ctx.caps.node_id.clone();
        let webrtc_signal_tx_inbound = ctx.webrtc_signal_tx.clone();
        let local_node_id_webrtc_inbound = ctx.caps.node_id.clone();
        let inbound_operator_surface_tx = ctx.ipc_operator_surface_tx.clone();
        let inbound_heal_queue = ctx.heal_queue.clone();
        tokio::spawn(async move {
            let mut gossip_alarm = ansible_mesh_core::mesh_alarm::GossipParseAlarm::default();
            while let Some(msg) = inbox_rx.recv().await {
                match msg.msg_type {
                    ansible_mesh_core::MsgType::Heartbeat => {
                        match serde_json::from_slice::<HeartbeatPayload>(&msg.payload) {
                            Ok(payload) => reconcile_peer_execution_reachability(
                                inbound_graph.as_ref(),
                                &payload.capabilities,
                                payload.execution_reachability.as_ref(),
                            ),
                            Err(err) => {
                                gossip_alarm.report(
                                    inbound_heal_queue.as_deref(),
                                    &msg.src_node,
                                    "heartbeat",
                                    &err.to_string(),
                                );
                            }
                        }
                    }
                    ansible_mesh_core::MsgType::CapabilitySync => {
                        match serde_json::from_slice::<CapabilitySyncPayload>(&msg.payload) {
                            Ok(payload) => reconcile_peer_execution_reachability(
                                inbound_graph.as_ref(),
                                &payload.capabilities,
                                payload.execution_reachability.as_ref(),
                            ),
                            Err(err) => {
                                gossip_alarm.report(
                                    inbound_heal_queue.as_deref(),
                                    &msg.src_node,
                                    "capability_sync",
                                    &err.to_string(),
                                );
                            }
                        }
                    }
                    ansible_mesh_core::MsgType::MeshEventBatch
                    | ansible_mesh_core::MsgType::ExecutionEventBatch => {
                        let (events, undecodable) = decode_inbound_batch(&msg.payload);
                        for bad in &undecodable {
                            report_undecodable_event(
                                &mut gossip_alarm,
                                inbound_heal_queue.as_deref(),
                                &msg.src_node,
                                bad,
                            );
                        }
                        if !events.is_empty() {
                            let max_seq = events.iter().map(|e| e.seq).max().unwrap_or(0);
                            for event in &events {
                                if !event_source_is_authenticated_sender(
                                    event,
                                    &msg.src_node,
                                    &inbound_local_node_id,
                                ) {
                                    warn!(
                                        event_id = %event.event_id,
                                        claimed_source = %event.source_node_id,
                                        authenticated_sender = %msg.src_node,
                                        kind = ?event.kind,
                                        "Dropping mesh event: it claims a source other than the peer that sent it (DEF-170)"
                                    );
                                    continue;
                                }
                                let outcome = IpcServer::deliver_event_envelope_or_park(
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
                                if let DeliveryOutcome::Dropped(reason) = &outcome {
                                    report_dropped_task(
                                        inbound_heal_queue.as_deref(),
                                        &dispatcher_inbound_tx,
                                        &inbound_local_node_id,
                                        event,
                                        reason,
                                    )
                                    .await;
                                }
                                // The control-plane handlers below act on their
                                // payload as addressed to THIS hotel. An event
                                // addressed to a third node is one the sender check
                                // above lets through only because delivery ignores
                                // it — running cron/identity/handoff handlers on it
                                // would apply a peer-supplied record with forged
                                // provenance (DEF-183).
                                if event_is_addressed_elsewhere(event, &inbound_local_node_id) {
                                    continue;
                                }
                                match &event.kind {
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
                                        ansible_mesh_core::event::EventKind::MaterializeRequest => {
                                            if let ansible_mesh_core::event::EventPayload::Inline {
                                                data,
                                            } = &event.payload
                                            {
                                                let graph = inbound_graph.clone();
                                                let inboxes = inbound_inboxes.clone();
                                                let mat_req = inbound_mat_req.clone();
                                                let node_id = inbound_local_node_id.clone();
                                                let source_node = event.source_node_id.clone();
                                                let data = data.clone();
                                                let dispatcher_tx = dispatcher_inbound_tx.clone();
                                                tokio::spawn(async move {
                                                    IpcServer::handle_remote_materialize_request(
                                                        graph.as_ref(),
                                                        &inboxes,
                                                        mat_req,
                                                        dispatcher_tx,
                                                        &node_id,
                                                        &source_node,
                                                        &data,
                                                    )
                                                    .await;
                                                });
                                            }
                                        }
                                        ansible_mesh_core::event::EventKind::MaterializeReady => {
                                            if let ansible_mesh_core::event::EventPayload::Inline {
                                                data,
                                            } = &event.payload
                                            {
                                                IpcServer::handle_remote_materialize_ready(
                                                    inbound_graph.as_ref(),
                                                    &event.source_node_id,
                                                    data,
                                                );
                                            }
                                        }
                                        ansible_mesh_core::event::EventKind::ContinuityImport => {
                                            if let ansible_mesh_core::event::EventPayload::Inline {
                                                data,
                                            } = &event.payload
                                            {
                                                let graph = inbound_graph.clone();
                                                let node_id = inbound_local_node_id.clone();
                                                let source_node = event.source_node_id.clone();
                                                let data = data.clone();
                                                let dispatcher_tx = dispatcher_inbound_tx.clone();
                                                tokio::spawn(async move {
                                                    IpcServer::handle_remote_continuity_import(
                                                        graph.as_ref(),
                                                        dispatcher_tx,
                                                        &node_id,
                                                        &source_node,
                                                        &data,
                                                    )
                                                    .await;
                                                });
                                            }
                                        }
                                        ansible_mesh_core::event::EventKind::ContinuityAck => {
                                            if let ansible_mesh_core::event::EventPayload::Inline {
                                                data,
                                            } = &event.payload
                                            {
                                                IpcServer::handle_remote_continuity_ack(
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
                                                        == Some(
                                                            crate::service::command_manifest::SYNC_ACTION,
                                                        )
                                                    {
                                                        let registry =
                                                            inbound_registry.read().await;
                                                        crate::service::command_manifest::handle_remote_manifest_sync(
                                                            inbound_graph.as_ref(),
                                                            &registry,
                                                            &inbound_hotel,
                                                            &event.source_node_id,
                                                            data,
                                                        );
                                                    } else if v.get("action")
                                                        .and_then(|a| a.as_str())
                                                        == Some("session.handoff")
                                                    {
                                                        let graph = inbound_graph.clone();
                                                        let inboxes = inbound_inboxes.clone();
                                                        let parked = inbound_parked.clone();
                                                        let mat_req = inbound_mat_req.clone();
                                                        let node_id =
                                                            inbound_local_node_id.clone();
                                                        let source_node =
                                                            event.source_node_id.clone();
                                                        let data = data.clone();
                                                        tokio::spawn(async move {
                                                            IpcServer::handle_remote_role_handoff(
                                                                graph.as_ref(),
                                                                &inboxes,
                                                                &parked,
                                                                mat_req,
                                                                &node_id,
                                                                &source_node,
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
                                let Some(auth_key) =
                                    mesh_auth_key_for_node(inbound_graph.as_ref(), &msg.src_node)
                                        .ok()
                                        .flatten()
                                else {
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
                                // Off the inbound loop: this loop is the only
                                // consumer of every peer's batches, heartbeats and
                                // signals, and an ACK to a peer that just went dark
                                // used to hold all of them for the connect timeout
                                // (DEF-181). The send has its own bounded timeouts.
                                let ack_dest = msg.src_node.clone();
                                tokio::spawn(async move {
                                    if let Err(err) =
                                        crate::service::execution_transport::send_execution_message(
                                            &target_addr,
                                            &ack,
                                        )
                                        .await
                                    {
                                        warn!(
                                            "Failed to return execution ACK to {} at {}: {}",
                                            ack_dest, target_addr, err
                                        );
                                    }
                                });
                            } else {
                                warn!(
                                    "No mesh target address found for ACK destination {}",
                                    msg.src_node
                                );
                            }
                        }
                    }
                    ansible_mesh_core::MsgType::MeshEventAck
                    | ansible_mesh_core::MsgType::ExecutionEventAck => {
                        debug!("Received MeshEventAck from {}", msg.src_node);
                        let parsed = serde_json::from_slice::<serde_json::Value>(&msg.payload);
                        if let Err(err) = &parsed {
                            gossip_alarm.report(
                                inbound_heal_queue.as_deref(),
                                &msg.src_node,
                                "event_ack",
                                &err.to_string(),
                            );
                        }
                        if let Ok(ack_payload) = parsed {
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

#[cfg(test)]
mod sender_binding_tests {
    use super::event_source_is_authenticated_sender;
    use ansible_mesh_core::event::{EventEnvelope, EventKind, EventPayload};
    use uuid::Uuid;

    fn event(source: &str, target: Option<&str>) -> EventEnvelope {
        EventEnvelope {
            event_id: Uuid::new_v4(),
            seq: 1,
            source_node_id: source.into(),
            target_node_id: target.map(str::to_string),
            source_agent_id: source.into(),
            target_agent_id: None,
            kind: EventKind::ContinuityImport,
            corr_id: String::new(),
            attempt: 0,
            created_at: 0,
            expires_at: None,
            payload: EventPayload::Inline { data: "{}".into() },
            trace: vec![],
        }
    }

    #[test]
    fn an_event_from_its_own_sender_is_accepted() {
        let e = event("mac-jane-aiua-01", Some("vps-jane-aiua-01"));
        assert!(event_source_is_authenticated_sender(
            &e,
            "mac-jane-aiua-01",
            "vps-jane-aiua-01"
        ));
    }

    #[test]
    fn a_peer_cannot_speak_as_another_hotel() {
        // mbp-jane authenticated the batch but claims mac-jane sent it.
        let e = event("mac-jane-aiua-01", Some("vps-jane-aiua-01"));
        assert!(!event_source_is_authenticated_sender(
            &e,
            "mbp-jane-aiua-01",
            "vps-jane-aiua-01"
        ));
        let untargeted = event("mac-jane-aiua-01", None);
        assert!(!event_source_is_authenticated_sender(
            &untargeted,
            "mbp-jane-aiua-01",
            "vps-jane-aiua-01"
        ));
    }

    #[test]
    fn control_plane_handlers_skip_events_addressed_to_a_third_node() {
        use super::event_is_addressed_elsewhere;
        assert!(event_is_addressed_elsewhere(
            &event("a", Some("mbp-jane-aiua-01")),
            "vps-jane-aiua-01"
        ));
        assert!(!event_is_addressed_elsewhere(
            &event("a", Some("vps-jane-aiua-01")),
            "vps-jane-aiua-01"
        ));
        assert!(
            !event_is_addressed_elsewhere(&event("a", None), "vps-jane-aiua-01"),
            "a broadcast is ours"
        );
    }

    #[test]
    fn an_event_addressed_elsewhere_is_not_ours_to_judge() {
        let e = event("mac-jane-aiua-01", Some("mbp-jane-aiua-01"));
        assert!(event_source_is_authenticated_sender(
            &e,
            "vps-jane-aiua-01",
            "vps-jane-aiua-01"
        ));
    }
}

#[cfg(test)]
mod decode_batch_tests {
    use super::decode_inbound_batch;
    use ansible_mesh_core::event::{EventEnvelope, EventKind, EventPayload};
    use uuid::Uuid;

    fn event(seq: u64) -> EventEnvelope {
        EventEnvelope {
            event_id: Uuid::new_v4(),
            seq,
            source_node_id: "mac-jane-aiua-01".into(),
            target_node_id: Some("vps-jane-aiua-01".into()),
            source_agent_id: "agent-bjork-01".into(),
            target_agent_id: Some("agent".into()),
            kind: EventKind::TaskInvoke,
            corr_id: String::new(),
            attempt: 0,
            created_at: 0,
            expires_at: None,
            payload: EventPayload::Inline { data: "{}".into() },
            trace: vec![],
        }
    }

    #[test]
    fn decode_inbound_batch_accepts_a_good_batch() {
        let batch = vec![event(1), event(2)];
        let (events, bad) = decode_inbound_batch(&serde_json::to_vec(&batch).unwrap());
        assert_eq!(events.len(), 2);
        assert!(bad.is_empty());
    }

    #[test]
    fn decode_inbound_batch_keeps_good_events_around_one_bad_element() {
        let mut values: Vec<serde_json::Value> = vec![
            serde_json::to_value(event(1)).unwrap(),
            serde_json::to_value(event(2)).unwrap(),
            serde_json::to_value(event(3)).unwrap(),
        ];
        // A newer peer's event kind this build does not know.
        values[1]["kind"] = serde_json::json!("FutureKindFromANewerPeer");
        let bad_id = values[1]["event_id"].as_str().unwrap().to_string();
        let (events, bad) = decode_inbound_batch(&serde_json::to_vec(&values).unwrap());
        assert_eq!(
            events.iter().map(|e| e.seq).collect::<Vec<_>>(),
            vec![1, 3],
            "one undecodable element must not drop the rest of the batch"
        );
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].kind.as_deref(), Some("FutureKindFromANewerPeer"));
        assert_eq!(bad[0].event_id.as_deref(), Some(bad_id.as_str()));
        assert_eq!(bad[0].seq, Some(2));
    }

    #[test]
    fn decode_inbound_batch_reports_a_non_json_payload() {
        let (events, bad) = decode_inbound_batch(b"\x00not json");
        assert!(events.is_empty());
        assert_eq!(bad.len(), 1);
        assert!(
            bad[0].error.contains("not a JSON array"),
            "{}",
            bad[0].error
        );
    }

    fn task(role: &str, data: serde_json::Value) -> EventEnvelope {
        let mut e = event(7);
        e.target_agent_id = Some(role.into());
        e.payload = EventPayload::Inline {
            data: data.to_string(),
        };
        e
    }

    fn reply_json(reply: &EventEnvelope) -> serde_json::Value {
        let EventPayload::Inline { data } = &reply.payload else {
            panic!("inline reply expected");
        };
        serde_json::from_str(data).unwrap()
    }

    #[test]
    fn dropped_execute_tool_replies_tool_result_to_the_caller() {
        let dropped = task(
            "tool.weather",
            serde_json::json!({
                "action": "execute_tool",
                "tool_name": "weather.now",
                "session_id": "s1",
                "turn_id": "t1",
                "return_route": {"node": "mac-jane-aiua-01", "role": "agent", "guest_id": "agent-bjork-01:orchestrator"},
                "final_reply_to": "mac-jane-aiua-01",
                "final_reply_role": "membrane",
            }),
        );
        let reply = super::dropped_task_error_reply(&dropped, "no guest", "vps-jane-aiua-01")
            .expect("an execute_tool drop owes its caller a reply");
        assert_eq!(reply.target_node_id.as_deref(), Some("mac-jane-aiua-01"));
        assert_eq!(reply.target_agent_id.as_deref(), Some("agent"));
        assert_eq!(reply.source_node_id, "vps-jane-aiua-01");
        let body = reply_json(&reply);
        assert_eq!(body["action"], "tool_result");
        assert_eq!(body["tool_name"], "weather.now");
        assert_eq!(body["turn_id"], "t1");
        assert_eq!(
            body["delivery_target_guest_id"],
            "agent-bjork-01:orchestrator"
        );
        // Philote parses `error` as a TaskErrorPayload object.
        let err: philotic_client::TaskErrorPayload =
            serde_json::from_value(body["error"].clone()).expect("TaskErrorPayload shape");
        assert_eq!(err.code.as_deref(), Some("TARGET_ROLE_UNSERVED"));
    }

    #[test]
    fn dropped_datasource_request_replies_datasource_response() {
        let dropped = task(
            "graph-datasource",
            serde_json::json!({
                "action": "graph.query",
                "session_id": "s1",
                "turn_id": "t1",
                "reply_to": "mac-jane-aiua-01",
                "reply_role": "agent",
                "reply_guest_id": "agent-bjork-01:orchestrator",
            }),
        );
        let body = reply_json(
            &super::dropped_task_error_reply(&dropped, "no guest", "vps-jane-aiua-01").unwrap(),
        );
        assert_eq!(body["action"], "datasource_response");
        assert_eq!(body["capability"], "graph.query");
        assert_eq!(body["session_id"], "s1");
    }

    #[test]
    fn dropped_user_turn_replies_to_the_final_reply_membrane() {
        let dropped = task(
            "agent",
            serde_json::json!({
                "content": "hello",
                "session_id": "telegram:1:agent-beacon",
                "chat_id": "1",
                "final_reply_to": "mac-jane-aiua-01",
                "final_reply_role": "membrane",
                "final_reply_guest_id": "membrane-telegram-beacon",
            }),
        );
        let reply = super::dropped_task_error_reply(&dropped, "no guest", "vps-jane-aiua-01")
            .expect("a dropped user turn owes the membrane a reply");
        assert_eq!(reply.target_agent_id.as_deref(), Some("membrane"));
        let body = reply_json(&reply);
        assert_eq!(body["action"], "send_reply");
        assert_eq!(body["chat_id"], "1");
        assert_eq!(body["delivery_target_guest_id"], "membrane-telegram-beacon");
    }

    #[test]
    fn a_dropped_reply_is_never_answered() {
        for action in [
            "tool_result",
            "datasource_response",
            "send_reply",
            "model_response",
        ] {
            let dropped = task(
                "agent",
                serde_json::json!({
                    "action": action,
                    "final_reply_to": "mac-jane-aiua-01",
                    "reply_to": "mac-jane-aiua-01",
                }),
            );
            assert!(
                super::dropped_task_error_reply(&dropped, "x", "vps-jane-aiua-01").is_none(),
                "{action} must not be answered (reply loops)"
            );
        }
    }

    #[test]
    fn a_dropped_user_turn_reply_is_stamped_with_its_agent_as_owner() {
        // No pinned membrane guest: the owner stamp is what keeps every other
        // bot's seat on that hotel from posting the notice too (DEF-166).
        let dropped = task(
            "agent",
            serde_json::json!({
                "agent_id": "agent-beacon",
                "session_id": "telegram:1:agent-beacon",
                "final_reply_to": "mac-jane-aiua-01",
                "final_reply_role": "membrane",
            }),
        );
        let body = reply_json(
            &super::dropped_task_error_reply(&dropped, "x", "vps-jane-aiua-01").unwrap(),
        );
        assert_eq!(body["reply_owner_agent_id"], "agent-beacon");
        assert!(body.get("delivery_target_guest_id").is_none());
    }

    #[test]
    fn a_dropped_user_turn_without_owner_or_guest_gets_no_reply() {
        let dropped = task(
            "agent",
            serde_json::json!({
                "session_id": "cron:job-1",
                "final_reply_to": "mac-jane-aiua-01",
                "final_reply_role": "membrane",
            }),
        );
        assert!(
            super::dropped_task_error_reply(&dropped, "x", "vps-jane-aiua-01").is_none(),
            "an unowned role-only reply would be posted by every seat"
        );
    }

    #[test]
    fn a_dropped_request_with_no_known_caller_gets_no_reply() {
        let dropped = task(
            "graph-datasource",
            serde_json::json!({
                "action": "graph.query",
                "session_id": "s1",
                "reply_to": "mac-jane-aiua-01",
                "reply_role": "agent",
            }),
        );
        assert!(super::dropped_task_error_reply(&dropped, "x", "vps-jane-aiua-01").is_none());
    }

    #[test]
    fn no_error_reply_is_addressed_to_this_hotel() {
        let dropped = task(
            "tool.weather",
            serde_json::json!({
                "action": "execute_tool",
                "return_route": {"node": "vps-jane-aiua-01", "role": "agent", "guest_id": "agent-x"},
            }),
        );
        assert!(super::dropped_task_error_reply(&dropped, "x", "vps-jane-aiua-01").is_none());
    }

    #[test]
    fn a_dropped_user_turn_without_a_route_gets_no_reply() {
        let dropped = task("agent", serde_json::json!({ "content": "hello" }));
        assert!(super::dropped_task_error_reply(&dropped, "x", "vps-jane-aiua-01").is_none());
    }
}
