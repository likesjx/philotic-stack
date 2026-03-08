use ansible_mesh_core::{BeaconMessage, MsgType, cursor::CursorTracker, ledger::EventLedger};
use anyhow::Result;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};

/// Continuously polls the EventLedger and CursorTracker to dispatch durable
/// mesh events over UDP to their target nodes.
pub async fn outbound_dispatcher(
    ledger: Arc<EventLedger>,
    tracker: Arc<CursorTracker>,
    udp_socket: Arc<UdpSocket>,
    local_node_id: String,
    targets: Vec<(String, String)>, // (node_id, ip:port)
    mut shutdown_rx: broadcast::Receiver<()>,
) {
    info!(
        "Started Outbound Mesh Dispatcher Loop for {} targets.",
        targets.len()
    );

    // Poll every 1 second
    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                for (target_node_id, target_addr) in &targets {
                    if let Err(e) = dispatch_for_target(&ledger, &tracker, &udp_socket, &local_node_id, target_node_id, target_addr).await {
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

async fn dispatch_for_target(
    ledger: &EventLedger,
    tracker: &CursorTracker,
    socket: &UdpSocket,
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

        // Wrap in BeaconMessage
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_micros() as u64;
        let msg = BeaconMessage {
            version: 1,
            msg_id: uuid::Uuid::new_v4(),
            src_node: local_node_id.to_string(),
            dest_node: target_node_id.to_string(),
            msg_type: MsgType::MeshEventBatch,
            seq: event.seq as u32,
            total: 1,
            payload,
            timestamp: ts,
            hmac: vec![], // Future authz implementation
        };

        let packet_bytes = serde_json::to_vec(&msg)?;
        match socket.send_to(&packet_bytes, target_addr).await {
            Ok(bytes_sent) => {
                debug!(
                    "Dispatched Event {} (seq: {}) to {} ({} bytes)",
                    event.event_id, event.seq, target_node_id, bytes_sent
                );
            }
            Err(e) => {
                warn!("Failed to send UDP packet to {}: {}", target_node_id, e);
                // Break out of the loop and try again next tick so we don't spam errors
                break;
            }
        }
    }

    Ok(())
}
