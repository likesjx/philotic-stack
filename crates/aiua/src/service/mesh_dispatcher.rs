use ansible_mesh_core::authz::MeshAuth;
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::registry::NodeRegistry;
use ansible_mesh_core::storage::{CursorStorage, EventStorage};
use ansible_mesh_core::{BeaconMessage, MsgType};
use anyhow::Result;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::{RwLock, broadcast};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::mesh::mesh_auth_key_for_node;
use crate::service::execution_transport::send_execution_message;

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
                    if let Err(e) = dispatch_for_target(ledger.as_ref(), tracker.as_ref(), graph.as_ref(), &local_node_id, target_node_id, target_addr).await {
                        error!("Failed to dispatch to {}: {}", target_node_id, e);
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
) -> Result<()> {
    // 1. Where does the target node's cursor currently sit?
    let cursor = tracker.get_cursor(target_node_id)?;

    // 2. Query up to 50 un-acked events
    let unacked_events = ledger.query_unacked_events(target_node_id, cursor, 50)?;

    if unacked_events.is_empty() {
        return Ok(());
    }

    debug!(
        "Found {} unacked events for {}, cursor is at seq {}",
        unacked_events.len(),
        target_node_id,
        cursor
    );

    // 3. Prepare the BeaconMessage batch (for now sending one event in the batch)
    for event in unacked_events {
        let payload = serde_json::to_vec(&vec![&event])?;
        let auth_key = mesh_auth_key_for_node(graph, local_node_id, target_node_id)?
            .ok_or_else(|| anyhow::anyhow!("no mesh auth key for node {target_node_id}"))?;

        // Wrap in BeaconMessage
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let msg_id = Uuid::new_v4();
        let hmac = MeshAuth::new(auth_key).sign(&msg_id, event.seq, &payload, ts);
        let msg = BeaconMessage {
            version: 1,
            msg_id,
            src_node: local_node_id.to_string(),
            dest_node: target_node_id.to_string(),
            msg_type: MsgType::ExecutionEventBatch,
            seq: event.seq as u32,
            total: 1,
            payload,
            timestamp: ts,
            hmac,
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

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::execution_targets;
    use ansible_mesh_core::domain::GraphDomain;
    use ansible_mesh_core::registry::{ExecutionReachability, NodeRegistry};
    use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
    use ansible_mesh_core::storage::HotelRecord;
    use ansible_mesh_core::{NodeCapabilities, NodeConstraints, NodeRole};
    use std::sync::Arc;
    use tokio::sync::RwLock;

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
}
