use ansible_mesh_core::authz::MeshAuth;
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::registry::NodeRegistry;
use ansible_mesh_core::storage::{CursorStorage, EventStorage};
use ansible_mesh_core::{BeaconMessage, MsgType};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::{RwLock, broadcast};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::mesh::mesh_auth_key_for_node;
use crate::service::execution_transport::send_execution_message;

/// How long to stay quiet between "no auth key" warnings for a single target.
/// The dispatcher loop ticks once a second; without throttling a target we
/// cannot authenticate to (an unenrolled/orphan node, or an enrolled peer
/// whose local secret is transiently broken) produces ~60 log lines a minute.
const NO_AUTH_KEY_WARN_INTERVAL: Duration = Duration::from_secs(300);

/// Outcome of one `dispatch_for_target` attempt, so the caller can react
/// without treating an unauthenticatable (but otherwise benign) target as a
/// hard error to be logged every tick.
enum DispatchStatus {
    /// No unacked events for this target — nothing to do.
    Idle,
    /// We have events to send but no mesh auth key for the target, so we
    /// cannot sign them. Not an error — the caller skips (throttled-warn).
    NoAuthKey,
    /// At least one event was dispatched (or attempted over the wire).
    Dispatched,
}

/// Continuously polls the EventStorage and CursorStorage to dispatch durable
/// mesh events over UDP to their target nodes.
pub async fn outbound_dispatcher(
    ledger: Arc<dyn EventStorage>,
    tracker: Arc<dyn CursorStorage>,
    _udp_socket: Arc<UdpSocket>,
    graph: Arc<GraphDomain>,
    registry: Arc<RwLock<NodeRegistry>>,
    local_node_id: String,
    mut shutdown_rx: broadcast::Receiver<()>,
) {
    info!("Started Outbound Mesh Dispatcher Loop.");

    // Poll every 1 second
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));

    // Last time we warned about a target we could not authenticate to, keyed by
    // target node id. Throttles the "no auth key" warning to at most once per
    // target per `NO_AUTH_KEY_WARN_INTERVAL` so a single unauthenticatable
    // target cannot spam the log every tick.
    let mut last_no_auth_warn: HashMap<String, Instant> = HashMap::new();

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let targets = match execution_targets(graph.as_ref(), &registry, &local_node_id).await {
                    Ok(targets) => targets,
                    Err(e) => {
                        warn!("Failed to resolve execution targets: {}", e);
                        continue;
                    }
                };
                for (target_node_id, target_addr) in &targets {
                    match dispatch_for_target(ledger.as_ref(), tracker.as_ref(), graph.as_ref(), &local_node_id, target_node_id, target_addr).await {
                        Ok(DispatchStatus::Idle) | Ok(DispatchStatus::Dispatched) => {}
                        Ok(DispatchStatus::NoAuthKey) => {
                            let now = Instant::now();
                            let due = last_no_auth_warn
                                .get(target_node_id)
                                .map(|last| now.duration_since(*last) >= NO_AUTH_KEY_WARN_INTERVAL)
                                .unwrap_or(true);
                            if due {
                                last_no_auth_warn.insert(target_node_id.clone(), now);
                                warn!(
                                    "skipping mesh dispatch to {}: no auth key (unauthenticated/orphan target)",
                                    target_node_id
                                );
                            }
                        }
                        Err(e) => {
                            error!("Failed to dispatch to {}: {}", target_node_id, e);
                        }
                    }
                }
            }
            _ = shutdown_rx.recv() => {
                info!("Outbound Mesh Dispatcher received shutdown signal.");
                break;
            }
        }
    }
}

async fn execution_targets(
    graph: &GraphDomain,
    registry: &Arc<RwLock<NodeRegistry>>,
    local_node_id: &str,
) -> Result<Vec<(String, String)>> {
    let registry_guard = registry.read().await;
    let mut targets = Vec::new();
    for hotel in graph.list_hotels()? {
        if hotel.capabilities.node_id == local_node_id {
            continue;
        }

        let target_addr = registry_guard
            .get_node(&hotel.capabilities.node_id)
            .and_then(|status| status.execution_reachability.as_ref())
            .map(|execution| format!("{}:{}", execution.host, execution.port))
            .or_else(|| {
                hotel
                    .mesh_host
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .map(|host| format!("{host}:{}", hotel.execution_port))
            })
            .unwrap_or_else(|| format!("127.0.0.1:{}", hotel.execution_port));
        targets.push((hotel.capabilities.node_id, target_addr));
    }

    Ok(targets)
}

async fn dispatch_for_target(
    ledger: &dyn EventStorage,
    tracker: &dyn CursorStorage,
    graph: &GraphDomain,
    local_node_id: &str,
    target_node_id: &str,
    target_addr: &str,
) -> Result<DispatchStatus> {
    // 1. Where does the target node's cursor currently sit?
    let cursor = tracker.get_cursor(target_node_id)?;

    // 2. Query up to 50 un-acked events
    let unacked_events = ledger.query_unacked_events(target_node_id, cursor, 50)?;

    if unacked_events.is_empty() {
        return Ok(DispatchStatus::Idle);
    }

    // 3. Resolve the mesh auth key ONCE per target (it is per-target, not
    //    per-event). A `None` here is not an error: it means the target is
    //    unauthenticatable (an unenrolled/orphan node, or an enrolled peer
    //    whose local secret is broken). Signal that up so the caller can skip
    //    quietly instead of erroring on every 1-second tick.
    let Some(auth_key) = mesh_auth_key_for_node(graph, local_node_id, target_node_id)? else {
        return Ok(DispatchStatus::NoAuthKey);
    };
    // Build the signer once — the auth key is per-target, identical for every
    // event in the batch.
    let auth = MeshAuth::new(auth_key);

    debug!(
        "Found {} unacked events for {}, cursor is at seq {}",
        unacked_events.len(),
        target_node_id,
        cursor
    );

    // 4. Prepare the BeaconMessage batch (for now sending one event in the batch)
    for event in unacked_events {
        let payload = serde_json::to_vec(&vec![&event])?;

        // Wrap in BeaconMessage
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let msg_id = Uuid::new_v4();
        let hmac = auth.sign(&msg_id, event.seq, &payload, ts);
        let msg = BeaconMessage {
            version: 1,
            msg_id,
            src_node: local_node_id.to_string(),
            dest_node: target_node_id.to_string(),
            msg_type: MsgType::ExecutionEventBatch,
            seq: event.seq as u32,
            total: 1,
            payload: payload.into(),
            timestamp: ts,
            hmac: hmac.into(),
        };

        match send_execution_message(target_addr, &msg).await {
            Ok(()) => {
                let bytes_sent = serde_json::to_vec(&msg)?.len();
                debug!(
                    "Dispatched Event {} (seq: {}) to {} over execution transport ({} bytes)",
                    event.event_id, event.seq, target_node_id, bytes_sent
                );
            }
            Err(e) => {
                warn!(
                    "Failed to send execution packet to {} at {}: {}",
                    target_node_id, target_addr, e
                );
                // Break out of the loop and try again next tick so we don't spam errors
                break;
            }
        }
    }

    Ok(DispatchStatus::Dispatched)
}

#[cfg(test)]
mod tests {
    use super::{DispatchStatus, dispatch_for_target, execution_targets};
    use ansible_mesh_core::domain::GraphDomain;
    use ansible_mesh_core::event::{EventEnvelope, EventKind, EventPayload};
    use ansible_mesh_core::registry::{ExecutionReachability, NodeRegistry};
    use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
    use ansible_mesh_core::sqlite_storage::{SqliteCursorStorage, SqliteEventStorage};
    use ansible_mesh_core::storage::EventStorage;
    use ansible_mesh_core::storage::HotelRecord;
    use ansible_mesh_core::{NodeCapabilities, NodeConstraints, NodeRole};
    use std::sync::Arc;
    use tokio::sync::RwLock;
    use uuid::Uuid;

    fn hotel_record(
        hotel_name: &str,
        node_id: &str,
        mesh_host: Option<&str>,
        execution_port: u16,
    ) -> HotelRecord {
        HotelRecord {
            hotel_name: hotel_name.into(),
            capabilities: NodeCapabilities {
                node_id: node_id.into(),
                roles: vec![NodeRole::AnsibleNode],
                models: vec![],
                tools: vec![],
                constraints: NodeConstraints {
                    max_concurrent_jobs: None,
                    latency_hint_ms: None,
                    trust_level: None,
                },
            },
            mesh_host: mesh_host.map(str::to_string),
            mesh_port: execution_port.saturating_sub(2),
            blob_port: execution_port.saturating_sub(1),
            execution_port,
            ipc_socket_path: String::new(),
            active_pid: None,
        }
    }

    #[tokio::test]
    async fn execution_targets_fall_back_to_hotel_mesh_host() {
        let storage = SqliteGraphStorage::open(":memory:").expect("open sqlite graph");
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        let local = hotel_record("default", "default-aiua-01", Some("100.64.230.106"), 24851);
        let remote = hotel_record("mbp-jane", "mbp-jane-aiua-01", Some("100.79.239.64"), 13106);
        graph.upsert_hotel(&local).expect("upsert local hotel");
        graph.upsert_hotel(&remote).expect("upsert remote hotel");

        let registry = Arc::new(RwLock::new(NodeRegistry::new()));
        let targets = execution_targets(&graph, &registry, "default-aiua-01")
            .await
            .expect("resolve targets");

        assert_eq!(
            targets,
            vec![("mbp-jane-aiua-01".into(), "100.79.239.64:13106".into())]
        );
    }

    #[tokio::test]
    async fn execution_targets_prefer_registry_reachability() {
        let storage = SqliteGraphStorage::open(":memory:").expect("open sqlite graph");
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        let local = hotel_record("default", "default-aiua-01", Some("100.64.230.106"), 24851);
        let remote = hotel_record("mbp-jane", "mbp-jane-aiua-01", Some("100.79.239.64"), 13106);
        graph.upsert_hotel(&local).expect("upsert local hotel");
        graph.upsert_hotel(&remote).expect("upsert remote hotel");

        let registry = Arc::new(RwLock::new(NodeRegistry::new()));
        registry.write().await.update_node(
            remote.capabilities.clone(),
            vec![],
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "100.79.239.65".into(),
                port: 14000,
            }),
            None,
        );

        let targets = execution_targets(&graph, &registry, "default-aiua-01")
            .await
            .expect("resolve targets");

        assert_eq!(
            targets,
            vec![("mbp-jane-aiua-01".into(), "100.79.239.65:14000".into())]
        );
    }

    fn unacked_event(target_node_id: &str) -> EventEnvelope {
        EventEnvelope {
            event_id: Uuid::new_v4(),
            seq: 0,
            source_node_id: "local-aiua-01".into(),
            target_node_id: Some(target_node_id.into()),
            source_agent_id: "agent-a".into(),
            target_agent_id: Some("agent-b".into()),
            kind: EventKind::TaskInvoke,
            corr_id: Uuid::new_v4().to_string(),
            attempt: 1,
            created_at: 0,
            expires_at: None,
            payload: EventPayload::Inline {
                data: r#"{"msg":"hi"}"#.into(),
            },
            trace: vec![],
        }
    }

    /// No unacked events for the target → `Idle`, regardless of auth key.
    #[tokio::test]
    async fn dispatch_for_target_idle_when_no_events() {
        let storage = SqliteGraphStorage::open(":memory:").expect("open sqlite graph");
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        let ledger = SqliteEventStorage::open(":memory:").expect("open event storage");
        let tracker = SqliteCursorStorage::open(":memory:").expect("open cursor storage");

        let status = dispatch_for_target(
            &ledger,
            &tracker,
            &graph,
            "local-aiua-01",
            "orphan-aiua-01",
            "127.0.0.1:1",
        )
        .await
        .expect("dispatch");
        assert!(matches!(status, DispatchStatus::Idle));
    }

    /// Events pending but no mesh auth key for the target (unenrolled/orphan or
    /// a peer whose local secret is broken) → `NoAuthKey`, NOT an error. This is
    /// what lets the caller skip quietly instead of erroring every tick.
    #[tokio::test]
    async fn dispatch_for_target_no_auth_key_when_unauthenticatable() {
        let storage = SqliteGraphStorage::open(":memory:").expect("open sqlite graph");
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        let ledger = SqliteEventStorage::open(":memory:").expect("open event storage");
        let tracker = SqliteCursorStorage::open(":memory:").expect("open cursor storage");

        // A pending event for a target the graph has no auth key / hotel for.
        let mut env = unacked_event("orphan-aiua-01");
        ledger.append_event(&mut env).expect("append event");

        let status = dispatch_for_target(
            &ledger,
            &tracker,
            &graph,
            "local-aiua-01",
            "orphan-aiua-01",
            "127.0.0.1:1",
        )
        .await
        .expect("dispatch");
        assert!(matches!(status, DispatchStatus::NoAuthKey));
    }
}
