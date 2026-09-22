use ansible_mesh_core::authz::MeshAuth;
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::registry::NodeRegistry;
use ansible_mesh_core::storage::{CursorStorage, EventStorage};
use ansible_mesh_core::{BeaconMessage, MsgType};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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

/// First pause before retrying a peer that failed; it doubles per consecutive
/// failure up to [`BACKOFF_MAX`]. Without it a dead peer was dialled every
/// second forever.
const BACKOFF_BASE: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// After sending an event, wait this long for its ACK before sending it
/// again. The sender used to re-send every unacked event on every 1 s tick,
/// so a receiver that took a few seconds to ACK got the same event several
/// times (measured 2026-09-19: 1.72x overall, one event 10x).
const ACK_GRACE: Duration = Duration::from_secs(5);

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
    Dispatched {
        /// Highest event seq that reached the peer's socket this attempt
        /// (0 if none did).
        sent_through: u64,
        /// The attempt stopped at a failed send.
        send_failed: bool,
    },
}

/// What the dispatcher remembers about one peer between ticks.
#[derive(Debug, Default)]
struct TargetProgress {
    failures: u32,
    next_attempt: Option<Instant>,
    sent_through: u64,
    sent_at: Option<Instant>,
}

/// The pause after `failures` consecutive failures: 1, 2, 4, 8, 16, 30, 30…
fn backoff_after(failures: u32) -> Duration {
    BACKOFF_BASE
        .checked_mul(1u32 << failures.saturating_sub(1).min(5))
        .unwrap_or(BACKOFF_MAX)
        .min(BACKOFF_MAX)
}

impl TargetProgress {
    /// Is this peer due another attempt (not still backing off)?
    fn ready(&self, now: Instant) -> bool {
        self.next_attempt.is_none_or(|at| now >= at)
    }

    /// Events at or below this seq were sent recently enough to be waiting on
    /// their ACK and must not be sent again yet; 0 once the grace has run out.
    fn skip_through(&self, now: Instant) -> u64 {
        match self.sent_at {
            Some(at) if now.duration_since(at) < ACK_GRACE => self.sent_through,
            _ => 0,
        }
    }

    fn record(&mut self, now: Instant, status: &DispatchStatus) {
        match status {
            DispatchStatus::Idle | DispatchStatus::NoAuthKey => {}
            DispatchStatus::Dispatched {
                sent_through,
                send_failed,
            } => {
                if *sent_through > 0 {
                    self.sent_through = *sent_through;
                    self.sent_at = Some(now);
                }
                if *send_failed {
                    self.record_failure(now);
                } else {
                    self.failures = 0;
                    self.next_attempt = None;
                }
            }
        }
    }

    fn record_failure(&mut self, now: Instant) {
        self.failures = self.failures.saturating_add(1);
        self.next_attempt = Some(now + backoff_after(self.failures));
    }
}

/// One peer's slot: its own progress, and a guard so a slow peer is never
/// dispatched to twice at once.
#[derive(Default)]
struct TargetSlot {
    in_flight: AtomicBool,
    progress: Mutex<TargetProgress>,
    last_no_auth_warn: Mutex<Option<Instant>>,
}

/// Continuously polls the EventStorage and CursorStorage to dispatch durable
/// mesh events over UDP to their target nodes.
///
/// Every peer is dispatched to in its OWN task (DEF-181). Targets used to be
/// walked one after another inside the tick, so one unreachable peer — whose
/// connect waited out the OS SYN timeout — delayed every other peer's traffic
/// on every tick. Now a slow peer occupies only its own slot, a failing one
/// backs off, and events already sent are given time to be ACKed before they
/// are sent again.
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
    let mut slots: HashMap<String, Arc<TargetSlot>> = HashMap::new();

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
                let now = Instant::now();
                for (target_node_id, target_addr) in targets {
                    let slot = slots.entry(target_node_id.clone()).or_default().clone();
                    let skip_through = {
                        let progress = slot.progress.lock().unwrap_or_else(|e| e.into_inner());
                        if !progress.ready(now) {
                            continue;
                        }
                        progress.skip_through(now)
                    };
                    // Still working on the previous tick's attempt: leave it be.
                    if slot.in_flight.swap(true, Ordering::AcqRel) {
                        continue;
                    }
                    let ledger = ledger.clone();
                    let tracker = tracker.clone();
                    let graph = graph.clone();
                    let local_node_id = local_node_id.clone();
                    tokio::spawn(async move {
                        let result = dispatch_for_target(
                            ledger.as_ref(),
                            tracker.as_ref(),
                            graph.as_ref(),
                            &local_node_id,
                            &target_node_id,
                            &target_addr,
                            skip_through,
                        )
                        .await;
                        let finished = Instant::now();
                        match result {
                            Ok(status) => {
                                if matches!(status, DispatchStatus::NoAuthKey) {
                                    let mut last = slot
                                        .last_no_auth_warn
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner());
                                    let due = last
                                        .map(|at| finished.duration_since(at) >= NO_AUTH_KEY_WARN_INTERVAL)
                                        .unwrap_or(true);
                                    if due {
                                        *last = Some(finished);
                                        warn!(
                                            "skipping mesh dispatch to {}: no auth key (unauthenticated/orphan target)",
                                            target_node_id
                                        );
                                    }
                                }
                                slot.progress
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .record(finished, &status);
                            }
                            Err(e) => {
                                error!("Failed to dispatch to {}: {}", target_node_id, e);
                                slot.progress
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .record_failure(finished);
                            }
                        }
                        slot.in_flight.store(false, Ordering::Release);
                    });
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
    skip_through: u64,
) -> Result<DispatchStatus> {
    // 1. Where does the target node's cursor currently sit?
    let cursor = tracker.get_cursor(target_node_id)?;

    // 2. Query up to 50 un-acked events, minus those sent so recently that
    //    their ACK is still on its way.
    let unacked_events: Vec<_> = ledger
        .query_unacked_events(target_node_id, cursor, 50)?
        .into_iter()
        .filter(|event| event.seq > skip_through)
        .collect();

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
    let mut sent_through = 0u64;
    let mut send_failed = false;
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
                sent_through = sent_through.max(event.seq);
                debug!(
                    "Dispatched Event {} (seq: {}) to {} over execution transport",
                    event.event_id, event.seq, target_node_id
                );
            }
            Err(e) => {
                warn!(
                    "Failed to send execution packet to {} at {}: {}",
                    target_node_id, target_addr, e
                );
                // Stop here; the caller backs off and retries later, so a dead
                // peer is not dialled every second.
                send_failed = true;
                break;
            }
        }
    }

    Ok(DispatchStatus::Dispatched {
        sent_through,
        send_failed,
    })
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
                build_version: String::new(),
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
            0,
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
            0,
        )
        .await
        .expect("dispatch");
        assert!(matches!(status, DispatchStatus::NoAuthKey));
    }

    use super::{
        ACK_GRACE, BACKOFF_MAX, DispatchStatus as Status, TargetProgress, TargetSlot, backoff_after,
    };
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    #[test]
    fn a_failing_peer_backs_off_and_a_success_resets_it() {
        assert_eq!(backoff_after(1), Duration::from_secs(1));
        assert_eq!(backoff_after(2), Duration::from_secs(2));
        assert_eq!(backoff_after(4), Duration::from_secs(8));
        assert_eq!(backoff_after(6), Duration::from_secs(30));
        assert_eq!(backoff_after(60), BACKOFF_MAX, "capped, never overflowing");

        let mut progress = TargetProgress::default();
        let t0 = Instant::now();
        assert!(progress.ready(t0));
        let failed = Status::Dispatched {
            sent_through: 0,
            send_failed: true,
        };
        progress.record(t0, &failed);
        progress.record(t0, &failed);
        progress.record(t0, &failed);
        assert!(!progress.ready(t0 + Duration::from_secs(3)), "backing off");
        assert!(progress.ready(t0 + Duration::from_secs(5)));

        progress.record(
            t0 + Duration::from_secs(5),
            &Status::Dispatched {
                sent_through: 9,
                send_failed: false,
            },
        );
        assert!(
            progress.ready(t0 + Duration::from_secs(5)),
            "success resets it"
        );
    }

    /// An event just sent is left alone while its ACK is due, and re-sent once
    /// the grace has run out — a lost ACK still recovers.
    #[test]
    fn a_sent_event_waits_for_its_ack_then_is_retried() {
        let mut progress = TargetProgress::default();
        let t0 = Instant::now();
        progress.record(
            t0,
            &Status::Dispatched {
                sent_through: 42,
                send_failed: false,
            },
        );
        assert_eq!(progress.skip_through(t0 + Duration::from_secs(1)), 42);
        assert_eq!(
            progress.skip_through(t0 + ACK_GRACE - Duration::from_millis(1)),
            42
        );
        assert_eq!(
            progress.skip_through(t0 + ACK_GRACE + Duration::from_millis(1)),
            0,
            "no ACK after the grace: send it again"
        );
    }

    /// A slow peer occupies only its own slot and is never dispatched twice.
    #[test]
    fn a_peer_being_dispatched_to_is_not_dispatched_to_again() {
        let slot = TargetSlot::default();
        assert!(
            !slot.in_flight.swap(true, Ordering::AcqRel),
            "first tick takes it"
        );
        assert!(
            slot.in_flight.swap(true, Ordering::AcqRel),
            "second tick sees it busy"
        );
        slot.in_flight.store(false, Ordering::Release);
        assert!(!slot.in_flight.swap(true, Ordering::AcqRel), "free again");
    }

    /// Events inside their ACK grace are not selected for sending.
    #[tokio::test]
    async fn events_inside_the_ack_grace_are_not_resent() {
        let storage = SqliteGraphStorage::open(":memory:").expect("open sqlite graph");
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        let ledger = SqliteEventStorage::open(":memory:").expect("open event storage");
        let tracker = SqliteCursorStorage::open(":memory:").expect("open cursor storage");
        let mut env = unacked_event("peer-aiua-01");
        ledger.append_event(&mut env).expect("append");
        let seq = env.seq;
        assert!(seq > 0);

        // Already sent through this seq and awaiting its ACK: nothing to do.
        let status = dispatch_for_target(
            &ledger,
            &tracker,
            &graph,
            "local-aiua-01",
            "peer-aiua-01",
            "127.0.0.1:1",
            seq,
        )
        .await
        .expect("dispatch");
        assert!(matches!(status, Status::Idle));

        // Past the grace it is selected again (no auth key here, so it stops there).
        let status = dispatch_for_target(
            &ledger,
            &tracker,
            &graph,
            "local-aiua-01",
            "peer-aiua-01",
            "127.0.0.1:1",
            0,
        )
        .await
        .expect("dispatch");
        assert!(matches!(status, Status::NoAuthKey));
    }
}
