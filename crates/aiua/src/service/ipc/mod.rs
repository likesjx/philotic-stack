use super::golgi::{GOLGI_SINK_ROLE, PendingPipelineRegistry};
use super::lease_handlers::LoggingSubagentLeaseObserver;
use crate::LedgerCommand;
use crate::service::guest_manager::{GuestMaterializationRequester, HealRestartVerdict};
use crate::service::lease::{LeaseProvider, RuntimeLeaseRegistry};
use crate::vault::{SecretAccess, SecretInput, resolve_secret, store_secret};
use ansible_mesh_core::agent_graph_storage::{
    AgentGraphSnapshot, AgentGraphStorage, SqliteAgentGraphStorage,
};
use ansible_mesh_core::catalog_rights::{
    component_right, has_right, normalize_rights, skill_right, tool_right,
};
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::event::{EventEnvelope, EventKind, EventPayload};
use ansible_mesh_core::graph::{
    AbstractSkillRecord, ModelProfileRecord, RoleIncarnationRecord, RoleReadinessState,
    SkillRegistrationAuditRecord, SkillSourceSnapshot, SkillValidationState,
};
use ansible_mesh_core::membership::{
    DEFAULT_INVITE_TTL_SECS, MeshInvite, MeshInvitePayload, MeshJoinRequestPayload,
    derive_transport_session_key, fingerprint_from_base64url, generate_nonce,
    generate_transport_keypair, now_epoch_secs, sign_invite, sign_join_request,
    signing_key_from_hex, verify_invite, verifying_key_to_base64url,
};
use ansible_mesh_core::procedure::{
    ProcedureGraphRecord, ProcedurePatchOp, ProcedurePatchRecord, ProcedurePatchStatus,
    ProcedureProvenance, ProcedureRunRecord, TrialDecision, TrialWindow, decide_trial,
    trial_runs_required,
};
use ansible_mesh_core::registry::{
    CapabilityAdvertisement, ExecutionReachability, NodeRegistry, NodeStatus,
};
use ansible_mesh_core::storage::{
    ComponentManifest, GuestRecord, HotelRecord, SessionRecord, SessionTurnRecord,
};
use ansible_mesh_core::validation::{
    SkillDraft, apply_validation_to_record, validate_skill_layer1,
};
use ansible_mesh_core::{NodeCapabilities, NodeConstraints};
use anyhow::{Context, bail};
use ed25519_dalek::SigningKey;
use philotic_client::{
    AgentMigrationBundle, ComponentInventoryEntryView, ConfigEntryExport, DesktopMembraneAgentView,
    DesktopMembraneGuestView, DesktopMembraneStatusView, DesktopMembraneTargetGuestInventoryView,
    DesktopMembraneTargetReachabilityView, DesktopMembraneTargetStatusView,
    DesktopMembraneTargetView, GuestExport, GuestIdentity, HookRoute, HookSubscription, IpcRequest,
    IpcResponse, MemoryConfigPayload, OPERATOR_CHAT_REPLY_ROLE,
    OPERATOR_SURFACE_QUERY_HANDOFF_KIND, OPERATOR_SURFACE_QUERY_REPLY_ROLE,
    OPERATOR_SURFACE_QUERY_ROLE, OperatorAgentView, OperatorChatTurnReply,
    OperatorSurfaceQueryHandoff, OperatorTargetAgentInventoryView,
    OperatorTargetComponentInventoryView, OperatorTargetGuestInventoryView,
    OperatorTargetStatusView, PhiloticClient, ResponseRoutePolicyView, RestartReason,
    VaultEntryExport,
};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UdpSocket, UnixListener, UnixStream};
use tokio::sync::{Mutex, RwLock, mpsc};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

pub(super) const OPERATOR_SURFACE_QUERY_TIMEOUT_SECS: u64 = 30;

pub(crate) type InboxRegistry = Arc<Mutex<HashMap<String, Vec<RoleSubscriber>>>>;

/// Per-subagent hook routing record stored in the hotel's in-memory registry.
/// Created at SpawnSubagent time; dropped on ReleaseSubagent or lease expiry.
#[derive(Clone)]
#[allow(dead_code)]
pub(super) struct SubagentHookRecord {
    /// Which persona agent spawned this subagent (for PersonaAgent route).
    pub(super) persona_guest_id: String,
    /// The role the persona agent registered under (for inbox lookup).
    pub(super) persona_role: String,
    /// Hook subscriptions declared by the delegation skill.
    pub(super) hook_subscriptions: Vec<HookSubscription>,
    /// Where to deliver `subagent.complete`.
    pub(super) completion_route: HookRoute,
    /// Where to deliver `subagent.failed`.
    pub(super) failure_route: HookRoute,
    /// Lease TTL (seconds) the delegation skill configured at spawn time.
    /// Used to renew the subagent lease with the same terms it was acquired under.
    pub(super) configured_ttl_secs: u64,
    /// The resolved delegation, held from SpawnSubagent until the worker
    /// accepts its lease, then delivered to its inbox (DEF-128). The worker
    /// registers ~30 ms after SpawnSubagentOk returns, and `deliver_inbound_task`
    /// does not park, so a parent that assigned immediately would lose the
    /// task — and the philote's `subagent.spawn` tool never assigned at all:
    /// live 2026-09-14 20:51 UTC two workers spawned for
    /// `music.repertoire-gardener` sat "Worker idle — waiting for
    /// SubagentDelegation…" until their leases expired.
    pub(super) pending_delegation: Option<philotic_client::SubagentDelegation>,
}

/// Maps `subagent_guest_id` → routing record.
pub(super) type SubagentHookRegistry = Arc<Mutex<HashMap<String, SubagentHookRecord>>>;

/// How many undrained outbound frames mark a subscriber as wedged. A healthy
/// guest drains its socket in milliseconds; a backlog this deep means the
/// guest event loop is stuck while its socket stays open (the 2026-07-19
/// Beacon dead-delivery mode: "Delivering … to 1 local subscriber" into a
/// process that never read another frame).
const SUBSCRIBER_BACKLOG_WEDGE_THRESHOLD: u64 = 32;

/// How long a delivered InboundTask may sit unflushed to the guest socket
/// before delivery is declared unconfirmed and a heal entry is filed.
#[cfg(not(test))]
const DELIVERY_WRITE_CONFIRM_TIMEOUT_SECS: u64 = 10;
#[cfg(test)]
const DELIVERY_WRITE_CONFIRM_TIMEOUT_SECS: u64 = 1;

/// Poll cadence for the write-confirmation watcher.
#[cfg(not(test))]
const DELIVERY_WRITE_CONFIRM_POLL_MS: u64 = 250;
#[cfg(test)]
const DELIVERY_WRITE_CONFIRM_POLL_MS: u64 = 50;

/// After the confirmation window expires the watcher keeps observing (at a
/// slower cadence) for the one outcome that makes redelivery PROVABLY safe:
/// the connection closing while the frame is still undrained. Frames drain
/// FIFO, so closed + undrained ⇒ the guest never received this task ⇒
/// re-parking cannot duplicate. Capped so watchers never outlive an episode.
#[cfg(not(test))]
const DELIVERY_LOST_WATCH_CAP_SECS: u64 = 600;
#[cfg(test)]
const DELIVERY_LOST_WATCH_CAP_SECS: u64 = 3;

/// Outbound IPC sender with frame accounting.
///
/// `enqueued` counts frames accepted into the (unbounded) channel; `drained`
/// counts frames the connection's write task actually flushed to the guest
/// socket (serialize failures also count as drained so the gauge can't
/// drift). `backlog() = enqueued − drained` is therefore "frames the guest
/// has not received yet": a send into this channel proves nothing about the
/// guest, only a drained frame does. Delivery paths use the gauge to detect
/// alive-but-wedged guests, which reader-EOF cleanup can never catch.
#[derive(Clone)]
pub(crate) struct CountedSender {
    tx: mpsc::UnboundedSender<IpcResponse>,
    enqueued: Arc<std::sync::atomic::AtomicU64>,
    drained: Arc<std::sync::atomic::AtomicU64>,
    /// Latched when the wedge threshold is crossed so heal entries file once
    /// per wedge episode, not once per delivery attempt.
    backlog_flagged: Arc<std::sync::atomic::AtomicBool>,
    /// Heal sink for delivery anomalies; `None` for detached senders
    /// (internal consumers, tests).
    heal: Option<Arc<dyn ansible_mesh_core::heal_queue::HealQueueStorage>>,
    /// Re-park registry for provably-lost frames (claim-until-confirmed):
    /// when this connection dies with a delivered InboundTask still
    /// undrained, the task is parked under the subscriber's guest id so the
    /// guest's next registration (e.g. after a `subscriber_wedged` auto
    /// restart) flushes it. `None` disables redelivery (detached senders).
    repark: Option<ParkedInboundRegistry>,
}

impl CountedSender {
    pub(crate) fn new(
        tx: mpsc::UnboundedSender<IpcResponse>,
        heal: Option<Arc<dyn ansible_mesh_core::heal_queue::HealQueueStorage>>,
        repark: Option<ParkedInboundRegistry>,
    ) -> Self {
        Self {
            tx,
            enqueued: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            drained: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            backlog_flagged: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            heal,
            repark,
        }
    }

    /// Sender with fresh counters and no heal sink — for callers that manage
    /// their own receive loop (cron ticker, role materialization, tests).
    pub(crate) fn detached(tx: &mpsc::UnboundedSender<IpcResponse>) -> Self {
        Self::new(tx.clone(), None, None)
    }

    fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    pub(crate) fn send(
        &self,
        response: IpcResponse,
    ) -> Result<(), mpsc::error::SendError<IpcResponse>> {
        self.tx.send(response).map(|()| {
            self.enqueued
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        })
    }

    /// Handle the write task uses to mark frames flushed (or dropped).
    pub(crate) fn drained_handle(&self) -> Arc<std::sync::atomic::AtomicU64> {
        Arc::clone(&self.drained)
    }

    pub(crate) fn backlog(&self) -> u64 {
        self.enqueued
            .load(std::sync::atomic::Ordering::Relaxed)
            .saturating_sub(self.drained.load(std::sync::atomic::Ordering::Relaxed))
    }

    fn enqueued_now(&self) -> u64 {
        self.enqueued.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn drained_now(&self) -> u64 {
        self.drained.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[derive(Clone)]
pub(crate) struct RoleSubscriber {
    conn_id: Uuid,
    pub(crate) guest_id: String,
    supported_tools: Vec<String>,
    pub(crate) tx: CountedSender,
}

#[cfg(not(test))]
const LOCAL_DELIVERY_PROVENANCE_TTL_SECS: u64 = 900;
#[cfg(test)]
const LOCAL_DELIVERY_PROVENANCE_TTL_SECS: u64 = 5;

/// The client-SDK fallback node id sent when `PHILOTIC_NODE_ID` is unset
/// (bare SDK use, external smoke drivers). Semantically it means "the hotel
/// this client is connected to" — EmitTask normalizes it to the local node,
/// because no node with this literal id ever exists on a real mesh.
pub(crate) const CLIENT_DEFAULT_NODE_ID: &str = "local-aiua-01";

/// Test-only ledger dispatcher channel. In production the durable-writer
/// thread in `main.rs` drains `dispatcher_rx`; unit tests do not spawn that
/// thread, so a bare bounded channel deadlocks once its buffer fills (the
/// server's `dispatcher_tx.send().await` parks forever and the test hangs).
/// This helper forwards the bounded channel into an unbounded one via a
/// background drain task, so ledger sends never block while tests can still
/// observe every `LedgerCommand` (or simply drop the receiver to sink them).
#[cfg(test)]
pub(crate) fn test_dispatcher_channel() -> (
    mpsc::Sender<LedgerCommand>,
    mpsc::UnboundedReceiver<LedgerCommand>,
) {
    let (tx, mut rx) = mpsc::channel::<LedgerCommand>(64);
    let (fwd_tx, fwd_rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(cmd) = rx.recv().await {
            let _ = fwd_tx.send(cmd);
        }
    });
    (tx, fwd_rx)
}

#[derive(Debug, Clone)]
pub(crate) struct ParkedInboundTask {
    pub(super) source_node: String,
    pub(super) task_id: Uuid,
    pub(super) task_json: String,
    pub(super) activate_session_id: Option<String>,
    /// Unix epoch (seconds) when this task was parked. A park older than
    /// [`PARKED_TASK_TTL_SECS`] is dead on arrival — the caller's turn has
    /// long since timed out — and is dropped at flush time instead of being
    /// delivered as a stale prompt to a freshly-woken guest (live 2026-08-25:
    /// a 17:29 whisper park was still parked at 18:20 and flushed alongside
    /// the fresh whisper that woke the specialist).
    pub(super) parked_at: u64,
}

/// How long a parked inbound task stays deliverable. Matches the paracrine
/// whisper wait (the longest any caller waits on a parked dispatch) plus one
/// watchdog tick of slack.
pub(crate) const PARKED_TASK_TTL_SECS: u64 = 720;

pub(crate) type ParkedInboundRegistry = Arc<Mutex<HashMap<String, Vec<ParkedInboundTask>>>>;

/// Bounded in-process set of event ids whose local delivery has already been
/// claimed by exactly one consumer.
///
/// A fired cron job's `TaskInvoke` used to be observable by two independent
/// delivery paths — `CronTicker::fire`'s own direct delivery/park and the
/// mesh/ledger consumer (`deliver_event_envelope_or_park`) reacting to the same
/// envelope — racing non-deterministically (session-18 finding: both "won" the
/// same fire on an unchanged binary). The claim set gives every event a single
/// delivery owner structurally: whichever consumer claims the `event_id` first
/// delivers, every later observer of the same envelope (mesh echo, batch
/// retransmit before ACK, ledger replay) is a no-op.
///
/// Claims are in-process only: they guard duplicate delivery *within* one hotel
/// process, which is exactly the scope of the race. Cross-hotel duplicate fires
/// for guaranteed jobs are handled separately by `last_fired_epoch`/`CronFired`.
#[derive(Debug, Default)]
pub(crate) struct DeliveryClaims {
    claimed: std::collections::HashSet<Uuid>,
    order: std::collections::VecDeque<Uuid>,
}

impl DeliveryClaims {
    /// Upper bound on remembered claims; oldest are evicted first. Sized so a
    /// claim comfortably outlives any realistic mesh retransmit window without
    /// growing unboundedly over a hotel's uptime.
    const MAX_CLAIMS: usize = 8192;

    /// Claim delivery ownership of `event_id`. Returns `true` when the caller is
    /// the first — and therefore only — delivery owner; `false` when another
    /// consumer already owns it.
    pub(crate) fn claim(&mut self, event_id: Uuid) -> bool {
        if !self.claimed.insert(event_id) {
            return false;
        }
        self.order.push_back(event_id);
        while self.order.len() > Self::MAX_CLAIMS {
            if let Some(evicted) = self.order.pop_front() {
                self.claimed.remove(&evicted);
            }
        }
        true
    }
}

/// Shared handle to the hotel-wide delivery claim set. Uses a `std` mutex —
/// the guard is never held across an `.await`.
pub(crate) type DeliveryClaimRegistry = Arc<std::sync::Mutex<DeliveryClaims>>;

pub(crate) fn new_delivery_claim_registry() -> DeliveryClaimRegistry {
    Arc::new(std::sync::Mutex::new(DeliveryClaims::default()))
}

/// Claim delivery ownership of `event_id`. Returns `true` when the caller is
/// the first — and therefore only — delivery owner.
pub(crate) fn claim_delivery(claims: &DeliveryClaimRegistry, event_id: Uuid) -> bool {
    match claims.lock() {
        Ok(mut guard) => guard.claim(event_id),
        Err(poisoned) => poisoned.into_inner().claim(event_id),
    }
}

/// Which flavor of dormant target a parked task is waiting on.
///
/// Forces the local-vs-cross-hotel materialization semantics to be an explicit
/// compile-time choice at every call site. These used to be two near-twin helpers
/// (`park_and_materialize_local_role` / `park_and_materialize_role_philote`) and
/// picking the wrong one once shipped a bug (PR #80: a cron fire targeting a local
/// role incarnation spawned a wrong-named `{hotel}:philote-{role}` guest and
/// dead-ended, because the cross-hotel helper was reused for a local target).
pub(crate) enum ParkTarget<'a> {
    /// A *local* role incarnation (single-process, lives inside the base philote),
    /// resolved via its `RoleIncarnationRecord`. Parked under `role_record.guest_id`
    /// and woken via `ensure_role_materialized` — which also wakes an
    /// already-configured dormant local guest.
    LocalRoleIncarnation {
        role_record: &'a RoleIncarnationRecord,
    },
    /// A cross-hotel `TaskInvoke` addressed to `delivery_target_guest_id`. Parked
    /// under the agent-centric guest id and materialized via the dedicated-process
    /// `{hotel}:philote-{role}` naming scheme (seeding the hotel guest record if
    /// it does not exist yet). Does not wake local single-process incarnations.
    CrossHotelGuest { agent_guest_id: &'a str },
}

/// Special sink role: CapabilityInvoke responses route here for synchronous round-trip.
pub(super) const CAPABILITY_ROUTER_ROLE: &str = "hotel:capability-router";

/// Pending synchronous capability calls keyed by turn_id (== dispatch task_id).
pub(crate) type PendingCapabilityRegistry =
    Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum AgentRouteResolution {
    Deliver(Option<String>),
    Park { guest_id: String },
}

pub struct IpcServer {
    protected_authority: Option<Arc<ansible_mesh_core::privacy_rpc::LocalAuthorityRpc>>,
    socket_path: String,
    local_node_id: String,
    dispatcher_tx: mpsc::Sender<LedgerCommand>,
    graph: Arc<GraphDomain>,
    inboxes: InboxRegistry,
    parked_inbound: ParkedInboundRegistry,
    pending_pipelines: PendingPipelineRegistry,
    pending_capability_calls: PendingCapabilityRegistry,
    materialization_requester: Option<Arc<dyn GuestMaterializationRequester>>,
    telegram_poll_leases: Arc<Mutex<RuntimeLeaseRegistry>>,
    desktop_membrane_leases: Arc<Mutex<RuntimeLeaseRegistry>>,
    mcp_membrane_leases: Arc<Mutex<RuntimeLeaseRegistry>>,
    discord_gateway_leases: Arc<Mutex<RuntimeLeaseRegistry>>,
    subagent_leases: Arc<Mutex<RuntimeLeaseRegistry>>,
    subagent_hooks: SubagentHookRegistry,
    registry: Arc<RwLock<NodeRegistry>>,
    /// Smoke-test peer socket map: node_id → UDS socket path for direct
    /// cross-hotel task forwarding without full mesh infrastructure.
    peer_sockets: Arc<RwLock<HashMap<String, String>>>,
    muninn_config: Option<Arc<memory_core::MuninnConfig>>,
    training_storage: Option<Arc<dyn ansible_mesh_core::whisper_training::WhisperTrainingStorage>>,
    heal_queue: Option<Arc<dyn ansible_mesh_core::heal_queue::HealQueueStorage>>,
    webrtc_signal_tx: Option<mpsc::Sender<ansible_mesh_core::webrtc::WebRtcSignalMessage>>,
    /// Broadcast channel for hotel-wide push events (e.g. NetworkState, MuninnStatus).
    /// The sender is cloned into each `handle_client` task for forwarding.
    network_broadcast: tokio::sync::broadcast::Sender<IpcResponse>,
    /// Tracks whether MuninnDB was reachable on the most recent probe. Shared with the
    /// probe loop and per-connection handlers for inline `RefreshMemoryConfig` probes.
    muninn_reachable: Arc<std::sync::atomic::AtomicBool>,
    /// Per-vault timestamp of the last `HealMemoryToken` mint attempt — the
    /// token self-heal mint budget. A misconfigured MuninnDB must produce one
    /// throttled escalation, not an unbounded mint loop.
    muninn_heal_attempts: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    /// In-process channel for operator surface query tasks.
    /// When set, tasks addressed to `OPERATOR_SURFACE_QUERY_ROLE` are sent here
    /// instead of through the UDS inbox registry, eliminating the self-connection.
    operator_surface_tx: Option<mpsc::Sender<String>>,
    /// Hotel network security perimeter service — wired in via `with_perimeter()`.
    perimeter_svc: Option<Arc<crate::service::perimeter::HotelPerimeterService>>,
    /// Hotel egress gateway — wired in via `with_egress()`.
    egress_gw: Option<Arc<crate::service::egress::HotelEgressGateway>>,
    /// Signal channel: send `()` to trigger a hotel-state broadcast to all backbone peers.
    /// Fired on guest register/deregister so peers stay in sync.
    hotel_state_dirty_tx: Option<mpsc::Sender<()>>,
    /// Agent-resource-broker registry (agent-resource-broker seam). Records
    /// resource grants/denials and answers routing-table queries. Inert this
    /// slice: it does NOT materialize or tear down guests — that stays with the
    /// GuestManager path. Seeded at boot via `boot_reconcile` and shared with
    /// the boot reconciler through the same `Arc`.
    resource_registry: Arc<Mutex<crate::service::resource_registry::ResourceRegistry>>,
}

impl IpcServer {
    pub(super) fn pid_exists(pid: u32) -> bool {
        ProcessCommand::new("ps")
            .arg("-p")
            .arg(pid.to_string())
            .arg("-o")
            .arg("stat=")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .map(|output| {
                if !output.status.success() {
                    return false;
                }
                let stat = String::from_utf8_lossy(&output.stdout).trim().to_string();
                !stat.is_empty() && !stat.starts_with('Z')
            })
            .unwrap_or(false)
    }

    pub(super) fn local_hotel_name(graph: &GraphDomain, local_node_id: &str) -> Option<String> {
        graph.list_hotels().ok().and_then(|hotels| {
            hotels
                .into_iter()
                .find(|hotel| hotel.capabilities.node_id == local_node_id)
                .map(|hotel| hotel.hotel_name)
        })
    }

    /// May the authenticated peer `sender_node_id` place (pre-warm, or hand
    /// continuity for) this agent's role on this hotel?
    ///
    /// Only the role's current home may move it (DEF-170). A role this
    /// hotel has never seen, or has no home for, is open to the sender — a
    /// first move has nothing to check against. A role this hotel itself is
    /// home to is not a peer's to rewrite. The home record is gossiped
    /// last-writer-wins (DEF-171), so this raises the bar rather than closing
    /// it; the sender itself is authenticated by the batch HMAC.
    pub(super) fn peer_may_place_role(
        graph: &GraphDomain,
        local_node_id: &str,
        sender_node_id: &str,
        agent_id: &str,
        role_name: &str,
    ) -> Result<(), String> {
        let home = graph
            .get_role_incarnation(agent_id, role_name)
            .ok()
            .flatten()
            .and_then(|record| record.home_node);
        let Some(home) = home else {
            return Ok(());
        };
        let home_node = Self::resolve_hotel_node_id(graph, &home).unwrap_or(home);
        if home_node == sender_node_id {
            return Ok(());
        }
        if home_node == local_node_id {
            return Err(format!(
                "role '{role_name}' of agent '{agent_id}' is home on this hotel; \
                 peer '{sender_node_id}' may not place it"
            ));
        }
        Err(format!(
            "role '{role_name}' of agent '{agent_id}' is home on '{home_node}', \
             not on the requesting peer '{sender_node_id}'"
        ))
    }

    /// Resolve a caller-supplied hotel reference to its canonical mesh
    /// `node_id`. Every cross-hotel placement tool (`role.set_home`,
    /// `transport.set_home`, `hotel.materialize_request`) documents its
    /// `target_hotel` argument with an example like `"vps-jane"` — the bare
    /// `hotel_name` — but every routing comparison in this codebase
    /// (`home_node != local_node_id`, `target_node_id` on an `EventEnvelope`)
    /// is actually keyed on the full `node_id` (e.g. `"vps-jane-aiua-01"`,
    /// `hotels.capabilities.node_id`). Passing the documented example
    /// silently fails to route anywhere — no error, no delivery (DEF-124,
    /// found live 2026-09-09 rehearsing the R3 watched-live gate).
    ///
    /// Accepts either form so both the documented example and the "correct"
    /// internal value work: an exact `node_id` match returns as-is; an exact
    /// `hotel_name` match resolves to that hotel's `node_id`. `None` if
    /// neither matches any known hotel — callers should surface that as a
    /// rejection rather than silently mis-routing.
    pub(crate) fn resolve_hotel_node_id(graph: &GraphDomain, hotel_ref: &str) -> Option<String> {
        let hotels = graph.list_hotels().ok()?;
        if hotels
            .iter()
            .any(|hotel| hotel.capabilities.node_id == hotel_ref)
        {
            return Some(hotel_ref.to_string());
        }
        hotels
            .into_iter()
            .find(|hotel| hotel.hotel_name == hotel_ref)
            .map(|hotel| hotel.capabilities.node_id)
    }

    fn desktop_membrane_status_view(
        graph: &GraphDomain,
        local_node_id: &str,
    ) -> anyhow::Result<DesktopMembraneStatusView> {
        let hotel_name = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;
        let daemon = match graph.get_hotel(&hotel_name)? {
            Some(hotel) if Self::hotel_record_pid_is_live(&hotel) => "running",
            _ => "stopped",
        };
        Ok(DesktopMembraneStatusView {
            hotel: hotel_name,
            daemon: daemon.into(),
        })
    }

    pub(super) async fn desktop_membrane_target_status_view(
        registry: &Arc<RwLock<NodeRegistry>>,
        graph: &GraphDomain,
        local_node_id: &str,
        target_node_id: &str,
    ) -> anyhow::Result<DesktopMembraneTargetStatusView> {
        let source_hotel = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;

        if target_node_id == local_node_id {
            let local_status = Self::desktop_membrane_status_view(graph, local_node_id)?;
            return Ok(DesktopMembraneTargetStatusView {
                target_node_id: local_node_id.to_string(),
                target_hotel: local_status.hotel.clone(),
                source_hotel,
                observation_kind: "local-canonical".into(),
                daemon_status: local_status.daemon,
                freshness_state: "local-now".into(),
                freshness_age_secs: 0,
                freshness_ttl_secs: 0,
                reachability: None,
                note: Some("derived from the local hotel record".into()),
            });
        }

        let guard = registry.read().await;
        let status = guard.get_node(target_node_id).ok_or_else(|| {
            anyhow::anyhow!(
                "mesh target [{target_node_id}] is not currently active in the registry"
            )
        })?;
        let target_hotel = Self::target_hotel_name(graph, status, &source_hotel);
        let reachability = status
            .execution_reachability
            .as_ref()
            .map(Self::desktop_membrane_target_reachability_view);
        let freshness_age_secs = status.last_seen.elapsed().as_secs();
        drop(guard);

        match Self::query_remote_desktop_membrane_status(
            graph,
            local_node_id,
            target_node_id,
            &target_hotel,
        )
        .await
        {
            Ok(view) => return Ok(view),
            Err(err) => {
                return Ok(DesktopMembraneTargetStatusView {
                    target_node_id: target_node_id.to_string(),
                    target_hotel,
                    source_hotel,
                    observation_kind: "remote-heartbeat-observed".into(),
                    daemon_status: "observed-reachable".into(),
                    freshness_state: "heartbeat-fresh".into(),
                    freshness_age_secs,
                    freshness_ttl_secs: NodeRegistry::freshness_ttl_secs(),
                    reachability,
                    note: Some(format!(
                        "derived from local heartbeat registry observation after remote query failed: {}",
                        err
                    )),
                });
            }
        }
    }

    fn desktop_membrane_guest_views(
        graph: &GraphDomain,
        local_node_id: &str,
    ) -> anyhow::Result<Vec<DesktopMembraneGuestView>> {
        let hotel_name = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;
        let mut guests = graph.list_guests(&hotel_name, false)?;
        guests.sort_by(|left, right| right.last_active_at.cmp(&left.last_active_at));
        Ok(guests
            .into_iter()
            .map(Self::desktop_membrane_guest_view)
            .collect())
    }

    pub(super) async fn desktop_membrane_target_guest_inventory_view(
        registry: &Arc<RwLock<NodeRegistry>>,
        graph: &GraphDomain,
        local_node_id: &str,
        target_node_id: &str,
    ) -> anyhow::Result<DesktopMembraneTargetGuestInventoryView> {
        let source_hotel = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;

        if target_node_id == local_node_id {
            let guests = Self::desktop_membrane_guest_views(graph, local_node_id)?;
            return Ok(DesktopMembraneTargetGuestInventoryView {
                target_node_id: target_node_id.to_string(),
                target_hotel: source_hotel.clone(),
                source_hotel,
                observation_kind: "local-canonical".into(),
                available: true,
                pending_remote_query_state: "none".into(),
                guests,
                note: Some("derived from the local hotel guest table".into()),
            });
        }

        let guard = registry.read().await;
        let status = guard.get_node(target_node_id).ok_or_else(|| {
            anyhow::anyhow!(
                "mesh target [{target_node_id}] is not currently active in the registry"
            )
        })?;
        let target_hotel = Self::target_hotel_name(graph, status, &source_hotel);
        drop(guard);

        match Self::query_remote_desktop_membrane_guests(
            graph,
            local_node_id,
            target_node_id,
            &target_hotel,
        )
        .await
        {
            Ok(view) => Ok(view),
            Err(err) => Ok(DesktopMembraneTargetGuestInventoryView {
                target_node_id: target_node_id.to_string(),
                target_hotel,
                source_hotel,
                observation_kind: "remote-query-failed".into(),
                available: false,
                pending_remote_query_state: "error".into(),
                guests: Vec::new(),
                note: Some(format!(
                    "remote guest inventory query failed: {}; management-plane fallback remains required until the remote query path is healthy",
                    err
                )),
            }),
        }
    }

    async fn query_remote_desktop_membrane_guests(
        graph: &GraphDomain,
        local_node_id: &str,
        target_node_id: &str,
        target_hotel: &str,
    ) -> anyhow::Result<DesktopMembraneTargetGuestInventoryView> {
        let source_hotel = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;
        let socket_path = graph
            .get_hotel(&source_hotel)?
            .map(|hotel| hotel.ipc_socket_path)
            .ok_or_else(|| anyhow::anyhow!("local hotel [{}] record missing", source_hotel))?;
        let reply_guest_id = format!("operator-surface-query-{}", Uuid::new_v4());
        let reply_role = OPERATOR_SURFACE_QUERY_REPLY_ROLE;
        let mut client = PhiloticClient::connect_at(
            &socket_path,
            GuestIdentity {
                guest_id: reply_guest_id.clone(),
                role: reply_role.into(),
                supported_tools: Vec::new(),
            },
        )
        .await?;
        match client
            .send_request(IpcRequest::SubscribeInbox {
                role: reply_role.into(),
            })
            .await?
        {
            IpcResponse::Standard { ok: true, .. } => {}
            other => anyhow::bail!("unexpected query reply inbox subscribe response: {other:?}"),
        }
        let task_json = serde_json::to_string(&OperatorSurfaceQueryHandoff {
            handoff_kind: OPERATOR_SURFACE_QUERY_HANDOFF_KIND.into(),
            surface: "operator.targets.guests".into(),
            request_id: Uuid::new_v4().to_string(),
            source_hotel: source_hotel.clone(),
            target_hotel: target_hotel.to_string(),
            target_node_id: target_node_id.to_string(),
            caller_kind: "operator_surface_adapter".into(),
            caller_id: local_node_id.to_string(),
            visibility_scope: "operator".into(),
            grant_scope: "default".into(),
            intent: "query target guest inventory".into(),
            payload: serde_json::json!({
                "target_node_id": target_node_id,
            }),
            reply_to_node: local_node_id.to_string(),
            reply_to_role: reply_role.into(),
            reply_to_guest_id: Some(reply_guest_id),
            session_id: None,
            trace: None,
        })?;
        match client
            .send_request(IpcRequest::EmitTask {
                target_node: target_node_id.to_string(),
                target_role: OPERATOR_SURFACE_QUERY_ROLE.into(),
                target_guest_id: None,
                task_json,
            })
            .await?
        {
            IpcResponse::Standard { ok: true, .. } => {}
            other => anyhow::bail!("unexpected remote guest query emit response: {other:?}"),
        }
        let task_json = Self::recv_operator_surface_reply(
            &mut client,
            OPERATOR_SURFACE_QUERY_TIMEOUT_SECS,
            "remote guest inventory",
        )
        .await?;
        let view: OperatorTargetGuestInventoryView = serde_json::from_str(&task_json)?;
        if view.target_node_id != target_node_id {
            anyhow::bail!(
                "remote guest inventory reply target mismatch: expected [{}], got [{}]",
                target_node_id,
                view.target_node_id
            );
        }
        if view.target_hotel != target_hotel {
            anyhow::bail!(
                "remote guest inventory reply hotel mismatch: expected [{}], got [{}]",
                target_hotel,
                view.target_hotel
            );
        }
        Ok(view)
    }

    async fn query_remote_desktop_membrane_status(
        graph: &GraphDomain,
        local_node_id: &str,
        target_node_id: &str,
        target_hotel: &str,
    ) -> anyhow::Result<DesktopMembraneTargetStatusView> {
        let source_hotel = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;
        let socket_path = graph
            .get_hotel(&source_hotel)?
            .map(|hotel| hotel.ipc_socket_path)
            .ok_or_else(|| anyhow::anyhow!("local hotel [{}] record missing", source_hotel))?;
        let reply_guest_id = format!("operator-surface-query-{}", Uuid::new_v4());
        let reply_role = OPERATOR_SURFACE_QUERY_REPLY_ROLE;
        let mut client = PhiloticClient::connect_at(
            &socket_path,
            GuestIdentity {
                guest_id: reply_guest_id.clone(),
                role: reply_role.into(),
                supported_tools: Vec::new(),
            },
        )
        .await?;
        match client
            .send_request(IpcRequest::SubscribeInbox {
                role: reply_role.into(),
            })
            .await?
        {
            IpcResponse::Standard { ok: true, .. } => {}
            other => anyhow::bail!("unexpected query reply inbox subscribe response: {other:?}"),
        }
        let task_json = serde_json::to_string(&OperatorSurfaceQueryHandoff {
            handoff_kind: OPERATOR_SURFACE_QUERY_HANDOFF_KIND.into(),
            surface: "operator.targets.status".into(),
            request_id: Uuid::new_v4().to_string(),
            source_hotel: source_hotel.clone(),
            target_hotel: target_hotel.to_string(),
            target_node_id: target_node_id.to_string(),
            caller_kind: "operator_surface_adapter".into(),
            caller_id: local_node_id.to_string(),
            visibility_scope: "operator".into(),
            grant_scope: "default".into(),
            intent: "query target daemon status".into(),
            payload: serde_json::json!({
                "target_node_id": target_node_id,
            }),
            reply_to_node: local_node_id.to_string(),
            reply_to_role: reply_role.into(),
            reply_to_guest_id: Some(reply_guest_id),
            session_id: None,
            trace: None,
        })?;
        match client
            .send_request(IpcRequest::EmitTask {
                target_node: target_node_id.to_string(),
                target_role: OPERATOR_SURFACE_QUERY_ROLE.into(),
                target_guest_id: None,
                task_json,
            })
            .await?
        {
            IpcResponse::Standard { ok: true, .. } => {}
            other => anyhow::bail!("unexpected remote status query emit response: {other:?}"),
        }
        let task_json = Self::recv_operator_surface_reply(
            &mut client,
            OPERATOR_SURFACE_QUERY_TIMEOUT_SECS,
            "remote target status",
        )
        .await?;
        let view: OperatorTargetStatusView = serde_json::from_str(&task_json)?;
        if view.target_node_id != target_node_id {
            anyhow::bail!(
                "remote target status reply target mismatch: expected [{}], got [{}]",
                target_node_id,
                view.target_node_id
            );
        }
        if view.target_hotel != target_hotel {
            anyhow::bail!(
                "remote target status reply hotel mismatch: expected [{}], got [{}]",
                target_hotel,
                view.target_hotel
            );
        }
        Ok(view)
    }

    async fn send_operator_chat_turn(
        registry: &Arc<RwLock<NodeRegistry>>,
        graph: &GraphDomain,
        local_node_id: &str,
        target_node_id: &str,
        target_agent_id: &str,
        operator_session_id: &str,
        conversation_id: Option<&str>,
        content: &str,
    ) -> anyhow::Result<OperatorChatTurnReply> {
        let source_hotel = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;
        let target_hotel = if target_node_id == local_node_id {
            source_hotel.clone()
        } else {
            let guard = registry.read().await;
            let status = guard.get_node(target_node_id).ok_or_else(|| {
                anyhow::anyhow!(
                    "mesh target [{target_node_id}] is not currently active in the registry"
                )
            })?;
            Self::target_hotel_name(graph, status, &source_hotel)
        };
        let socket_path = graph
            .get_hotel(&source_hotel)?
            .map(|hotel| hotel.ipc_socket_path)
            .ok_or_else(|| anyhow::anyhow!("local hotel [{}] record missing", source_hotel))?;
        let reply_guest_id = format!("operator-chat-{}", Uuid::new_v4());
        let reply_role = OPERATOR_CHAT_REPLY_ROLE;
        let mut client = PhiloticClient::connect_at(
            &socket_path,
            GuestIdentity {
                guest_id: reply_guest_id.clone(),
                role: reply_role.into(),
                supported_tools: Vec::new(),
            },
        )
        .await?;
        match client
            .send_request(IpcRequest::SubscribeInbox {
                role: reply_role.into(),
            })
            .await?
        {
            IpcResponse::Standard { ok: true, .. } => {}
            other => anyhow::bail!("unexpected operator chat inbox subscribe response: {other:?}"),
        }

        let conversation_id = conversation_id
            .map(str::to_string)
            .unwrap_or_else(|| format!("operator-chat:{operator_session_id}:{target_agent_id}"));
        let turn_id = format!("operator-chat-turn-{}", Uuid::new_v4());
        let session_id = conversation_id.clone();
        let authority_hotel = lookup_agent_authority_hotel(graph, target_agent_id);

        match client
            .send_request(IpcRequest::EmitTask {
                target_node: target_node_id.to_string(),
                target_role: "agent".into(),
                target_guest_id: Some(target_agent_id.to_string()),
                task_json: serde_json::json!({
                    "agent_id": target_agent_id,
                    "authority_hotel": authority_hotel,
                    "source": "operator_chat",
                    "transport": "operator_chat",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": conversation_id,
                    "content": content,
                    "final_reply_to": local_node_id,
                    "final_reply_role": reply_role,
                    "final_reply_guest_id": reply_guest_id
                })
                .to_string(),
            })
            .await?
        {
            IpcResponse::Standard { ok: true, .. } => {}
            other => anyhow::bail!("unexpected operator chat emit response: {other:?}"),
        }

        let mut observed_events = Vec::new();
        let mut observed_partial_replies = Vec::new();
        let payload = loop {
            let reply =
                tokio::time::timeout(std::time::Duration::from_secs(30), client.recv_task())
                    .await
                    .map_err(|_| anyhow::anyhow!("timed out waiting for operator chat reply"))??;
            // MuninnStatus and NetworkState are OOB hotel broadcasts that can arrive
            // on any connection at any time — skip them, keep waiting for InboundTask.
            if matches!(
                reply,
                IpcResponse::MuninnStatus { .. }
                    | IpcResponse::NetworkState { .. }
                    | IpcResponse::ApartmentUpdate { .. }
                    | IpcResponse::GracefulShutdown { .. }
            ) {
                continue;
            }
            let IpcResponse::InboundTask { task_json, .. } = reply else {
                anyhow::bail!("unexpected operator chat reply envelope: {reply:?}");
            };
            let payload: serde_json::Value = serde_json::from_str(&task_json)?;
            let action = payload
                .get("action")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("send_reply");
            if action == "turn_event" {
                if let Some(event) = payload.get("event").and_then(serde_json::Value::as_str) {
                    observed_events.push(event.to_string());
                }
                continue;
            }
            if action == "partial_reply" {
                if let Some(content) = payload.get("content").and_then(serde_json::Value::as_str) {
                    observed_partial_replies.push(content.to_string());
                }
                continue;
            }
            // turn_status is a progress update (e.g. "waiting_tool"), never the terminal reply.
            // Skip it and keep waiting for send_reply.
            if action == "turn_status" {
                continue;
            }
            break payload;
        };
        let reply_action = payload
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("send_reply")
            .to_string();
        let reply_content = payload
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();

        Ok(OperatorChatTurnReply {
            source_hotel,
            target_hotel,
            target_node_id: target_node_id.to_string(),
            target_agent_id: target_agent_id.to_string(),
            operator_session_id: operator_session_id.to_string(),
            conversation_id: payload
                .get("chat_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&conversation_id)
                .to_string(),
            session_id: payload
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&session_id)
                .to_string(),
            turn_id: payload
                .get("turn_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(&turn_id)
                .to_string(),
            delivery_kind: if target_node_id == local_node_id {
                "local-direct".into()
            } else {
                "router-routed".into()
            },
            reply_action,
            observed_events,
            observed_partial_replies,
            content: reply_content,
        })
    }

    // ── Session history reads (edge/operator clients) ─────────────────────────

    /// Handle [`IpcRequest::ListOperatorSessions`]: list session records from
    /// the context graph, most recent activity first.
    fn handle_list_operator_sessions(
        graph: &GraphDomain,
        target_agent_id: Option<&str>,
        limit: Option<u32>,
    ) -> IpcResponse {
        const DEFAULT_SESSION_LIMIT: usize = 50;
        const MAX_SESSION_LIMIT: usize = 500;
        let limit = limit
            .map(|l| l as usize)
            .unwrap_or(DEFAULT_SESSION_LIMIT)
            .clamp(1, MAX_SESSION_LIMIT);

        let mut sessions = match graph.list_sessions() {
            Ok(sessions) => sessions,
            Err(e) => {
                return IpcResponse::error(
                    "list_operator_sessions",
                    "STORAGE_ERROR",
                    e.to_string(),
                );
            }
        };
        if let Some(agent_id) = target_agent_id {
            sessions.retain(|s| s.primary_agent_id.as_deref() == Some(agent_id));
        }
        sessions.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.session_id.cmp(&b.session_id))
        });
        sessions.truncate(limit);

        let operator_sessions = sessions
            .into_iter()
            .map(|session| {
                let preview = Self::derive_session_preview(graph, &session.session_id);
                philotic_client::OperatorSessionView {
                    session_id: session.session_id,
                    agent_id: session.primary_agent_id,
                    transport: session.channel_kind,
                    status: session.status,
                    last_activity_at: session.updated_at,
                    title: session.channel_session_key,
                    preview,
                }
            })
            .collect();
        IpcResponse::OperatorSessionList { operator_sessions }
    }

    /// Short excerpt of the most recent turn's content for a session, if any.
    fn derive_session_preview(graph: &GraphDomain, session_id: &str) -> Option<String> {
        let mut turns = graph.list_session_turns(session_id, 0).unwrap_or_else(|e| {
            warn!("derive_session_preview: turn listing failed for [{session_id}]: {e}");
            Vec::new()
        });
        turns.sort_by(|a, b| {
            a.started_at
                .unwrap_or(0)
                .cmp(&b.started_at.unwrap_or(0))
                .then_with(|| a.turn_id.cmp(&b.turn_id))
        });
        let last = turns.pop()?;
        let content = last
            .response_json
            .as_ref()
            .and_then(|r| r.get("content"))
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                last.user_message_json
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .filter(|s| !s.trim().is_empty())
            })?;
        const PREVIEW_MAX_CHARS: usize = 160;
        let trimmed = content.trim();
        let preview: String = trimmed.chars().take(PREVIEW_MAX_CHARS).collect();
        if preview.chars().count() < trimmed.chars().count() {
            Some(format!("{preview}…"))
        } else {
            Some(preview)
        }
    }

    /// Handle [`IpcRequest::ListSessionTurns`]: list one session's turns as
    /// operator/agent messages, oldest first. Malformed stored records are
    /// skipped with a warning by the underlying `list_session_turns`.
    fn handle_list_session_turns(
        graph: &GraphDomain,
        session_id: &str,
        limit: Option<u32>,
        before_turn_id: Option<&str>,
    ) -> IpcResponse {
        const DEFAULT_TURN_LIMIT: usize = 50;
        const MAX_TURN_LIMIT: usize = 500;
        let limit = limit
            .map(|l| l as usize)
            .unwrap_or(DEFAULT_TURN_LIMIT)
            .clamp(1, MAX_TURN_LIMIT);

        // limit=0 means "all records"; ordering and pagination happen here
        // because stored node-key order (random turn-id UUIDs) is not time order.
        let mut records = match graph.list_session_turns(session_id, 0) {
            Ok(records) => records,
            Err(e) => {
                return IpcResponse::error("list_session_turns", "STORAGE_ERROR", e.to_string());
            }
        };
        records.sort_by(|a, b| {
            a.started_at
                .unwrap_or(0)
                .cmp(&b.started_at.unwrap_or(0))
                .then_with(|| a.turn_id.cmp(&b.turn_id))
        });
        if let Some(before) = before_turn_id {
            match records.iter().position(|r| r.turn_id == before) {
                Some(idx) => records.truncate(idx),
                // Unknown cursor: return an empty page so paginating clients
                // terminate instead of looping on a bad cursor.
                None => records.clear(),
            }
        }
        if records.len() > limit {
            records.drain(..records.len() - limit);
        }

        let mut session_turns = Vec::with_capacity(records.len() * 2);
        for record in &records {
            Self::expand_session_turn_views(record, &mut session_turns);
        }
        IpcResponse::SessionTurnList {
            turns_session_id: session_id.to_string(),
            session_turns,
        }
    }

    /// Expand one stored turn record into operator/agent message views.
    /// The operator message is always emitted (it carries the turn status even
    /// when content is empty); the agent reply only when it has content.
    fn expand_session_turn_views(
        record: &SessionTurnRecord,
        out: &mut Vec<philotic_client::SessionTurnView>,
    ) {
        let operator_content = record
            .user_message_json
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        out.push(philotic_client::SessionTurnView {
            turn_id: record.turn_id.clone(),
            role: "operator".into(),
            content: operator_content.to_string(),
            created_at: record.started_at,
            status: record.status.clone(),
        });
        if let Some(reply_content) = record
            .response_json
            .as_ref()
            .and_then(|r| r.get("content"))
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
        {
            out.push(philotic_client::SessionTurnView {
                turn_id: record.turn_id.clone(),
                role: "agent".into(),
                content: reply_content.to_string(),
                created_at: record.completed_at.or(record.started_at),
                status: record.status.clone(),
            });
        }
    }

    // ── Mesh roster read ───────────────────────────────────────────────────────

    /// Handle [`IpcRequest::GetMeshRoster`]: read-only roster of self plus every
    /// fresh peer in the node registry, with advertised endpoints/exposure.
    async fn handle_get_mesh_roster(
        registry: &Arc<RwLock<NodeRegistry>>,
        graph: &GraphDomain,
        local_node_id: &str,
    ) -> IpcResponse {
        let reg = registry.read().await;
        let display_names: HashMap<String, String> = reg
            .remote_hotel_states()
            .map(|state| (state.node_id.clone(), state.hotel_name.clone()))
            .collect();

        let mut peers: Vec<philotic_client::MeshRosterEntryView> = reg
            .active_nodes()
            .filter(|status| status.capabilities.node_id != local_node_id)
            .map(|status| {
                Self::mesh_roster_entry(
                    status,
                    display_names.get(&status.capabilities.node_id).cloned(),
                    false,
                )
            })
            .collect();
        peers.sort_by(|a, b| a.node_id.cmp(&b.node_id));

        let self_display_name = Self::local_hotel_name(graph, local_node_id);
        let self_entry = match reg.get_node(local_node_id) {
            Some(status) => Self::mesh_roster_entry(status, self_display_name, true),
            None => philotic_client::MeshRosterEntryView {
                node_id: local_node_id.to_string(),
                is_self: true,
                display_name: self_display_name,
                roles: Vec::new(),
                exposure_ceiling: None,
                endpoints: Vec::new(),
            },
        };
        drop(reg);

        let mut mesh_roster = Vec::with_capacity(peers.len() + 1);
        mesh_roster.push(self_entry);
        mesh_roster.extend(peers);
        IpcResponse::MeshRosterView { mesh_roster }
    }

    fn mesh_roster_entry(
        status: &NodeStatus,
        display_name: Option<String>,
        is_self: bool,
    ) -> philotic_client::MeshRosterEntryView {
        fn snake_case_name<T: serde::Serialize + std::fmt::Debug>(value: &T) -> String {
            match serde_json::to_value(value) {
                Ok(serde_json::Value::String(s)) => s,
                _ => format!("{value:?}"),
            }
        }

        let roles = status
            .capabilities
            .roles
            .iter()
            .map(snake_case_name)
            .collect();

        let perimeter = status
            .node_health
            .as_ref()
            .and_then(|health| health.perimeter.as_ref());
        let exposure_ceiling = perimeter.map(|p| snake_case_name(&p.ceiling));
        let mut endpoints: Vec<philotic_client::MeshEndpointView> = perimeter
            .map(|p| {
                p.listeners
                    .iter()
                    .map(|listener| philotic_client::MeshEndpointView {
                        purpose: listener.purpose.clone(),
                        host: listener.bind_addr.to_string(),
                        port: listener.port,
                        tier: Some(snake_case_name(&listener.tier)),
                        protocol: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        if let Some(reach) = status.execution_reachability.as_ref() {
            endpoints.push(philotic_client::MeshEndpointView {
                purpose: "execution".into(),
                host: reach.host.clone(),
                port: reach.port,
                tier: None,
                protocol: Some(reach.protocol.clone()),
            });
        }

        philotic_client::MeshRosterEntryView {
            node_id: status.capabilities.node_id.clone(),
            is_self,
            display_name,
            roles,
            exposure_ceiling,
            endpoints,
        }
    }

    fn desktop_membrane_guest_view(guest: GuestRecord) -> DesktopMembraneGuestView {
        let pid_live = guest
            .active_pid
            .as_deref()
            .and_then(|pid| pid.parse::<u32>().ok())
            .map(Self::pid_exists)
            .unwrap_or(false);
        let status = if guest.is_active && pid_live {
            "running"
        } else if guest.is_active {
            "stopped"
        } else {
            "inactive"
        };

        DesktopMembraneGuestView {
            name: Self::guest_role_display_name(&guest.role),
            guest_id: guest.guest_id,
            role: guest.role,
            pid: guest.active_pid,
            status: status.into(),
            uptime: None,
        }
    }

    pub(super) fn operator_agent_views(
        graph: &GraphDomain,
        local_node_id: &str,
    ) -> anyhow::Result<Vec<OperatorAgentView>> {
        let hotel_name = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;
        let mut seen = std::collections::HashSet::new();
        let mut agents = graph
            .list_agent_identities()?
            .into_iter()
            .filter(|identity| identity.authority_hotel == hotel_name)
            .filter(|identity| seen.insert(identity.agent_id.clone()))
            .map(Self::desktop_membrane_agent_view)
            .collect::<Vec<_>>();
        agents.sort_by(|left, right| left.agent_id.cmp(&right.agent_id));
        Ok(agents)
    }

    fn desktop_membrane_agent_views(
        graph: &GraphDomain,
        local_node_id: &str,
    ) -> anyhow::Result<Vec<DesktopMembraneAgentView>> {
        Self::operator_agent_views(graph, local_node_id)
    }

    fn operator_agent_view(
        identity: ansible_mesh_core::storage::AgentIdentityRecord,
    ) -> OperatorAgentView {
        let str_vec = |key: &str| {
            identity
                .bundle_json
                .get(key)
                .and_then(serde_json::Value::as_array)
                .map(|arr| {
                    arr.iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let str_opt = |key: &str| {
            identity
                .bundle_json
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        let import_workspace = str_opt("import_workspace");
        let response_route_policy = identity
            .bundle_json
            .get("response_route_policy")
            .and_then(serde_json::Value::as_object)
            .and_then(|policy| policy.get("default_route"))
            .and_then(serde_json::Value::as_str)
            .map(|default_route| ResponseRoutePolicyView {
                default_route: default_route.to_string(),
            });

        OperatorAgentView {
            agent_id: identity.agent_id,
            persona_name: identity.persona_name,
            authority_hotel: identity.authority_hotel,
            soul_text: str_opt("soul_text"),
            identity_text: str_opt("identity_text"),
            user_context_text: str_opt("user_context_text"),
            system_prompt: str_opt("system_prompt"),
            import_workspace,
            toolset_tags: str_vec("toolset_tags"),
            default_toolset: str_vec("default_toolset"),
            default_skillset: str_vec("default_skillset"),
            response_route_policy,
            active_session: false,
        }
    }

    fn desktop_membrane_agent_view(
        identity: ansible_mesh_core::storage::AgentIdentityRecord,
    ) -> DesktopMembraneAgentView {
        Self::operator_agent_view(identity)
    }

    pub(super) async fn desktop_membrane_target_views(
        registry: &Arc<RwLock<NodeRegistry>>,
        graph: &GraphDomain,
        local_node_id: &str,
    ) -> anyhow::Result<Vec<DesktopMembraneTargetView>> {
        let source_hotel = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;
        let freshness_ttl_secs = NodeRegistry::freshness_ttl_secs();
        let guard = registry.read().await;
        let mut targets = guard
            .active_nodes()
            .map(|status| {
                Self::desktop_membrane_target_view(
                    graph,
                    status,
                    local_node_id,
                    &source_hotel,
                    freshness_ttl_secs,
                )
            })
            .collect::<Vec<_>>();
        targets.sort_by(|left, right| {
            left.is_local
                .cmp(&right.is_local)
                .reverse()
                .then_with(|| left.target_hotel.cmp(&right.target_hotel))
                .then_with(|| left.target_node_id.cmp(&right.target_node_id))
        });
        Ok(targets)
    }

    fn desktop_membrane_target_view(
        graph: &GraphDomain,
        status: &NodeStatus,
        local_node_id: &str,
        source_hotel: &str,
        freshness_ttl_secs: u64,
    ) -> DesktopMembraneTargetView {
        let target_hotel = Self::target_hotel_name(graph, status, source_hotel);
        let mut advertised_roles = status
            .advertisements
            .iter()
            .map(|advertisement| advertisement.target_role.clone())
            .collect::<Vec<_>>();
        advertised_roles.sort();
        advertised_roles.dedup();

        DesktopMembraneTargetView {
            target_node_id: status.capabilities.node_id.clone(),
            target_hotel,
            source_hotel: source_hotel.to_string(),
            is_local: status.capabilities.node_id == local_node_id,
            roles: status
                .capabilities
                .roles
                .iter()
                .map(Self::node_role_display_name)
                .collect(),
            models: status.capabilities.models.clone(),
            tools: status.capabilities.tools.clone(),
            advertised_roles,
            freshness_state: "heartbeat-fresh".into(),
            freshness_age_secs: status.last_seen.elapsed().as_secs(),
            freshness_ttl_secs,
            reachability: status
                .execution_reachability
                .as_ref()
                .map(Self::desktop_membrane_target_reachability_view),
        }
    }

    pub(super) fn target_hotel_name(
        graph: &GraphDomain,
        status: &NodeStatus,
        source_hotel: &str,
    ) -> String {
        status
            .advertisements
            .first()
            .map(|advertisement| advertisement.hotel_id.clone())
            .or_else(|| {
                graph.list_hotels().ok().and_then(|hotels| {
                    hotels
                        .into_iter()
                        .find(|hotel| hotel.capabilities.node_id == status.capabilities.node_id)
                        .map(|hotel| hotel.hotel_name)
                })
            })
            .unwrap_or_else(|| source_hotel.to_string())
    }

    pub(super) async fn best_place_to_run_view(
        registry: &Arc<RwLock<NodeRegistry>>,
        graph: &GraphDomain,
        local_node_id: &str,
        agent_id: Option<&str>,
        role_name: Option<&str>,
        tool_name: Option<&str>,
        required_markers: &[String],
        prefer_locality: bool,
    ) -> anyhow::Result<serde_json::Value> {
        let source_hotel = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;
        // R4 (G8): this hotel's own build, for the version-compatibility
        // ranking signal below. Empty if this hotel's own record predates
        // the `build_version` field — treated as unknown, not a mismatch.
        let source_build_version = graph
            .get_hotel(&source_hotel)
            .ok()
            .flatten()
            .map(|hotel| hotel.capabilities.build_version)
            .unwrap_or_default();

        if let (Some(agent_id), Some(role_name)) = (agent_id, role_name) {
            if let Some(role) = graph.get_role_incarnation(agent_id, role_name)? {
                if let Some(home_node) = role.home_node {
                    return Ok(serde_json::json!({
                        "recommended_node_id": home_node,
                        "recommended_hotel": graph
                            .list_hotels()?
                            .into_iter()
                            .find(|hotel| hotel.capabilities.node_id == home_node)
                            .map(|hotel| hotel.hotel_name)
                            .unwrap_or_else(|| source_hotel.clone()),
                        "decision_basis": "role_home_pin",
                        "reason": format!(
                            "role [{}:{}] is explicitly pinned to a home hotel",
                            agent_id, role_name
                        ),
                        "candidates": []
                    }));
                }
            }
        }

        let mut markers = required_markers.to_vec();
        if let Some(tool_name) = tool_name {
            if let Some(tool) = graph.get_abstract_tool(tool_name)? {
                markers.extend(tool.tool_markers);
            }
        }
        markers.sort();
        markers.dedup();

        if markers
            .iter()
            .any(|marker| marker == "local_only" || marker == "desktop_bound")
        {
            return Ok(serde_json::json!({
                "recommended_node_id": local_node_id,
                "recommended_hotel": source_hotel,
                "decision_basis": "local_marker_policy",
                "reason": "required markers force local execution on the current hotel",
                "candidates": [{
                    "node_id": local_node_id,
                    "hotel_name": source_hotel,
                    "score": 100,
                    "healthy": true,
                    "tool_match": true
                }]
            }));
        }

        let guard = registry.read().await;
        let mut candidates = Vec::new();
        candidates.push(serde_json::json!({
            "node_id": local_node_id,
            "hotel_name": source_hotel,
            "score": if prefer_locality { 35 } else { 10 },
            "healthy": true,
            "tool_match": tool_name.is_none(),
            "reason": if prefer_locality {
                "local canonical hotel gets a locality preference"
            } else {
                "local canonical hotel remains a valid fallback"
            }
        }));

        // R4 (Feasibility and placement, G8): the primary model-controller
        // role this placement would need, so candidates missing a live
        // controller for it can be penalized rather than silently ranked as
        // if any candidate were equally viable. Best-effort from gossiped
        // state (a candidate hasn't been asked yet, unlike the authoritative
        // target-side check in `evaluate_role_relocation_feasibility`).
        let primary_controller_tier = agent_id.zip(role_name).and_then(|(agent_id, role_name)| {
            graph
                .get_role_incarnation(agent_id, role_name)
                .ok()
                .flatten()
                .map(|role| {
                    role.turn_loop_config
                        .fallback_tiers
                        .first()
                        .cloned()
                        .unwrap_or_else(|| {
                            ansible_mesh_core::model_routing::DEFAULT_FALLBACK_TIERS[0].to_string()
                        })
                })
        });

        for status in guard.active_nodes() {
            let healthy = guard.is_node_healthy(&status.capabilities.node_id);
            let tool_match = tool_name.map_or(true, |needle| {
                status.capabilities.tools.iter().any(|tool| tool == needle)
            });
            let role_match = role_name.map_or(false, |needle| {
                status
                    .advertisements
                    .iter()
                    .any(|advertisement| advertisement.target_role == needle)
            });
            let mut score = 0i32;
            if status.execution_reachability.is_some() {
                score += 25;
            }
            if healthy {
                score += 20;
            } else {
                score -= 50;
            }
            if tool_match {
                score += 35;
            }
            if role_match {
                score += 20;
            }
            if prefer_locality && status.capabilities.node_id == local_node_id {
                score += 25;
            }
            if status
                .capabilities
                .roles
                .iter()
                .any(|role| matches!(role, ansible_mesh_core::NodeRole::BatteryConstrained))
            {
                score -= 10;
            }

            // R4 (G8): headroom. `node_health` is `None` for a peer that has
            // never reported (older build, or not seen yet) — treated as
            // neutral, never penalized for silence.
            let mem_free_pct = status.node_health.as_ref().and_then(|h| h.mem_free_pct);
            let disk_free_pct = status.node_health.as_ref().and_then(|h| h.disk_free_pct);
            let low_headroom = mem_free_pct.is_some_and(|pct| pct < 10.0)
                || disk_free_pct.is_some_and(|pct| pct < 10.0);
            let ample_headroom = mem_free_pct.is_some_and(|pct| pct > 30.0)
                && disk_free_pct.is_some_and(|pct| pct > 30.0);
            if low_headroom {
                score -= 20;
            } else if ample_headroom {
                score += 10;
            }

            // R4 (G8): max_concurrent_jobs. Only meaningful when both the
            // candidate's declared capacity and its current guest count are
            // known; silent otherwise rather than guessing.
            let at_capacity = match (
                status.capabilities.constraints.max_concurrent_jobs,
                status.node_health.as_ref().and_then(|h| h.guest_count),
            ) {
                (Some(max), Some(count)) => count >= max,
                _ => false,
            };
            if at_capacity {
                score -= 25;
            }

            // R4 (G8): controller resource presence. Gossiped guest roster
            // (`HotelStateSync`) is the same data `evaluate_role_relocation_feasibility`
            // checks live on the target — here it's a ranking signal, not a
            // hard gate, since a stale gossip snapshot could be wrong in
            // either direction and this is only choosing whom to ask.
            let controller_present = primary_controller_tier.as_deref().map(|tier| {
                guard
                    .remote_hotel_states()
                    .find(|remote| remote.node_id == status.capabilities.node_id)
                    .is_some_and(|remote| {
                        remote
                            .guests
                            .iter()
                            .any(|guest| guest.role == tier && guest.active)
                    })
            });
            match controller_present {
                Some(true) => score += 10,
                Some(false) => score -= 30,
                None => {}
            }

            // R4 (G8): version compatibility. Empty `build_version` means an
            // older build (field didn't exist yet) — unknown, not a mismatch.
            let version_compatible = if source_build_version.is_empty()
                || status.capabilities.build_version.is_empty()
            {
                None
            } else {
                Some(status.capabilities.build_version == source_build_version)
            };
            if version_compatible == Some(false) {
                score -= 40;
            }

            candidates.push(serde_json::json!({
                "node_id": status.capabilities.node_id,
                "hotel_name": Self::target_hotel_name(graph, status, &source_hotel),
                "score": score,
                "healthy": healthy,
                "tool_match": tool_match,
                "role_match": role_match,
                "execution_reachable": status.execution_reachability.is_some(),
                "controller_present": controller_present,
                "at_capacity": at_capacity,
                "low_headroom": low_headroom,
                "version_compatible": version_compatible,
            }));
        }
        drop(guard);

        candidates.sort_by(|left, right| {
            let left_score = left
                .get("score")
                .and_then(|value| value.as_i64())
                .unwrap_or(0);
            let right_score = right
                .get("score")
                .and_then(|value| value.as_i64())
                .unwrap_or(0);
            right_score.cmp(&left_score)
        });

        let recommended = candidates.first().cloned().unwrap_or_else(|| {
            serde_json::json!({
                "node_id": local_node_id,
                "hotel_name": source_hotel,
                "score": 0
            })
        });
        Ok(serde_json::json!({
            "recommended_node_id": recommended.get("node_id").and_then(|value| value.as_str()).unwrap_or(local_node_id),
            "recommended_hotel": recommended.get("hotel_name").and_then(|value| value.as_str()).unwrap_or(source_hotel.as_str()),
            "decision_basis": "registry_health_and_locality",
            "reason": "ranked live mesh candidates using role home pins, locality policy, health, reachability, and tool affinity",
            "required_markers": markers,
            "candidates": candidates,
        }))
    }

    fn desktop_membrane_target_reachability_view(
        reachability: &ExecutionReachability,
    ) -> DesktopMembraneTargetReachabilityView {
        DesktopMembraneTargetReachabilityView {
            protocol: reachability.protocol.clone(),
            host: reachability.host.clone(),
            port: reachability.port,
        }
    }

    fn node_role_display_name(role: &ansible_mesh_core::NodeRole) -> String {
        match role {
            ansible_mesh_core::NodeRole::PersonalDevice => "personal-device".into(),
            ansible_mesh_core::NodeRole::BatteryConstrained => "battery-constrained".into(),
            ansible_mesh_core::NodeRole::ModelNode => "model-node".into(),
            ansible_mesh_core::NodeRole::McpNode => "mcp-node".into(),
            ansible_mesh_core::NodeRole::StorageNode => "storage-node".into(),
            ansible_mesh_core::NodeRole::ModelManager => "model-manager".into(),
            ansible_mesh_core::NodeRole::AnsibleNode => "ansible-node".into(),
            ansible_mesh_core::NodeRole::InfraController => "infra-controller".into(),
            ansible_mesh_core::NodeRole::Other(other) => other.clone(),
        }
    }

    fn guest_role_display_name(role: &str) -> String {
        role.split('.')
            .last()
            .map(|segment| {
                let mut chars = segment.chars();
                match chars.next() {
                    None => String::new(),
                    Some(first) => first.to_uppercase().to_string() + chars.as_str(),
                }
            })
            .unwrap_or_else(|| role.to_string())
    }

    fn hotel_record_pid_is_live(hotel: &HotelRecord) -> bool {
        hotel
            .active_pid
            .as_deref()
            .and_then(|pid| pid.parse::<u32>().ok())
            .map(Self::pid_exists)
            .unwrap_or(false)
    }

    async fn write_frame<W: AsyncWriteExt + Unpin>(
        writer: &mut W,
        payload: &[u8],
    ) -> std::io::Result<()> {
        let len = u32::try_from(payload.len())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "frame too large"))?;
        writer.write_all(&len.to_be_bytes()).await?;
        writer.write_all(payload).await?;
        Ok(())
    }

    async fn read_frame<R: AsyncReadExt + Unpin>(
        reader: &mut R,
    ) -> std::io::Result<Option<Vec<u8>>> {
        let mut len_buf = [0u8; 4];
        match reader.read_exact(&mut len_buf).await {
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(err) => return Err(err),
        }

        let len = u32::from_be_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        reader.read_exact(&mut buf).await?;
        Ok(Some(buf))
    }

    pub fn new(
        socket_path: impl Into<String>,
        local_node_id: impl Into<String>,
        dispatcher_tx: mpsc::Sender<LedgerCommand>,
        graph: Arc<GraphDomain>,
    ) -> Self {
        let (network_broadcast, _) = tokio::sync::broadcast::channel(16);
        Self {
            protected_authority: None,
            socket_path: socket_path.into(),
            local_node_id: local_node_id.into(),
            dispatcher_tx,
            graph,
            inboxes: Arc::new(Mutex::new(HashMap::new())),
            parked_inbound: Arc::new(Mutex::new(HashMap::new())),
            pending_pipelines: Arc::new(Mutex::new(HashMap::new())),
            pending_capability_calls: Arc::new(Mutex::new(HashMap::new())),
            materialization_requester: None,
            telegram_poll_leases: Arc::new(Mutex::new(RuntimeLeaseRegistry::default())),
            desktop_membrane_leases: Arc::new(Mutex::new(RuntimeLeaseRegistry::default())),
            mcp_membrane_leases: Arc::new(Mutex::new(RuntimeLeaseRegistry::default())),
            discord_gateway_leases: Arc::new(Mutex::new(RuntimeLeaseRegistry::default())),
            subagent_leases: Arc::new(Mutex::new(RuntimeLeaseRegistry::default())),
            subagent_hooks: Arc::new(Mutex::new(HashMap::new())),
            registry: Arc::new(RwLock::new(NodeRegistry::new())),
            peer_sockets: Arc::new(RwLock::new(HashMap::new())),
            muninn_config: None,
            training_storage: None,
            heal_queue: None,
            webrtc_signal_tx: None,
            network_broadcast,
            muninn_reachable: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            muninn_heal_attempts: Arc::new(Mutex::new(HashMap::new())),
            operator_surface_tx: None,
            perimeter_svc: None,
            egress_gw: None,
            hotel_state_dirty_tx: None,
            resource_registry: Arc::new(Mutex::new(
                crate::service::resource_registry::ResourceRegistry::new(),
            )),
        }
    }

    /// Opt-in: one canonical authority shared with supervisor/context/voice.
    /// Legacy Register remains unauthenticated and cannot issue protected handles.
    pub fn with_protected_authority(
        mut self,
        authority: Arc<ansible_mesh_core::privacy_rpc::LocalAuthorityRpc>,
    ) -> Self {
        self.protected_authority = Some(authority);
        self
    }

    pub fn with_hotel_state_dirty_tx(mut self, tx: mpsc::Sender<()>) -> Self {
        self.hotel_state_dirty_tx = Some(tx);
        self
    }

    /// Share a boot-seeded resource registry with the front desk. The same
    /// `Arc` is populated by `boot_reconcile` at startup so live IPC queries
    /// see the demand-derived tenancy state, not an empty table.
    pub fn with_resource_registry(
        mut self,
        registry: Arc<Mutex<crate::service::resource_registry::ResourceRegistry>>,
    ) -> Self {
        self.resource_registry = registry;
        self
    }

    pub fn with_operator_surface_channel(mut self, tx: mpsc::Sender<String>) -> Self {
        self.operator_surface_tx = Some(tx);
        self
    }

    pub fn with_training_storage(
        mut self,
        storage: Arc<dyn ansible_mesh_core::whisper_training::WhisperTrainingStorage>,
    ) -> Self {
        self.training_storage = Some(storage);
        self
    }

    pub fn with_heal_queue(
        mut self,
        hq: Arc<dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
    ) -> Self {
        self.heal_queue = Some(hq);
        self
    }

    /// Returns a sender for the hotel-wide broadcast channel.
    /// Clone this before spawning `run()` to push `NetworkState` events to all connected guests.
    pub fn network_broadcast_tx(&self) -> tokio::sync::broadcast::Sender<IpcResponse> {
        self.network_broadcast.clone()
    }

    pub fn with_memory_config(mut self, config: Option<Arc<memory_core::MuninnConfig>>) -> Self {
        self.muninn_config = config;
        self
    }

    pub fn with_materialization_requester(
        mut self,
        materialization_requester: Arc<dyn GuestMaterializationRequester>,
    ) -> Self {
        self.materialization_requester = Some(materialization_requester);
        self
    }

    pub fn with_webrtc_signal_tx(
        mut self,
        webrtc_signal_tx: mpsc::Sender<ansible_mesh_core::webrtc::WebRtcSignalMessage>,
    ) -> Self {
        self.webrtc_signal_tx = Some(webrtc_signal_tx);
        self
    }

    pub fn with_registry(mut self, registry: Arc<RwLock<NodeRegistry>>) -> Self {
        self.registry = registry;
        self
    }

    pub fn with_perimeter(
        mut self,
        svc: Arc<crate::service::perimeter::HotelPerimeterService>,
    ) -> Self {
        self.perimeter_svc = Some(svc);
        self
    }

    pub fn with_egress(mut self, gw: Arc<crate::service::egress::HotelEgressGateway>) -> Self {
        self.egress_gw = Some(gw);
        self
    }

    pub(crate) fn inboxes(&self) -> InboxRegistry {
        self.inboxes.clone()
    }

    pub(crate) fn parked_inbound(&self) -> ParkedInboundRegistry {
        self.parked_inbound.clone()
    }

    pub(crate) fn pending_pipelines(&self) -> PendingPipelineRegistry {
        self.pending_pipelines.clone()
    }

    pub(crate) fn materialization_requester_arc(
        &self,
    ) -> Option<Arc<dyn GuestMaterializationRequester>> {
        self.materialization_requester.clone()
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let path = Path::new(&self.socket_path);

        if path.exists() {
            std::fs::remove_file(path)?;
        }

        let listener = UnixListener::bind(path)?;
        info!("Hotel Front Desk (UDS) listening on: {}", self.socket_path);

        // Golgi pipeline TTL watchdog — evicts entries that never received a capability reply.
        {
            let pending_pipelines = self.pending_pipelines.clone();
            let inboxes = self.inboxes.clone();
            let local_node_id = self.local_node_id.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
                loop {
                    interval.tick().await;
                    Self::golgi_pipeline_watchdog(&pending_pipelines, &inboxes, &local_node_id)
                        .await;
                }
            });
        }

        // MuninnDB reachability probe — checks every 60s and broadcasts MuninnStatus on flip.
        // Pushes to heal queue when the endpoint goes down so the heal-dispatcher can act.
        if let Some(cfg) = self.muninn_config.clone() {
            let reachable = self.muninn_reachable.clone();
            let broadcast_tx = self.network_broadcast.clone();
            let heal_queue = self.heal_queue.clone();
            tokio::spawn(async move {
                Self::run_muninn_probe_loop(cfg, reachable, broadcast_tx, heal_queue).await;
            });
        }

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let dispatcher = self.dispatcher_tx.clone();
                    let local_node_id = self.local_node_id.clone();
                    let graph = self.graph.clone();
                    let inboxes = self.inboxes.clone();
                    let parked_inbound = self.parked_inbound.clone();
                    let pending_pipelines = self.pending_pipelines.clone();
                    let pending_capability_calls = self.pending_capability_calls.clone();
                    let materialization_requester = self.materialization_requester.clone();
                    let telegram_poll_leases = self.telegram_poll_leases.clone();
                    let desktop_membrane_leases = self.desktop_membrane_leases.clone();
                    let mcp_membrane_leases = self.mcp_membrane_leases.clone();
                    let discord_gateway_leases = self.discord_gateway_leases.clone();
                    let subagent_leases = self.subagent_leases.clone();
                    let subagent_hooks = self.subagent_hooks.clone();
                    let registry = self.registry.clone();
                    let peer_sockets = self.peer_sockets.clone();
                    let muninn_config = self.muninn_config.clone();
                    let muninn_reachable = self.muninn_reachable.clone();
                    let muninn_heal_attempts = self.muninn_heal_attempts.clone();
                    let training_storage = self.training_storage.clone();
                    let heal_queue = self.heal_queue.clone();
                    let webrtc_signal_tx = self.webrtc_signal_tx.clone();
                    let network_broadcast_tx = self.network_broadcast.clone();
                    let network_broadcast_rx = self.network_broadcast.subscribe();
                    let operator_surface_tx = self.operator_surface_tx.clone();
                    let socket_path = self.socket_path.clone();
                    let perimeter_svc = self.perimeter_svc.clone();
                    let egress_gw = self.egress_gw.clone();
                    let hotel_state_dirty_tx = self.hotel_state_dirty_tx.clone();
                    let resource_registry = self.resource_registry.clone();
                    let protected_authority = self.protected_authority.clone();
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_client(
                            stream,
                            local_node_id,
                            dispatcher,
                            graph,
                            inboxes,
                            parked_inbound,
                            pending_pipelines,
                            pending_capability_calls,
                            materialization_requester,
                            telegram_poll_leases,
                            desktop_membrane_leases,
                            mcp_membrane_leases,
                            discord_gateway_leases,
                            subagent_leases,
                            subagent_hooks,
                            registry,
                            peer_sockets,
                            muninn_config,
                            muninn_reachable,
                            muninn_heal_attempts,
                            training_storage,
                            heal_queue,
                            webrtc_signal_tx,
                            network_broadcast_rx,
                            network_broadcast_tx,
                            operator_surface_tx,
                            socket_path,
                            perimeter_svc,
                            egress_gw,
                            hotel_state_dirty_tx,
                            resource_registry,
                            protected_authority,
                        )
                        .await
                        {
                            error!("IPC client connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
                    // EMFILE / ENFILE: FD table is full. Back off to let existing
                    // connections drain rather than spinning and burning CPU.
                    if e.raw_os_error() == Some(24) || e.raw_os_error() == Some(23) {
                        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                    }
                    error!("IPC listener accept error: {}", e);
                }
            }
        }
    }

    async fn handle_client(
        stream: UnixStream,
        local_node_id: String,
        dispatcher_tx: mpsc::Sender<LedgerCommand>,
        graph: Arc<GraphDomain>,
        inboxes: InboxRegistry,
        parked_inbound: Arc<Mutex<HashMap<String, Vec<ParkedInboundTask>>>>,
        pending_pipelines: PendingPipelineRegistry,
        pending_capability_calls: PendingCapabilityRegistry,
        materialization_requester: Option<Arc<dyn GuestMaterializationRequester>>,
        telegram_poll_leases: Arc<Mutex<RuntimeLeaseRegistry>>,
        desktop_membrane_leases: Arc<Mutex<RuntimeLeaseRegistry>>,
        mcp_membrane_leases: Arc<Mutex<RuntimeLeaseRegistry>>,
        discord_gateway_leases: Arc<Mutex<RuntimeLeaseRegistry>>,
        subagent_leases: Arc<Mutex<RuntimeLeaseRegistry>>,
        subagent_hooks: SubagentHookRegistry,
        registry: Arc<RwLock<NodeRegistry>>,
        peer_sockets: Arc<RwLock<HashMap<String, String>>>,
        muninn_config: Option<Arc<memory_core::MuninnConfig>>,
        muninn_reachable: Arc<std::sync::atomic::AtomicBool>,
        muninn_heal_attempts: Arc<Mutex<HashMap<String, std::time::Instant>>>,
        training_storage: Option<
            Arc<dyn ansible_mesh_core::whisper_training::WhisperTrainingStorage>,
        >,
        heal_queue: Option<Arc<dyn ansible_mesh_core::heal_queue::HealQueueStorage>>,
        webrtc_signal_tx: Option<mpsc::Sender<ansible_mesh_core::webrtc::WebRtcSignalMessage>>,
        network_broadcast_rx: tokio::sync::broadcast::Receiver<IpcResponse>,
        network_broadcast_tx: tokio::sync::broadcast::Sender<IpcResponse>,
        operator_surface_tx: Option<mpsc::Sender<String>>,
        socket_path: String,
        perimeter_svc: Option<Arc<crate::service::perimeter::HotelPerimeterService>>,
        egress_gw: Option<Arc<crate::service::egress::HotelEgressGateway>>,
        hotel_state_dirty_tx: Option<mpsc::Sender<()>>,
        resource_registry: Arc<Mutex<crate::service::resource_registry::ResourceRegistry>>,
        protected_authority: Option<Arc<ansible_mesh_core::privacy_rpc::LocalAuthorityRpc>>,
    ) -> anyhow::Result<()> {
        let conn_id = Uuid::new_v4();
        let (mut reader, mut writer) = stream.into_split();
        let (raw_outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<IpcResponse>();
        let outbound_tx = CountedSender::new(
            raw_outbound_tx,
            heal_queue.clone(),
            Some(parked_inbound.clone()),
        );
        let drained_frames = outbound_tx.drained_handle();
        let write_task = tokio::spawn(async move {
            while let Some(response) = outbound_rx.recv().await {
                let res_bytes = match serde_json::to_vec(&response) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        error!("Failed to serialize IPC response: {}", e);
                        // Count dropped frames as drained so the backlog
                        // gauge measures only frames the guest still owes.
                        drained_frames.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        continue;
                    }
                };
                if let Err(e) = Self::write_frame(&mut writer, &res_bytes).await {
                    return Err(e);
                }
                drained_frames.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            Ok::<(), std::io::Error>(())
        });

        // Forward hotel-wide broadcast events (e.g. NetworkState) to this guest.
        {
            let broadcast_outbound_tx = outbound_tx.clone();
            let mut broadcast_rx = network_broadcast_rx;
            tokio::spawn(async move {
                loop {
                    match broadcast_rx.recv().await {
                        Ok(msg) => {
                            if broadcast_outbound_tx.send(msg).is_err() {
                                break; // guest disconnected
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!("Guest broadcast receiver lagged by {} messages.", n);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }

        // Push current MuninnDB state to the new guest so guests that connect while
        // MuninnDB is down don't falsely assume available=true until the next probe flip.
        if let Some(cfg) = muninn_config.as_deref() {
            let available = muninn_reachable.load(std::sync::atomic::Ordering::Relaxed);
            let _ = outbound_tx.send(IpcResponse::MuninnStatus {
                available,
                endpoint: cfg.base_url.clone(),
            });
        }

        let mut subscribed_roles = Vec::new();
        let mut current_identity: Option<GuestIdentity> = None;
        loop {
            match Self::read_frame(&mut reader).await {
                Ok(None) => {
                    Self::remove_subscriptions(&inboxes, conn_id, &subscribed_roles).await;
                    Self::remove_telegram_poll_leases(&telegram_poll_leases, conn_id).await;
                    Self::remove_desktop_membrane_leases(&desktop_membrane_leases, conn_id).await;
                    Self::remove_mcp_membrane_leases(&mcp_membrane_leases, conn_id).await;
                    Self::remove_discord_gateway_leases(&discord_gateway_leases, conn_id).await;
                    // A registered guest disconnected — peers need updated roster.
                    if current_identity.is_some() {
                        if let Some(ref tx) = hotel_state_dirty_tx {
                            let _ = tx.try_send(());
                        }
                    }
                    let _ = write_task.await;
                    return Ok(());
                }
                Ok(Some(frame)) => match serde_json::from_slice::<IpcRequest>(&frame) {
                    Ok(IpcRequest::ProtectedAuthority(request)) => {
                        let request_id = request.request_id();
                        // Authenticate from THIS socket before blocking SQLite work.
                        // Claimed guest only selects a supervisor record; it grants no identity.
                        let proof = protected_authority
                            .as_ref()
                            .and_then(|rpc| rpc.authenticate(reader.as_ref(), &request).ok());
                        let reply = if let (Some(rpc), Some(proof)) =
                            (protected_authority.clone(), proof)
                        {
                            tokio::task::spawn_blocking(move || rpc.handle(&proof, &request))
                                .await
                                .unwrap_or_else(|_| {
                                    ansible_mesh_core::privacy_rpc::ProtectedAuthorityReply::denied(
                                        request_id,
                                    )
                                })
                        } else {
                            ansible_mesh_core::privacy_rpc::ProtectedAuthorityReply::denied(
                                request_id,
                            )
                        };
                        let _ = outbound_tx.send(IpcResponse::ProtectedAuthorityReply {
                            protected_authority: reply,
                        });
                    }
                    Ok(IpcRequest::FetchMemoryConfig) => {
                        // DBs are truth: serve the config live from the Context
                        // Graph so a token rotated after boot (manual resync or
                        // HealMemoryToken) reaches re-fetching guests without a
                        // hotel restart. The boot-time snapshot is only the
                        // fallback when the live load errors.
                        let config_json = match crate::memory::load_muninn_config(&graph) {
                            Ok(cfg) => cfg.and_then(|c| serde_json::to_string(&c).ok()),
                            Err(err) => {
                                warn!(error = %err, "FetchMemoryConfig: live config load failed — serving boot snapshot");
                                muninn_config
                                    .as_deref()
                                    .and_then(|cfg| serde_json::to_string(cfg).ok())
                            }
                        };
                        info!(
                            has_config = config_json.is_some(),
                            "FetchMemoryConfig handled"
                        );
                        let _ = outbound_tx.send(IpcResponse::MemoryConfig(MemoryConfigPayload {
                            config_json,
                        }));
                    }
                    Ok(IpcRequest::HealMemoryToken { vault }) => {
                        let response = Self::handle_heal_memory_token(
                            &graph,
                            heal_queue.as_deref(),
                            &muninn_heal_attempts,
                            &vault,
                        )
                        .await;
                        let _ = outbound_tx.send(response);
                    }
                    Ok(IpcRequest::RefreshMemoryConfig) => {
                        let endpoint = muninn_config
                            .as_deref()
                            .map(|c| c.base_url.clone())
                            .unwrap_or_default();
                        let available = if endpoint.is_empty() {
                            false
                        } else {
                            let http = reqwest::Client::builder()
                                .timeout(std::time::Duration::from_secs(5))
                                .build()
                                .unwrap_or_default();
                            Self::probe_muninn_endpoint(&http, &endpoint).await
                        };
                        let was =
                            muninn_reachable.swap(available, std::sync::atomic::Ordering::Relaxed);
                        if available != was {
                            let _ = network_broadcast_tx.send(IpcResponse::MuninnStatus {
                                available,
                                endpoint: endpoint.clone(),
                            });
                            if !available {
                                if let Some(hq) = heal_queue.as_deref() {
                                    let msg = format!(
                                        "MuninnDB unreachable: connection refused at {endpoint}"
                                    );
                                    let _ = hq.push_error("hotel", &msg);
                                }
                            }
                        }
                        info!(available, "RefreshMemoryConfig probe complete");
                        let _ = outbound_tx.send(IpcResponse::MuninnStatus {
                            available,
                            endpoint,
                        });
                    }
                    Ok(IpcRequest::GetPerimeterStatus) => {
                        use perimeter_core::service::PerimeterService as _;
                        let snapshot = perimeter_svc
                            .as_deref()
                            .map(|s| s.snapshot())
                            .unwrap_or_default();
                        let snapshot_json =
                            serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".into());
                        let _ = outbound_tx.send(IpcResponse::PerimeterStatus { snapshot_json });
                    }
                    Ok(IpcRequest::RefreshPerimeter) => {
                        use perimeter_core::service::PerimeterService as _;
                        if let Some(svc) = perimeter_svc.as_deref() {
                            svc.refresh();
                        }
                        let snapshot = perimeter_svc
                            .as_deref()
                            .map(|s| s.snapshot())
                            .unwrap_or_default();
                        let snapshot_json =
                            serde_json::to_string(&snapshot).unwrap_or_else(|_| "{}".into());
                        let _ = outbound_tx.send(IpcResponse::PerimeterStatus { snapshot_json });
                    }
                    Ok(IpcRequest::CheckEgress {
                        agent_id,
                        target_url,
                        method,
                    }) => {
                        use perimeter_core::egress::{
                            EgressDecision, EgressGateway as _, EgressRequest,
                        };
                        use perimeter_core::service::PerimeterService as _;
                        let tier = perimeter_svc
                            .as_deref()
                            .map(|s| s.ceiling())
                            .unwrap_or_default();
                        let req = EgressRequest {
                            agent_id: agent_id.clone(),
                            target_url: target_url.clone(),
                            method: method.clone(),
                            headers: std::collections::HashMap::new(),
                            traffic_class: perimeter_core::egress::EgressTrafficClass::GeneralApi,
                            tier,
                        };
                        let resp = match egress_gw.as_deref() {
                            Some(gw) => {
                                let decision = gw.check(&req);
                                let credential_binding_configured =
                                    decision.is_allowed() && gw.credential_binding_configured(&req);
                                match decision {
                                    EgressDecision::Allow => IpcResponse::EgressGrant {
                                        allowed: true,
                                        audit: false,
                                        deny_reason: None,
                                        credential_binding_configured,
                                    },
                                    EgressDecision::AllowWithAudit => IpcResponse::EgressGrant {
                                        allowed: true,
                                        audit: true,
                                        deny_reason: None,
                                        credential_binding_configured,
                                    },
                                    EgressDecision::Deny { reason } => IpcResponse::EgressGrant {
                                        allowed: false,
                                        audit: false,
                                        deny_reason: Some(reason),
                                        credential_binding_configured: false,
                                    },
                                }
                            }
                            None => IpcResponse::EgressGrant {
                                allowed: true,
                                audit: false,
                                deny_reason: None,
                                credential_binding_configured: false,
                            },
                        };
                        let _ = outbound_tx.send(resp);
                    }
                    Ok(IpcRequest::ListTrainingSamples {
                        agent_id,
                        limit,
                        filter,
                    }) => {
                        let resp = Self::handle_list_training_samples(
                            training_storage.as_deref(),
                            agent_id.as_deref(),
                            limit,
                            &filter,
                        );
                        let _ = outbound_tx.send(resp);
                    }
                    Ok(IpcRequest::CorrectTrainingSample {
                        turn_id,
                        corrected_transcript,
                    }) => {
                        let resp = Self::handle_correct_training_sample(
                            training_storage.as_deref(),
                            &turn_id,
                            &corrected_transcript,
                        );
                        let _ = outbound_tx.send(resp);
                    }
                    Ok(IpcRequest::ExportTrainingSamples {
                        format,
                        output_path,
                        limit,
                    }) => {
                        let resp = Self::handle_export_training_samples(
                            training_storage.as_deref(),
                            &format,
                            &output_path,
                            limit,
                        );
                        let _ = outbound_tx.send(resp);
                    }
                    Ok(IpcRequest::GetTrainingStatus { agent_id }) => {
                        let resp = Self::handle_get_training_status(
                            training_storage.as_deref(),
                            agent_id.as_deref(),
                        );
                        let _ = outbound_tx.send(resp);
                    }
                    Ok(IpcRequest::AsrSetup {
                        python_path,
                        model_name,
                        auto_install,
                    }) => {
                        let graph_clone = Arc::clone(&graph);
                        let local_node_id_clone = local_node_id.clone();
                        let socket_path_clone = socket_path.clone();
                        let resp = tokio::task::spawn_blocking(move || {
                            Self::handle_asr_setup(
                                &graph_clone,
                                &local_node_id_clone,
                                &socket_path_clone,
                                python_path.as_deref().unwrap_or("python3"),
                                model_name.as_deref(),
                                auto_install,
                            )
                        })
                        .await
                        .unwrap_or_else(|e| {
                            IpcResponse::error("asr_setup", "INTERNAL_ERROR", &e.to_string())
                        });
                        let _ = outbound_tx.send(resp);
                    }
                    Ok(IpcRequest::AsrStatus {}) => {
                        let graph_clone = Arc::clone(&graph);
                        let local_node_id_clone = local_node_id.clone();
                        let resp = tokio::task::spawn_blocking(move || {
                            Self::handle_asr_status(&graph_clone, &local_node_id_clone)
                        })
                        .await
                        .unwrap_or_else(|e| {
                            IpcResponse::error("asr_status", "INTERNAL_ERROR", &e.to_string())
                        });
                        let _ = outbound_tx.send(resp);
                    }
                    Ok(IpcRequest::VisionSetup { repo_id }) => {
                        let graph_clone = Arc::clone(&graph);
                        let local_node_id_clone = local_node_id.clone();
                        let socket_path_clone = socket_path.clone();
                        let resp = tokio::task::spawn_blocking(move || {
                            Self::handle_vision_setup(
                                &graph_clone,
                                &local_node_id_clone,
                                &socket_path_clone,
                                repo_id.as_deref(),
                            )
                        })
                        .await
                        .unwrap_or_else(|e| {
                            IpcResponse::error("vision_setup", "INTERNAL_ERROR", &e.to_string())
                        });
                        let _ = outbound_tx.send(resp);
                    }
                    Ok(IpcRequest::CapabilityInvoke { request }) => {
                        let resp = Self::dispatch_capability_invoke(
                            graph.as_ref(),
                            &inboxes,
                            &pending_capability_calls,
                            &local_node_id,
                            request,
                        )
                        .await;
                        let _ = outbound_tx.send(resp);
                    }
                    Ok(IpcRequest::VisionStatus {}) => {
                        let graph_clone = Arc::clone(&graph);
                        let local_node_id_clone = local_node_id.clone();
                        let resp = tokio::task::spawn_blocking(move || {
                            Self::handle_vision_status(&graph_clone, &local_node_id_clone)
                        })
                        .await
                        .unwrap_or_else(|e| {
                            IpcResponse::error("vision_status", "INTERNAL_ERROR", &e.to_string())
                        });
                        let _ = outbound_tx.send(resp);
                    }
                    Ok(req) => {
                        let mut follow_up_responses = Vec::new();
                        let response = Self::process_request(
                            req,
                            &local_node_id,
                            &socket_path,
                            &dispatcher_tx,
                            graph.as_ref(),
                            &inboxes,
                            &parked_inbound,
                            &pending_pipelines,
                            &pending_capability_calls,
                            materialization_requester.as_deref(),
                            &telegram_poll_leases,
                            &desktop_membrane_leases,
                            &mcp_membrane_leases,
                            &discord_gateway_leases,
                            &subagent_leases,
                            &subagent_hooks,
                            &registry,
                            &peer_sockets,
                            webrtc_signal_tx.as_ref(),
                            heal_queue.as_deref(),
                            conn_id,
                            &outbound_tx,
                            &mut subscribed_roles,
                            &mut current_identity,
                            &mut follow_up_responses,
                            operator_surface_tx.as_ref(),
                            &resource_registry,
                        )
                        .await;
                        // New guest registered — broadcast updated roster to peers.
                        if matches!(response, IpcResponse::ComponentRegistered { .. }) {
                            if let Some(ref tx) = hotel_state_dirty_tx {
                                let _ = tx.try_send(());
                            }
                        }
                        // A placement change (role home / transport home) is graph
                        // truth peers must learn NOW, not on the next roster change
                        // (DEF-107): mark hotel state dirty so the next sync carries it.
                        if matches!(
                            response,
                            IpcResponse::RoleHomeSet { .. } | IpcResponse::TransportHomeSet { .. }
                        ) {
                            if let Some(ref tx) = hotel_state_dirty_tx {
                                let _ = tx.try_send(());
                            }
                        }
                        // R2 (DEF-107): tell every local guest NOW that a transport
                        // home moved, so the affected membrane seat stands down or
                        // probes on its next tick instead of after a lease denial +
                        // 180 s re-probe. Remote (gossiped) changes take the same
                        // push path from main.rs.
                        if let IpcResponse::TransportHomeSet {
                            agent_id,
                            transport,
                            resource_ref,
                            active_home_hotel,
                            standby_hotels,
                        } = &response
                        {
                            // DEF-143: `active_home_hotel` is canonicalized to
                            // node_id by transport.set_home (DEF-124) — resolve
                            // before comparing rather than comparing against the
                            // bare local hotel name, which only matched records
                            // never rewritten since before that fix.
                            let hotel_is_home =
                                Self::resolve_hotel_node_id(graph.as_ref(), active_home_hotel)
                                    .as_deref()
                                    == Some(local_node_id.as_str());
                            let updated_unix = graph
                                .get_membrane_transport_home(agent_id, transport, resource_ref)
                                .ok()
                                .flatten()
                                .map(|home| home.updated_unix)
                                .unwrap_or(0);
                            let _ = network_broadcast_tx.send(IpcResponse::TransportHomeChanged {
                                transport_home_changed: true,
                                agent_id: agent_id.clone(),
                                transport: transport.clone(),
                                resource_ref: resource_ref.clone(),
                                active_home_hotel: active_home_hotel.clone(),
                                standby_hotels: standby_hotels.clone(),
                                updated_unix,
                                hotel_is_home,
                            });
                        }
                        let _ = outbound_tx.send(response);
                        for follow_up in follow_up_responses {
                            let _ = outbound_tx.send(follow_up);
                        }
                    }
                    Err(e) => {
                        warn!("Malformed IPC request payload: {}", e);
                        let _ = outbound_tx.send(IpcResponse::error(
                            "unknown",
                            "MALFORMED_PAYLOAD",
                            e.to_string(),
                        ));
                    }
                },
                Err(e) => {
                    Self::remove_subscriptions(&inboxes, conn_id, &subscribed_roles).await;
                    Self::remove_telegram_poll_leases(&telegram_poll_leases, conn_id).await;
                    Self::remove_desktop_membrane_leases(&desktop_membrane_leases, conn_id).await;
                    Self::remove_discord_gateway_leases(&discord_gateway_leases, conn_id).await;
                    Self::remove_mcp_membrane_leases(&mcp_membrane_leases, conn_id).await;
                    let _ = write_task.await;
                    return Err(e.into());
                }
            }
        }
    }

    pub(crate) async fn add_subscription(
        inboxes: &InboxRegistry,
        role: &str,
        conn_id: Uuid,
        guest_id: &str,
        supported_tools: &[String],
        tx: &CountedSender,
        subscribed_roles: &mut Vec<String>,
    ) {
        let mut guard = inboxes.lock().await;
        let entry = guard.entry(role.to_string()).or_default();
        // One live subscription per guest identity: a guest registering again
        // (reconnect, respawn, or a raced duplicate spawn) REPLACES its older
        // subscription instead of accumulating. Two subscribers sharing one
        // guest_id double-deliver every task — live 2026-08-25: duplicate
        // philote-Chronos processes each ran the same whisper and their LWW
        // apartment checkpoints clobbered each other mid-turn, dropping the
        // reply ("Dropped stale active turn on checkpoint restore").
        let before = entry.len();
        entry.retain(|subscriber| subscriber.guest_id != guest_id || subscriber.conn_id == conn_id);
        if entry.len() < before {
            warn!(
                role,
                guest_id,
                dropped = before - entry.len(),
                "Inbox subscription replaced: newer registration for this guest supersedes stale one(s)"
            );
        }
        if !entry.iter().any(|subscriber| subscriber.conn_id == conn_id) {
            entry.push(RoleSubscriber {
                conn_id,
                guest_id: guest_id.to_string(),
                supported_tools: supported_tools.to_vec(),
                tx: tx.clone(),
            });
        }
        if !subscribed_roles.iter().any(|existing| existing == role) {
            subscribed_roles.push(role.to_string());
        }
    }

    async fn remove_subscriptions(
        inboxes: &InboxRegistry,
        conn_id: Uuid,
        subscribed_roles: &[String],
    ) {
        let mut guard = inboxes.lock().await;
        for role in subscribed_roles {
            if let Some(subscribers) = guard.get_mut(role) {
                subscribers.retain(|subscriber| subscriber.conn_id != conn_id);
            }
        }
        guard.retain(|_, subscribers| !subscribers.is_empty());
    }

    /// Deliver a hook event to the appropriate inbox based on its `HookRoute`.
    async fn deliver_hook_to_route(
        inboxes: &InboxRegistry,
        route: &HookRoute,
        persona_guest_id: &str,
        persona_role: &str,
        local_node_id: &str,
        task_id: Uuid,
        task_json: String,
    ) {
        match route {
            HookRoute::PersonaAgent => {
                Self::deliver_inbound_task(
                    inboxes,
                    local_node_id,
                    persona_role,
                    Some(persona_guest_id),
                    task_id,
                    task_json,
                )
                .await;
            }
            HookRoute::Role { role_name } => {
                Self::deliver_inbound_task(
                    inboxes,
                    local_node_id,
                    role_name,
                    None, // any subscriber for this role
                    task_id,
                    task_json,
                )
                .await;
            }
            HookRoute::Discard => {
                // Side-effect only — handler_skill invocation is out of scope for hotel.
                // Hotel logs and drops. The skill runtime will invoke handler_skill locally.
                info!(
                    "Hook task {} routed to Discard (local side-effect only).",
                    task_id
                );
            }
        }
    }

    /// Park a provably-lost InboundTask under its target guest id so the
    /// guest's next registration flushes it (the same parked-inbound path
    /// used for not-yet-materialized guests). Only called when the frame
    /// demonstrably never reached the guest — closed channel before send, or
    /// connection closed with the frame still undrained. Returns whether the
    /// task was actually requeued.
    async fn repark_lost_task(
        repark: Option<&ParkedInboundRegistry>,
        guest_id: &str,
        source_node: &str,
        task_id: Uuid,
        response: &IpcResponse,
    ) -> bool {
        let Some(registry) = repark else {
            return false;
        };
        if guest_id.is_empty() {
            return false;
        }
        let IpcResponse::InboundTask { task_json, .. } = response else {
            return false;
        };
        let mut guard = registry.lock().await;
        let entry = guard.entry(guest_id.to_string()).or_default();
        // At-most-once requeue per task id — the immediate-failure path and
        // the lost-watcher can both fire for one task.
        if entry.iter().any(|parked| parked.task_id == task_id) {
            return false;
        }
        entry.push(ParkedInboundTask {
            source_node: source_node.to_string(),
            task_id,
            task_json: task_json.clone(),
            activate_session_id: None,
            parked_at: unix_ts(),
        });
        info!(
            %task_id,
            guest_id,
            "re-parked provably-lost inbound task; will flush on the guest's next registration"
        );
        true
    }

    /// Deliver a task to every local inbox subscriber for `target_role`.
    ///
    /// Returns `true` when at least one subscriber received it. `false` means the
    /// task was dropped permanently: `SubscribeInbox` does not replay, so a guest
    /// that subscribes later never sees it. Callers that recorded a session turn
    /// before dispatching must close it on `false` — otherwise the turn sits
    /// `running` until the 300s stale-turn reaper mislabels it
    /// `ZOMBIE_TURN_REPAIR`. See `fail_undelivered_session_turn`.
    /// Push a task (`update_mcp_config`, `revoke_mcp_config`, perimeter
    /// updates) to ONE membrane-mcp endpoint guest. Inboxes are keyed by role,
    /// so the guest is addressed as role `mcp-membrane` pinned to its guest id
    /// (`mcp-membrane-<endpoint_id>`). Keying by the guest id alone matched no
    /// subscriber and the push was dropped: a running endpoint never saw
    /// config updates, new grants, or revocations until it restarted.
    pub(crate) async fn push_to_mcp_endpoint_guest(
        inboxes: &InboxRegistry,
        source_node: &str,
        guest_id: &str,
        task_json: String,
    ) -> bool {
        Self::deliver_inbound_task(
            inboxes,
            source_node,
            "mcp-membrane",
            Some(guest_id),
            Uuid::new_v4(),
            task_json,
        )
        .await
    }

    pub(crate) async fn deliver_inbound_task(
        inboxes: &InboxRegistry,
        source_node: &str,
        target_role: &str,
        target_guest_id: Option<&str>,
        task_id: Uuid,
        task_json: String,
    ) -> bool {
        if let Err(err) = Self::hydrate_agent_graph_snapshot(&task_json) {
            warn!(
                "Failed to hydrate agent graph snapshot before delivering task {} to role='{}' guest={:?}: {}",
                task_id, target_role, target_guest_id, err
            );
        }

        let subscribers = {
            let guard = inboxes.lock().await;
            let role_subscribers = guard.get(target_role).cloned().unwrap_or_default();
            match target_guest_id {
                Some(guest_id) => {
                    let live: Vec<&str> = role_subscribers
                        .iter()
                        .map(|subscriber| subscriber.guest_id.as_str())
                        .collect();
                    let chosen = select_guest_targets(&live, guest_id);
                    if chosen.len() == 1 && chosen[0] != guest_id {
                        info!(
                            target_role,
                            requested = guest_id,
                            resolved = chosen[0].as_str(),
                            "Resolved unscoped guest target to a single live incarnation"
                        );
                    }
                    role_subscribers
                        .into_iter()
                        .filter(|subscriber| chosen.iter().any(|c| c == &subscriber.guest_id))
                        .collect()
                }
                None => role_subscribers,
            }
        };

        if subscribers.is_empty() {
            match target_guest_id {
                Some(guest_id) => warn!(
                    "No local inbox subscriber for role '{}' and guest '{}'; task {} stays ledger-only for now.",
                    target_role, guest_id, task_id
                ),
                None => warn!(
                    "No local inbox subscribers for role '{}'; task {} stays ledger-only for now.",
                    target_role, task_id
                ),
            }
            return false;
        }

        info!(
            "Delivering inbound task {} to {} local subscriber(s) for role='{}' guest={:?} (payload {} bytes).",
            task_id,
            subscribers.len(),
            target_role,
            target_guest_id,
            task_json.len()
        );

        let response = IpcResponse::InboundTask {
            source_node: source_node.to_string(),
            task_id,
            task_json,
        };

        let mut stale = Vec::new();
        for subscriber in subscribers {
            // Wedge gauge: a growing backlog means this guest holds its
            // socket open but is not draining it — a state reader-EOF
            // cleanup can never detect. Latch a heal entry once per episode
            // so the heal dispatcher (which knows restart_guest) can act.
            let backlog = subscriber.tx.backlog();
            if backlog >= SUBSCRIBER_BACKLOG_WEDGE_THRESHOLD {
                if !subscriber
                    .tx
                    .backlog_flagged
                    .swap(true, std::sync::atomic::Ordering::Relaxed)
                {
                    warn!(
                        guest_id = subscriber.guest_id.as_str(),
                        target_role,
                        backlog,
                        "Subscriber outbound backlog crossed wedge threshold — guest appears alive but not draining its socket"
                    );
                    if let Some(hq) = subscriber.tx.heal.as_deref() {
                        let message = format!(
                            "[subscriber_wedged] guest [{}] role [{target_role}] has {backlog} undrained outbound frames (threshold {SUBSCRIBER_BACKLOG_WEDGE_THRESHOLD}) — deliveries are queuing into a socket the guest is not reading",
                            subscriber.guest_id
                        );
                        if let Err(err) = hq.push_classified(
                            &subscriber.guest_id,
                            &message,
                            "high",
                            "subscriber_wedged",
                        ) {
                            warn!(error = %err, "Failed to push subscriber-wedged heal entry");
                        }
                    }
                }
            } else if backlog < SUBSCRIBER_BACKLOG_WEDGE_THRESHOLD / 2 {
                subscriber
                    .tx
                    .backlog_flagged
                    .store(false, std::sync::atomic::Ordering::Relaxed);
            }

            if subscriber.tx.send(response.clone()).is_err() {
                warn!(
                    "Failed to deliver inbound task {} to local subscriber role='{}' guest='{}'. Removing stale inbox subscription.",
                    task_id, target_role, subscriber.guest_id
                );
                // Channel already closed ⇒ provably undelivered ⇒ re-park
                // under the guest id so its next registration flushes it.
                let requeued = Self::repark_lost_task(
                    subscriber.tx.repark.as_ref(),
                    &subscriber.guest_id,
                    source_node,
                    task_id,
                    &response,
                )
                .await;
                if let Some(hq) = subscriber.tx.heal.as_deref() {
                    let message = format!(
                        "[delivery_channel_closed] inbound task {task_id} for role [{target_role}] guest [{}] hit a closed outbound channel — the task was NOT received by this guest (requeued={requeued})",
                        subscriber.guest_id
                    );
                    if let Err(err) = hq.push_classified(
                        &subscriber.guest_id,
                        &message,
                        "medium",
                        "delivery_channel_closed",
                    ) {
                        warn!(error = %err, "Failed to push delivery-channel-closed heal entry");
                    }
                }
                stale.push(subscriber.conn_id);
                continue;
            }

            // Write confirmation: a send only proves the frame entered the
            // channel. Watch the drain gauge until every frame enqueued so
            // far (ours included) has been flushed to the guest socket;
            // past the timeout, file a heal entry — the frame is sitting in
            // a buffer the guest has not read, and the task will otherwise
            // vanish silently (2026-07-19 Beacon dead-delivery).
            let confirm_target = subscriber.tx.enqueued_now();
            let sender = subscriber.tx.clone();
            let guest_id = subscriber.guest_id.clone();
            let role = target_role.to_string();
            let watched_response = response.clone();
            let watched_source_node = source_node.to_string();
            tokio::spawn(async move {
                let deadline = tokio::time::Instant::now()
                    + tokio::time::Duration::from_secs(DELIVERY_WRITE_CONFIRM_TIMEOUT_SECS);
                loop {
                    if sender.drained_now() >= confirm_target {
                        debug!(
                            %task_id,
                            guest_id = guest_id.as_str(),
                            "inbound task write-confirmed to guest socket"
                        );
                        return;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(
                        DELIVERY_WRITE_CONFIRM_POLL_MS,
                    ))
                    .await;
                }
                warn!(
                    %task_id,
                    guest_id = guest_id.as_str(),
                    role = role.as_str(),
                    backlog = sender.backlog(),
                    "inbound task delivery UNCONFIRMED after {DELIVERY_WRITE_CONFIRM_TIMEOUT_SECS}s — frame never flushed to the guest socket"
                );
                if let Some(hq) = sender.heal.as_deref() {
                    let message = format!(
                        "[delivery_write_unconfirmed] inbound task {task_id} for role [{role}] guest [{guest_id}] was queued but never flushed to the guest socket within {DELIVERY_WRITE_CONFIRM_TIMEOUT_SECS}s (backlog {})",
                        sender.backlog()
                    );
                    if let Err(err) = hq.push_classified(
                        &guest_id,
                        &message,
                        "high",
                        "delivery_write_unconfirmed",
                    ) {
                        warn!(error = %err, "Failed to push delivery-unconfirmed heal entry");
                    }
                }
                // Claim-until-confirmed tail: keep watching (slow cadence)
                // for the one provably-safe redelivery trigger — the
                // connection closing while this frame is still undrained.
                // Frames drain FIFO, so closed + undrained ⇒ the guest never
                // received the task ⇒ re-parking cannot duplicate. A late
                // flush (wedge cleared) ends the watch with no action.
                let lost_cap = tokio::time::Instant::now()
                    + tokio::time::Duration::from_secs(DELIVERY_LOST_WATCH_CAP_SECS);
                loop {
                    if sender.drained_now() >= confirm_target {
                        info!(
                            %task_id,
                            guest_id = guest_id.as_str(),
                            "previously-unconfirmed inbound task flushed late — no redelivery needed"
                        );
                        return;
                    }
                    if sender.is_closed() {
                        let requeued = Self::repark_lost_task(
                            sender.repark.as_ref(),
                            &guest_id,
                            &watched_source_node,
                            task_id,
                            &watched_response,
                        )
                        .await;
                        warn!(
                            %task_id,
                            guest_id = guest_id.as_str(),
                            requeued,
                            "guest connection closed with inbound task still undrained — provably lost"
                        );
                        return;
                    }
                    if tokio::time::Instant::now() >= lost_cap {
                        return;
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(
                        DELIVERY_WRITE_CONFIRM_POLL_MS.max(250),
                    ))
                    .await;
                }
            });
        }

        if !stale.is_empty() {
            let mut guard = inboxes.lock().await;
            if let Some(entries) = guard.get_mut(target_role) {
                entries.retain(|subscriber| !stale.contains(&subscriber.conn_id));
            }
        }
        true
    }

    /// Roles delivered in-process by the hotel itself, which by design never
    /// have an inbox subscriber (see `deliver_event_envelope_or_park`). An
    /// empty subscriber set is normal for these, so they must be exempt from
    /// undelivered-task accounting or every one would file a false failure.
    pub(crate) fn is_hotel_intercepted_role(role: &str) -> bool {
        role == philotic_client::OPERATOR_SURFACE_QUERY_ROLE
            || role == philotic_client::MEMORY_WRITE_FORWARD_ROLE
    }

    /// A local task was accepted, then dropped because no guest serves its role.
    /// Close the turn now and file the reason.
    ///
    /// `EmitTask` records the turn `running` before dispatching, and a dropped
    /// task leaves nothing to move it off that state, so it used to sit until
    /// `RepairStaleSessionTurns` failed it 300s later as `ZOMBIE_TURN_REPAIR`.
    /// That reads as a timeout and hides the cause: a missing
    /// `egress-http-runner` guest produced a phantom ~315s "stuck turn" every
    /// 6h on every hotel for over a week, and `model-catalog-sync` never once
    /// succeeded, because the only signal was a reaper message about staleness.
    ///
    /// This is deliberately a post-drop report rather than a pre-accept
    /// rejection. Delivery here can be rewritten by a Golgi pipeline, satisfied
    /// by the `Park` branch's on-demand materialization, or handled in-process
    /// for the roles `is_hotel_intercepted_role` names — so "no subscriber right
    /// now" is not sufficient grounds to refuse the task, and refusing early
    /// would break all three. Once delivery has actually returned `false` the
    /// drop is final: `SubscribeInbox` does not replay.
    /// Try to rescue a task whose role has no live subscriber by reviving this
    /// hotel's own guest for that role: park the task (flushed on the guest's
    /// next registration) and ask the supervisor to bring the guest up.
    ///
    /// Returns the guest id the task was parked for, or `None` when no rescue
    /// applies and the caller should fall through to its drop handling.
    ///
    /// Two revival cases, deliberately different:
    /// - An ACTIVE record whose process is dead (hotel crash leaves
    ///   `is_active=1` with a stale pid — observed live on mac-jane 2026-08-11
    ///   when the hotel died and left `active_pid=4002` pointing at what became
    ///   macOS ReportCrash): any role qualifies, this is pure respawn.
    /// - A DORMANT record is revived only for `egress-http-runner`. That guest
    ///   is seeded dormant-by-design, "until a binding selects this hotel" —
    ///   and a governed task arriving for it IS that selection, just observed
    ///   at dispatch instead of at binding registration (which only fires once,
    ///   on first registration, and so cannot re-activate after a deploy wipes
    ///   the flag). Other dormant guests stay down: an operator's deliberate
    ///   deactivation must not be overridden by any task that names the role.
    pub(super) async fn rescue_unserved_role_task(
        graph: &GraphDomain,
        parked_inbound: &ParkedInboundRegistry,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        local_node_id: &str,
        target_role: &str,
        source_node: &str,
        task_id: Uuid,
        task_json: &str,
    ) -> Option<String> {
        if Self::is_hotel_intercepted_role(target_role) {
            return None;
        }
        let hotel_name = Self::local_hotel_name(graph, local_node_id)?;
        let record = graph
            .list_guests(&hotel_name, false)
            .ok()?
            .into_iter()
            .find(|guest| guest.role == target_role)?;

        if !record.is_active && target_role != "egress-http-runner" {
            return None;
        }
        if !record.is_active {
            if let Err(err) = graph.set_guest_active(&hotel_name, &record.guest_id, true) {
                warn!(
                    guest_id = %record.guest_id,
                    "rescue_unserved_role_task: failed to activate dormant guest: {err}"
                );
                return None;
            }
            info!(
                guest_id = %record.guest_id,
                target_role,
                "Dormant runner guest activated by an arriving task for its role."
            );
        }

        {
            let mut guard = parked_inbound.lock().await;
            let entry = guard.entry(record.guest_id.clone()).or_default();
            // At-most-once park per task id, mirroring `repark_lost_task`.
            if !entry.iter().any(|parked| parked.task_id == task_id) {
                entry.push(ParkedInboundTask {
                    source_node: source_node.to_string(),
                    task_id,
                    task_json: task_json.to_string(),
                    activate_session_id: None,
                    parked_at: unix_ts(),
                });
            }
        }
        info!(
            %task_id,
            guest_id = %record.guest_id,
            target_role,
            "Unserved-role task parked; will flush when the guest registers."
        );

        if let Some(requester) = materialization_requester {
            if let Err(err) = requester.ensure_guest_active(&record.guest_id).await {
                warn!(
                    guest_id = %record.guest_id,
                    "rescue_unserved_role_task: materialization request failed: {err}"
                );
            }
        }
        Some(record.guest_id)
    }

    fn report_unserved_local_role(
        graph: &GraphDomain,
        heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
        target_role: &str,
        target_guest_id: Option<&str>,
        task_id: Uuid,
        task_json: &str,
    ) {
        if Self::is_hotel_intercepted_role(target_role) {
            return;
        }
        let guest_suffix = target_guest_id
            .map(|g| format!(" guest [{g}]"))
            .unwrap_or_default();
        let message = format!(
            "[emit_task_unserved_local_role] task {task_id} for local role [{target_role}]{guest_suffix} \
             was accepted but no guest on this hotel subscribes that role — dropped undelivered \
             (SubscribeInbox does not replay). Materialize a guest for this role."
        );
        warn!(
            %task_id,
            target_role,
            target_guest_id = target_guest_id.unwrap_or("-"),
            "EmitTask: local role has no subscriber — task dropped, failing its turn now"
        );
        if let Ok(payload) = serde_json::from_str::<serde_json::Value>(task_json) {
            Self::fail_undelivered_session_turn(graph, &payload, "TARGET_ROLE_UNSERVED", &message);
        }
        if let Some(hq) = heal_queue {
            if let Err(err) = hq.push_classified(
                "aiua.emit_task_route",
                &message,
                "medium",
                "emit_task_unserved_local_role",
            ) {
                warn!(error = %err, "Failed to push unserved-local-role to heal queue");
            }
        }
    }

    // ── MuninnDB token self-heal ──────────────────────────────────────────────

    /// Minimum interval between token mint attempts per vault. A genuinely
    /// misconfigured MuninnDB must produce one throttled escalation per
    /// window, not a mint storm.
    const MUNINN_HEAL_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(600);

    // ── MuninnDB reachability probe ───────────────────────────────────────────

    // ── end MuninnDB reachability probe ───────────────────────────────────────

    pub(super) fn configured_local_guest_exists(
        graph: &GraphDomain,
        local_node_id: &str,
        guest_id: &str,
    ) -> bool {
        let Some(local_hotel_name) = Self::local_hotel_name(graph, local_node_id) else {
            return false;
        };
        graph
            .list_guests(&local_hotel_name, false)
            .map(|guests| {
                guests.into_iter().any(|guest| {
                    guest.guest_id == guest_id || Self::guest_record_hosts_agent(&guest, guest_id)
                })
            })
            .unwrap_or(false)
    }

    /// A base philote guest record is keyed `<hotel>:philote-<name>` but registers
    /// over IPC as its `PHILOTIC_AGENT_ID` (e.g. `agent-coach`). Treat that record
    /// as hosting the agent id so routing doesn't mistake a locally configured
    /// agent for a remote one (DEF-217).
    fn guest_record_hosts_agent(guest: &GuestRecord, agent_id: &str) -> bool {
        if guest.role != "agent" {
            return false;
        }
        serde_json::from_str::<serde_json::Value>(&guest.config_json)
            .ok()
            .and_then(|config| {
                config
                    .get("env")
                    .and_then(|env| env.get("PHILOTIC_AGENT_ID"))
                    .and_then(serde_json::Value::as_str)
                    .map(|id| id == agent_id)
            })
            .unwrap_or(false)
    }

    /// Whether `guest_id` can legitimately fill an agent placement for `role="agent"`
    /// routing decisions (2026-07-06 parked-tool-result incident guard). Infrastructure
    /// guests — tool runners, datasources, gateways, model routers, life-graph-runner —
    /// never consume agent tasks, so a placement-provenance hint naming one is poison:
    /// parking an agent's tool RESULT for the runner that produced it kills the turn at
    /// the watchdog. Role-incarnation guests and guests whose hotel record carries the
    /// "agent" role are agent placements; unknown guests are left to the existing
    /// routing behavior (we only reject on a positively-known non-agent role).
    pub(super) fn guest_can_fill_agent_placement(
        graph: &GraphDomain,
        local_node_id: &str,
        guest_id: &str,
    ) -> bool {
        if graph
            .list_role_incarnations_by_guest_id(guest_id)
            .map(|records| !records.is_empty())
            .unwrap_or(false)
        {
            return true;
        }
        let Some(local_hotel_name) = Self::local_hotel_name(graph, local_node_id) else {
            return true;
        };
        match graph.get_guest(&local_hotel_name, guest_id) {
            Ok(Some(guest)) => !is_non_agent_infra_role(&guest.role),
            _ => true,
        }
    }

    /// Resolve the node_id hosting a guest, with a fallback to `home_node` from the role
    /// incarnation record. Necessary for the first cross-hotel task before the guest appears
    /// in HotelStateSync (its hotel guest record may not exist yet on the remote hotel).
    fn resolve_guest_home_node(
        graph: &GraphDomain,
        registry: &NodeRegistry,
        guest_id: &str,
    ) -> Option<String> {
        // 1. Live registry: advertisements (running philote) + HotelStateSync roster.
        if let Some(node_id) = registry.find_node_id_for_guest(guest_id) {
            return Some(node_id);
        }
        // 2. Fallback: home_node from the role incarnation record. It should
        // hold a node_id ("mac-jane-aiua-01"), but a record can still carry
        // the bare hotel name ("mac-jane") — resolve it, or EmitTask routes
        // the task to a mesh peer that does not exist ("target node unknown
        // to this hotel — task may never deliver", live 2026-09-15 14:45 UTC,
        // DEF-132).
        if let Some(home) = graph
            .list_role_incarnations_by_guest_id(guest_id)
            .unwrap_or_default()
            .into_iter()
            .next()
            .and_then(|record| record.home_node)
        {
            let resolved = Self::resolve_hotel_node_id(graph, &home);
            return Some(resolved.unwrap_or(home));
        }
        // 3. The guest names an agent this hotel has no record of — the
        // Telegram seat addresses the base agent (`agent-beacon`) while
        // rosters list its role guests (`agent-beacon:orchestrator`). Route to
        // the hotel that actually runs that agent, so a transport can poll on
        // one hotel while its agent answers from another.
        let agent_id = guest_id.split(':').next().unwrap_or(guest_id);
        registry.find_node_id_for_agent(agent_id)
    }

    pub(super) fn local_delivery_provenance_hint(
        session: &SessionRecord,
        local_hotel_name: Option<&str>,
    ) -> Option<LocalDeliveryProvenanceHint> {
        let provenance = session.summary_json.get("agent_runtime_provenance")?;
        let delivery_hotel = provenance.get("delivery_hotel")?.as_str()?;
        let delivery_target_guest_id = provenance.get("delivery_target_guest_id")?.as_str()?;
        let marker_kind = provenance
            .get("marker_kind")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let marker_strength = provenance
            .get("marker_strength")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| infer_marker_strength(None, marker_kind.as_deref()).map(str::to_string));
        let policy = placement_marker_policy(marker_kind.as_deref(), marker_strength.as_deref());
        let freshness_anchor = provenance
            .get("updated_at")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(session.updated_at);
        if Some(delivery_hotel) != local_hotel_name {
            return None;
        }
        if unix_ts().saturating_sub(freshness_anchor) > policy.ttl_secs {
            return None;
        }
        Some(LocalDeliveryProvenanceHint {
            guest_id: delivery_target_guest_id.to_string(),
            updated_at: freshness_anchor,
            marker_kind,
            marker_strength,
        })
    }

    /// Do two agent-role guest ids belong to the same agent? A guest is the
    /// base agent (`agent-jane`) or one of its role incarnations
    /// (`agent-jane:orchestrator`), so the agent is everything before the
    /// first `:`.
    pub(super) fn same_agent_guest(a: &str, b: &str) -> bool {
        a.split(':').next() == b.split(':').next()
    }

    pub(super) fn resolve_orchestrator_guest_id(
        graph: &GraphDomain,
        session: &SessionRecord,
        live_agent_guests: &[String],
    ) -> Option<String> {
        let is_registered = |guest_id: &str| live_agent_guests.iter().any(|live| live == guest_id);

        if let Some(agent_id) = session.primary_agent_id.as_deref() {
            if let Ok(Some(role_record)) = graph.get_role_incarnation(agent_id, "orchestrator") {
                if is_registered(&role_record.guest_id) {
                    return Some(role_record.guest_id);
                }
            }
            // Single-process philote registers as the base agent_id and handles all roles
            // internally. If the specific incarnation guest is not live but the base agent is,
            // treat the base agent as the orchestrator.
            if is_registered(agent_id) {
                return Some(agent_id.to_string());
            }
        }

        // Last resort: any live orchestrator — but never another agent's when
        // the session names its own (DEF-177). Beacon's bot polled from a
        // hotel that does not host her fell through to Björk's orchestrator
        // here and was answered as Björk. A session with no primary agent
        // (a cron or system session) may still use the hotel's orchestrator.
        live_agent_guests
            .iter()
            .find(|guest_id| {
                guest_id.ends_with(":orchestrator")
                    && session.primary_agent_id.as_deref().is_none_or(|agent_id| {
                        Self::guest_belongs_to_agent(graph, guest_id, agent_id)
                    })
            })
            .cloned()
    }

    /// Does `guest_id` belong to `agent_id`? Guests are named after the agent
    /// (`agent-jane`, `agent-jane:orchestrator`), but a role record is the
    /// authority where the two differ (`agent-jane-01` owning
    /// `agent-jane:orchestrator`).
    pub(super) fn guest_belongs_to_agent(
        graph: &GraphDomain,
        guest_id: &str,
        agent_id: &str,
    ) -> bool {
        guest_id.split(':').next() == Some(agent_id)
            || graph
                .list_role_incarnations_by_guest_id(guest_id)
                .ok()
                .into_iter()
                .flatten()
                .any(|record| record.agent_id == agent_id)
    }

    fn hydrate_agent_graph_snapshot(task_json: &str) -> anyhow::Result<Option<String>> {
        apply_embedded_agent_graph_snapshot(task_json)
    }

    pub(super) fn update_session_active_incarnation(
        graph: &GraphDomain,
        session_id: &str,
        guest_id: &str,
    ) -> anyhow::Result<()> {
        let Some(mut session) = graph.get_session(session_id)? else {
            anyhow::bail!("session [{}] not found", session_id);
        };
        session.active_incarnation_id = Some(guest_id.to_string());
        session.updated_at = unix_ts();
        graph.upsert_session(&session)?;
        for role_record in graph.list_role_incarnations_by_guest_id(guest_id)? {
            graph.set_role_incarnation_readiness(
                &role_record.agent_id,
                &role_record.role_name,
                RoleReadinessState::ActiveInSession,
            )?;
        }
        Ok(())
    }

    pub(super) fn is_agent_handoff_caller(graph: &GraphDomain, identity: &GuestIdentity) -> bool {
        if identity.role == "agent" {
            return true;
        }

        // A role-incarnation guest registers under its routing role; a guest
        // seeded from mesh-config by a pre-DEF-134 philote registered under
        // the bare role name. Both are the incarnation the record describes.
        graph
            .list_role_incarnations_by_guest_id(&identity.guest_id)
            .map(|records| {
                records.iter().any(|record| {
                    record.routing_role() == identity.role || record.role_name == identity.role
                })
            })
            .unwrap_or(false)
    }

    pub(super) fn resolve_role_incarnation(
        graph: &GraphDomain,
        session_id: &str,
        role_name: &str,
    ) -> anyhow::Result<RoleIncarnationRecord> {
        let Some(session) = graph.get_session(session_id)? else {
            anyhow::bail!("session [{}] not found", session_id);
        };
        let Some(agent_id) = session.primary_agent_id else {
            anyhow::bail!("session [{}] has no primary_agent_id", session_id);
        };
        if let Some(role_record) = graph.get_role_incarnation(&agent_id, role_name)? {
            return Ok(role_record);
        }
        // Role names are stored with whatever casing they were configured with
        // (e.g. "Chronos"), but operators type `/role chronos` from memory or a
        // model emits a lowercased argument. Fall back to a case-insensitive
        // scan rather than forcing exact-case recall for a lookup that's
        // effectively an enum choice over a short, known list.
        if let Some(role_record) = graph
            .list_role_incarnations(&agent_id)?
            .into_iter()
            .find(|r| r.role_name.eq_ignore_ascii_case(role_name))
        {
            return Ok(role_record);
        }
        anyhow::bail!(
            "role [{}] is not configured for agent [{}]",
            role_name,
            agent_id
        );
    }

    pub(super) fn role_worker_manifest(
        graph: &GraphDomain,
        local_node_id: &str,
        role_record: &RoleIncarnationRecord,
    ) -> anyhow::Result<ComponentManifest> {
        let hotel_name = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;
        let hotel = graph
            .get_hotel(&hotel_name)?
            .ok_or_else(|| anyhow::anyhow!("hotel [{hotel_name}] not found"))?;
        let mut env = HashMap::new();
        env.insert(
            "PHILOTIC_HOTEL_SOCKET".into(),
            hotel.ipc_socket_path.to_string(),
        );
        env.insert("PHILOTIC_HOTEL_NAME".into(), hotel_name.clone());
        env.insert("PHILOTIC_NODE_ID".into(), local_node_id.to_string());
        env.insert("PHILOTIC_AGENT_ID".into(), role_record.agent_id.clone());
        env.insert("PHILOTIC_GUEST_ID".into(), role_record.guest_id.clone());
        env.insert("PHILOTIC_ROLE_NAME".into(), role_record.role_name.clone());
        env.insert("PHILOTIC_ROLE_INBOX".into(), role_record.routing_role());

        Ok(ComponentManifest {
            guest_id: role_record.guest_id.clone(),
            role: "agent".into(),
            hotel: hotel_name,
            command: "philote".into(),
            args: Vec::new(),
            env,
            component_config: serde_json::json!({
                "component_kind": "role_worker",
                "agent_id": role_record.agent_id,
                "role_name": role_record.role_name,
                "routing_role": role_record.routing_role(),
            }),
            auto_start: true,
        })
    }

    pub(super) async fn role_route_is_live(
        inboxes: &InboxRegistry,
        routing_role: &str,
        guest_id: &str,
    ) -> bool {
        let guard = inboxes.lock().await;
        guard
            .get(routing_role)
            .into_iter()
            .flatten()
            .any(|subscriber| subscriber.guest_id == guest_id)
    }

    pub(super) async fn deliver_live_guest_task(
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        local_node_id: &str,
        target_role: &str,
        guest_id: &str,
        task_id: Uuid,
        task_json: String,
        activate_session_id: Option<String>,
    ) -> anyhow::Result<bool> {
        let task_json = attach_delivery_context(
            graph,
            local_node_id,
            target_role,
            Some(guest_id),
            &task_json,
        );
        let is_live = {
            let guard = inboxes.lock().await;
            guard
                .get(target_role)
                .into_iter()
                .flatten()
                .any(|subscriber| subscriber.guest_id == guest_id)
        };
        if !is_live {
            return Ok(false);
        }
        if let Some(session_id) = activate_session_id.as_deref() {
            if let Err(err) = Self::update_session_active_incarnation(graph, session_id, guest_id) {
                // Session may live on a remote hotel; skip the local update rather than
                // blocking delivery.
                warn!(
                    "deliver_live_guest_task: skipping session activation for [{}]: {}",
                    session_id, err
                );
            }
        }
        Self::deliver_inbound_task(
            inboxes,
            local_node_id,
            target_role,
            Some(guest_id),
            task_id,
            task_json,
        )
        .await;
        Ok(true)
    }

    pub(super) fn role_guest_process_is_live(
        graph: &GraphDomain,
        local_node_id: &str,
        guest_id: &str,
    ) -> anyhow::Result<bool> {
        let Some(local_hotel_name) = Self::local_hotel_name(graph, local_node_id) else {
            return Ok(false);
        };
        let guest = graph
            .list_guests(&local_hotel_name, false)?
            .into_iter()
            .find(|guest| guest.guest_id == guest_id);
        Ok(guest
            .and_then(|guest| guest.active_pid)
            .and_then(|pid| pid.parse::<u32>().ok())
            .is_some_and(Self::pid_exists))
    }

    /// Verify a self-asserted MCP `owner_agent_id` against the guest identity
    /// registered on this IPC connection.
    ///
    /// Philotes register as `<agent_id>` or `<agent_id>:<role>`; both forms
    /// bind. Admin-class roles may act on any agent's behalf. An unregistered
    /// connection (local ops tooling speaking raw IPC) is treated as
    /// admin-equivalent — holding the hotel socket is already root-equivalent
    /// for the hotel, which is the documented residual risk.
    fn mcp_owner_identity_ok(
        current_identity: &Option<GuestIdentity>,
        owner_agent_id: &str,
    ) -> bool {
        let Some(identity) = current_identity.as_ref() else {
            return true;
        };
        if matches!(
            identity.role.as_str(),
            "operator" | "admin" | "desktop-membrane"
        ) {
            return true;
        }
        identity.guest_id == owner_agent_id
            || identity
                .guest_id
                .strip_prefix(owner_agent_id)
                .is_some_and(|rest| rest.starts_with(':'))
    }

    /// Resolve a binding's exit policy against the live mesh registry and map
    /// the policy hotel identity to its concrete node id.
    async fn integration_binding_entry(
        binding: ansible_mesh_core::integration::IntegrationBinding,
        registry: &Arc<RwLock<NodeRegistry>>,
        graph: &GraphDomain,
        local_node_id: &str,
    ) -> philotic_client::IntegrationBindingEntry {
        use ansible_mesh_core::integration::{
            EgressPlacementDecision, EgressPlacementPolicy, decide_egress_placement,
        };

        let requested_hotel = match &binding.placement {
            EgressPlacementPolicy::PreferHotel { hotel_id, .. }
            | EgressPlacementPolicy::RequireHotel { hotel_id } => Some(hotel_id.as_str()),
            EgressPlacementPolicy::Local | EgressPlacementPolicy::Deny => None,
        };
        let local_hotel = Self::local_hotel_name(graph, local_node_id);
        let mut exit_node_id = None;
        let mut exit_hotel_reachable = requested_hotel.is_none();

        if let Some(hotel_id) = requested_hotel {
            if hotel_id == local_node_id || local_hotel.as_deref() == Some(hotel_id) {
                exit_node_id = Some(local_node_id.to_string());
                exit_hotel_reachable = true;
            } else {
                let guard = registry.read().await;
                if let Some(status) = guard.active_nodes().find(|status| {
                    status.capabilities.node_id == hotel_id
                        || Self::target_hotel_name(
                            graph,
                            status,
                            local_hotel.as_deref().unwrap_or_default(),
                        ) == hotel_id
                }) {
                    let node_id = status.capabilities.node_id.clone();
                    exit_hotel_reachable =
                        status.execution_reachability.is_some() && guard.is_node_healthy(&node_id);
                    exit_node_id = Some(node_id);
                }
            }
        }

        let placement = decide_egress_placement(&binding.placement, exit_hotel_reachable);
        let execution_node_id = match &placement {
            EgressPlacementDecision::ExecuteLocal { .. } => Some(local_node_id.to_string()),
            EgressPlacementDecision::ExecuteAtHotel { .. } => exit_node_id,
            EgressPlacementDecision::Deny { .. } => None,
        };
        philotic_client::IntegrationBindingEntry {
            binding,
            placement,
            execution_node_id,
            exit_hotel_reachable,
        }
    }

    async fn materialize_integration_runner(
        entry: &philotic_client::IntegrationBindingEntry,
        registry: &Arc<RwLock<NodeRegistry>>,
        graph: &GraphDomain,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        local_node_id: &str,
    ) -> Option<String> {
        let target_node_id = entry.execution_node_id.as_deref()?;
        let target_hotel = if target_node_id == local_node_id {
            Self::local_hotel_name(graph, local_node_id)?
        } else {
            let guard = registry.read().await;
            let status = guard.get_node(target_node_id)?;
            Self::target_hotel_name(
                graph,
                status,
                Self::local_hotel_name(graph, local_node_id)
                    .as_deref()
                    .unwrap_or_default(),
            )
        };
        let guest_id = format!("{target_hotel}:egress-http");

        let response = Self::handle_operator_target_request(
            IpcRequest::SetOperatorTargetComponentActive {
                target_node_id: target_node_id.to_string(),
                guest_id,
                active: true,
            },
            registry,
            graph,
            materialization_requester,
            local_node_id,
        )
        .await;
        match response {
            IpcResponse::OperatorTargetComponentMutationAckView {
                operator_target_component_mutation,
            } if operator_target_component_mutation.ok => Some(target_node_id.to_string()),
            IpcResponse::Standard { ok: true, .. } => Some(target_node_id.to_string()),
            other => {
                warn!(
                    binding_id = entry.binding.binding_id,
                    target_node_id,
                    ?other,
                    "integration binding persisted but runner materialization did not complete"
                );
                None
            }
        }
    }

    async fn exchange_operator_oidc(
        socket_path: &str,
        local_node_id: &str,
        graph: &GraphDomain,
        provider: &str,
        authorization_code: String,
        code_verifier: String,
        redirect_uri: String,
    ) -> anyhow::Result<ansible_mesh_core::integration::OidcExchangeResponse> {
        use ansible_mesh_core::integration::{
            EgressPlacementPolicy, EgressTrafficClass, HttpNetworkScope, IntegrationBinding,
            IntegrationTarget, OidcExchangeRequest, OidcIntegrationTarget,
        };

        let provider = provider.trim().to_ascii_lowercase();
        let (client_id_key, client_secret_ref_key, default_token_url, default_userinfo_url) =
            match provider.as_str() {
                "google" => (
                    "oidc_google_client_id",
                    "oidc_google_client_secret_ref",
                    "https://oauth2.googleapis.com/token",
                    "https://openidconnect.googleapis.com/v1/userinfo",
                ),
                "github" => (
                    "oidc_github_client_id",
                    "oidc_github_client_secret_ref",
                    "https://github.com/login/oauth/access_token",
                    "https://api.github.com/user",
                ),
                _ => anyhow::bail!("unsupported operator OIDC provider '{provider}'"),
            };
        let read_config_string = |key: &str| -> anyhow::Result<Option<String>> {
            Ok(graph
                .get_config_value(key)?
                .and_then(|value| serde_json::from_str::<String>(&value).ok())
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()))
        };
        let client_id = read_config_string(client_id_key)?
            .ok_or_else(|| anyhow::anyhow!("{client_id_key} is not configured"))?;
        let client_secret_ref = read_config_string(client_secret_ref_key)?
            .ok_or_else(|| anyhow::anyhow!("{client_secret_ref_key} is not configured"))?;
        let smoke_mode = std::env::var("PHILOTIC_SMOKE_MODE").as_deref() == Ok("1");
        let token_url = if smoke_mode {
            read_config_string(&format!("smoke_oidc_{provider}_token_url"))?
                .unwrap_or_else(|| default_token_url.into())
        } else {
            default_token_url.into()
        };
        let userinfo_url = if smoke_mode {
            read_config_string(&format!("smoke_oidc_{provider}_userinfo_url"))?
                .unwrap_or_else(|| default_userinfo_url.into())
        } else {
            default_userinfo_url.into()
        };
        let endpoint_is_loopback = |raw: &str| -> anyhow::Result<bool> {
            let url = reqwest::Url::parse(raw).context("operator OIDC endpoint URL is invalid")?;
            let host = url
                .host_str()
                .ok_or_else(|| anyhow::anyhow!("operator OIDC endpoint URL has no host"))?;
            Ok(host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback()))
        };
        let token_is_loopback = endpoint_is_loopback(&token_url)?;
        let userinfo_is_loopback = endpoint_is_loopback(&userinfo_url)?;
        if token_is_loopback != userinfo_is_loopback {
            anyhow::bail!(
                "operator OIDC token and userinfo endpoints must share one network scope"
            );
        }
        let network_scope = if token_is_loopback {
            HttpNetworkScope::Loopback
        } else {
            HttpNetworkScope::Public
        };

        let redirect =
            reqwest::Url::parse(&redirect_uri).context("operator OIDC redirect_uri is invalid")?;
        if !redirect.username().is_empty() || redirect.password().is_some() {
            anyhow::bail!("operator OIDC redirect_uri must not contain userinfo");
        }
        let expected_path = format!("/auth/oidc/{provider}/callback");
        if redirect.path() != expected_path {
            anyhow::bail!(
                "operator OIDC redirect_uri path '{}' does not match '{}'",
                redirect.path(),
                expected_path
            );
        }
        if redirect.scheme() != "https"
            && !(redirect.scheme() == "http"
                && redirect
                    .host_str()
                    .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "::1")))
        {
            anyhow::bail!("operator OIDC redirect_uri must use HTTPS or HTTP loopback");
        }

        let binding_id = format!("operator-oidc-{provider}");
        let binding = IntegrationBinding {
            binding_id: binding_id.clone(),
            owner_agent_id: "operator-auth-egress".into(),
            display_name: Some(format!("{provider} operator OIDC exchange")),
            target: IntegrationTarget::Oidc(OidcIntegrationTarget {
                provider_id: provider,
                client_id,
                client_secret_ref: Some(client_secret_ref),
                token_url,
                userinfo_url,
                redirect_uri: redirect.to_string(),
                network_scope,
                timeout_secs: 15,
                max_response_bytes: 64 * 1024,
            }),
            grant_agents: Vec::new(),
            grant_skills: Vec::new(),
            traffic_class: EgressTrafficClass::GeneralApi,
            placement: EgressPlacementPolicy::Local,
            requires_approval: false,
            enabled: true,
            updated_at: unix_ts(),
        };
        crate::service::governed_http::GovernedHttpService {
            socket_path: socket_path.to_string(),
            local_node_id: local_node_id.to_string(),
            guest_id: "operator-auth-egress".into(),
            role: "operator-auth-egress".into(),
        }
        .execute_oidc(
            binding,
            OidcExchangeRequest {
                binding_id,
                authorization_code,
                code_verifier,
            },
            "operator OIDC exchange",
        )
        .await
    }

    async fn process_request(
        req: IpcRequest,
        local_node_id: &str,
        socket_path: &str,
        dispatcher_tx: &mpsc::Sender<LedgerCommand>,
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        parked_inbound: &Arc<Mutex<HashMap<String, Vec<ParkedInboundTask>>>>,
        pending_pipelines: &PendingPipelineRegistry,
        pending_capability_calls: &PendingCapabilityRegistry,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        telegram_poll_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        desktop_membrane_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        mcp_membrane_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        discord_gateway_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        subagent_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        subagent_hooks: &SubagentHookRegistry,
        registry: &Arc<RwLock<NodeRegistry>>,
        peer_sockets: &Arc<RwLock<HashMap<String, String>>>,
        webrtc_signal_tx: Option<&mpsc::Sender<ansible_mesh_core::webrtc::WebRtcSignalMessage>>,
        heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
        conn_id: Uuid,
        outbound_tx: &CountedSender,
        subscribed_roles: &mut Vec<String>,
        current_identity: &mut Option<GuestIdentity>,
        follow_up_responses: &mut Vec<IpcResponse>,
        operator_surface_tx: Option<&mpsc::Sender<String>>,
        resource_registry: &Arc<Mutex<crate::service::resource_registry::ResourceRegistry>>,
    ) -> IpcResponse {
        match req {
            // This fallback has no peer proof; it must never resolve authority.
            IpcRequest::ProtectedAuthority(request) => IpcResponse::ProtectedAuthorityReply {
                protected_authority:
                    ansible_mesh_core::privacy_rpc::ProtectedAuthorityReply::denied(
                        request.request_id(),
                    ),
            },
            IpcRequest::Register(identity) => {
                info!(
                    "Guest registered over UDS: [{}] Role: {}",
                    identity.guest_id, identity.role
                );
                Self::add_subscription(
                    inboxes,
                    &identity.role,
                    conn_id,
                    &identity.guest_id,
                    &identity.supported_tools,
                    outbound_tx,
                    subscribed_roles,
                )
                .await;
                if identity.role == "tool" {
                    if let Err(err) = Self::upsert_tool_runner_registry_entry(graph, &identity) {
                        error!("Failed to persist tool runner registry entry: {}", err);
                    }
                }
                *current_identity = Some(identity.clone());
                if let Some(parked) = {
                    let mut guard = parked_inbound.lock().await;
                    guard.remove(&identity.guest_id)
                } {
                    // Expired parks are dead on arrival: their caller's turn
                    // timed out long ago. Close their ledger rows with the
                    // real reason and drop them instead of delivering stale
                    // prompts to the freshly-registered guest.
                    let now = unix_ts();
                    let (parked, expired): (Vec<_>, Vec<_>) =
                        parked.into_iter().partition(|task| {
                            now.saturating_sub(task.parked_at) <= PARKED_TASK_TTL_SECS
                        });
                    for task in expired {
                        warn!(
                            guest_id = %identity.guest_id,
                            task_id = %task.task_id,
                            age_secs = now.saturating_sub(task.parked_at),
                            "Dropping expired parked task at flush — caller timed out long ago"
                        );
                        if let Ok(payload) =
                            serde_json::from_str::<serde_json::Value>(&task.task_json)
                        {
                            Self::fail_undelivered_session_turn(
                                graph,
                                &payload,
                                "PARKED_TASK_EXPIRED",
                                &format!(
                                    "parked for guest {} longer than {PARKED_TASK_TTL_SECS}s; \
                                     dropped at flush",
                                    identity.guest_id
                                ),
                            );
                        }
                    }
                    let mut activated_sessions = std::collections::HashSet::new();
                    for task in &parked {
                        if let Some(session_id) = task.activate_session_id.as_deref() {
                            if activated_sessions.insert(session_id.to_string()) {
                                if let Err(err) = Self::update_session_active_incarnation(
                                    graph,
                                    session_id,
                                    &identity.guest_id,
                                ) {
                                    warn!(
                                        "Failed to activate session [{}] for guest [{}] during parked-task flush: {}",
                                        session_id, identity.guest_id, err
                                    );
                                }
                            }
                        }
                    }
                    info!(
                        "Flushing {} parked inbound task(s) to newly registered guest [{}].",
                        parked.len(),
                        identity.guest_id
                    );
                    for task in parked {
                        follow_up_responses.push(IpcResponse::InboundTask {
                            source_node: task.source_node,
                            task_id: task.task_id,
                            task_json: task.task_json,
                        });
                    }
                }
                IpcResponse::success("reg", None)
            }
            IpcRequest::GetConfig { key } => {
                info!("GetConfig requested: {}", key);
                if key == "__mesh_registry__" {
                    let snapshot = Self::compose_mesh_registry_snapshot(registry).await;
                    return IpcResponse::ConfigData {
                        key,
                        value_json: Some(snapshot.to_string()),
                    };
                }
                // Read-only operator surface for the self-heal circuit's filed
                // work items (finding F8, `phil heal list`). A serialization
                // failure degrades to an empty list rather than erroring — this
                // is a visibility read for the resilience system.
                if key == "__heal_work_items__" {
                    let items = graph.list_heal_work_items().unwrap_or_default();
                    let value_json = serde_json::to_string(&items).ok();
                    return IpcResponse::ConfigData { key, value_json };
                }
                // Read-only operator surface for the Autopoiesis Slice A9
                // trust ledger (`phil autonomy status`). `__autonomy_status__`
                // reports every granted lane; `__autonomy_status__:{lane}`
                // scopes to one.
                if key == "__autonomy_status__" {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    return Self::handle_query_autonomy_status(graph, None, now);
                }
                if let Some(lane) = key.strip_prefix("__autonomy_status__:") {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    return Self::handle_query_autonomy_status(graph, Some(lane), now);
                }
                // Read-only operator surface for the A9 outcome-stamping
                // follow-up slice (`phil autonomy pending`): every audit
                // record across all lanes still awaiting an operator
                // outcome. Companion to `__autonomy_status__` — status
                // reports the trust-ledger counters, this reports the raw
                // review backlog those counters are waiting on.
                if key == "__autonomy_pending__" {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    return Self::handle_query_autonomy_pending(graph, now);
                }
                // Read-only operator/steward surface for the Memory
                // Transparency Slice M3 delta digest (`memory.delta_digest`
                // philote tool). `__memory_delta_digest__` uses the default
                // 24h window; `__memory_delta_digest__:{hours}` overrides it.
                // A query, not a write — no autonomy grant is consulted (the
                // Autonomy Contract governs autonomous actions, not reads
                // that render already-durable state).
                if key == "__memory_delta_digest__" {
                    return Self::handle_memory_delta_digest(
                        graph,
                        local_node_id,
                        crate::memory_delta_digest::DEFAULT_WINDOW_HOURS,
                    )
                    .await;
                }
                if let Some(hours_str) = key.strip_prefix("__memory_delta_digest__:") {
                    let window_hours = hours_str
                        .parse::<u64>()
                        .ok()
                        .filter(|h| *h > 0)
                        .unwrap_or(crate::memory_delta_digest::DEFAULT_WINDOW_HOURS);
                    return Self::handle_memory_delta_digest(graph, local_node_id, window_hours)
                        .await;
                }
                // Returns a JSON array of memory_type strings for all session apartments
                // belonging to the given agent — used by philote at startup for stale-turn sweep.
                // Key format: `__session_apartments__:{agent_id}`
                if let Some(agent_id) = key.strip_prefix("__session_apartments__:") {
                    let memory_types: Vec<String> = graph
                        .list_apartments(agent_id)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|mt| mt.starts_with("short_session:"))
                        .collect();
                    let json = serde_json::to_string(&memory_types).unwrap_or_else(|_| "[]".into());
                    return IpcResponse::ConfigData {
                        key,
                        value_json: Some(json),
                    };
                }

                if let Some(rest) = key.strip_prefix("__session_snapshot__:") {
                    // Format: `{session_id}` (orchestrator) or `{session_id}@{role_name}` (role process)
                    let (session_id, role_name) = match rest.split_once('@') {
                        Some((sess, role)) => (sess, Some(role)),
                        None => (rest, None),
                    };
                    match Self::compose_session_snapshot(
                        graph,
                        inboxes,
                        registry,
                        local_node_id,
                        session_id,
                        role_name,
                    )
                    .await
                    {
                        Ok(value) => {
                            return IpcResponse::ConfigData {
                                key,
                                value_json: value.map(|v| v.to_string()),
                            };
                        }
                        Err(e) => {
                            error!("Failed to compose session snapshot: {}", e);
                            return IpcResponse::error("config", "CONFIG_ERROR", e.to_string());
                        }
                    }
                }
                if let Some((agent_id, memory_type)) = key
                    .strip_prefix("__apartment__:")
                    .and_then(|rest| rest.split_once(':'))
                {
                    match graph.get_apartment(agent_id, memory_type) {
                        Ok(value) => {
                            return IpcResponse::ConfigData {
                                key,
                                value_json: value.map(|v| v.to_string()),
                            };
                        }
                        Err(e) => {
                            error!("Failed to load apartment from GraphStorage: {}", e);
                            return IpcResponse::error("config", "CONFIG_ERROR", e.to_string());
                        }
                    }
                }
                if let Some(agent_id) = key.strip_prefix("__agent_bundle__:") {
                    match graph.get_agent_identity(agent_id) {
                        Ok(Some(identity)) => {
                            return IpcResponse::ConfigData {
                                key,
                                value_json: Some(identity.bundle_json.to_string()),
                            };
                        }
                        Ok(None) => {
                            return IpcResponse::ConfigData {
                                key,
                                value_json: None,
                            };
                        }
                        Err(e) => {
                            error!("Failed to load agent bundle from GraphStorage: {}", e);
                            return IpcResponse::error("config", "CONFIG_ERROR", e.to_string());
                        }
                    }
                }
                match graph.get_config_value(&key) {
                    Ok(value_json) => IpcResponse::ConfigData { key, value_json },
                    Err(e) => {
                        error!("Failed to load config key from GraphStorage: {}", e);
                        IpcResponse::error("config", "CONFIG_ERROR", e.to_string())
                    }
                }
            }
            IpcRequest::GetSecret { secret_ref } => {
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "secret",
                        "SECRET_UNREGISTERED",
                        "guest must register before requesting vault secrets",
                    );
                };

                match resolve_secret(
                    graph,
                    &secret_ref,
                    &SecretAccess {
                        role: identity.role.clone(),
                        guest_id: identity.guest_id.clone(),
                    },
                ) {
                    Ok(value_json) => IpcResponse::SecretData {
                        secret_ref,
                        value_json: value_json.map(|value| serde_json::to_string(&value).unwrap()),
                    },
                    Err(err) => {
                        error!("Failed to resolve vault secret [{}]: {}", secret_ref, err);
                        IpcResponse::error("secret", "SECRET_ERROR", err.to_string())
                    }
                }
            }
            IpcRequest::GetToolsetProfile { profile_name } => {
                match graph.get_toolset_profile(&profile_name) {
                    Ok(Some(p)) => IpcResponse::success(
                        "toolset_profile",
                        Some(serde_json::to_value(&p).unwrap_or(serde_json::Value::Null)),
                    ),
                    Ok(None) => IpcResponse::success("toolset_profile", None),
                    Err(e) => IpcResponse::error("toolset_profile", "PROFILE_ERROR", e.to_string()),
                }
            }
            IpcRequest::ListToolsetProfiles {} => match graph.list_toolset_profiles() {
                Ok(profiles) => IpcResponse::success(
                    "list_toolset_profiles",
                    Some(
                        serde_json::to_value(&profiles).unwrap_or(serde_json::Value::Array(vec![])),
                    ),
                ),
                Err(e) => {
                    IpcResponse::error("list_toolset_profiles", "PROFILES_ERROR", e.to_string())
                }
            },
            IpcRequest::SetConfig { key, value_json } => {
                info!("SetConfig requested: {}", key);
                // Reserved prefix: MCP endpoint/route/preapproval state changes
                // only through their dedicated, validated handlers — the generic
                // config writer must not be a side door around identity checks.
                if key.starts_with("__mcp_") {
                    return IpcResponse::error(
                        "config",
                        "RESERVED_KEY",
                        format!(
                            "config key '{key}' uses the reserved __mcp_ prefix; \
                             use the dedicated MCP provisioning IPC instead"
                        ),
                    );
                }
                match graph.set_config_value(&key, &value_json) {
                    Ok(()) => IpcResponse::success("config", None),
                    Err(e) => IpcResponse::error("config", "CONFIG_ERROR", e.to_string()),
                }
            }
            IpcRequest::RotateSecret {
                secret_ref,
                plaintext,
            } => {
                info!("RotateSecret requested for ref: {}", secret_ref);
                match crate::vault::rotate_secret(graph, &secret_ref, &plaintext) {
                    Ok(()) => IpcResponse::success("secret", None),
                    Err(e) => IpcResponse::error("secret", "SECRET_ERROR", e.to_string()),
                }
            }
            IpcRequest::AddVaultEntry {
                vault_name,
                plaintext,
                allowed_roles,
                secret_kind,
            } => {
                info!("AddVaultEntry requested: {}", vault_name);
                match Self::handle_add_vault_entry(
                    graph,
                    vault_name,
                    plaintext,
                    allowed_roles,
                    secret_kind,
                ) {
                    Ok(secret_ref) => IpcResponse::success(
                        "vault",
                        Some(serde_json::json!({ "secret_ref": secret_ref })),
                    ),
                    Err(e) => IpcResponse::error("vault", "VAULT_ERROR", e.to_string()),
                }
            }
            IpcRequest::CreateMeshInvite {
                hotel_name,
                mesh_host,
                ttl_secs,
            } => match Self::handle_create_mesh_invite(
                graph,
                local_node_id,
                hotel_name,
                mesh_host,
                ttl_secs,
            )
            .await
            {
                Ok(data) => IpcResponse::success("mesh_invite", Some(data)),
                Err(err) => IpcResponse::error("mesh_invite", "MESH_INVITE_ERROR", err.to_string()),
            },
            IpcRequest::AcceptMeshInvite {
                hotel_name,
                mesh_host,
                invite_json,
            } => match Self::handle_accept_mesh_invite(
                graph,
                local_node_id,
                hotel_name,
                mesh_host,
                invite_json,
            )
            .await
            {
                Ok(data) => IpcResponse::success("mesh_accept", Some(data)),
                Err(err) => IpcResponse::error("mesh_accept", "MESH_ACCEPT_ERROR", err.to_string()),
            },
            IpcRequest::PublishMessage {
                target_role,
                payload,
            } => {
                info!("PublishMessage for role: {}", target_role);
                let task_id = Uuid::new_v4();
                // CronTicker-only keys are never trusted from a guest (DEF-220).
                let (payload_json, _) = strip_forged_cron_keys(payload.to_string());
                Self::record_session_activity_from_value(
                    graph,
                    &payload,
                    Some(task_id),
                    None,
                    Some(&target_role),
                    "publish_message",
                );
                let env = EventEnvelope {
                    event_id: task_id,
                    seq: 0, // Set by the sequence manager in PORT-BP-003
                    source_node_id: local_node_id.to_string(),
                    target_node_id: Some(local_node_id.to_string()),
                    source_agent_id: "unknown".into(), // Will be pulled from connection context
                    target_agent_id: Some(target_role.clone()),
                    kind: EventKind::TaskInvoke,
                    corr_id: "pub".into(),
                    attempt: 0,
                    created_at: 0,
                    expires_at: None,
                    payload: EventPayload::Inline {
                        data: payload_json.clone(),
                    },
                    trace: vec![],
                };
                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;
                Self::deliver_inbound_task(
                    inboxes,
                    local_node_id,
                    &target_role,
                    None,
                    task_id,
                    payload_json,
                )
                .await;
                IpcResponse::success("pub", None)
            }
            IpcRequest::StartWebRtcSession {
                target_node_id,
                target_guest_id,
                session_id,
            } => {
                Self::handle_start_webrtc_session(
                    graph,
                    local_node_id,
                    current_identity.as_ref(),
                    webrtc_signal_tx,
                    target_node_id,
                    target_guest_id,
                    session_id,
                )
                .await
            }
            IpcRequest::GetWebRtcSessionStatus { session_id } => {
                Self::handle_get_webrtc_session_status(session_id).await
            }
            IpcRequest::CreateTask {
                target_role,
                payload,
            } => {
                info!("CreateTask for role: {}", target_role);
                let task_id = Uuid::new_v4();
                // CronTicker-only keys are never trusted from a guest (DEF-220).
                let (payload_json, _) = strip_forged_cron_keys(payload.to_string());
                Self::record_session_activity_from_value(
                    graph,
                    &payload,
                    Some(task_id),
                    Some("queued"),
                    Some(&target_role),
                    "create_task",
                );
                let env = EventEnvelope {
                    event_id: task_id,
                    seq: 0,
                    source_node_id: local_node_id.to_string(),
                    target_node_id: Some(local_node_id.to_string()),
                    source_agent_id: "unknown".into(),
                    target_agent_id: Some(target_role.clone()),
                    kind: EventKind::TaskInvoke,
                    corr_id: "create".into(),
                    attempt: 0,
                    created_at: 0,
                    expires_at: None,
                    payload: EventPayload::Inline {
                        data: payload_json.clone(),
                    },
                    trace: vec![],
                };
                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;
                Self::deliver_inbound_task(
                    inboxes,
                    local_node_id,
                    &target_role,
                    None,
                    task_id,
                    payload_json,
                )
                .await;
                IpcResponse::success(
                    "create",
                    Some(serde_json::json!({ "task_id": task_id.to_string() })),
                )
            }
            IpcRequest::AckEvent { event_id } => {
                info!("AckEvent for: {}", event_id);
                IpcResponse::success("ack", None)
            }
            IpcRequest::UpdateTask {
                task_id,
                state,
                payload,
            } => {
                info!("UpdateTask for: {} to state: {}", task_id, state);
                Self::record_session_activity_from_value(
                    graph,
                    &payload,
                    None,
                    Some(&state),
                    None,
                    "update_task",
                );
                let env = EventEnvelope {
                    event_id: Uuid::new_v4(),
                    seq: 0,
                    source_node_id: local_node_id.to_string(),
                    target_node_id: Some(local_node_id.to_string()),
                    source_agent_id: "unknown".into(),
                    target_agent_id: None,
                    kind: EventKind::TaskInvoke, // Or potentially a new TaskUpdate kind if required
                    corr_id: task_id.to_string(),
                    attempt: 0,
                    created_at: 0,
                    expires_at: None,
                    payload: EventPayload::Inline {
                        data: payload.to_string(),
                    },
                    trace: vec![],
                };
                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;
                IpcResponse::success("update", None)
            }
            IpcRequest::CompleteTask { task_id, result } => {
                info!("CompleteTask for: {}", task_id);
                Self::record_session_activity_from_value(
                    graph,
                    &result,
                    None,
                    Some("completed"),
                    None,
                    "complete_task",
                );
                let env = EventEnvelope {
                    event_id: Uuid::new_v4(),
                    seq: 0,
                    source_node_id: local_node_id.to_string(),
                    target_node_id: Some(local_node_id.to_string()),
                    source_agent_id: "unknown".into(),
                    target_agent_id: None,
                    kind: EventKind::TaskResult,
                    corr_id: task_id.to_string(),
                    attempt: 0,
                    created_at: 0,
                    expires_at: None,
                    payload: EventPayload::Inline {
                        data: result.to_string(),
                    },
                    trace: vec![],
                };
                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;
                IpcResponse::success("complete", None)
            }
            IpcRequest::FailTask {
                task_id,
                error_code,
                reason,
                session_id: fail_session_id,
                turn_id: fail_turn_id,
            } => {
                info!("FailTask for: {} ({}): {}", task_id, error_code, reason);
                // When the caller provides session_id + turn_id (e.g. evict_timed_out_turns),
                // directly mark the session_turn record failed. record_session_activity_from_value
                // can't do this because it requires session_id inside the payload JSON.
                if let (Some(sid), Some(tid)) = (&fail_session_id, &fail_turn_id) {
                    match graph.get_session_turn(sid, tid) {
                        Ok(Some(mut existing)) => {
                            existing.status = "failed".into();
                            existing.error_json =
                                Some(serde_json::json!({"error": &error_code, "reason": &reason}));
                            existing.completed_at = Some(unix_ts());
                            if let Err(e) = graph.upsert_session_turn(&existing) {
                                warn!("FailTask: upsert_session_turn {sid}:{tid} failed: {e}");
                            }
                        }
                        Ok(None) => {} // turn not yet written to DB
                        Err(e) => warn!("FailTask: get_session_turn {sid}:{tid} error: {e}"),
                    }
                }
                // Turn-failure heal intake: provider/model failures flow into
                // the self-heal queue so the heal-dispatcher and A3
                // pattern-filing lane see them instead of only the operator.
                Self::push_turn_failure_heal_entry(
                    heal_queue,
                    current_identity.as_ref().map(|i| i.guest_id.as_str()),
                    &error_code,
                    &reason,
                );
                Self::record_session_activity_from_value(
                    graph,
                    &serde_json::json!({
                        "error": error_code,
                        "reason": reason,
                    }),
                    None,
                    Some("failed"),
                    None,
                    "fail_task",
                );
                let env = EventEnvelope {
                    event_id: Uuid::new_v4(),
                    seq: 0,
                    source_node_id: local_node_id.to_string(),
                    target_node_id: Some(local_node_id.to_string()),
                    source_agent_id: "unknown".into(),
                    target_agent_id: None,
                    kind: EventKind::TaskResult,
                    corr_id: task_id.to_string(),
                    attempt: 0,
                    created_at: 0,
                    expires_at: None,
                    payload: EventPayload::Inline {
                        data: serde_json::json!({
                            "error": error_code,
                            "reason": reason
                        })
                        .to_string(),
                    },
                    trace: vec![],
                };
                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;
                IpcResponse::success("fail", None)
            }
            IpcRequest::RepairStaleSessionTurns { min_age_secs } => {
                // Agent-originated calls (session.repair_stale steward tool)
                // require operational admin authority; the heal-dispatcher
                // and CLI paths pass through unchanged.
                if let Err(refusal) = steward_agent_admin_gate(
                    graph,
                    current_identity.as_ref(),
                    "repair_stale_session_turns",
                ) {
                    return refusal;
                }
                Self::handle_repair_stale_session_turns(graph, heal_queue, min_age_secs)
            }
            // TODO(role-loop): schedule alongside the RepairStaleSessionTurns cron so
            // this runs periodically; for now it is on-demand like RepairStaleSessionTurns.
            IpcRequest::HealRoleHandoffLoops {} => {
                let incarnations = match graph.list_all_role_incarnations() {
                    Ok(v) => v,
                    Err(e) => {
                        error!("HealRoleHandoffLoops: query failed: {e}");
                        return IpcResponse::error("heal", "QUERY_ERROR", e.to_string());
                    }
                };
                // Group active incarnations by agent_id.
                let mut active_by_agent: std::collections::HashMap<
                    String,
                    Vec<RoleIncarnationRecord>,
                > = std::collections::HashMap::new();
                for rec in incarnations {
                    if matches!(rec.readiness_state, RoleReadinessState::ActiveInSession) {
                        active_by_agent
                            .entry(rec.agent_id.clone())
                            .or_default()
                            .push(rec);
                    }
                }

                let mut healed_agents: u32 = 0;
                let mut demoted_guest_ids: std::collections::HashSet<String> =
                    std::collections::HashSet::new();
                for (agent_id, actives) in active_by_agent {
                    if actives.len() <= 1 {
                        continue; // healthy: at most one active incarnation.
                    }
                    // Corrupt state: demote ALL of this agent's incarnations to Routable
                    // so the base agent resumes as orchestrator (matches the manual fix).
                    let all = graph.list_role_incarnations(&agent_id).unwrap_or_default();
                    for rec in &all {
                        demoted_guest_ids.insert(rec.guest_id.clone());
                        if let Err(e) = graph.set_role_incarnation_readiness(
                            &agent_id,
                            &rec.role_name,
                            RoleReadinessState::Routable,
                        ) {
                            warn!(
                                "HealRoleHandoffLoops: demote {agent_id}:{}: {e}",
                                rec.role_name
                            );
                        }
                    }
                    if let Some(hq) = heal_queue {
                        let _ = hq.push_error(
                            &agent_id,
                            &format!(
                                "role_handoff_loop: {} incarnations were ActiveInSession simultaneously; demoted to routable + cleared session pin",
                                actives.len()
                            ),
                        );
                    }
                    warn!(
                        agent_id,
                        active = actives.len(),
                        "HealRoleHandoffLoops: demoted duplicate active incarnations"
                    );
                    healed_agents += 1;
                }

                // Clear any session pin that points at a demoted incarnation.
                if !demoted_guest_ids.is_empty() {
                    if let Ok(sessions) = graph.list_all_sessions() {
                        for mut session in sessions {
                            let pinned = session
                                .active_incarnation_id
                                .as_deref()
                                .map(|g| demoted_guest_ids.contains(g))
                                .unwrap_or(false);
                            if pinned {
                                session.active_incarnation_id = None;
                                session.updated_at = unix_ts();
                                let _ = graph.upsert_session(&session);
                            }
                        }
                    }
                }

                if healed_agents > 0 {
                    info!(
                        healed_agents,
                        "HealRoleHandoffLoops: healed role-handoff loops"
                    );
                }
                IpcResponse::success(
                    "heal",
                    Some(serde_json::json!({"healed_agents": healed_agents})),
                )
            }
            IpcRequest::SubscribeInbox { role } => {
                info!("SubscribeInbox for role: {}", role);
                let guest = {
                    let guard = inboxes.lock().await;
                    guard
                        .values()
                        .flat_map(|subscribers| subscribers.iter())
                        .find(|subscriber| subscriber.conn_id == conn_id)
                        .cloned()
                };
                let guest_id = guest
                    .as_ref()
                    .map(|subscriber| subscriber.guest_id.as_str())
                    .unwrap_or("unknown");
                let supported_tools = guest
                    .as_ref()
                    .map(|subscriber| subscriber.supported_tools.as_slice())
                    .unwrap_or(&[]);
                Self::add_subscription(
                    inboxes,
                    &role,
                    conn_id,
                    guest_id,
                    supported_tools,
                    outbound_tx,
                    subscribed_roles,
                )
                .await;
                if let Some(guest_id) = guest.as_ref().map(|subscriber| subscriber.guest_id.clone())
                {
                    if let Ok(role_records) = graph.list_role_incarnations_by_routing_role(&role) {
                        for role_record in role_records
                            .into_iter()
                            .filter(|record| record.guest_id == guest_id)
                        {
                            if let Err(err) = graph.set_role_incarnation_readiness(
                                &role_record.agent_id,
                                &role_record.role_name,
                                RoleReadinessState::Routable,
                            ) {
                                warn!(
                                    "Failed to mark role [{}] routable on SubscribeInbox: {}",
                                    role_record.role_name, err
                                );
                            }
                        }
                    }
                }
                IpcResponse::success("sub", None)
            }
            IpcRequest::AcquireTelegramPollLease {
                lease_key,
                agent_id,
                resource_ref,
            } => {
                Self::handle_acquire_telegram_poll_lease(
                    graph,
                    local_node_id,
                    telegram_poll_leases,
                    conn_id,
                    current_identity.as_ref(),
                    lease_key,
                    agent_id,
                    resource_ref,
                )
                .await
            }
            IpcRequest::ReportTelegramPollConflict {
                agent_id,
                resource_ref,
                conflicts,
            } => Self::handle_report_telegram_poll_conflict(
                graph,
                local_node_id,
                agent_id,
                resource_ref,
                conflicts,
            ),
            IpcRequest::GetTelegramPollLeaseOwner { lease_key } => {
                Self::handle_get_telegram_poll_lease_owner(
                    graph,
                    local_node_id,
                    telegram_poll_leases,
                    lease_key,
                )
                .await
            }
            IpcRequest::RenewTelegramPollLease {
                lease_key,
                agent_id,
                resource_ref,
                lease_epoch,
            } => {
                Self::handle_renew_telegram_poll_lease(
                    graph,
                    local_node_id,
                    telegram_poll_leases,
                    conn_id,
                    current_identity.as_ref(),
                    lease_key,
                    agent_id,
                    resource_ref,
                    lease_epoch,
                )
                .await
            }
            IpcRequest::AcquireDesktopMembraneLease { lease_key, port } => {
                Self::handle_acquire_desktop_membrane_lease(
                    graph,
                    local_node_id,
                    desktop_membrane_leases,
                    conn_id,
                    current_identity.as_ref(),
                    lease_key,
                    port,
                )
                .await
            }
            IpcRequest::GetDesktopMembraneLeaseOwner { lease_key } => {
                Self::handle_get_desktop_membrane_lease_owner(desktop_membrane_leases, lease_key)
                    .await
            }
            IpcRequest::GetDesktopMembraneStatus => {
                match Self::desktop_membrane_status_view(graph, local_node_id) {
                    Ok(membrane_status) => {
                        IpcResponse::DesktopMembraneStatusView { membrane_status }
                    }
                    Err(err) => IpcResponse::error(
                        "desktop_membrane_status",
                        "DESKTOP_MEMBRANE_STATUS_ERROR",
                        err.to_string(),
                    ),
                }
            }
            IpcRequest::GetDesktopMembraneTargetStatus { target_node_id } => {
                match Self::desktop_membrane_target_status_view(
                    registry,
                    graph,
                    local_node_id,
                    &target_node_id,
                )
                .await
                {
                    Ok(membrane_target_status) => IpcResponse::DesktopMembraneTargetStatusView {
                        membrane_target_status,
                    },
                    Err(err) => IpcResponse::error(
                        "desktop_membrane_target_status",
                        "DESKTOP_MEMBRANE_TARGET_STATUS_ERROR",
                        err.to_string(),
                    ),
                }
            }
            request @ (IpcRequest::QueryOperatorTargets
            | IpcRequest::QueryOperatorTargetStatus { .. }
            | IpcRequest::QueryOperatorTargetGuests { .. }
            | IpcRequest::QueryOperatorTargetAgents { .. }
            | IpcRequest::QueryOperatorTargetComponents { .. }
            | IpcRequest::QueryOperatorTargetConfig { .. }
            | IpcRequest::QueryOperatorTargetSecrets { .. }
            | IpcRequest::QueryOperatorTargetPlacement { .. }
            | IpcRequest::QueryOperatorTargetSurface { .. }
            | IpcRequest::RegisterOperatorTargetComponent { .. }
            | IpcRequest::SetOperatorTargetComponentActive { .. }
            | IpcRequest::RestartOperatorTargetComponent { .. }
            | IpcRequest::RemoveOperatorTargetComponent { .. }
            | IpcRequest::SetOperatorTargetConfig { .. }
            | IpcRequest::RotateOperatorTargetSecret { .. }
            | IpcRequest::AddOperatorTargetVaultEntry { .. }
            | IpcRequest::SetOperatorTargetRoleHome { .. }) => {
                Self::handle_operator_target_request(
                    request,
                    registry,
                    graph,
                    materialization_requester,
                    local_node_id,
                )
                .await
            }
            IpcRequest::ListDesktopMembraneGuests => {
                match Self::desktop_membrane_guest_views(graph, local_node_id) {
                    Ok(membrane_guests) => {
                        IpcResponse::DesktopMembraneGuestsView { membrane_guests }
                    }
                    Err(err) => IpcResponse::error(
                        "desktop_membrane_guests",
                        "DESKTOP_MEMBRANE_GUESTS_ERROR",
                        err.to_string(),
                    ),
                }
            }
            IpcRequest::ListDesktopMembraneTargetGuests { target_node_id } => {
                match Self::desktop_membrane_target_guest_inventory_view(
                    registry,
                    graph,
                    local_node_id,
                    &target_node_id,
                )
                .await
                {
                    Ok(membrane_target_guests) => IpcResponse::DesktopMembraneTargetGuestsView {
                        membrane_target_guests,
                    },
                    Err(err) => IpcResponse::error(
                        "desktop_membrane_target_guests",
                        "DESKTOP_MEMBRANE_TARGET_GUESTS_ERROR",
                        err.to_string(),
                    ),
                }
            }
            IpcRequest::ListDesktopMembraneTargetComponents { target_node_id } => {
                match Self::operator_target_component_inventory_view(
                    registry,
                    graph,
                    local_node_id,
                    &target_node_id,
                )
                .await
                {
                    Ok(membrane_target_components) => {
                        IpcResponse::DesktopMembraneTargetComponentsView {
                            membrane_target_components,
                        }
                    }
                    Err(err) => IpcResponse::error(
                        "desktop_membrane_target_components",
                        "DESKTOP_MEMBRANE_TARGET_COMPONENTS_ERROR",
                        err.to_string(),
                    ),
                }
            }
            IpcRequest::SendOperatorChatTurn {
                target_node_id,
                target_agent_id,
                operator_session_id,
                conversation_id,
                content,
            } => {
                match Self::send_operator_chat_turn(
                    registry,
                    graph,
                    local_node_id,
                    &target_node_id,
                    &target_agent_id,
                    &operator_session_id,
                    conversation_id.as_deref(),
                    &content,
                )
                .await
                {
                    Ok(operator_chat_reply) => IpcResponse::OperatorChatTurnReply {
                        operator_chat_reply,
                    },
                    Err(err) => {
                        IpcResponse::error("operator_chat", "OPERATOR_CHAT_ERROR", err.to_string())
                    }
                }
            }
            IpcRequest::ListOperatorSessions {
                target_agent_id,
                limit,
            } => Self::handle_list_operator_sessions(graph, target_agent_id.as_deref(), limit),
            IpcRequest::ListSessionTurns {
                session_id,
                limit,
                before_turn_id,
            } => Self::handle_list_session_turns(
                graph,
                &session_id,
                limit,
                before_turn_id.as_deref(),
            ),
            IpcRequest::GetMeshRoster => {
                Self::handle_get_mesh_roster(registry, graph, local_node_id).await
            }
            IpcRequest::ListDesktopMembraneAgents => {
                match Self::desktop_membrane_agent_views(graph, local_node_id) {
                    Ok(membrane_agents) => {
                        IpcResponse::DesktopMembraneAgentsView { membrane_agents }
                    }
                    Err(err) => IpcResponse::error(
                        "desktop_membrane_agents",
                        "DESKTOP_MEMBRANE_AGENTS_ERROR",
                        err.to_string(),
                    ),
                }
            }
            IpcRequest::ListDesktopMembraneTargets => {
                match Self::desktop_membrane_target_views(registry, graph, local_node_id).await {
                    Ok(membrane_targets) => {
                        IpcResponse::DesktopMembraneTargetsView { membrane_targets }
                    }
                    Err(err) => IpcResponse::error(
                        "desktop_membrane_targets",
                        "DESKTOP_MEMBRANE_TARGETS_ERROR",
                        err.to_string(),
                    ),
                }
            }
            IpcRequest::RenewDesktopMembraneLease {
                lease_key,
                lease_epoch,
            } => {
                Self::handle_renew_desktop_membrane_lease(
                    desktop_membrane_leases,
                    conn_id,
                    current_identity.as_ref(),
                    lease_key,
                    lease_epoch,
                )
                .await
            }
            IpcRequest::ReleaseDesktopMembraneLease { lease_key } => {
                Self::handle_release_desktop_membrane_lease(
                    desktop_membrane_leases,
                    conn_id,
                    current_identity.as_ref(),
                    lease_key,
                )
                .await
            }
            IpcRequest::AcquireDiscordGatewayLease {
                lease_key,
                agent_id,
            } => {
                Self::handle_acquire_discord_gateway_lease(
                    graph,
                    local_node_id,
                    discord_gateway_leases,
                    conn_id,
                    current_identity.as_ref(),
                    lease_key,
                    agent_id,
                )
                .await
            }
            IpcRequest::GetDiscordGatewayLeaseOwner { lease_key } => {
                Self::handle_get_discord_gateway_lease_owner(
                    graph,
                    local_node_id,
                    discord_gateway_leases,
                    lease_key,
                )
                .await
            }
            IpcRequest::RenewDiscordGatewayLease {
                lease_key,
                agent_id,
                lease_epoch,
            } => {
                Self::handle_renew_discord_gateway_lease(
                    graph,
                    local_node_id,
                    discord_gateway_leases,
                    conn_id,
                    current_identity.as_ref(),
                    lease_key,
                    agent_id,
                    lease_epoch,
                )
                .await
            }
            IpcRequest::ReleaseDiscordGatewayLease { lease_key } => {
                Self::handle_release_discord_gateway_lease(
                    discord_gateway_leases,
                    conn_id,
                    current_identity.as_ref(),
                    lease_key,
                )
                .await
            }
            IpcRequest::ReleaseTelegramPollLease { lease_key } => {
                Self::handle_release_telegram_poll_lease(
                    telegram_poll_leases,
                    conn_id,
                    current_identity.as_ref(),
                    lease_key,
                )
                .await
            }
            IpcRequest::SyncApartment {
                agent_id,
                memory_type,
                content_json,
            } => {
                info!("SyncApartment for: {} ({})", agent_id, memory_type);
                if let Err(e) = graph.sync_apartment(&agent_id, &memory_type, &content_json) {
                    error!("Failed to sync memory apartment: {}", e);
                    return IpcResponse::error("sync", "SYNC_ERROR", e.to_string());
                }
                Self::record_apartment_checkpoint(graph, &agent_id, &memory_type, &content_json);
                if memory_type == "command_manifest" {
                    // The philote just published its slash-command manifest: tell
                    // peer hotels, so a Telegram seat there can menu it (DEF-180).
                    crate::service::command_manifest::on_local_manifest_written(
                        graph,
                        &dispatcher_tx,
                        local_node_id,
                        &agent_id,
                        &content_json,
                    )
                    .await;
                }
                IpcResponse::success("sync", None)
            }
            IpcRequest::QueryStatus { task_id: _ } => IpcResponse::success("query", None),
            IpcRequest::QueryTimeline { task_id: _ } => IpcResponse::success("timeline", None),
            IpcRequest::EmitTask {
                target_node,
                target_role,
                target_guest_id,
                task_json,
            } => {
                // Silent-ack suppression (Hermes `[SILENT]` convention): a
                // `send_reply` from an isolated `silent_ok` cron session whose
                // content is a silence token is never delivered to the
                // operator channel — see `silent_cron_reply_suppressed`.
                if silent_cron_reply_suppressed(graph, &task_json) {
                    info!(
                        target_role = target_role.as_str(),
                        "EmitTask: suppressing silent cron reply (silent_ok job, [SILENT] token)"
                    );
                    return IpcResponse::success("emit", None);
                }
                let (task_json, forged) = strip_forged_cron_keys(task_json);
                if !forged.is_empty() {
                    warn!(
                        target_role = target_role.as_str(),
                        guest_id = current_identity.as_ref().map(|i| i.guest_id.as_str()).unwrap_or("-"),
                        stripped = ?forged,
                        "EmitTask: stripped CronTicker-only keys from a guest task"
                    );
                }
                let task_json = stamp_reply_owner_agent(
                    graph,
                    current_identity.as_ref(),
                    &target_role,
                    target_guest_id.as_deref(),
                    task_json,
                );
                // Normalize the client-SDK default node id sentinel. A client
                // that never learned its node (PHILOTIC_NODE_ID unset) sends
                // "local-aiua-01", which means "the hotel I am connected to".
                // Before this normalization such tasks were appended to the
                // ledger addressed to a node that exists nowhere and silently
                // black-holed — surfacing only as healed zombie turns
                // (2026-07-19 Beacon/life.observe investigation).
                //
                // Guarded twice, because "local-aiua-01" is only *usually* a
                // sentinel — a node can legitimately carry that literal name
                // (bridge tests, single-node dev hotels):
                //   1. if a mesh peer is actually NAMED local-aiua-01
                //      (registry entry or peer socket), it is a real remote
                //      target — e.g. the return address of a cross-hotel
                //      reply — and must not be hijacked;
                //   2. only rewrite when the role has a live local subscriber,
                //      i.e. the task is actually deliverable here. Otherwise
                //      leave the envelope alone (ledger/bridge relays may own
                //      it) and let the unknown-node guard below make the
                //      misroute visible instead.
                let target_node = if target_node == CLIENT_DEFAULT_NODE_ID
                    && target_node != local_node_id
                {
                    let sentinel_is_real_peer =
                        {
                            let reg = registry.read().await;
                            reg.get_node(&target_node).is_some()
                        } || peer_sockets.read().await.contains_key(&target_node);
                    let role_has_local_subscriber = {
                        let guard = inboxes.lock().await;
                        guard
                            .get(target_role.as_str())
                            .map(|subs| !subs.is_empty())
                            .unwrap_or(false)
                    };
                    if !sentinel_is_real_peer && role_has_local_subscriber {
                        info!(
                            target_role = target_role.as_str(),
                            local_node_id,
                            "EmitTask: normalizing client-default node id sentinel to this hotel"
                        );
                        local_node_id.to_string()
                    } else {
                        target_node
                    }
                } else {
                    target_node
                };
                let response_like_agent_action = response_like_agent_action_for_task(
                    &target_role,
                    target_guest_id.as_deref(),
                    &task_json,
                );
                let target_guest_id = if target_guest_id.is_none() {
                    let live_agent_guests: Vec<String> = {
                        let guard = inboxes.lock().await;
                        guard
                            .get(target_role.as_str())
                            .into_iter()
                            .flatten()
                            .map(|subscriber| subscriber.guest_id.clone())
                            .collect()
                    };
                    let inferred = infer_response_target_guest_id_for_agent_task(
                        graph,
                        local_node_id,
                        &target_role,
                        None,
                        &task_json,
                        &live_agent_guests,
                        heal_queue,
                    );
                    if let Some(ref guest_id) = inferred {
                        info!(
                            target_role = target_role.as_str(),
                            guest_id = guest_id.as_str(),
                            "EmitTask: inferred explicit guest target for response-like agent payload"
                        );
                    }
                    inferred
                } else {
                    target_guest_id
                };
                if target_guest_id.is_none() && target_node != local_node_id {
                    // A response bound for a REMOTE hotel cannot be resolved
                    // here: the return guest and its session live on the
                    // target hotel, so local subscriber inference is
                    // meaningless. Rejecting these locally silently ate
                    // cross-hotel replies (live 2026-08-25: mac-jane's life.*
                    // datasource_response returns died at this gate on vps).
                    // Forward; the target hotel resolves or rejects with the
                    // session context only it has.
                    if let Some(action) = response_like_agent_action.as_deref() {
                        info!(
                            action,
                            target_node = target_node.as_str(),
                            "EmitTask: forwarding guest-less response-like task to its home hotel for resolution"
                        );
                    }
                }
                if target_guest_id.is_none() && target_node == local_node_id {
                    if let Some(action) = response_like_agent_action {
                        let message = format!(
                            "[response_route_unresolved] response-like action [{action}] targeted role [agent] without a concrete return guest"
                        );
                        warn!(
                            action = action.as_str(),
                            target_role = target_role.as_str(),
                            target_node = target_node.as_str(),
                            "EmitTask rejected unresolved response return route"
                        );
                        // RC-4 (2026-07-09 stuck-turn forensic): classify+tag so this
                        // becomes A3-countable instead of filing zero heal rows.
                        if let Some(hq) = heal_queue {
                            match hq.push_classified(
                                "aiua.response_return_route",
                                &message,
                                "medium",
                                "response_route_unresolved",
                            ) {
                                Ok(_) => {}
                                Err(err) => {
                                    warn!(
                                        error = %err,
                                        "Failed to push unresolved response route to heal queue"
                                    );
                                }
                            }
                        }
                        return IpcResponse::error(
                            "emit_task",
                            "RESPONSE_ROUTE_UNRESOLVED",
                            message,
                        );
                    }
                }

                // Short-circuit operator surface queries to the in-process channel,
                // eliminating the UDS self-connection and its socket leak.
                // Only short-circuit for local-node targets; cross-hotel queries must
                // fall through to the normal mesh dispatch path.
                if target_role == philotic_client::OPERATOR_SURFACE_QUERY_ROLE
                    && target_node == local_node_id
                {
                    if let Some(tx) = operator_surface_tx {
                        let _ = tx.try_send(task_json).ok();
                        return IpcResponse::Standard {
                            ok: true,
                            code: "OK".into(),
                            message: "operator surface query dispatched in-process".into(),
                            corr_id: String::new(),
                            data: None,
                        };
                    }
                }
                // Short-circuit synchronous CapabilityInvoke responses.
                if target_role == CAPABILITY_ROUTER_ROLE {
                    if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&task_json) {
                        let turn_id = payload
                            .get("turn_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let sender = {
                            let mut guard = pending_capability_calls.lock().await;
                            guard.remove(&turn_id)
                        };
                        if let Some(tx) = sender {
                            let result = if let Some(err) = payload.get("error") {
                                Err(err
                                    .get("message")
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("capability failed")
                                    .to_string())
                            } else {
                                Ok(payload
                                    .get("result")
                                    .cloned()
                                    .unwrap_or(serde_json::Value::Null))
                            };
                            let _ = tx.send(result);
                        }
                    }
                    return IpcResponse::success("capability_router", None);
                }
                // Short-circuit Golgi capability responses — skip agent-context enrichment.
                if target_role == GOLGI_SINK_ROLE {
                    Self::handle_golgi_capability_response(
                        &pending_pipelines,
                        &inboxes,
                        local_node_id,
                        &task_json,
                    )
                    .await;
                    return IpcResponse::success("golgi", None);
                }
                let task_json = match (
                    infer_agent_context_for_task(
                        graph,
                        &target_role,
                        target_guest_id.as_deref(),
                        &task_json,
                    ),
                    Self::local_hotel_name(graph, local_node_id),
                ) {
                    (Some(context), Some(local_hotel))
                        if context.authority_hotel.as_deref() == Some(local_hotel.as_str()) =>
                    {
                        attach_agent_graph_snapshot(
                            &task_json,
                            Some(&context.agent_id),
                            local_node_id,
                        )
                    }
                    _ => task_json,
                };
                // Response-like payloads already went through
                // infer_response_target_guest_id_for_agent_task above, which is the
                // authoritative resolver for outbound responses. Don't also run them
                // through resolve_agent_route: that resolver re-derives from
                // session.active_incarnation_id whenever the resolved guest_id equals
                // primary_agent_id (its "targets_base_agent" case, meant for fresh
                // inbound task delivery) and would clobber the inferred fallback right
                // back to the unregistered active incarnation it was meant to avoid.
                let mut route_resolution = if response_like_agent_action.is_some() {
                    AgentRouteResolution::Deliver(target_guest_id.clone())
                } else {
                    Self::resolve_agent_route(
                        graph,
                        inboxes,
                        local_node_id,
                        &target_role,
                        target_guest_id.clone(),
                        &task_json,
                    )
                    .await
                };
                // Last-resort orchestrator fallback for fresh inbound agent tasks:
                // resolve_agent_route returns Deliver(active_incarnation) for guests
                // that are not configured locally so the auto-reroute below can forward
                // them to their home hotel. But if the guest is ALSO unknown to the mesh
                // (no advertisement, no HotelStateSync roster entry, no home_node), the
                // task would be "delivered" to a guest that exists nowhere and silently
                // black-holed. Route it to the session's live orchestrator instead.
                if response_like_agent_action.is_none()
                    && target_role == "agent"
                    && target_node == local_node_id
                {
                    if let AgentRouteResolution::Deliver(Some(ref resolved_guest_id)) =
                        route_resolution
                    {
                        let is_live_local = {
                            let guard = inboxes.lock().await;
                            guard
                                .get(target_role.as_str())
                                .into_iter()
                                .flatten()
                                .any(|subscriber| &subscriber.guest_id == resolved_guest_id)
                        };
                        if !is_live_local
                            && !Self::configured_local_guest_exists(
                                graph,
                                local_node_id,
                                resolved_guest_id,
                            )
                        {
                            let remote_home = {
                                let reg = registry.read().await;
                                Self::resolve_guest_home_node(graph, &reg, resolved_guest_id)
                            };
                            if remote_home.is_none() {
                                let session = serde_json::from_str::<serde_json::Value>(&task_json)
                                    .ok()
                                    .and_then(|payload| {
                                        payload
                                            .get("session_id")
                                            .and_then(serde_json::Value::as_str)
                                            .map(str::to_string)
                                    })
                                    .and_then(|session_id| {
                                        graph.get_session(&session_id).ok().flatten()
                                    });
                                if let Some(session) = session {
                                    let live_agent_guests: Vec<String> = {
                                        let guard = inboxes.lock().await;
                                        guard
                                            .get(target_role.as_str())
                                            .into_iter()
                                            .flatten()
                                            .map(|subscriber| subscriber.guest_id.clone())
                                            .collect()
                                    };
                                    if let Some(orchestrator_guest_id) =
                                        Self::resolve_orchestrator_guest_id(
                                            graph,
                                            &session,
                                            &live_agent_guests,
                                        )
                                    {
                                        if Self::same_agent_guest(
                                            &orchestrator_guest_id,
                                            resolved_guest_id,
                                        ) {
                                            warn!(
                                                "Resolved agent guest [{}] is not configured locally and unknown to the mesh; falling back to orchestrator guest [{}].",
                                                resolved_guest_id, orchestrator_guest_id
                                            );
                                            route_resolution = AgentRouteResolution::Deliver(Some(
                                                orchestrator_guest_id,
                                            ));
                                        } else {
                                            // A task addressed to one agent must never be
                                            // answered as another (DEF-177): Beacon's bot
                                            // polled from a hotel that does not host her
                                            // reached Björk's orchestrator and was answered
                                            // as Björk. Better undelivered than impersonated.
                                            error!(
                                                "Resolved agent guest [{}] is not configured locally and unknown to the mesh; refusing to hand its task to another agent's orchestrator [{}].",
                                                resolved_guest_id, orchestrator_guest_id
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // Compute resolved_target_guest_id BEFORE the auto-reroute block so we can
                // gate on it — target_guest_id may be None (e.g. membrane sends target_role="agent"
                // with no explicit guest_id), but resolve_agent_route fills in the active brain/
                // orchestrator guest from the session's active_incarnation_id.
                let resolved_target_guest_id = match &route_resolution {
                    AgentRouteResolution::Deliver(guest_id) => guest_id.clone(),
                    AgentRouteResolution::Park { guest_id } => Some(guest_id.clone()),
                };
                // If the caller targeted this node but the resolved guest isn't configured here,
                // look up the live node via registry (advertisements + HotelStateSync roster)
                // so the mesh dispatcher forwards the task to the correct hotel automatically.
                let target_node = if target_node == local_node_id {
                    if let Some(ref guest_id) = resolved_target_guest_id {
                        // A guest subscribed to this inbox right now is local by
                        // definition — never forward its task to a peer whose
                        // roster happens to name the same agent (DEF-217).
                        let is_live_local = {
                            let guard = inboxes.lock().await;
                            guard
                                .get(target_role.as_str())
                                .into_iter()
                                .flatten()
                                .any(|subscriber| &subscriber.guest_id == guest_id)
                        };
                        if !is_live_local
                            && !Self::configured_local_guest_exists(graph, local_node_id, guest_id)
                        {
                            let reg = registry.read().await;
                            if let Some(remote) =
                                Self::resolve_guest_home_node(graph, &reg, guest_id)
                            {
                                info!(guest_id, remote, "EmitTask: auto-routing to mesh peer");
                                remote
                            } else {
                                target_node
                            }
                        } else {
                            target_node
                        }
                    } else if target_role != "agent" {
                        // No specific guest_id. For non-agent roles, discover the nearest
                        // mesh peer advertising this role when no local subscriber is active.
                        let has_local_sub = {
                            let guard = inboxes.lock().await;
                            guard
                                .get(target_role.as_str())
                                .map(|v| !v.is_empty())
                                .unwrap_or(false)
                        };
                        if !has_local_sub {
                            let reg = registry.read().await;
                            if let Some(ad) = reg
                                .advertisements_for_role(target_role.as_str())
                                .filter(|ad| ad.node_id != local_node_id)
                                .min_by_key(|ad| ad.latency_hint_ms.unwrap_or(u32::MAX))
                            {
                                info!(
                                    target_role = target_role.as_str(),
                                    remote_node = ad.node_id.as_str(),
                                    "EmitTask: role-based mesh discovery"
                                );
                                ad.node_id.clone()
                            } else {
                                target_node
                            }
                        } else {
                            target_node
                        }
                    } else {
                        target_node
                    }
                } else {
                    target_node
                };
                let task_json = if target_node == local_node_id {
                    attach_delivery_context(
                        graph,
                        local_node_id,
                        &target_role,
                        resolved_target_guest_id.as_deref(),
                        &task_json,
                    )
                } else if let Some(guest_id) = resolved_target_guest_id.as_deref() {
                    // Inject delivery_target_guest_id so the remote hotel's
                    // deliver_event_envelope_or_park can filter to this specific guest.
                    if let Ok(mut payload) = serde_json::from_str::<serde_json::Value>(&task_json) {
                        if let Some(obj) = payload.as_object_mut() {
                            obj.entry("delivery_target_guest_id")
                                .or_insert_with(|| serde_json::json!(guest_id));
                        }
                        serde_json::to_string(&payload).unwrap_or(task_json)
                    } else {
                        task_json
                    }
                } else {
                    task_json
                };
                // Acceptance is a delivery contract: never acknowledge a task
                // whose destination is absent from both the live registry and
                // the explicit peer bridge. A later node appearance cannot
                // rescue a caller that already received a false-success reply.
                if target_node != local_node_id {
                    let has_peer_socket = peer_sockets.read().await.contains_key(&target_node);
                    let node_known = {
                        let reg = registry.read().await;
                        reg.get_node(&target_node).is_some()
                    } || has_peer_socket;
                    if !node_known {
                        let message = format!(
                            "[emit_task_unknown_target_node] task for role [{target_role}] addressed to node [{target_node}] unknown to this hotel (no registry entry, no peer socket) — undeliverable until that node appears on the mesh"
                        );
                        warn!(
                            target_node = target_node.as_str(),
                            target_role = target_role.as_str(),
                            "EmitTask: target node unknown to this hotel — task may never deliver"
                        );
                        if let Some(hq) = heal_queue {
                            if let Err(err) = hq.push_classified(
                                "aiua.emit_task_route",
                                &message,
                                "medium",
                                "emit_task_unknown_target_node",
                            ) {
                                warn!(
                                    error = %err,
                                    "Failed to push unknown-target-node route to heal queue"
                                );
                            }
                        }
                        return IpcResponse::error("emit_task", "TARGET_NODE_UNREACHABLE", message);
                    }

                    // Fail-fast for INTERACTIVE tool dispatch to a peer whose
                    // mesh link is down: a registered peer that has stopped
                    // heartbeating (>TTL) still passes the unknown-node gate,
                    // so the task enters the store-and-forward ledger and the
                    // caller's turn hangs in WaitingTool until the 300s
                    // watchdog (live incident 2026-08-25: mac-jane's tailnet
                    // was down; every cross-hotel life.* call black-holed).
                    // Scoped to execute_tool payloads on purpose — replies and
                    // turn events keep riding store-and-forward through brief
                    // peer blips, which is exactly what the ledger is for.
                    // The peer-socket bridge has no heartbeat behind it and is
                    // exempt.
                    let is_tool_dispatch = serde_json::from_str::<serde_json::Value>(&task_json)
                        .ok()
                        .and_then(|v| {
                            v.get("action")
                                .and_then(serde_json::Value::as_str)
                                .map(|a| a == "execute_tool")
                        })
                        .unwrap_or(false);
                    if is_tool_dispatch && !has_peer_socket {
                        let stale = {
                            let reg = registry.read().await;
                            reg.is_node_stale(&target_node)
                        };
                        if stale {
                            let ttl =
                                ansible_mesh_core::registry::NodeRegistry::freshness_ttl_secs();
                            let message = format!(
                                "[emit_task_unknown_target_node:stale] tool dispatch for role [{target_role}] addressed to node [{target_node}], which has not heartbeated in >{ttl}s — mesh link down; failing fast instead of queueing an interactive task"
                            );
                            warn!(
                                target_node = target_node.as_str(),
                                target_role = target_role.as_str(),
                                "EmitTask: tool dispatch to stale peer — failing fast"
                            );
                            if let Some(hq) = heal_queue {
                                if let Err(err) = hq.push_classified(
                                    "aiua.emit_task_route",
                                    &message,
                                    "medium",
                                    "emit_task_unknown_target_node:stale",
                                ) {
                                    warn!(
                                        error = %err,
                                        "Failed to push stale-target-node route to heal queue"
                                    );
                                }
                            }
                            return IpcResponse::error(
                                "emit_task",
                                "TARGET_NODE_UNREACHABLE",
                                message,
                            );
                        }
                    }
                }
                info!(
                    "EmitTask mapped to TaskInvoke for {}/{} guest={:?}",
                    target_node, target_role, resolved_target_guest_id
                );
                let task_id = Uuid::new_v4();
                if let Ok(payload) = serde_json::from_str::<serde_json::Value>(&task_json) {
                    Self::record_session_activity_from_value(
                        graph,
                        &payload,
                        Some(task_id),
                        Some("running"),
                        Some(&target_role),
                        "emit_task",
                    );
                }
                // An attachment's `blob_download_url` is this hotel's loopback,
                // meaningless to the peer (DEF-200: a voice note from a Telegram
                // seat on mac-jane died on the vps dialling its own 127.0.0.1).
                // Carry small blobs with the task; the peer re-files them.
                let wire_json = if target_node != local_node_id {
                    crate::service::blob_transfer::embed_local_blobs(&task_json).await
                } else {
                    task_json.clone()
                };
                let env = EventEnvelope {
                    event_id: task_id,
                    seq: 0,
                    source_node_id: local_node_id.to_string(),
                    target_node_id: Some(target_node.clone()),
                    source_agent_id: "unknown".into(),
                    target_agent_id: Some(target_role.clone()),
                    kind: EventKind::TaskInvoke,
                    corr_id: "emit".into(),
                    attempt: 0,
                    created_at: 0,
                    expires_at: None,
                    payload: EventPayload::Inline { data: wire_json },
                    trace: vec![],
                };
                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;
                if target_node != local_node_id {
                    // When a peer socket is registered for this node (smoke-test cross-hotel
                    // forwarding), relay the task directly via the peer's UDS socket.
                    let peer_path = peer_sockets.read().await.get(&target_node).cloned();
                    if let Some(peer_path) = peer_path {
                        let task_json_fwd = task_json.clone();
                        let target_node_fwd = target_node.clone();
                        let target_role_fwd = target_role.clone();
                        // Strip the "<node_id>:" incarnation prefix from target_guest_id
                        // before forwarding: the remote hotel's subscriber is registered
                        // under its short guest_id, not the full incarnation_id.
                        let target_guest_id_fwd = target_guest_id.as_deref().map(|g| {
                            let prefix = format!("{}:", target_node);
                            g.strip_prefix(prefix.as_str()).unwrap_or(g).to_string()
                        });
                        tokio::spawn(async move {
                            match PhiloticClient::connect_at(
                                &peer_path,
                                GuestIdentity {
                                    guest_id: "cross-hotel-proxy".into(),
                                    role: "proxy".into(),
                                    supported_tools: vec![],
                                },
                            )
                            .await
                            {
                                Ok(mut peer_client) => {
                                    let _ = peer_client
                                        .send_request(IpcRequest::EmitTask {
                                            target_node: target_node_fwd,
                                            target_role: target_role_fwd,
                                            target_guest_id: target_guest_id_fwd,
                                            task_json: task_json_fwd,
                                        })
                                        .await;
                                }
                                Err(err) => {
                                    warn!(
                                        "Cross-hotel proxy failed to connect to {peer_path}: {err}"
                                    );
                                }
                            }
                        });
                    }
                }
                if target_node == local_node_id {
                    match route_resolution {
                        AgentRouteResolution::Deliver(target_guest_id) => {
                            // Golgi trans hook: intercept if a pipeline rule matches.
                            if let Some((cap_role, cap_id, cap_json)) = Self::try_golgi_intercept(
                                graph,
                                local_node_id,
                                &target_role,
                                target_guest_id.as_deref(),
                                task_id,
                                &task_json,
                                &pending_pipelines,
                            )
                            .await
                            {
                                info!(
                                    "Golgi: intercepting task {} → capability '{}'",
                                    task_id, cap_role
                                );
                                let delivered = Self::deliver_inbound_task(
                                    inboxes,
                                    local_node_id,
                                    &cap_role,
                                    None,
                                    cap_id,
                                    cap_json,
                                )
                                .await;
                                if !delivered {
                                    Self::report_unserved_local_role(
                                        graph, heal_queue, &cap_role, None, task_id, &task_json,
                                    );
                                }
                            } else {
                                let delivered = Self::deliver_inbound_task(
                                    inboxes,
                                    local_node_id,
                                    &target_role,
                                    target_guest_id.as_deref(),
                                    task_id,
                                    task_json.clone(),
                                )
                                .await;
                                if !delivered {
                                    // Before declaring the drop final, try to revive
                                    // this hotel's own guest for the role and park the
                                    // task for it — the runner may be dormant after a
                                    // deploy or dead after a hotel crash. Only when no
                                    // rescue applies is the turn failed.
                                    let rescued = Self::rescue_unserved_role_task(
                                        graph,
                                        &parked_inbound,
                                        materialization_requester,
                                        local_node_id,
                                        &target_role,
                                        local_node_id,
                                        task_id,
                                        &task_json,
                                    )
                                    .await;
                                    if rescued.is_none() {
                                        Self::report_unserved_local_role(
                                            graph,
                                            heal_queue,
                                            &target_role,
                                            target_guest_id.as_deref(),
                                            task_id,
                                            &task_json,
                                        );
                                    }
                                }
                            }
                        }
                        AgentRouteResolution::Park { guest_id } => {
                            {
                                let mut guard = parked_inbound.lock().await;
                                guard.entry(guest_id.clone()).or_default().push(
                                    ParkedInboundTask {
                                        source_node: local_node_id.to_string(),
                                        task_id,
                                        task_json: task_json.clone(),
                                        activate_session_id: None,
                                        parked_at: unix_ts(),
                                    },
                                );
                            }
                            if let Some(requester) = materialization_requester {
                                if let Err(err) = requester.ensure_guest_active(&guest_id).await {
                                    warn!(
                                        "Failed to request on-demand materialization for guest [{}]: {}",
                                        guest_id, err
                                    );
                                }
                            } else {
                                warn!(
                                    "Inbound task {} parked for guest [{}], but no materialization requester is configured.",
                                    task_id, guest_id
                                );
                            }
                        }
                    }
                }
                IpcResponse::success("emit", None)
            }
            IpcRequest::HandoffToRole {
                session_id,
                role_name,
                handoff_bundle,
            } => {
                Self::handle_handoff_to_role(
                    graph,
                    inboxes,
                    dispatcher_tx,
                    materialization_requester,
                    local_node_id,
                    current_identity.as_ref(),
                    session_id,
                    role_name,
                    handoff_bundle,
                )
                .await
            }
            IpcRequest::HandoffBack {
                session_id,
                summary,
                return_to,
            } => {
                Self::handle_handoff_back(
                    graph,
                    inboxes,
                    dispatcher_tx,
                    materialization_requester,
                    local_node_id,
                    current_identity.as_ref(),
                    session_id,
                    summary,
                    return_to,
                )
                .await
            }
            IpcRequest::SetRoleHome {
                agent_id,
                role_name,
                calling_role,
                target_hotel,
            } => Self::handle_set_role_home(
                graph,
                current_identity.as_ref(),
                agent_id,
                role_name,
                calling_role,
                target_hotel,
            ),
            IpcRequest::SetTransportHome {
                agent_id,
                transport,
                resource_ref,
                calling_role,
                target_hotel,
                standby_hotels,
            } => {
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "set_transport_home",
                        "SET_TRANSPORT_HOME_UNREGISTERED",
                        "guest must register before calling set_transport_home",
                    );
                };
                if !Self::is_agent_handoff_caller(graph, identity) {
                    return IpcResponse::error(
                        "set_transport_home",
                        "SET_TRANSPORT_HOME_FORBIDDEN",
                        "only agent guests may call set_transport_home",
                    );
                }

                let calling_role_record = graph.get_role_incarnation(&agent_id, &calling_role);
                let is_admin = calling_role_record
                    .ok()
                    .flatten()
                    .map(|r| r.has_operational_admin_authority())
                    .unwrap_or(false);
                if !is_admin {
                    return IpcResponse::error(
                        "set_transport_home",
                        "SET_TRANSPORT_HOME_FORBIDDEN",
                        format!(
                            "role '{}' does not have authority to set transport homes",
                            calling_role
                        ),
                    );
                }

                Self::perform_set_transport_home(
                    graph,
                    agent_id,
                    transport,
                    resource_ref,
                    calling_role,
                    target_hotel,
                    standby_hotels,
                )
            }
            IpcRequest::MaterializeRequest {
                agent_id,
                role_name,
                calling_role,
                target_hotel,
                dry_run,
            } => {
                Self::handle_materialize_request(
                    graph,
                    dispatcher_tx,
                    local_node_id,
                    current_identity.as_ref(),
                    agent_id,
                    role_name,
                    calling_role,
                    target_hotel,
                    dry_run,
                )
                .await
            }
            IpcRequest::MaterializeStatus { request_id } => {
                Self::handle_materialize_status(graph, request_id)
            }
            IpcRequest::RelocateHotel {
                agent_id,
                role_name,
                calling_role,
                target_hotel,
                include_transport,
                transport,
                transport_resource_ref,
                reason,
            } => {
                Self::handle_relocate_hotel(
                    graph,
                    dispatcher_tx,
                    local_node_id,
                    current_identity.as_ref(),
                    agent_id,
                    role_name,
                    calling_role,
                    target_hotel,
                    include_transport,
                    transport,
                    transport_resource_ref,
                    reason,
                )
                .await
            }
            IpcRequest::RelocateHotelStatus { ceremony_id } => {
                Self::handle_relocate_hotel_status(graph, ceremony_id)
            }
            IpcRequest::ListMembraneTransportHomes {
                agent_id,
                transport,
            } => Self::handle_list_membrane_transport_homes(graph, agent_id, transport),
            IpcRequest::DelegateToPeer {
                target_agent_id,
                task_description,
                context_package,
                chat_id,
                source,
                expected_artifacts,
                timeout_secs,
            } => {
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "delegate_to_peer",
                        "DELEGATION_UNREGISTERED",
                        "guest must register before requesting peer delegation",
                    );
                };

                let delegation_id = Uuid::new_v4();
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                // Generate a derived session_id ensuring isolation but persistence to the same chat
                let session_id = format!("{}:peer:{}", chat_id, target_agent_id);
                let authority_hotel = lookup_agent_authority_hotel(graph, &target_agent_id);

                // Resolve the peer's hotel to a mesh node BEFORE acking. The
                // ledger writer treats an envelope without a target node as
                // same-hotel and skips it (`AppendLocal`: `unwrap_or(true)`),
                // so a delegation whose node was left for "the router" was
                // never stored, never routed, never delivered — and the guest
                // was still told "status: dispatched". Live 2026-09-15 19:35
                // UTC (DEF-139): two delegations to agent-bjork-01 acked
                // "dispatched"; neither reached mac-jane; Beacon told the
                // operator Björk had received them.
                let mut target_node_id = match authority_hotel.as_deref() {
                    Some(hotel) => IpcServer::resolve_hotel_node_id(graph, hotel),
                    None => None,
                };
                // The graph only holds identities for agents this hotel has
                // materialized; a peer's agents are known from its roster
                // gossip (HotelStateSync → NodeRegistry). Live 2026-09-16
                // 02:10 UTC the vps received mac-jane's roster ("45 guests,
                // 4 agents") every 30 s and still had no graph identity for
                // agent-bjork-01.
                if target_node_id.is_none() {
                    let reg = registry.read().await;
                    target_node_id =
                        peer_agent_node_from_roster(reg.remote_hotel_states(), &target_agent_id);
                }
                let Some(target_node_id) = target_node_id else {
                    // Name the peers this hotel CAN reach, so the model can
                    // correct a wrong agent id instead of guessing.
                    let known: Vec<String> = {
                        let reg = registry.read().await;
                        let mut ids: Vec<String> = reg
                            .remote_hotel_states()
                            .flat_map(|state| state.agents.iter().map(|a| a.agent_id.clone()))
                            .collect();
                        ids.sort();
                        ids.dedup();
                        ids
                    };
                    warn!(
                        target_agent_id = %target_agent_id,
                        authority_hotel = ?authority_hotel,
                        "delegate_to_peer refused: no known hotel hosts the target agent"
                    );
                    return IpcResponse::error(
                        "delegate_to_peer",
                        "DELEGATION_UNROUTABLE",
                        &format!(
                            "no hotel on this mesh is known to host agent '{}'{}; the delegation was NOT sent \
                             and nothing is queued — the peer agent ids this hotel can see are [{}]; retry with \
                             one of them, or tell the user the peer is unreachable",
                            target_agent_id,
                            authority_hotel
                                .as_deref()
                                .map(|h| format!(
                                    " (its recorded authority hotel '{h}' resolves to no mesh node)"
                                ))
                                .unwrap_or_default(),
                            if known.is_empty() {
                                "none visible right now".to_string()
                            } else {
                                known.join(", ")
                            }
                        ),
                    );
                };
                if target_node_id == local_node_id {
                    warn!(
                        target_agent_id = %target_agent_id,
                        "delegate_to_peer: target agent is hosted on this hotel; the mesh ledger will not carry a same-hotel delegation"
                    );
                }

                // Build the mesh envelope for TaskInvoke
                let env = EventEnvelope {
                    event_id: delegation_id,
                    seq: 0,
                    source_node_id: local_node_id.to_string(),
                    target_node_id: Some(target_node_id),
                    source_agent_id: identity.guest_id.clone(),
                    target_agent_id: Some(target_agent_id.clone()),
                    kind: ansible_mesh_core::event::EventKind::TaskInvoke,
                    corr_id: delegation_id.to_string(),
                    attempt: 0,
                    created_at: ts,
                    expires_at: timeout_secs.map(|s| ts + s),
                    payload: ansible_mesh_core::event::EventPayload::Inline {
                        data: serde_json::json!({
                            "action": "peer.delegate",
                            "agent_id": target_agent_id,
                            "authority_hotel": authority_hotel,
                            "session_id": session_id,
                            "chat_id": chat_id,
                            "source": source.unwrap_or_else(|| "peer".into()),
                            "content": format!(
                                "Handoff from peer {}:\n\nTask: {}\n\nContext:\n{}\n\nExpected Artifacts: {:?}",
                                identity.guest_id, task_description, context_package, expected_artifacts
                            ),
                            "task": task_description,
                            "context": context_package,
                            "expected_artifacts": expected_artifacts,
                        })
                        .to_string(),
                    },
                    trace: vec![],
                };

                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;

                IpcResponse::DelegationAck {
                    delegation_id: delegation_id.to_string(),
                    status: "dispatched".into(),
                }
            }
            IpcRequest::DelegateToExternalPeer {
                target_peer_type,
                task_description,
                context_package,
                expected_artifacts,
            } => {
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "delegate_to_external_peer",
                        "DELEGATION_UNREGISTERED",
                        "guest must register before requesting external delegation",
                    );
                };

                let delegation_id = Uuid::new_v4();
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                // Handle external delegation - locally recorded for visibility/trace
                // even if handled by a specific hotel-side connector later.
                let env = EventEnvelope {
                    event_id: delegation_id,
                    seq: 0,
                    source_node_id: local_node_id.to_string(),
                    target_node_id: Some(local_node_id.to_string()),
                    source_agent_id: identity.guest_id.clone(),
                    target_agent_id: Some(format!("external:{}", target_peer_type)),
                    kind: ansible_mesh_core::event::EventKind::TaskInvoke,
                    corr_id: delegation_id.to_string(),
                    attempt: 0,
                    created_at: ts,
                    expires_at: None,
                    payload: ansible_mesh_core::event::EventPayload::Inline {
                        data: serde_json::json!({
                            "action": "external.delegate",
                            "peer_type": target_peer_type,
                            "task": task_description,
                            "context": context_package,
                            "expected_artifacts": expected_artifacts,
                        })
                        .to_string(),
                    },
                    trace: vec![],
                };

                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;

                IpcResponse::DelegationAck {
                    delegation_id: delegation_id.to_string(),
                    status: "dispatched_external".into(),
                }
            }
            IpcRequest::SpawnSubagent {
                session_id,
                delegation,
            } => {
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "spawn_subagent",
                        "SUBAGENT_UNREGISTERED",
                        "guest must register before spawning a subagent",
                    );
                };
                // Role-incarnation philotes register as
                // "role:{agent_id}:{role_name}", not "agent" — live 2026-09-14
                // 18:43 UTC bjork's orchestrator incarnation was refused here
                // (SUBAGENT_FORBIDDEN) and fell back to doing the delegated
                // work inline. Judge agent-ness the way handoff does.
                if !Self::is_agent_handoff_caller(graph, identity) {
                    return IpcResponse::error(
                        "spawn_subagent",
                        "SUBAGENT_FORBIDDEN",
                        "only agent guests may request subagent delegation",
                    );
                }

                // Spawn-by-name: resolve the registered skill's template, kind,
                // and tool bounds into the delegation (fail closed).
                let delegation = match resolve_skill_delegation(graph, delegation) {
                    Ok(delegation) => delegation,
                    Err(response) => return response,
                };

                Self::handle_spawn_subagent(
                    local_node_id,
                    graph,
                    inboxes,
                    materialization_requester,
                    subagent_leases,
                    subagent_hooks,
                    conn_id,
                    identity,
                    &session_id,
                    delegation,
                )
                .await
            }
            IpcRequest::ListRoleIncarnations { agent_id } => {
                match graph.list_role_incarnations(&agent_id) {
                    Ok(roles) => IpcResponse::success(
                        "list_role_incarnations",
                        Some(serde_json::json!({
                            "agent_id": agent_id,
                            "roles": roles,
                        })),
                    ),
                    Err(err) => IpcResponse::error(
                        "list_role_incarnations",
                        "ROLE_LIST_FAILED",
                        err.to_string(),
                    ),
                }
            }
            IpcRequest::AssignSubagentTask {
                subagent_guest_id,
                lease_epoch,
                delegation,
            } => {
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "assign_subagent_task",
                        "SUBAGENT_UNREGISTERED",
                        "guest must register before assigning subagent tasks",
                    );
                };
                // Verify the lease is still live and epoch matches.
                let lease_ok = {
                    let guard = subagent_leases.lock().await;
                    let scope = Self::subagent_lease_scope(&subagent_guest_id);
                    guard
                        .inspect(&scope)
                        .is_some_and(|l| l.lease_epoch == lease_epoch && l.is_active())
                };
                if !lease_ok {
                    return IpcResponse::error(
                        "assign_subagent_task",
                        "SUBAGENT_LEASE_INVALID",
                        format!(
                            "No active subagent lease for guest [{}] at epoch {}",
                            subagent_guest_id, lease_epoch
                        ),
                    );
                }
                let task_id = Uuid::new_v4();
                let task_json = match serde_json::to_string(&delegation) {
                    Ok(j) => j,
                    Err(e) => {
                        return IpcResponse::error(
                            "assign_subagent_task",
                            "DELEGATION_SERIALIZE_FAILED",
                            e.to_string(),
                        );
                    }
                };
                // Route to the subagent worker's inbox by subagent_kind + guest_id.
                Self::deliver_inbound_task(
                    inboxes,
                    &identity.guest_id,
                    &delegation.subagent_kind,
                    Some(&subagent_guest_id),
                    task_id,
                    task_json,
                )
                .await;
                IpcResponse::success(
                    "assign_subagent_task",
                    Some(serde_json::json!({
                        "subagent_guest_id": subagent_guest_id,
                        "task_id": task_id.to_string(),
                    })),
                )
            }
            IpcRequest::RenewSubagentLease {
                subagent_guest_id,
                lease_epoch,
            } => {
                Self::handle_renew_subagent_lease(
                    subagent_leases,
                    subagent_hooks,
                    conn_id,
                    current_identity.as_ref(),
                    subagent_guest_id,
                    lease_epoch,
                )
                .await
            }
            IpcRequest::ReleaseSubagent { subagent_guest_id } => {
                Self::handle_release_subagent(
                    graph,
                    local_node_id,
                    subagent_leases,
                    subagent_hooks,
                    conn_id,
                    subagent_guest_id,
                )
                .await
            }
            IpcRequest::FireSubagentHook {
                subagent_guest_id,
                hook_kind,
                payload,
            } => {
                let hook_record = subagent_hooks.lock().await.get(&subagent_guest_id).cloned();
                let Some(record) = hook_record else {
                    return IpcResponse::error(
                        "fire_subagent_hook",
                        "SUBAGENT_HOOK_UNKNOWN",
                        format!(
                            "No hook registry entry for subagent guest [{}]",
                            subagent_guest_id
                        ),
                    );
                };
                // Find the matching subscription for this hook_kind.
                let subscription = record
                    .hook_subscriptions
                    .iter()
                    .find(|s| s.hook_kind == hook_kind)
                    .cloned();

                let Some(sub) = subscription else {
                    // Hook not subscribed — fire-and-forget discard is valid.
                    return IpcResponse::success(
                        "fire_subagent_hook",
                        Some(serde_json::json!({
                            "subagent_guest_id": subagent_guest_id,
                            "note": "hook not subscribed, discarded",
                        })),
                    );
                };

                let task_id = Uuid::new_v4();
                let task_json = serde_json::json!({
                    "kind": "subagent_hook",
                    "subagent_guest_id": subagent_guest_id,
                    "hook_kind": sub.hook_kind,
                    "payload": payload,
                })
                .to_string();

                Self::deliver_hook_to_route(
                    inboxes,
                    &sub.route,
                    &record.persona_guest_id,
                    &record.persona_role,
                    local_node_id,
                    task_id,
                    task_json,
                )
                .await;

                IpcResponse::success(
                    "fire_subagent_hook",
                    Some(serde_json::json!({
                        "subagent_guest_id": subagent_guest_id,
                        "task_id": task_id.to_string(),
                    })),
                )
            }
            IpcRequest::AcceptSubagentLease { subagent_guest_id } => {
                Self::handle_accept_subagent_lease(
                    subagent_leases,
                    subagent_hooks,
                    inboxes,
                    subagent_guest_id,
                )
                .await
            }
            IpcRequest::ConfigureRole {
                agent_id,
                role_name,
                guest_id,
                calling_role,
                toolset_profile,
                role_identity_addendum,
                role_manifest,
                is_admin,
                inactive_ttl_seconds,
                iteration_cap,
                approval_policy,
                model_profile,
                context_window_policy,
                fallback_tiers,
                model_bindings,
                content_policy,
            } => {
                Self::configure_role_record(
                    graph,
                    inboxes,
                    materialization_requester,
                    local_node_id,
                    current_identity.as_ref(),
                    agent_id,
                    role_name,
                    guest_id,
                    calling_role,
                    toolset_profile,
                    role_identity_addendum,
                    role_manifest,
                    is_admin,
                    inactive_ttl_seconds,
                    iteration_cap,
                    approval_policy,
                    model_profile,
                    context_window_policy,
                    fallback_tiers,
                    model_bindings,
                    content_policy,
                )
                .await
            }
            IpcRequest::ExecuteWorkflow {
                workflow_name,
                agent_id,
                calling_role,
                arguments,
            } => match workflow_name.as_str() {
                "role.create_or_update" => {
                    let Some(role_name) = arguments
                        .get("role_name")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                    else {
                        return IpcResponse::error(
                            "execute_workflow",
                            "WORKFLOW_ARGUMENT_INVALID",
                            "role.create_or_update requires role_name",
                        );
                    };
                    let Some(toolset_profile) = arguments
                        .get("toolset_profile")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                    else {
                        return IpcResponse::error(
                            "execute_workflow",
                            "WORKFLOW_ARGUMENT_INVALID",
                            "role.create_or_update requires toolset_profile",
                        );
                    };
                    if !arguments.get("reasoning").is_some_and(|v| v.is_object()) {
                        return IpcResponse::error(
                            "execute_workflow",
                            "WORKFLOW_ARGUMENT_INVALID",
                            "role.create_or_update requires reasoning object",
                        );
                    }
                    let guest_id = arguments
                        .get("guest_id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("{agent_id}:{role_name}"));
                    let response = Self::configure_role_record(
                        graph,
                        inboxes,
                        materialization_requester,
                        local_node_id,
                        current_identity.as_ref(),
                        agent_id,
                        role_name.clone(),
                        guest_id,
                        calling_role,
                        toolset_profile,
                        arguments
                            .get("role_identity_addendum")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        arguments
                            .get("role_manifest")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        arguments
                            .get("is_admin")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        arguments
                            .get("inactive_ttl_seconds")
                            .and_then(|v| v.as_u64()),
                        arguments
                            .get("iteration_cap")
                            .and_then(|v| v.as_u64())
                            .map(|v| v as u32),
                        arguments
                            .get("approval_policy")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        arguments
                            .get("model_profile")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        arguments
                            .get("context_window_policy")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        arguments.get("fallback_tiers").and_then(|v| {
                            v.as_array().map(|arr| {
                                arr.iter()
                                    .filter_map(|t| t.as_str().map(str::to_string))
                                    .collect::<Vec<String>>()
                            })
                        }),
                        arguments.get("model_bindings").and_then(|v| {
                            v.as_object().map(|obj| {
                                obj.iter()
                                    .filter_map(|(k, val)| {
                                        val.as_str().map(|s| (k.clone(), s.to_string()))
                                    })
                                    .collect::<std::collections::BTreeMap<String, String>>()
                            })
                        }),
                        arguments
                            .get("content_policy")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                    )
                    .await;
                    match response {
                        IpcResponse::ConfigureRoleOk { role_name } => {
                            IpcResponse::WorkflowExecutionOk {
                                workflow_name,
                                result: serde_json::json!({ "role_name": role_name }),
                            }
                        }
                        other => other,
                    }
                }
                _ => IpcResponse::error(
                    "execute_workflow",
                    "WORKFLOW_UNKNOWN",
                    format!("unknown workflow [{workflow_name}]"),
                ),
            },
            IpcRequest::RegisterSkill {
                skill_name,
                description,
                subagent_kind,
                goal,
                allowed_tools,
                allowed_classes,
                allowed_skills,
                origin,
                hook_subscriptions: _,
                completion_route: _,
                failure_route: _,
                idle_behavior: _,
                lease_terms: _,
            } => handle_register_skill_with_origin(
                current_identity.as_ref(),
                graph,
                skill_name,
                description,
                subagent_kind,
                goal,
                allowed_tools,
                allowed_classes,
                allowed_skills,
                origin,
            ),
            IpcRequest::SetSkillState {
                skill_name,
                state,
                reason,
            } => {
                handle_set_skill_state(current_identity.as_ref(), graph, skill_name, state, reason)
            }
            IpcRequest::ListSkillAudits { skill_name, limit } => {
                if let Err(response) = require_skill_admin(
                    current_identity.as_ref(),
                    "list_skill_audits",
                    "LIST_SKILL_AUDITS",
                    "reading the skill audit trail",
                ) {
                    return response;
                }
                let audits = match graph.list_skill_registration_audits() {
                    Ok(audits) => audits,
                    Err(e) => {
                        return IpcResponse::error(
                            "list_skill_audits",
                            "LIST_SKILL_AUDITS_FAILED",
                            format!("failed to list skill audits: {e}"),
                        );
                    }
                };
                let limit = limit.unwrap_or(100) as usize;
                let skill_audits: Vec<serde_json::Value> = audits
                    .iter()
                    .filter(|audit| {
                        skill_name
                            .as_deref()
                            .is_none_or(|name| audit.skill_name == name)
                    })
                    .rev()
                    .take(limit)
                    .map(|audit| {
                        serde_json::json!({
                            "audit_id": audit.audit_id,
                            "skill_name": audit.skill_name,
                            "action": audit.action,
                            "by": audit.registered_by,
                            "by_role": audit.registered_by_role,
                            "validation_state": audit.validation_state,
                            "at": audit.registered_at,
                            "detail": audit.detail,
                        })
                    })
                    .collect();
                IpcResponse::SkillAuditList { skill_audits }
            }
            IpcRequest::PatchAgentBundle {
                agent_id,
                persona_name,
                soul_text,
                identity_text,
                user_context_text,
                system_prompt,
                import_workspace,
                default_toolset,
                default_skillset,
                response_route_policy,
            } => {
                let response_route_policy = match response_route_policy {
                    Some(policy) => match serde_json::to_value(policy) {
                        Ok(value) => Some(value),
                        Err(err) => {
                            return IpcResponse::Standard {
                                ok: false,
                                code: "serialize_error".into(),
                                message: err.to_string(),
                                corr_id: String::new(),
                                data: None,
                            };
                        }
                    },
                    None => None,
                };
                match Self::handle_patch_agent_bundle(
                    graph,
                    local_node_id,
                    &agent_id,
                    persona_name,
                    soul_text,
                    identity_text,
                    user_context_text,
                    system_prompt,
                    import_workspace,
                    default_toolset,
                    default_skillset,
                    response_route_policy,
                ) {
                    Ok(agent) => IpcResponse::AgentUpdated { agent },
                    Err(err) => IpcResponse::error(
                        "patch_agent_bundle",
                        "PATCH_AGENT_BUNDLE_ERROR",
                        err.to_string(),
                    ),
                }
            }
            IpcRequest::GetUserProfile { hotel_name } => {
                let local_user_id = format!("root-user:{hotel_name}");
                let projected_identity = graph
                    .find_projected_user_identity_for_local_user(&local_user_id)
                    .ok()
                    .flatten();
                match graph.get_user_profile(&hotel_name) {
                    Ok(Some(p)) => {
                        IpcResponse::UserProfileData(philotic_client::UserProfileDataPayload {
                            timezone: p.timezone,
                            display_name: p.display_name,
                            principal_id: projected_identity
                                .as_ref()
                                .map(|identity| identity.principal_id.clone()),
                            preferred_name: projected_identity
                                .as_ref()
                                .and_then(|identity| identity.preferred_name.clone()),
                            primary_email: projected_identity
                                .as_ref()
                                .and_then(|identity| identity.primary_email.clone()),
                            home_hotel: projected_identity
                                .as_ref()
                                .map(|identity| identity.home_hotel.clone()),
                            linked_providers: projected_identity
                                .as_ref()
                                .map(|identity| {
                                    identity
                                        .linked_identities
                                        .iter()
                                        .map(|link| link.provider.clone())
                                        .collect()
                                })
                                .unwrap_or_default(),
                        })
                    }
                    Ok(None) => {
                        IpcResponse::UserProfileData(philotic_client::UserProfileDataPayload {
                            timezone: None,
                            display_name: None,
                            principal_id: projected_identity
                                .as_ref()
                                .map(|identity| identity.principal_id.clone()),
                            preferred_name: projected_identity
                                .as_ref()
                                .and_then(|identity| identity.preferred_name.clone()),
                            primary_email: projected_identity
                                .as_ref()
                                .and_then(|identity| identity.primary_email.clone()),
                            home_hotel: projected_identity
                                .as_ref()
                                .map(|identity| identity.home_hotel.clone()),
                            linked_providers: projected_identity
                                .as_ref()
                                .map(|identity| {
                                    identity
                                        .linked_identities
                                        .iter()
                                        .map(|link| link.provider.clone())
                                        .collect()
                                })
                                .unwrap_or_default(),
                        })
                    }
                    Err(e) => IpcResponse::error(
                        "get_user_profile",
                        "GET_USER_PROFILE_ERROR",
                        e.to_string(),
                    ),
                }
            }
            IpcRequest::PatchUserProfile {
                hotel_name,
                timezone,
                display_name,
            } => {
                // Timezone is fanned out into every agent's prompt clock and
                // every cron fire-time echo — validate at THIS boundary so a
                // typo can't silently break time rendering fleet-wide.
                if let Some(tz) = timezone.as_deref() {
                    if tz.parse::<chrono_tz::Tz>().is_err() {
                        return IpcResponse::error(
                            "patch_user_profile",
                            "INVALID_TIMEZONE",
                            format!(
                                "'{tz}' is not a valid IANA timezone name (e.g. America/New_York)"
                            ),
                        );
                    }
                }
                let existing = match graph.get_user_profile(&hotel_name) {
                    Ok(profile) => profile.unwrap_or_default(),
                    Err(e) => {
                        return IpcResponse::error(
                            "patch_user_profile",
                            "PATCH_USER_PROFILE_READ_ERROR",
                            e.to_string(),
                        );
                    }
                };
                let updated = ansible_mesh_core::storage::UserProfile {
                    timezone: timezone.or(existing.timezone),
                    display_name: display_name.or(existing.display_name),
                };
                match graph.upsert_user_profile(&hotel_name, &updated) {
                    Ok(()) => {
                        let local_user_id = format!("root-user:{hotel_name}");
                        let projected_identity = graph
                            .find_projected_user_identity_for_local_user(&local_user_id)
                            .ok()
                            .flatten();
                        IpcResponse::UserProfileData(philotic_client::UserProfileDataPayload {
                            timezone: updated.timezone,
                            display_name: updated.display_name,
                            principal_id: projected_identity
                                .as_ref()
                                .map(|identity| identity.principal_id.clone()),
                            preferred_name: projected_identity
                                .as_ref()
                                .and_then(|identity| identity.preferred_name.clone()),
                            primary_email: projected_identity
                                .as_ref()
                                .and_then(|identity| identity.primary_email.clone()),
                            home_hotel: projected_identity
                                .as_ref()
                                .map(|identity| identity.home_hotel.clone()),
                            linked_providers: projected_identity
                                .as_ref()
                                .map(|identity| {
                                    identity
                                        .linked_identities
                                        .iter()
                                        .map(|link| link.provider.clone())
                                        .collect()
                                })
                                .unwrap_or_default(),
                        })
                    }
                    Err(e) => IpcResponse::error(
                        "patch_user_profile",
                        "PATCH_USER_PROFILE_ERROR",
                        e.to_string(),
                    ),
                }
            }
            IpcRequest::AssignSkill {
                agent_id,
                role_name,
                skill_name,
            } => {
                let identity = match require_skill_admin(
                    current_identity.as_ref(),
                    "assign_skill",
                    "ASSIGN",
                    "assigning skills",
                ) {
                    Ok(identity) => identity,
                    Err(response) => return response,
                };
                let is_management = identity.role == "management";
                if !is_management && !guest_owns_agent(&identity.guest_id, &agent_id) {
                    return IpcResponse::error(
                        "assign_skill",
                        "ASSIGN_FORBIDDEN",
                        "orchestrator guests may only assign skills for their own agent identity",
                    );
                }
                // Verify the skill exists in the catalog.
                match graph.get_abstract_skill(&skill_name) {
                    Ok(None) => {
                        return IpcResponse::error(
                            "assign_skill",
                            "SKILL_NOT_FOUND",
                            format!("skill [{}] not found in catalog", skill_name),
                        );
                    }
                    Err(e) => {
                        return IpcResponse::error(
                            "assign_skill",
                            "SKILL_LOOKUP_FAILED",
                            format!("failed to look up skill: {e}"),
                        );
                    }
                    Ok(Some(_)) => {}
                }
                // Load the role incarnation record.
                let role_record = match graph.get_role_incarnation(&agent_id, &role_name) {
                    Ok(Some(r)) => r,
                    Ok(None) => {
                        return IpcResponse::error(
                            "assign_skill",
                            "ROLE_NOT_FOUND",
                            format!(
                                "role [{}] not configured for agent [{}]",
                                role_name, agent_id
                            ),
                        );
                    }
                    Err(e) => {
                        return IpcResponse::error(
                            "assign_skill",
                            "ROLE_LOOKUP_FAILED",
                            format!("failed to look up role: {e}"),
                        );
                    }
                };
                // Load the toolset profile.
                let mut profile = match graph.get_toolset_profile(&role_record.toolset_profile) {
                    Ok(Some(p)) => p,
                    Ok(None) => {
                        return IpcResponse::error(
                            "assign_skill",
                            "PROFILE_NOT_FOUND",
                            format!(
                                "toolset profile [{}] not found",
                                role_record.toolset_profile
                            ),
                        );
                    }
                    Err(e) => {
                        return IpcResponse::error(
                            "assign_skill",
                            "PROFILE_LOOKUP_FAILED",
                            format!("failed to look up toolset profile: {e}"),
                        );
                    }
                };
                // Idempotent: if already assigned, return success.
                if !profile.allowed_skills.contains(&skill_name) {
                    // Fail-closed audit before the mutation.
                    if let Err(response) = record_skill_admin_audit(
                        graph,
                        identity,
                        "assign_skill",
                        "assign",
                        &skill_name,
                        "",
                        Some(format!(
                            "agent={agent_id} role={role_name} profile={}",
                            profile.profile_name
                        )),
                    ) {
                        return response;
                    }
                    profile.allowed_skills.push(skill_name.clone());
                    if let Err(e) = graph.upsert_toolset_profile(&profile) {
                        return IpcResponse::error(
                            "assign_skill",
                            "PROFILE_PERSIST_FAILED",
                            format!("failed to persist toolset profile: {e}"),
                        );
                    }
                }
                info!(role_name = %role_name, skill_name = %skill_name, "Skill assigned to role via IPC");
                IpcResponse::SkillAssigned {
                    role_name,
                    skill_name,
                    operation: "assigned".into(),
                }
            }
            IpcRequest::RevokeSkill {
                agent_id,
                role_name,
                skill_name,
            } => {
                let identity = match require_skill_admin(
                    current_identity.as_ref(),
                    "revoke_skill",
                    "REVOKE",
                    "revoking skills",
                ) {
                    Ok(identity) => identity,
                    Err(response) => return response,
                };
                let is_management = identity.role == "management";
                if !is_management && !guest_owns_agent(&identity.guest_id, &agent_id) {
                    return IpcResponse::error(
                        "revoke_skill",
                        "REVOKE_FORBIDDEN",
                        "orchestrator guests may only revoke skills for their own agent identity",
                    );
                }
                // Load the role incarnation record.
                let role_record = match graph.get_role_incarnation(&agent_id, &role_name) {
                    Ok(Some(r)) => r,
                    Ok(None) => {
                        return IpcResponse::error(
                            "revoke_skill",
                            "ROLE_NOT_FOUND",
                            format!(
                                "role [{}] not configured for agent [{}]",
                                role_name, agent_id
                            ),
                        );
                    }
                    Err(e) => {
                        return IpcResponse::error(
                            "revoke_skill",
                            "ROLE_LOOKUP_FAILED",
                            format!("failed to look up role: {e}"),
                        );
                    }
                };
                // Load the toolset profile.
                let mut profile = match graph.get_toolset_profile(&role_record.toolset_profile) {
                    Ok(Some(p)) => p,
                    Ok(None) => {
                        return IpcResponse::error(
                            "revoke_skill",
                            "PROFILE_NOT_FOUND",
                            format!(
                                "toolset profile [{}] not found",
                                role_record.toolset_profile
                            ),
                        );
                    }
                    Err(e) => {
                        return IpcResponse::error(
                            "revoke_skill",
                            "PROFILE_LOOKUP_FAILED",
                            format!("failed to look up toolset profile: {e}"),
                        );
                    }
                };
                // Idempotent: if not present, return success.
                if profile.allowed_skills.contains(&skill_name) {
                    // Fail-closed audit before the mutation.
                    if let Err(response) = record_skill_admin_audit(
                        graph,
                        identity,
                        "revoke_skill",
                        "revoke",
                        &skill_name,
                        "",
                        Some(format!(
                            "agent={agent_id} role={role_name} profile={}",
                            profile.profile_name
                        )),
                    ) {
                        return response;
                    }
                    profile.allowed_skills.retain(|s| s != &skill_name);
                    if let Err(e) = graph.upsert_toolset_profile(&profile) {
                        return IpcResponse::error(
                            "revoke_skill",
                            "PROFILE_PERSIST_FAILED",
                            format!("failed to persist toolset profile: {e}"),
                        );
                    }
                }
                info!(role_name = %role_name, skill_name = %skill_name, "Skill revoked from role via IPC");
                IpcResponse::SkillAssigned {
                    role_name,
                    skill_name,
                    operation: "revoked".into(),
                }
            }
            IpcRequest::RegisterProcedure { procedure, origin } => {
                handle_register_procedure(current_identity.as_ref(), graph, procedure, origin)
            }
            IpcRequest::GetProcedure { procedure_id } => match graph.get_procedure(&procedure_id) {
                Ok(Some(p)) => IpcResponse::success(
                    "get_procedure",
                    Some(serde_json::to_value(&p).unwrap_or(serde_json::Value::Null)),
                ),
                Ok(None) => IpcResponse::error(
                    "get_procedure",
                    "PROCEDURE_NOT_FOUND",
                    format!("no procedure named {procedure_id}"),
                ),
                Err(e) => IpcResponse::error("get_procedure", "PROCEDURE_ERROR", e.to_string()),
            },
            IpcRequest::ApplySurfaceMessages {
                surface_id,
                messages,
                title,
                session_id,
                chat_id,
                transport,
            } => Self::handle_apply_surface_messages(
                surface_id,
                messages,
                title,
                session_id,
                chat_id,
                transport,
                local_node_id,
                graph,
                current_identity,
            ),
            IpcRequest::GetSurface { surface_id } => Self::handle_get_surface(surface_id, graph),
            IpcRequest::ListSurfaces {
                owner_agent_id,
                session_id,
                include_deleted,
                limit,
            } => Self::handle_list_surfaces(
                owner_agent_id,
                session_id,
                include_deleted,
                limit,
                graph,
            ),
            IpcRequest::ListProcedures {} => match graph.list_procedures() {
                Ok(list) => IpcResponse::success(
                    "list_procedures",
                    Some(serde_json::json!({ "procedures": list })),
                ),
                Err(e) => IpcResponse::error("list_procedures", "PROCEDURE_ERROR", e.to_string()),
            },
            IpcRequest::RecordProcedureRun { run } => {
                // The ledger is the refiner's evidence and the trial gate's
                // score source; an unregistered peer must not be able to
                // write either.
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "record_procedure_run",
                        "PROCEDURE_RUN_UNREGISTERED",
                        "guest must register before recording procedure runs",
                    );
                };
                let mut run: ProcedureRunRecord = match serde_json::from_value(run) {
                    Ok(r) => r,
                    Err(e) => {
                        return IpcResponse::error(
                            "record_procedure_run",
                            "PROCEDURE_RUN_INVALID",
                            format!("malformed run record: {e}"),
                        );
                    }
                };
                if run.run_id.trim().is_empty() || run.procedure_id.trim().is_empty() {
                    return IpcResponse::error(
                        "record_procedure_run",
                        "PROCEDURE_RUN_INVALID",
                        "run_id and procedure_id are required",
                    );
                }
                if run.agent_id.trim().is_empty() {
                    run.agent_id = identity.guest_id.clone();
                }
                if run.recorded_at == 0 {
                    run.recorded_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                }
                // The score is derived, never trusted from the wire.
                run.score = ProcedureRunRecord::score_for(&run.verdict, &run.basis);
                match graph.record_procedure_run(&run) {
                    Ok(()) => {
                        // P4: every run may close a trial window.
                        let trials = evaluate_procedure_trials(graph, &run.procedure_id);
                        IpcResponse::success(
                            "record_procedure_run",
                            Some(serde_json::json!({
                                "run_id": run.run_id,
                                "procedure_id": run.procedure_id,
                                "graph_version": run.graph_version,
                                "score": run.score,
                                "trials_decided": trials,
                            })),
                        )
                    }
                    Err(e) => IpcResponse::error(
                        "record_procedure_run",
                        "PROCEDURE_RUN_ERROR",
                        e.to_string(),
                    ),
                }
            }
            IpcRequest::ListProcedureRuns {
                procedure_id,
                graph_version,
                limit,
            } => match graph.list_procedure_runs(
                &procedure_id,
                graph_version,
                limit.unwrap_or(20).clamp(1, 200),
            ) {
                Ok(runs) => IpcResponse::success(
                    "list_procedure_runs",
                    Some(serde_json::json!({ "procedure_id": procedure_id, "runs": runs })),
                ),
                Err(e) => {
                    IpcResponse::error("list_procedure_runs", "PROCEDURE_RUN_ERROR", e.to_string())
                }
            },
            IpcRequest::ProposeProcedurePatch {
                procedure_id,
                ops,
                rationale,
                evidence_run_ids,
                origin,
            } => handle_propose_procedure_patch(
                current_identity.as_ref(),
                graph,
                procedure_id,
                ops,
                rationale,
                evidence_run_ids,
                origin,
            ),
            IpcRequest::ListProcedurePatches {
                procedure_id,
                status,
            } => {
                let status = match status.as_deref() {
                    None => None,
                    Some("pending") => Some(ProcedurePatchStatus::Pending),
                    Some("trial") => Some(ProcedurePatchStatus::Trial),
                    Some("accepted") => Some(ProcedurePatchStatus::Accepted),
                    Some("rejected") => Some(ProcedurePatchStatus::Rejected),
                    Some(other) => {
                        return IpcResponse::error(
                            "list_procedure_patches",
                            "PROCEDURE_PATCH_INVALID",
                            format!("unknown status filter {other:?}"),
                        );
                    }
                };
                match graph.list_procedure_patches(procedure_id.as_deref(), status) {
                    Ok(patches) => IpcResponse::success(
                        "list_procedure_patches",
                        Some(serde_json::json!({ "patches": patches })),
                    ),
                    Err(e) => IpcResponse::error(
                        "list_procedure_patches",
                        "PROCEDURE_PATCH_ERROR",
                        e.to_string(),
                    ),
                }
            }
            IpcRequest::DecideProcedurePatch {
                patch_id,
                decision,
                reason,
            } => handle_decide_procedure_patch(
                current_identity.as_ref(),
                graph,
                patch_id,
                decision,
                reason,
            ),
            IpcRequest::ListSkills {} => {
                // The catalog names every tool a skill can project; require at
                // least a registered guest identity before enumerating it.
                if current_identity.is_none() {
                    return IpcResponse::error(
                        "list_skills",
                        "LIST_SKILLS_UNREGISTERED",
                        "guest must register before listing skills",
                    );
                }
                let skills = match graph.list_abstract_skills() {
                    Ok(s) => s,
                    Err(e) => {
                        return IpcResponse::error(
                            "list_skills",
                            "LIST_SKILLS_FAILED",
                            format!("failed to list skills: {e}"),
                        );
                    }
                };
                let json_skills: Vec<serde_json::Value> = skills
                    .iter()
                    .map(|s| {
                        let (state_str, _) = skill_state_label(&s.validation_state);
                        serde_json::json!({
                            "skill_name": s.skill_name,
                            "description": s.description,
                            "implied_tools": s.implied_tools,
                            "implied_classes": s.implied_classes,
                            "allowed_skills": s.allowed_skills,
                            "subagent_kind": s.subagent_kind,
                            "goal_template": s.goal_template,
                            "validation_state": state_str,
                        })
                    })
                    .collect();
                IpcResponse::SkillList {
                    skills: json_skills,
                }
            }
            IpcRequest::AbortSubagentSpawn { subagent_guest_id } => {
                // Persona cancels before the worker has connected.
                // Release the lease and clean up hooks; worker spawn is no-op if it arrives late.
                let scope = Self::subagent_lease_scope(&subagent_guest_id);
                {
                    let mut guard = subagent_leases.lock().await;
                    let mut observer = LoggingSubagentLeaseObserver;
                    guard.release(&scope, conn_id, &mut observer);
                }
                subagent_hooks.lock().await.remove(&subagent_guest_id);
                info!(
                    "Subagent spawn aborted by persona for guest [{}].",
                    subagent_guest_id
                );
                IpcResponse::success(
                    "abort_subagent_spawn",
                    Some(serde_json::json!({ "subagent_guest_id": subagent_guest_id })),
                )
            }
            // Handled before process_request is called (in handle_client).
            IpcRequest::FetchMemoryConfig
            | IpcRequest::RefreshMemoryConfig
            | IpcRequest::HealMemoryToken { .. } => IpcResponse::error(
                "memory",
                "UNREACHABLE",
                "FetchMemoryConfig/RefreshMemoryConfig/HealMemoryToken dispatched early",
            ),
            IpcRequest::ListTrainingSamples { .. }
            | IpcRequest::CorrectTrainingSample { .. }
            | IpcRequest::ExportTrainingSamples { .. }
            | IpcRequest::GetTrainingStatus { .. } => IpcResponse::error(
                "training",
                "UNREACHABLE",
                "Training request intercepted before process_request",
            ),
            IpcRequest::AsrSetup { .. } | IpcRequest::AsrStatus {} => IpcResponse::error(
                "asr",
                "UNREACHABLE",
                "ASR request intercepted before process_request",
            ),
            IpcRequest::VisionSetup { .. } | IpcRequest::VisionStatus {} => IpcResponse::error(
                "vision",
                "UNREACHABLE",
                "Vision request intercepted before process_request",
            ),
            IpcRequest::CapabilityInvoke { .. } => IpcResponse::error(
                "capability_invoke",
                "UNREACHABLE",
                "CapabilityInvoke intercepted before process_request",
            ),
            IpcRequest::GetPerimeterStatus | IpcRequest::RefreshPerimeter => IpcResponse::error(
                "perimeter",
                "UNREACHABLE",
                "Perimeter request intercepted before process_request",
            ),
            IpcRequest::CheckEgress { .. } => IpcResponse::error(
                "egress",
                "UNREACHABLE",
                "Egress check intercepted before process_request",
            ),
            IpcRequest::RegisterGraphInstance {
                graph_id,
                instance_id,
            } => {
                use ansible_mesh_core::storage::GraphRunnerInstanceRecord;
                let record = GraphRunnerInstanceRecord {
                    graph_id: graph_id.clone(),
                    instance_id: instance_id.clone(),
                    registered_at: unix_ts(),
                };
                match graph.upsert_graph_runner_instance(&record) {
                    Ok(()) => {
                        info!(
                            graph_id = %graph_id,
                            instance_id = %instance_id,
                            "Graph runner instance registered"
                        );
                        IpcResponse::GraphInstanceRegistered { graph_id }
                    }
                    Err(err) => {
                        error!("Failed to register graph runner instance: {err}");
                        IpcResponse::error(
                            "register_graph_instance",
                            "STORAGE_ERROR",
                            err.to_string(),
                        )
                    }
                }
            }
            IpcRequest::ProposeRule {
                agent_id,
                description,
                rationale,
            } => {
                use ansible_mesh_core::graph::RuleRecord;
                let rule_id = Uuid::new_v4().to_string();
                let record = RuleRecord {
                    rule_id: rule_id.clone(),
                    agent_id: agent_id.clone(),
                    description,
                    rationale,
                    created_at: unix_ts(),
                };
                match graph.upsert_rule(&record) {
                    Ok(()) => {
                        info!(agent_id = %agent_id, rule_id = %rule_id, "Rule stored via IPC");
                        IpcResponse::RuleProposed { rule_id }
                    }
                    Err(err) => {
                        error!("Failed to store rule: {err}");
                        IpcResponse::error("propose_rule", "STORAGE_ERROR", err.to_string())
                    }
                }
            }
            IpcRequest::RecordRoutingPolicyProposal {
                agent_id,
                problem,
                proposed_change,
                evidence,
                affected_stage,
                affected_capability,
                learned_reflex_preference_key,
            } => {
                use ansible_mesh_core::graph::{
                    RoutingPolicyDispositionRecord, RoutingPolicyEvaluationRecord,
                    RoutingPolicyRecord,
                };
                let proposal_id = Uuid::new_v4().to_string();
                let created_at = unix_ts();
                let record = RoutingPolicyRecord {
                    proposal_id: proposal_id.clone(),
                    agent_id: agent_id.clone(),
                    problem,
                    proposed_change,
                    evidence,
                    affected_stage,
                    affected_capability,
                    learned_reflex_preference_key,
                    operator_disposition: RoutingPolicyDispositionRecord {
                        state: "approved".into(),
                        reason: "Approved via operator-gated routing.policy.propose execution."
                            .into(),
                        decided_at: created_at,
                    },
                    evaluations: vec![RoutingPolicyEvaluationRecord {
                        evaluation_kind: "operator_disposition".into(),
                        decision: "approved".into(),
                        reason: "routing.policy.propose executed after operator approval.".into(),
                        created_at,
                        source_tool: Some("routing.policy.propose".into()),
                    }],
                    created_at,
                };
                match graph.upsert_routing_policy(&record) {
                    Ok(()) => {
                        info!(
                            agent_id = %agent_id,
                            proposal_id = %proposal_id,
                            "Routing policy proposal stored via IPC"
                        );
                        IpcResponse::RoutingPolicyRecorded { proposal_id }
                    }
                    Err(err) => {
                        error!("Failed to store routing policy proposal: {err}");
                        IpcResponse::error(
                            "record_routing_policy_proposal",
                            "STORAGE_ERROR",
                            err.to_string(),
                        )
                    }
                }
            }
            IpcRequest::ListRoutingPolicies { agent_id } => match graph
                .list_routing_policies(&agent_id)
            {
                Ok(policies) => {
                    let json_policies: Vec<serde_json::Value> = policies
                        .into_iter()
                        .map(|policy| {
                            serde_json::to_value(policy).unwrap_or(serde_json::Value::Null)
                        })
                        .collect();
                    IpcResponse::RoutingPolicyList {
                        policies: json_policies,
                    }
                }
                Err(err) => {
                    error!("Failed to list routing policies: {err}");
                    IpcResponse::error("list_routing_policies", "STORAGE_ERROR", err.to_string())
                }
            },
            IpcRequest::ListRules { agent_id } => match graph.list_rules(&agent_id) {
                Ok(rules) => {
                    let json_rules: Vec<serde_json::Value> = rules
                        .iter()
                        .map(|r| {
                            serde_json::json!({
                                "rule_id": r.rule_id,
                                "agent_id": r.agent_id,
                                "description": r.description,
                                "rationale": r.rationale,
                                "created_at": r.created_at,
                            })
                        })
                        .collect();
                    IpcResponse::RuleList { rules: json_rules }
                }
                Err(err) => {
                    error!("Failed to list rules: {err}");
                    IpcResponse::error("list_rules", "STORAGE_ERROR", err.to_string())
                }
            },
            IpcRequest::UpsertAgentReflexPreference {
                agent_id,
                preference_key,
                precedence,
                reflexes_json,
                config_json,
            } => {
                use ansible_mesh_core::agent_graph_storage::AgentReflexPreference;
                let path = agent_graph_db_path(&agent_id);
                let result = (|| -> anyhow::Result<()> {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let storage = SqliteAgentGraphStorage::open(&agent_id, &path)?;
                    storage.upsert_reflex_preference(&AgentReflexPreference {
                        agent_id: agent_id.clone(),
                        preference_key: preference_key.clone(),
                        precedence,
                        reflexes_json,
                        config_json,
                        updated_at: 0,
                    })?;
                    Ok(())
                })();
                match result {
                    Ok(()) => IpcResponse::success(
                        "agent_reflex_preference",
                        Some(serde_json::json!({
                            "message": format!("Stored learned reflex preference '{preference_key}'.")
                        })),
                    ),
                    Err(err) => {
                        error!("Failed to store learned reflex preference: {err}");
                        IpcResponse::error(
                            "upsert_agent_reflex_preference",
                            "STORAGE_ERROR",
                            err.to_string(),
                        )
                    }
                }
            }
            IpcRequest::GetAgentReflexPreferences {
                agent_id,
                preference_key,
            } => {
                let path = agent_graph_db_path(&agent_id);
                let result = (|| -> anyhow::Result<Vec<serde_json::Value>> {
                    if !path.exists() {
                        return Ok(vec![]);
                    }
                    let storage = SqliteAgentGraphStorage::open(&agent_id, &path)?;
                    let preferences = if let Some(key) = preference_key {
                        storage
                            .get_reflex_preference(&key)?
                            .map(|r| vec![r])
                            .unwrap_or_default()
                    } else {
                        storage.list_reflex_preferences()?
                    };
                    Ok(preferences
                        .into_iter()
                        .map(|p| {
                            serde_json::json!({
                                "agent_id": p.agent_id,
                                "preference_key": p.preference_key,
                                "precedence": p.precedence,
                                "reflexes": p.reflexes_json,
                                "config": p.config_json,
                                "updated_at": p.updated_at,
                            })
                        })
                        .collect())
                })();
                match result {
                    Ok(rows) => IpcResponse::AgentReflexPreferences { rows },
                    Err(err) => {
                        error!("Failed to read reflex preferences: {err}");
                        IpcResponse::error(
                            "get_agent_reflex_preferences",
                            "STORAGE_ERROR",
                            err.to_string(),
                        )
                    }
                }
            }
            // ── routing pipeline rule CRUD ───────────────────────────────────
            IpcRequest::UpsertRoutingPipelineRule {
                agent_id,
                rule_id,
                rule_json,
            } => {
                use ansible_mesh_core::agent_graph_storage::RoutingPipelineRule;
                let path = agent_graph_db_path(&agent_id);
                let result = (|| -> anyhow::Result<()> {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let storage = SqliteAgentGraphStorage::open(&agent_id, &path)?;
                    storage.upsert_pipeline_rule(&RoutingPipelineRule {
                        agent_id: agent_id.clone(),
                        rule_id: rule_id.clone(),
                        rule_json,
                        updated_at: 0,
                    })?;
                    Ok(())
                })();
                match result {
                    Ok(()) => IpcResponse::success(
                        "routing_pipeline_rule",
                        Some(serde_json::json!({
                            "message": format!("Routing pipeline rule '{rule_id}' stored. Takes effect on the next inbound turn.")
                        })),
                    ),
                    Err(err) => {
                        error!("Failed to store routing pipeline rule: {err}");
                        IpcResponse::error(
                            "upsert_routing_pipeline_rule",
                            "STORAGE_ERROR",
                            err.to_string(),
                        )
                    }
                }
            }
            IpcRequest::RemoveRoutingPipelineRule { agent_id, rule_id } => {
                let path = agent_graph_db_path(&agent_id);
                let result = (|| -> anyhow::Result<bool> {
                    if !path.exists() {
                        return Ok(false);
                    }
                    let storage = SqliteAgentGraphStorage::open(&agent_id, &path)?;
                    storage.remove_pipeline_rule(&rule_id)
                })();
                match result {
                    Ok(deleted) => IpcResponse::success(
                        "routing_pipeline_rule",
                        Some(serde_json::json!({
                            "message": if deleted {
                                format!("Routing pipeline rule '{rule_id}' removed.")
                            } else {
                                format!("Routing pipeline rule '{rule_id}' not found.")
                            }
                        })),
                    ),
                    Err(err) => {
                        error!("Failed to remove routing pipeline rule: {err}");
                        IpcResponse::error(
                            "remove_routing_pipeline_rule",
                            "STORAGE_ERROR",
                            err.to_string(),
                        )
                    }
                }
            }
            IpcRequest::GetRoutingPipelineRules { agent_id, rule_id } => {
                let path = agent_graph_db_path(&agent_id);
                let result = (|| -> anyhow::Result<Vec<serde_json::Value>> {
                    if !path.exists() {
                        return Ok(vec![]);
                    }
                    let storage = SqliteAgentGraphStorage::open(&agent_id, &path)?;
                    let rules = if let Some(id) = rule_id {
                        storage
                            .get_pipeline_rule(&id)?
                            .map(|r| vec![r])
                            .unwrap_or_default()
                    } else {
                        storage.list_pipeline_rules()?
                    };
                    Ok(rules
                        .into_iter()
                        .map(|r| {
                            serde_json::json!({
                                "agent_id": r.agent_id,
                                "rule_id": r.rule_id,
                                "rule": r.rule_json,
                                "updated_at": r.updated_at,
                            })
                        })
                        .collect())
                })();
                match result {
                    Ok(pipeline_rules) => IpcResponse::RoutingPipelineRules { pipeline_rules },
                    Err(err) => {
                        error!("Failed to read routing pipeline rules: {err}");
                        IpcResponse::error(
                            "get_routing_pipeline_rules",
                            "STORAGE_ERROR",
                            err.to_string(),
                        )
                    }
                }
            }

            IpcRequest::RecordRoleHandoffReflexEvidence {
                agent_id,
                role_name,
                legacy_trigger_class,
                source_turn,
            } => {
                use ansible_mesh_core::agent_graph_storage::AgentReflexPreference;
                let path = agent_graph_db_path(&agent_id);
                let result = (|| -> anyhow::Result<serde_json::Value> {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let storage = SqliteAgentGraphStorage::open(&agent_id, &path)?;
                    let role_record = graph.get_role_incarnation(&agent_id, &role_name)?;
                    let toolset_profile = role_record
                        .as_ref()
                        .map(|role| role.toolset_profile.clone());
                    let toolset_record = toolset_profile.as_deref().and_then(|profile_name| {
                        graph.get_toolset_profile(profile_name).ok().flatten()
                    });
                    let preference_key = format!("same-self-role-handoff:{role_name}");
                    let existing = storage.get_reflex_preference(&preference_key)?;
                    let previous_count = existing
                        .as_ref()
                        .and_then(|pref| pref.config_json.get("success_count"))
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0);
                    let success_count = previous_count + 1;
                    let reinforced = success_count >= 2;
                    let existing_precedence =
                        existing.as_ref().map(|pref| pref.precedence).unwrap_or(70);
                    let updated_at = existing.as_ref().map(|pref| pref.updated_at).unwrap_or(0);
                    let existing_config = existing
                        .as_ref()
                        .map(|pref| pref.config_json.clone())
                        .unwrap_or_default();
                    let toolset_profile = toolset_profile.or_else(|| {
                        existing_config
                            .get("toolset_profile")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)
                    });
                    let toolset_description = toolset_record
                        .as_ref()
                        .and_then(|profile| profile.description.clone())
                        .or_else(|| {
                            existing_config
                                .get("toolset_description")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string)
                        });
                    let allowed_skills = toolset_record
                        .as_ref()
                        .map(|profile| profile.allowed_skills.clone())
                        .filter(|skills| !skills.is_empty())
                        .or_else(|| {
                            existing_config
                                .get("allowed_skills")
                                .and_then(serde_json::Value::as_array)
                                .map(|skills| {
                                    skills
                                        .iter()
                                        .filter_map(serde_json::Value::as_str)
                                        .map(str::to_string)
                                        .collect::<Vec<_>>()
                                })
                                .filter(|skills| !skills.is_empty())
                        })
                        .unwrap_or_default();
                    let role_identity_addendum = role_record
                        .as_ref()
                        .and_then(|role| role.role_identity_addendum.clone())
                        .or_else(|| {
                            existing_config
                                .get("role_identity_addendum")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string)
                        });
                    let role_manifest_excerpt = role_record
                        .as_ref()
                        .and_then(|role| role.role_manifest.as_deref())
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .map(|text| text.chars().take(180).collect::<String>())
                        .or_else(|| {
                            existing_config
                                .get("role_manifest_excerpt")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string)
                        });
                    let manifest_instructed = role_record
                        .as_ref()
                        .and_then(|role| role.role_manifest.as_ref())
                        .is_some()
                        || existing_config
                            .get("manifest_instructed")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false);
                    let manifest_markers = {
                        let mut sources = Vec::new();
                        sources.push(role_name.as_str());
                        if let Some(text) = role_identity_addendum.as_deref() {
                            sources.push(text);
                        }
                        if let Some(text) = role_manifest_excerpt.as_deref() {
                            sources.push(text);
                        }
                        let collected = collect_role_receptor_markers(&sources);
                        if collected.is_empty() {
                            existing_config
                                .get("manifest_markers")
                                .and_then(serde_json::Value::as_array)
                                .map(|items| {
                                    items
                                        .iter()
                                        .filter_map(serde_json::Value::as_str)
                                        .map(str::to_string)
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default()
                        } else {
                            collected
                        }
                    };
                    let skill_markers = if !allowed_skills.is_empty() {
                        collect_role_receptor_markers(
                            &allowed_skills
                                .iter()
                                .map(String::as_str)
                                .collect::<Vec<_>>(),
                        )
                    } else {
                        existing_config
                            .get("skill_markers")
                            .and_then(serde_json::Value::as_array)
                            .map(|items| {
                                items
                                    .iter()
                                    .filter_map(serde_json::Value::as_str)
                                    .map(str::to_string)
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default()
                    };
                    let toolset_markers = {
                        let mut sources = Vec::new();
                        if let Some(text) = toolset_profile.as_deref() {
                            sources.push(text);
                        }
                        if let Some(text) = toolset_description.as_deref() {
                            sources.push(text);
                        }
                        let collected = collect_role_receptor_markers(&sources);
                        if collected.is_empty() {
                            existing_config
                                .get("toolset_markers")
                                .and_then(serde_json::Value::as_array)
                                .map(|items| {
                                    items
                                        .iter()
                                        .filter_map(serde_json::Value::as_str)
                                        .map(str::to_string)
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default()
                        } else {
                            collected
                        }
                    };

                    storage.upsert_reflex_preference(&AgentReflexPreference {
                        agent_id: agent_id.clone(),
                        preference_key: preference_key.clone(),
                        precedence: existing_precedence,
                        reflexes_json: serde_json::json!({
                            "role_handoff_reflex": {
                                "target_role": role_name,
                                "trigger_class": legacy_trigger_class,
                                "source": "successful_same_self_handoff",
                                "tool_name": "handoff.to_role",
                            }
                        }),
                        config_json: serde_json::json!({
                            "reason": format!("remembered successful same-self handoff to role '{role_name}'"),
                            "role_name": role_name,
                            "trigger_class": legacy_trigger_class,
                            "source_tool": "handoff.to_role",
                            "source_turn": source_turn,
                            "toolset_profile": toolset_profile,
                            "toolset_description": toolset_description,
                            "allowed_skills": allowed_skills,
                            "role_identity_addendum": role_identity_addendum,
                            "role_manifest_excerpt": role_manifest_excerpt,
                            "manifest_markers": manifest_markers,
                            "skill_markers": skill_markers,
                            "toolset_markers": toolset_markers,
                            "workflow_skill": "handoff.to_role",
                            "manifest_instructed": manifest_instructed,
                            "success_count": success_count,
                            "habit_state": if reinforced { "reinforced" } else { "candidate" },
                        }),
                        updated_at,
                    })?;
                    Ok(serde_json::json!({
                        "preference_key": preference_key,
                        "success_count": success_count,
                        "habit_state": if reinforced { "reinforced" } else { "candidate" },
                    }))
                })();
                match result {
                    Ok(payload) => {
                        IpcResponse::success("role_handoff_reflex_evidence", Some(payload))
                    }
                    Err(err) => {
                        error!("Failed to record role handoff reflex evidence: {err}");
                        IpcResponse::error(
                            "record_role_handoff_reflex_evidence",
                            "STORAGE_ERROR",
                            err.to_string(),
                        )
                    }
                }
            }
            IpcRequest::AppendRoutingPolicyEvaluation {
                proposal_id,
                evaluation_kind,
                decision,
                reason,
                source_tool,
            } => match graph.append_routing_policy_evaluation(
                &proposal_id,
                ansible_mesh_core::graph::RoutingPolicyEvaluationRecord {
                    evaluation_kind,
                    decision,
                    reason,
                    created_at: unix_ts(),
                    source_tool,
                },
            ) {
                Ok(true) => IpcResponse::success(
                    "routing_policy_evaluation",
                    Some(serde_json::json!({
                        "proposal_id": proposal_id,
                    })),
                ),
                Ok(false) => IpcResponse::error(
                    "routing_policy_evaluation",
                    "NOT_FOUND",
                    format!("unknown routing policy proposal '{}'", proposal_id),
                ),
                Err(err) => {
                    error!("Failed to append routing policy evaluation: {err}");
                    IpcResponse::error(
                        "routing_policy_evaluation",
                        "STORAGE_ERROR",
                        err.to_string(),
                    )
                }
            },
            IpcRequest::SetRoutingPolicyDisposition {
                proposal_id,
                state,
                reason,
                source_tool,
            } => match graph.set_routing_policy_disposition(
                &proposal_id,
                state.clone(),
                reason.clone(),
                unix_ts(),
                source_tool,
            ) {
                Ok(true) => IpcResponse::success(
                    "routing_policy_disposition",
                    Some(serde_json::json!({
                        "proposal_id": proposal_id,
                        "state": state,
                        "reason": reason,
                    })),
                ),
                Ok(false) => IpcResponse::error(
                    "routing_policy_disposition",
                    "NOT_FOUND",
                    format!("unknown routing policy proposal '{}'", proposal_id),
                ),
                Err(err) => {
                    error!("Failed to set routing policy disposition: {err}");
                    IpcResponse::error(
                        "routing_policy_disposition",
                        "STORAGE_ERROR",
                        err.to_string(),
                    )
                }
            },
            // Resource broker seam (agent-resource-broker). The registry records
            // grants/denials and answers routing-table queries; it does NOT
            // materialize or tear down guests this slice (that stays with the
            // GuestManager path). No production guest issues these yet, so this
            // wiring is observably no-op for existing flows.
            IpcRequest::ResourceRequest(req) => {
                use crate::service::resource_registry::RegistryOutcome;
                let outcome = resource_registry.lock().await.register_request(req);
                match outcome {
                    RegistryOutcome::Granted(resource_granted) => {
                        IpcResponse::ResourceGranted { resource_granted }
                    }
                    RegistryOutcome::Materializing(resource_materializing) => {
                        IpcResponse::ResourceMaterializing {
                            resource_materializing,
                        }
                    }
                    RegistryOutcome::Denied(resource_denied) => {
                        IpcResponse::ResourceDenied { resource_denied }
                    }
                }
            }
            IpcRequest::ResourceReleased(rel) => {
                let zero_tenants = resource_registry.lock().await.apply_release(&rel);
                IpcResponse::Standard {
                    ok: true,
                    code: "RESOURCE_RELEASED".to_string(),
                    message: if zero_tenants {
                        "released; instance now has zero tenants".to_string()
                    } else {
                        "released".to_string()
                    },
                    corr_id: "resource_released".to_string(),
                    data: Some(serde_json::json!({
                        "instance_id": rel.instance_id,
                        "zero_tenants": zero_tenants,
                    })),
                }
            }
            IpcRequest::RegisterComponent { manifest } => {
                Self::handle_register_component(graph, materialization_requester, manifest).await
            }
            IpcRequest::ListGraphInstances {} => match graph.get_graph_runner_registry() {
                Ok(records) => {
                    let instances: Vec<serde_json::Value> = records
                        .into_iter()
                        .map(|r| {
                            serde_json::json!({
                                "graph_id": r.graph_id,
                                "instance_id": r.instance_id,
                                "registered_at": r.registered_at,
                            })
                        })
                        .collect();
                    IpcResponse::GraphInstanceList { instances }
                }
                Err(e) => {
                    IpcResponse::error("list_graph_instances", "STORAGE_ERROR", e.to_string())
                }
            },
            IpcRequest::ListComponents {} => Self::handle_list_components(graph, local_node_id),
            IpcRequest::SetComponentActive { guest_id, active } => {
                Self::handle_set_component_active(
                    graph,
                    materialization_requester,
                    local_node_id,
                    &guest_id,
                    active,
                )
                .await
            }
            IpcRequest::RestartComponent { guest_id, reason } => {
                // Agent-originated restarts (component.restart steward tool)
                // require operational admin authority, and are ALWAYS treated
                // as automatic remediation: an agent never gets the operator
                // (budget-exempt) reason regardless of what the wire says, so
                // a wedged guest cannot be restart-looped past the shared
                // respawn budget. Heal-dispatcher, CLI, and desktop paths
                // pass through unchanged.
                let reason = match steward_agent_admin_gate(
                    graph,
                    current_identity.as_ref(),
                    "restart_component",
                ) {
                    Err(refusal) => return refusal,
                    Ok(true) => RestartReason::Heal,
                    Ok(false) => reason,
                };
                Self::handle_restart_component(
                    graph,
                    materialization_requester,
                    local_node_id,
                    &guest_id,
                    reason,
                )
                .await
            }
            IpcRequest::RemoveComponent { guest_id } => {
                Self::handle_remove_component(graph, local_node_id, &guest_id).await
            }
            IpcRequest::SeedRemoteIncarnation {
                node_id,
                hotel_id,
                incarnation_id,
                target_role,
                socket_path,
            } => {
                let caps = NodeCapabilities {
                    node_id: node_id.clone(),
                    roles: vec![ansible_mesh_core::NodeRole::AnsibleNode],
                    models: vec![],
                    tools: vec![],
                    constraints: NodeConstraints::default(),
                    build_version: String::new(),
                };
                let ad = CapabilityAdvertisement {
                    hotel_id,
                    node_id: node_id.clone(),
                    incarnation_id,
                    target_role,
                    availability_state: "live".into(),
                    selection_hint: None,
                    latency_hint_ms: None,
                    max_concurrent_jobs: None,
                    active_jobs: 0,
                    queue_depth: 0,
                };
                registry
                    .write()
                    .await
                    .update_node(caps, vec![ad], None, None);
                if let Some(path) = socket_path {
                    peer_sockets.write().await.insert(node_id, path);
                }
                IpcResponse::success("seed_remote_incarnation", None)
            }
            // ── Cron scheduler ──────────────────────────────────────────────
            IpcRequest::RegisterCronJob { job } => {
                Self::handle_register_cron_job(job, graph, current_identity).await
            }
            IpcRequest::RemoveCronJob { job_id } => {
                Self::handle_remove_cron_job(job_id, graph, current_identity).await
            }
            IpcRequest::ListCronJobs => Self::handle_list_cron_jobs(graph, current_identity),
            IpcRequest::EnableCronJob { job_id } => {
                Self::handle_enable_cron_job(job_id, graph, current_identity).await
            }
            IpcRequest::DisableCronJob { job_id } => {
                Self::handle_disable_cron_job(job_id, graph, current_identity).await
            }
            IpcRequest::SetCronPolicy { job_id, policy } => {
                Self::handle_set_cron_policy(job_id, policy, graph, current_identity)
            }
            // GracefulShutdown is hotel→guest only; a guest sending it is a no-op.
            IpcRequest::GracefulShutdown { .. } => IpcResponse::error(
                "graceful_shutdown",
                "NOT_APPLICABLE",
                "GracefulShutdown is a hotel-to-guest signal; guests do not send it.",
            ),

            // Fire-and-forget paracrine dispatch.
            // The caller does NOT wait for the specialist's response — it arrives later
            // as a paracrine_response inbound task at reply_to_node/reply_to_role.
            IpcRequest::ParacrineEmit {
                role,
                exosome,
                reply_to_node,
                reply_to_role,
                reply_to_guest_id,
                ..
            } => {
                // Session key: paracrine:{chat_id}:{role}
                //
                // Keyed on chat_id (not the full source_session_id) so the specialist
                // accumulates turn-window context per conversation. Multiple whisper calls
                // from the same Telegram chat land in the same specialist session and are
                // queued by the FIFO pending_user_tasks mechanism rather than overwriting
                // each other. The role suffix keeps sessions distinct across specialists.
                let specialist_chat_id = exosome.source_chat_id.clone().unwrap_or_default();
                let specialist_session_id = if specialist_chat_id.is_empty() {
                    format!("paracrine:{role}")
                } else {
                    format!("paracrine:{}:{role}", specialist_chat_id)
                };
                // Derive transport from the source_session_id prefix (e.g. "telegram" from
                // "telegram:7898847424:agent-bjork-01") so the specialist's session tracks
                // its origin transport even though it runs in a distinct session.
                let specialist_transport = exosome
                    .source_session_id
                    .as_deref()
                    .and_then(|s| s.split(':').next())
                    .unwrap_or("paracrine")
                    .to_string();
                let paracrine_task = serde_json::json!({
                    "action": "paracrine_request",
                    "content": exosome.prompt,
                    "exosome": exosome,
                    "session_id": specialist_session_id,
                    "chat_id": specialist_chat_id,
                    "transport": specialist_transport,
                    "final_reply_to": reply_to_node,
                    "final_reply_role": reply_to_role,
                    "final_reply_guest_id": reply_to_guest_id,
                });
                let task_json = paracrine_task.to_string();
                let task_id = Uuid::new_v4();

                // Resolve the role incarnation once, up front. Role-incarnation
                // philotes register their inbox under `routing_role()`
                // ("role:{agent_id}:{role_name}"), never under the bare role
                // name — checking `inboxes.get(&role)` here always missed even
                // when the role was already live, which forced every whisper
                // down the "no subscriber" branch unconditionally and
                // materialized a SECOND, colliding process for a role that
                // may already be live (e.g. via an operator's own
                // `/role <name>` handoff) — the actual root cause of two
                // Chronos processes fighting over one inbox subscription.
                let incarnation = graph.find_role_incarnation_by_name(&role);
                let subscription_key = match &incarnation {
                    Ok(Some(inc)) => inc.routing_role(),
                    _ => role.clone(),
                };

                // Check if the target role has a live inbox subscriber.
                let has_subscriber = {
                    let guard = inboxes.lock().await;
                    guard
                        .get(&subscription_key)
                        .is_some_and(|subs| !subs.is_empty())
                };

                // A whisper the hotel cannot deliver-or-credibly-park must be
                // REFUSED, not swallowed: a philote blocking on
                // wait_for_response trusts a success response and parks its
                // whole turn for PARACRINE_WHISPER_WAIT_SECS. Live incident
                // 2026-08-25: Chronos could not be materialized (1ms after the
                // park) yet the handler returned success — Beacon sat deaf for
                // 660s and the operator got an eviction apology instead of an
                // immediate, actionable "specialist unavailable".
                let mut refusal: Option<String> = None;

                if has_subscriber {
                    let delivered = Self::deliver_inbound_task(
                        inboxes,
                        local_node_id,
                        &subscription_key,
                        None,
                        task_id,
                        task_json,
                    )
                    .await;
                    if !delivered {
                        refusal = Some(format!(
                            "specialist role '{role}' lost its inbox subscriber before delivery"
                        ));
                    }
                } else {
                    // No live subscriber — look up the role incarnation, park the task
                    // under the incarnation's guest_id, and trigger materialization of
                    // a dedicated role-philote so it can connect and flush the park.
                    match incarnation {
                        Ok(Some(inc)) => {
                            // The philote that handles this role incarnation registers
                            // with guest_id = "{agent_id}:{role_name}".
                            let role_guest_id = inc.guest_id.clone();

                            // Cross-hotel: if the role's guest is not configured on this hotel,
                            // look it up in the mesh registry (HotelStateSync) and dispatch
                            // cross-hotel rather than trying to materialize it locally.
                            if !Self::configured_local_guest_exists(
                                graph,
                                local_node_id,
                                &role_guest_id,
                            ) {
                                let remote_node = {
                                    let reg = registry.read().await;
                                    Self::resolve_guest_home_node(graph, &reg, &role_guest_id)
                                };
                                if let Some(remote_node) = remote_node {
                                    // Inject delivery_target_guest_id so the remote hotel
                                    // routes to the correct brain/specialist philote.
                                    let remote_task_json = if let Ok(mut v) =
                                        serde_json::from_str::<serde_json::Value>(&task_json)
                                    {
                                        if let Some(obj) = v.as_object_mut() {
                                            obj.entry("delivery_target_guest_id").or_insert_with(
                                                || serde_json::json!(role_guest_id),
                                            );
                                        }
                                        serde_json::to_string(&v).unwrap_or(task_json)
                                    } else {
                                        task_json
                                    };
                                    let env = EventEnvelope {
                                        event_id: task_id,
                                        seq: 0,
                                        source_node_id: local_node_id.to_string(),
                                        target_node_id: Some(remote_node.clone()),
                                        source_agent_id: "unknown".into(),
                                        target_agent_id: Some(role.clone()),
                                        kind: EventKind::TaskInvoke,
                                        corr_id: "paracrine".into(),
                                        attempt: 0,
                                        created_at: 0,
                                        expires_at: None,
                                        payload: EventPayload::Inline {
                                            data: remote_task_json,
                                        },
                                        trace: vec![],
                                    };
                                    info!(
                                        role = %role,
                                        remote_node = %remote_node,
                                        role_guest_id = %role_guest_id,
                                        "ParacrineEmit: cross-hotel dispatch to mesh peer"
                                    );
                                    let _ =
                                        dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;
                                    return IpcResponse::success("paracrine_emit", None);
                                } else {
                                    warn!(
                                        role = %role,
                                        role_guest_id = %role_guest_id,
                                        "ParacrineEmit: role guest not found in mesh registry; attempting local materialization"
                                    );
                                }
                            }

                            // Park the task — flushed when the role philote connects.
                            {
                                let mut guard = parked_inbound.lock().await;
                                guard.entry(role_guest_id.clone()).or_default().push(
                                    ParkedInboundTask {
                                        source_node: local_node_id.to_string(),
                                        task_id,
                                        task_json,
                                        activate_session_id: None,
                                        parked_at: unix_ts(),
                                    },
                                );
                            }
                            info!(
                                role = %role,
                                role_guest_id = %role_guest_id,
                                task_id = %task_id,
                                "Parked paracrine task; will trigger role-philote materialization."
                            );

                            // Ensure the role-philote guest record exists in the graph,
                            // then ask the materializer to spawn it.
                            if let Some(hotel_name) = Self::local_hotel_name(graph, local_node_id) {
                                let hotel_guest_id =
                                    format!("{}:philote-{}", hotel_name, inc.role_name);

                                // Derive socket path from hotel record (shared by philote + companion).
                                let socket_path = graph
                                    .list_hotels()
                                    .ok()
                                    .and_then(|hs| {
                                        hs.into_iter()
                                            .find(|h| h.capabilities.node_id == local_node_id)
                                            .map(|h| h.ipc_socket_path)
                                    })
                                    .unwrap_or_default();

                                // Create the guest record if it doesn't already exist.
                                if graph
                                    .get_guest(&hotel_name, &hotel_guest_id)
                                    .ok()
                                    .flatten()
                                    .is_none()
                                {
                                    let config_json = serde_json::json!({
                                        "command": "philote",
                                        "args": [],
                                        "env": {
                                            "PHILOTIC_AGENT_ID": inc.agent_id,
                                            "PHILOTIC_ROLE_NAME": inc.role_name,
                                            "PHILOTIC_HOTEL_SOCKET": socket_path,
                                            "PHILOTIC_NODE_ID": local_node_id,
                                        }
                                    });
                                    let rec = ansible_mesh_core::storage::GuestRecord {
                                        hotel_name: hotel_name.clone(),
                                        guest_id: hotel_guest_id.clone(),
                                        role: inc.role_name.clone(),
                                        config_json: config_json.to_string(),
                                        is_active: true,
                                        active_pid: None,
                                        last_active_at: None,
                                    };
                                    if let Err(e) = graph.seed_guests(&hotel_name, &[rec]) {
                                        warn!(
                                            "Failed to seed role-philote guest record [{}]: {e}",
                                            hotel_guest_id
                                        );
                                    } else {
                                        info!(
                                            "Created role-philote guest record: {}",
                                            hotel_guest_id
                                        );
                                    }
                                }

                                // Ensure the companion agent-graph guest exists and spawns the
                                // converged `agent-datasource` binary. Legacy records seeded by
                                // older builds may still point at `agent-graph-runner`; upgrade
                                // those in place so both seeding paths use one binary.
                                let graph_runner_id =
                                    format!("{}:agent-graph-{}", hotel_name, inc.agent_id);
                                let runner_needs_seed = match graph
                                    .get_guest(&hotel_name, &graph_runner_id)
                                    .ok()
                                    .flatten()
                                {
                                    None => true,
                                    Some(rec) => {
                                        serde_json::from_str::<serde_json::Value>(&rec.config_json)
                                            .ok()
                                            .and_then(|v| {
                                                v.get("command")
                                                    .and_then(|c| c.as_str())
                                                    .map(str::to_string)
                                            })
                                            .as_deref()
                                            != Some("agent-datasource")
                                    }
                                };
                                if runner_needs_seed {
                                    let runner_rec = crate::agent_graph_guest_record(
                                        &hotel_name,
                                        &inc.agent_id,
                                        &socket_path,
                                    );
                                    if let Err(e) = graph.seed_guests(&hotel_name, &[runner_rec]) {
                                        warn!(
                                            "Failed to seed agent-graph companion guest [{}]: {e}",
                                            graph_runner_id
                                        );
                                    } else {
                                        info!(
                                            "Seeded agent-graph companion guest (agent-datasource): {}",
                                            graph_runner_id
                                        );
                                    }
                                }

                                // Trigger materialization of philote. A park is only
                                // credible when a specialist will actually connect to
                                // flush it — a refused/failed materialization means the
                                // parked task would wait forever, so refuse the emit.
                                if let Some(requester) = materialization_requester {
                                    match requester.ensure_guest_active(&hotel_guest_id).await {
                                        Ok(true) => info!(
                                            "Role-philote [{}] materialization triggered.",
                                            hotel_guest_id
                                        ),
                                        Ok(false) => {
                                            warn!(
                                                "Role-philote [{}] could not be materialized.",
                                                hotel_guest_id
                                            );
                                            refusal = Some(format!(
                                                "specialist role '{role}' could not be \
                                                 materialized (guest {hotel_guest_id} refused — \
                                                 likely deactivated)"
                                            ));
                                        }
                                        Err(e) => {
                                            warn!(
                                                "Role-philote [{}] materialization error: {e}",
                                                hotel_guest_id
                                            );
                                            refusal = Some(format!(
                                                "specialist role '{role}' materialization \
                                                 error: {e}"
                                            ));
                                        }
                                    }
                                    // Trigger materialization of the companion agent-graph
                                    // guest. Non-fatal: the specialist can still answer
                                    // (datasource tools degrade, cognition does not).
                                    match requester.ensure_guest_active(&graph_runner_id).await {
                                        Ok(true) => info!(
                                            "Agent-graph guest [{}] materialization triggered.",
                                            graph_runner_id
                                        ),
                                        Ok(false) => warn!(
                                            "Agent-graph guest [{}] could not be materialized.",
                                            graph_runner_id
                                        ),
                                        Err(e) => warn!(
                                            "Agent-graph guest [{}] materialization error: {e}",
                                            graph_runner_id
                                        ),
                                    }
                                } else {
                                    refusal = Some(format!(
                                        "specialist role '{role}' is not running and this \
                                         hotel has no materializer to spawn it"
                                    ));
                                }
                            } else {
                                warn!(
                                    "Cannot materialize role-philote for '{}': local hotel record missing.",
                                    role
                                );
                                refusal = Some(format!(
                                    "cannot materialize specialist role '{role}': local \
                                     hotel record missing"
                                ));
                            }

                            // A refused park must not linger: a specialist that later
                            // materializes for another reason would flush a task whose
                            // caller already gave up and was told so.
                            if refusal.is_some() {
                                let mut guard = parked_inbound.lock().await;
                                if let Some(parked) = guard.get_mut(&role_guest_id) {
                                    parked.retain(|t| t.task_id != task_id);
                                    if parked.is_empty() {
                                        guard.remove(&role_guest_id);
                                    }
                                }
                            }
                        }
                        Ok(None) => {
                            warn!(
                                role = %role,
                                "No role incarnation found for paracrine target '{}'; refusing emit.",
                                role
                            );
                            refusal = Some(format!(
                                "no role incarnation named '{role}' exists on this hotel"
                            ));
                        }
                        Err(e) => {
                            warn!(
                                role = %role,
                                "Role incarnation lookup failed for '{}': {e}; refusing emit.",
                                role
                            );
                            refusal =
                                Some(format!("role incarnation lookup failed for '{role}': {e}"));
                        }
                    }
                }

                match refusal {
                    Some(reason) => {
                        IpcResponse::error("paracrine_emit", "SPECIALIST_UNAVAILABLE", reason)
                    }
                    None => IpcResponse::success("paracrine_emit", None),
                }
            }

            IpcRequest::GetHotelStatus => {
                Self::handle_get_hotel_status(local_node_id, graph, registry).await
            }

            IpcRequest::GetMemoryReport => Self::handle_get_memory_report(graph),

            IpcRequest::ReadCortex { vault, id, offset } => {
                Self::handle_read_cortex(vault, id, offset, local_node_id, graph, current_identity)
                    .await
            }

            IpcRequest::BestPlaceToRun {
                agent_id,
                role_name,
                tool_name,
                required_markers,
                prefer_locality,
            } => {
                Self::handle_best_place_to_run(
                    agent_id,
                    role_name,
                    tool_name,
                    required_markers,
                    prefer_locality,
                    local_node_id,
                    graph,
                    registry,
                )
                .await
            }

            IpcRequest::GetHotelLogs { lines } => Self::handle_get_hotel_logs(lines),

            IpcRequest::GetRouterStats { window_secs } => {
                use ansible_mesh_core::router_trace::{
                    RouterTraceStorage, SqliteRouterTraceStorage,
                };
                use std::time::{SystemTime, UNIX_EPOCH};

                let trace_db_path = {
                    let profile = std::env::var("PHILOTIC_PROFILE")
                        .ok()
                        .filter(|s| !s.is_empty());
                    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
                    match profile {
                        Some(p) => format!("{home}/.philotic/{p}/router_traces.db"),
                        None => format!("{home}/.philotic/router_traces.db"),
                    }
                };

                let generated_at = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                match SqliteRouterTraceStorage::open(&trace_db_path) {
                    Ok(store) => match store.provider_stats(window_secs) {
                        Ok(stats) => IpcResponse::RouterStats {
                            stats,
                            generated_at,
                        },
                        Err(e) => IpcResponse::error(
                            "router_stats",
                            "STATS_QUERY_FAILED",
                            &format!("failed to compute router stats: {e}"),
                        ),
                    },
                    Err(e) => IpcResponse::error(
                        "router_stats",
                        "TRACE_DB_UNAVAILABLE",
                        &format!("router trace DB not available at {trace_db_path}: {e}"),
                    ),
                }
            }

            // ── MCP membrane lease ─────────────────────────────────────────
            IpcRequest::AcquireMcpMembraneLease { lease_key, port } => {
                Self::handle_acquire_mcp_membrane_lease(
                    graph,
                    local_node_id,
                    mcp_membrane_leases,
                    conn_id,
                    current_identity.as_ref(),
                    lease_key,
                    port,
                )
                .await
            }

            IpcRequest::RenewMcpMembraneLease {
                lease_key,
                lease_epoch,
            } => {
                Self::handle_renew_mcp_membrane_lease(
                    mcp_membrane_leases,
                    conn_id,
                    lease_key,
                    lease_epoch,
                )
                .await
            }

            IpcRequest::ReleaseMcpMembraneLease { lease_key } => {
                Self::handle_release_mcp_membrane_lease(mcp_membrane_leases, conn_id, lease_key)
                    .await
            }

            // ── MCP route table management ────────────────────────────────
            IpcRequest::UpdateMcpRoutes {
                agent_id,
                routes,
                vault_ref,
            } => {
                let route_count = routes.len();
                // Persist so routes survive hotel restarts.
                let entry = serde_json::json!({
                    "agent_id": agent_id,
                    "routes": routes,
                    "vault_ref": vault_ref,
                });
                let mut all: std::collections::HashMap<String, serde_json::Value> = graph
                    .get_config_value("__mcp_routes__")
                    .ok()
                    .flatten()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                all.insert(agent_id.clone(), entry);
                if let Ok(json) = serde_json::to_string(&all) {
                    let _ = graph.set_config_value("__mcp_routes__", &json);
                }
                // Fan-out to any connected mcp-membrane guest.
                let task_json = serde_json::json!({
                    "action": "update_mcp_routes",
                    "agent_id": agent_id,
                    "routes": routes,
                })
                .to_string();
                Self::deliver_inbound_task(
                    inboxes,
                    local_node_id,
                    "mcp-membrane",
                    None,
                    Uuid::new_v4(),
                    task_json,
                )
                .await;
                IpcResponse::McpRoutesAccepted {
                    mcp_routes_agent_id: agent_id,
                    mcp_route_count: route_count,
                }
            }

            IpcRequest::RevokeMcpRoutes { agent_id } => {
                // Remove from persisted store.
                let mut all: std::collections::HashMap<String, serde_json::Value> = graph
                    .get_config_value("__mcp_routes__")
                    .ok()
                    .flatten()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                all.remove(&agent_id);
                if let Ok(json) = serde_json::to_string(&all) {
                    let _ = graph.set_config_value("__mcp_routes__", &json);
                }
                let task_json = serde_json::json!({
                    "action": "revoke_mcp_routes",
                    "agent_id": agent_id,
                })
                .to_string();
                Self::deliver_inbound_task(
                    inboxes,
                    local_node_id,
                    "mcp-membrane",
                    None,
                    Uuid::new_v4(),
                    task_json,
                )
                .await;
                IpcResponse::McpRoutesAccepted {
                    mcp_routes_agent_id: agent_id,
                    mcp_route_count: 0,
                }
            }

            IpcRequest::GetMcpRoutes {} => {
                let all: std::collections::HashMap<String, serde_json::Value> = graph
                    .get_config_value("__mcp_routes__")
                    .ok()
                    .flatten()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                let agents: Vec<philotic_client::PersistedMcpRouteEntry> = all
                    .into_values()
                    .filter_map(|v| serde_json::from_value(v).ok())
                    .collect();
                IpcResponse::McpRouteState { agents }
            }

            // ── MCP endpoint provisioning ──────────────────────────────────
            IpcRequest::ProvisionMcpEndpoint { config } => {
                let endpoint_id = config.endpoint_id.clone();
                let port = config.port;

                // Identity: the self-asserted owner must match the registered
                // guest identity on this connection.
                if !Self::mcp_owner_identity_ok(current_identity, &config.owner_agent_id) {
                    return IpcResponse::error(
                        "mcp_endpoint",
                        "FORBIDDEN",
                        format!(
                            "owner_agent_id '{}' does not match the registered guest identity '{}'",
                            config.owner_agent_id,
                            current_identity
                                .as_ref()
                                .map(|i| i.guest_id.as_str())
                                .unwrap_or("<unregistered>")
                        ),
                    );
                }

                // An existing endpoint may only be re-provisioned by its owner.
                {
                    let config_key = format!("__mcp_endpoint__:{endpoint_id}");
                    let existing_owner = graph
                        .get_config_value(&config_key)
                        .ok()
                        .flatten()
                        .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
                        .and_then(|v| v["owner_agent_id"].as_str().map(str::to_string));
                    if let Some(owner) = existing_owner {
                        if owner != config.owner_agent_id {
                            return IpcResponse::error(
                                "mcp_endpoint",
                                "FORBIDDEN",
                                format!("endpoint {endpoint_id} is already owned by {owner}"),
                            );
                        }
                    }
                }

                // Unauthenticated exposure beyond loopback requires an explicit
                // acknowledgment carried on the config (surfaced in the
                // provisioning approval prompt).
                if config.exposure > ansible_mesh_core::ExposureTier::Local
                    && !config.allow_unauthenticated
                {
                    let open_tools: Vec<&str> = config
                        .tools
                        .iter()
                        .filter(|t| {
                            matches!(
                                config.effective_auth(t),
                                ansible_mesh_core::mcp_route::McpAuthScheme::None
                            )
                        })
                        .map(|t| t.name.as_str())
                        .collect();
                    if !open_tools.is_empty() {
                        return IpcResponse::error(
                            "mcp_endpoint",
                            "UNAUTHENTICATED_EXPOSURE",
                            format!(
                                "endpoint '{}' declares {:?} exposure but tools [{}] have no auth \
                                 scheme; add bearer auth (mcp.grant_token), or pass \
                                 allow_unauthenticated=true to expose them anyway",
                                endpoint_id,
                                config.exposure,
                                open_tools.join(", ")
                            ),
                        );
                    }
                }

                // Fence: reject provision if the declared exposure exceeds the hotel's
                // current perimeter ceiling. An agent must not open a higher-tier
                // endpoint than the hotel is currently able to defend.
                {
                    use ansible_mesh_core::PerimeterSnapshot;
                    let ceiling = graph
                        .get_config_value("__hotel_perimeter__")
                        .ok()
                        .flatten()
                        .and_then(|j| serde_json::from_str::<PerimeterSnapshot>(&j).ok())
                        .map(|s| s.ceiling)
                        .unwrap_or(ansible_mesh_core::ExposureTier::Internet); // safe default: allow if unknown

                    if config.exposure > ceiling {
                        return IpcResponse::error(
                            "mcp_endpoint",
                            "EXPOSURE_EXCEEDS_PERIMETER",
                            format!(
                                "endpoint '{}' declares exposure {:?} but hotel ceiling is {:?}; \
                                 lower the exposure tier or wait for the perimeter to expand",
                                endpoint_id, config.exposure, ceiling
                            ),
                        );
                    }
                }

                // Handler policies must be structurally valid (known reflexes,
                // non-empty error fallbacks). The philote checks this too, but
                // operator scripts talk to this socket directly.
                for tool in &config.tools {
                    if let Some(policy) = &tool.handler {
                        if let Err(e) = policy.validate() {
                            return IpcResponse::error(
                                "mcp_endpoint",
                                "INVALID_HANDLER_POLICY",
                                format!("endpoint '{}' tool '{}': {e}", endpoint_id, tool.name),
                            );
                        }
                    }
                }

                // Persist the endpoint config in the context graph.
                let config_key = format!("__mcp_endpoint__:{endpoint_id}");
                let preapproval_key = format!("__mcp_preapproval__:{endpoint_id}");

                let config_json = match serde_json::to_string(&config) {
                    Ok(j) => j,
                    Err(e) => {
                        return IpcResponse::error(
                            "mcp_endpoint",
                            "SERIALIZE_ERROR",
                            e.to_string(),
                        );
                    }
                };

                if let Err(e) = graph.set_config_value(&config_key, &config_json) {
                    return IpcResponse::error("mcp_endpoint", "CONFIG_STORE_ERROR", e.to_string());
                }

                // Write a durable intent node capturing who provisioned this endpoint, why,
                // and at what exposure tier. Queryable later via agent graph reads without
                // needing to decode the full endpoint config.
                {
                    let tool_names: Vec<&str> =
                        config.tools.iter().map(|t| t.name.as_str()).collect();
                    let provisioned_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let intent = serde_json::json!({
                        "endpoint_id": endpoint_id,
                        "owner_agent_id": config.owner_agent_id,
                        "exposure": config.exposure,
                        "port": port,
                        "tool_names": tool_names,
                        "provisioned_at": provisioned_at,
                    });
                    let intent_key = format!("__mcp_endpoint_intent__:{endpoint_id}");
                    if let Ok(intent_json) = serde_json::to_string(&intent) {
                        let _ = graph.set_config_value(&intent_key, &intent_json);
                    }
                }

                // Persist pre-approval rules separately for fast lookup.
                if !config.preapproval_rules.is_empty() {
                    let rules_json = match serde_json::to_string(&config.preapproval_rules) {
                        Ok(j) => j,
                        Err(e) => {
                            return IpcResponse::error(
                                "mcp_endpoint",
                                "SERIALIZE_ERROR",
                                e.to_string(),
                            );
                        }
                    };
                    if let Err(e) = graph.set_config_value(&preapproval_key, &rules_json) {
                        return IpcResponse::error(
                            "mcp_endpoint",
                            "CONFIG_STORE_ERROR",
                            e.to_string(),
                        );
                    }
                }

                // Fan out the full config to the membrane-mcp guest inbox.
                let guest_id = format!("mcp-membrane-{endpoint_id}");
                let task_json = serde_json::json!({
                    "action": "update_mcp_config",
                    "config": config,
                })
                .to_string();
                Self::push_to_mcp_endpoint_guest(inboxes, local_node_id, &guest_id, task_json)
                    .await;

                // Also re-push the current perimeter tier so that a membrane-mcp guest
                // that reconnected after a hotel restart has the correct fence tier even
                // if it missed the initial startup push.
                {
                    use ansible_mesh_core::PerimeterSnapshot;
                    let ceiling = graph
                        .get_config_value("__hotel_perimeter__")
                        .ok()
                        .flatten()
                        .and_then(|j| serde_json::from_str::<PerimeterSnapshot>(&j).ok())
                        .map(|s| s.ceiling)
                        .unwrap_or_default();
                    let perimeter_task = serde_json::json!({
                        "action": "update_perimeter",
                        "tier": ceiling,
                    })
                    .to_string();
                    Self::push_to_mcp_endpoint_guest(
                        inboxes,
                        local_node_id,
                        &guest_id,
                        perimeter_task,
                    )
                    .await;
                }

                info!(
                    endpoint_id,
                    port, "MCP endpoint config stored and fanned out."
                );

                // Phase 3: materialize the membrane-mcp guest for this endpoint.
                let materialized = if let Some(hotel_name) =
                    Self::local_hotel_name(graph, local_node_id)
                {
                    let socket_path = graph
                        .list_hotels()
                        .ok()
                        .and_then(|hs| {
                            hs.into_iter()
                                .find(|h| h.capabilities.node_id == local_node_id)
                                .map(|h| h.ipc_socket_path)
                        })
                        .unwrap_or_default();

                    let mcp_guest_id = format!("mcp-membrane-{endpoint_id}");
                    let mcp_config = serde_json::json!({
                        "command": "membrane-mcp",
                        "args": [],
                        "env": {
                            "MCP_PORT": port.to_string(),
                            "PHILOTIC_HOTEL_SOCKET": socket_path,
                            "PHILOTIC_GUEST_ID": mcp_guest_id,
                            "PHILOTIC_NODE_ID": local_node_id,
                        }
                    });
                    let mcp_record = ansible_mesh_core::storage::GuestRecord {
                        hotel_name: hotel_name.clone(),
                        guest_id: mcp_guest_id.clone(),
                        role: "mcp-membrane".into(),
                        config_json: mcp_config.to_string(),
                        is_active: true,
                        active_pid: None,
                        last_active_at: None,
                    };
                    match graph.upsert_guest(&mcp_record) {
                        Err(e) => {
                            warn!(
                                "ProvisionMcpEndpoint: failed to upsert mcp-membrane guest [{}]: {e}",
                                mcp_guest_id
                            );
                            false
                        }
                        Ok(()) => {
                            if let Some(requester) = materialization_requester {
                                match requester.ensure_guest_active(&mcp_guest_id).await {
                                    Ok(true) => {
                                        info!(
                                            "mcp-membrane guest [{}] materialization triggered.",
                                            mcp_guest_id
                                        );
                                        true
                                    }
                                    Ok(false) => {
                                        warn!(
                                            "mcp-membrane guest [{}] could not be materialized.",
                                            mcp_guest_id
                                        );
                                        false
                                    }
                                    Err(e) => {
                                        warn!(
                                            "mcp-membrane guest [{}] materialization error: {e}",
                                            mcp_guest_id
                                        );
                                        false
                                    }
                                }
                            } else {
                                false
                            }
                        }
                    }
                } else {
                    warn!(
                        "ProvisionMcpEndpoint: hotel name not found for node [{}]; skipping guest spawn.",
                        local_node_id
                    );
                    false
                };

                IpcResponse::McpEndpointProvisioned {
                    endpoint_id,
                    port,
                    materialized,
                }
            }

            IpcRequest::RevokeMcpEndpoint {
                endpoint_id,
                owner_agent_id,
            } => {
                // Identity: the self-asserted owner must match the registered
                // guest identity on this connection.
                if !Self::mcp_owner_identity_ok(current_identity, &owner_agent_id) {
                    return IpcResponse::error(
                        "mcp_endpoint",
                        "FORBIDDEN",
                        format!(
                            "owner_agent_id '{owner_agent_id}' does not match the registered guest identity"
                        ),
                    );
                }
                // Verify ownership before clearing.
                let config_key = format!("__mcp_endpoint__:{endpoint_id}");
                let existing = graph.get_config_value(&config_key).ok().flatten();
                if let Some(json) = existing {
                    let existing_owner = serde_json::from_str::<serde_json::Value>(&json)
                        .ok()
                        .and_then(|v| v["owner_agent_id"].as_str().map(str::to_string));
                    if existing_owner.as_deref() != Some(&owner_agent_id) {
                        return IpcResponse::error(
                            "mcp_endpoint",
                            "FORBIDDEN",
                            format!("endpoint {endpoint_id} is not owned by {owner_agent_id}"),
                        );
                    }
                }

                // Read port before clearing (needed for response).
                let port = serde_json::from_str::<serde_json::Value>(
                    &graph
                        .get_config_value(&config_key)
                        .ok()
                        .flatten()
                        .unwrap_or_default(),
                )
                .ok()
                .and_then(|v| v["port"].as_u64())
                .unwrap_or(0) as u16;

                // Clear stored state.
                let _ = graph.set_config_value(&config_key, "null");
                let _ =
                    graph.set_config_value(&format!("__mcp_preapproval__:{endpoint_id}"), "null");
                let _ = graph
                    .set_config_value(&format!("__mcp_endpoint_intent__:{endpoint_id}"), "null");

                // Signal the membrane-mcp guest to shut down.
                let guest_id = format!("mcp-membrane-{endpoint_id}");
                let task_json = serde_json::json!({
                    "action": "revoke_mcp_config",
                    "endpoint_id": endpoint_id,
                })
                .to_string();
                Self::push_to_mcp_endpoint_guest(inboxes, local_node_id, &guest_id, task_json)
                    .await;

                // Mark guest inactive so hotel doesn't respawn after revoke.
                if let Some(hotel_name) = Self::local_hotel_name(graph, local_node_id) {
                    let _ = graph.set_guest_active(&hotel_name, &guest_id, false);
                }

                info!(endpoint_id, "MCP endpoint revoked.");

                IpcResponse::McpEndpointProvisioned {
                    endpoint_id,
                    port,
                    materialized: false,
                }
            }

            IpcRequest::GetMcpEndpointStatus { endpoint_id } => {
                use ansible_mesh_core::{ExposureTier, PerimeterSnapshot};
                let config_key = format!("__mcp_endpoint__:{endpoint_id}");
                let config: Option<serde_json::Value> = graph
                    .get_config_value(&config_key)
                    .ok()
                    .flatten()
                    .and_then(|j| serde_json::from_str(&j).ok());
                let ceiling = graph
                    .get_config_value("__hotel_perimeter__")
                    .ok()
                    .flatten()
                    .and_then(|j| serde_json::from_str::<PerimeterSnapshot>(&j).ok())
                    .map(|s| s.ceiling)
                    .unwrap_or(ExposureTier::Local);
                IpcResponse::success(
                    "mcp_status",
                    Some(serde_json::json!({
                        "endpoint_id": endpoint_id,
                        "config": config,
                        "hotel_ceiling": ceiling,
                        "active": config.is_some(),
                    })),
                )
            }

            IpcRequest::ProvisionMcpTokenGrant {
                endpoint_id,
                owner_agent_id,
                token_id,
                tool_name,
                scopes,
                expires_at,
                allotment,
                rotate,
            } => {
                use ansible_mesh_core::mcp_endpoint::McpEndpointConfig;
                use ansible_mesh_core::mcp_route::{McpAuthScheme, McpTokenGrant};

                if !Self::mcp_owner_identity_ok(current_identity, &owner_agent_id) {
                    return IpcResponse::error(
                        "mcp_token_grant",
                        "FORBIDDEN",
                        format!(
                            "owner_agent_id '{owner_agent_id}' does not match the registered guest identity"
                        ),
                    );
                }

                let config_key = format!("__mcp_endpoint__:{endpoint_id}");
                let mut config: McpEndpointConfig = match graph
                    .get_config_value(&config_key)
                    .ok()
                    .flatten()
                    .and_then(|j| serde_json::from_str(&j).ok())
                {
                    Some(c) => c,
                    None => {
                        return IpcResponse::error(
                            "mcp_token_grant",
                            "ENDPOINT_NOT_FOUND",
                            format!("no provisioned MCP endpoint '{endpoint_id}'"),
                        );
                    }
                };
                if config.owner_agent_id != owner_agent_id {
                    return IpcResponse::error(
                        "mcp_token_grant",
                        "FORBIDDEN",
                        format!("endpoint {endpoint_id} is not owned by {owner_agent_id}"),
                    );
                }

                // Mint the credential. Only its BLAKE3 hash is ever stored.
                let raw_token = {
                    use rand::RngCore;
                    let mut bytes = [0u8; 32];
                    rand::thread_rng().fill_bytes(&mut bytes);
                    format!("pmcp_{}", hex::encode(bytes))
                };
                let hash_hex = blake3::hash(raw_token.as_bytes()).to_hex().to_string();

                // Locate the auth slot: named tool, or the endpoint default.
                let auth_slot: &mut Option<McpAuthScheme> = match tool_name.as_deref() {
                    Some(name) => match config.tools.iter_mut().find(|t| t.name == name) {
                        Some(tool) => &mut tool.auth,
                        None => {
                            return IpcResponse::error(
                                "mcp_token_grant",
                                "TOOL_NOT_FOUND",
                                format!("endpoint '{endpoint_id}' has no tool '{name}'"),
                            );
                        }
                    },
                    None => &mut config.default_auth,
                };
                if auth_slot.is_none() || matches!(auth_slot, Some(McpAuthScheme::None)) {
                    *auth_slot = Some(McpAuthScheme::BearerToken { grants: vec![] });
                }
                let Some(McpAuthScheme::BearerToken { grants }) = auth_slot.as_mut() else {
                    unreachable!("auth slot normalized to BearerToken above");
                };

                let vault_ref = if rotate {
                    let Some(existing) = grants.iter().find(|g| g.token_id == token_id) else {
                        return IpcResponse::error(
                            "mcp_token_grant",
                            "GRANT_NOT_FOUND",
                            format!("no grant '{token_id}' on endpoint '{endpoint_id}' to rotate"),
                        );
                    };
                    let vault_ref = existing.vault_ref.clone();
                    if let Err(e) = crate::vault::rotate_secret(graph, &vault_ref, &hash_hex) {
                        return IpcResponse::error("mcp_token_grant", "VAULT_ERROR", e.to_string());
                    }
                    vault_ref
                } else {
                    if grants.iter().any(|g| g.token_id == token_id) {
                        return IpcResponse::error(
                            "mcp_token_grant",
                            "DUPLICATE_TOKEN_ID",
                            format!(
                                "grant '{token_id}' already exists on endpoint '{endpoint_id}'; \
                                 use rotate to replace its credential"
                            ),
                        );
                    }
                    let secret_ref = match store_secret(
                        graph,
                        SecretInput {
                            secret_kind: "mcp_endpoint_token".into(),
                            scope: "hotel".into(),
                            allowed_roles: vec!["mcp-membrane".into()],
                            allowed_guests: Vec::new(),
                            plaintext: hash_hex,
                        },
                    ) {
                        Ok(r) => r,
                        Err(e) => {
                            return IpcResponse::error(
                                "mcp_token_grant",
                                "VAULT_ERROR",
                                e.to_string(),
                            );
                        }
                    };
                    grants.push(McpTokenGrant {
                        token_id: token_id.clone(),
                        vault_ref: secret_ref.clone(),
                        scopes: scopes.clone(),
                        expires_at,
                        allotment: allotment.clone(),
                    });
                    secret_ref
                };

                config.updated_at = unix_ts();
                match serde_json::to_string(&config) {
                    Ok(json) => {
                        if let Err(e) = graph.set_config_value(&config_key, &json) {
                            return IpcResponse::error(
                                "mcp_token_grant",
                                "CONFIG_STORE_ERROR",
                                e.to_string(),
                            );
                        }
                    }
                    Err(e) => {
                        return IpcResponse::error(
                            "mcp_token_grant",
                            "SERIALIZE_ERROR",
                            e.to_string(),
                        );
                    }
                }

                // Fan the updated config out to the endpoint's membrane guest.
                // NOTE: the membrane caches vault hashes for up to 60s, so a
                // rotated-out credential may keep working for that window.
                let guest_id = format!("mcp-membrane-{endpoint_id}");
                let task_json = serde_json::json!({
                    "action": "update_mcp_config",
                    "config": config,
                })
                .to_string();
                Self::push_to_mcp_endpoint_guest(inboxes, local_node_id, &guest_id, task_json)
                    .await;

                info!(
                    endpoint_id,
                    token_id, rotate, "MCP token grant provisioned."
                );

                IpcResponse::success(
                    "mcp_token_grant",
                    Some(serde_json::json!({
                        "endpoint_id": endpoint_id,
                        "token_id": token_id,
                        "tool_name": tool_name,
                        "vault_ref": vault_ref,
                        "rotated": rotate,
                        "raw_token": raw_token,
                        "warning": "store this token now — only its hash is retained and it cannot be shown again",
                    })),
                )
            }

            IpcRequest::RevokeMcpTokenGrant {
                endpoint_id,
                owner_agent_id,
                token_id,
            } => {
                use ansible_mesh_core::mcp_endpoint::McpEndpointConfig;
                use ansible_mesh_core::mcp_route::McpAuthScheme;

                if !Self::mcp_owner_identity_ok(current_identity, &owner_agent_id) {
                    return IpcResponse::error(
                        "mcp_token_grant",
                        "FORBIDDEN",
                        format!(
                            "owner_agent_id '{owner_agent_id}' does not match the registered guest identity"
                        ),
                    );
                }

                let config_key = format!("__mcp_endpoint__:{endpoint_id}");
                let mut config: McpEndpointConfig = match graph
                    .get_config_value(&config_key)
                    .ok()
                    .flatten()
                    .and_then(|j| serde_json::from_str(&j).ok())
                {
                    Some(c) => c,
                    None => {
                        return IpcResponse::error(
                            "mcp_token_grant",
                            "ENDPOINT_NOT_FOUND",
                            format!("no provisioned MCP endpoint '{endpoint_id}'"),
                        );
                    }
                };
                if config.owner_agent_id != owner_agent_id {
                    return IpcResponse::error(
                        "mcp_token_grant",
                        "FORBIDDEN",
                        format!("endpoint {endpoint_id} is not owned by {owner_agent_id}"),
                    );
                }

                // Remove the grant everywhere it appears. An emptied BearerToken
                // list stays BearerToken (nobody can call) — it must not degrade
                // to None, which would open the tool to loopback callers.
                let mut removed = 0usize;
                let mut slots: Vec<&mut Option<McpAuthScheme>> = vec![&mut config.default_auth];
                slots.extend(config.tools.iter_mut().map(|t| &mut t.auth));
                for slot in slots {
                    if let Some(McpAuthScheme::BearerToken { grants }) = slot.as_mut() {
                        let before = grants.len();
                        grants.retain(|g| g.token_id != token_id);
                        removed += before - grants.len();
                    }
                }
                if removed == 0 {
                    return IpcResponse::error(
                        "mcp_token_grant",
                        "GRANT_NOT_FOUND",
                        format!("no grant '{token_id}' on endpoint '{endpoint_id}'"),
                    );
                }

                config.updated_at = unix_ts();
                match serde_json::to_string(&config) {
                    Ok(json) => {
                        if let Err(e) = graph.set_config_value(&config_key, &json) {
                            return IpcResponse::error(
                                "mcp_token_grant",
                                "CONFIG_STORE_ERROR",
                                e.to_string(),
                            );
                        }
                    }
                    Err(e) => {
                        return IpcResponse::error(
                            "mcp_token_grant",
                            "SERIALIZE_ERROR",
                            e.to_string(),
                        );
                    }
                }

                let guest_id = format!("mcp-membrane-{endpoint_id}");
                let task_json = serde_json::json!({
                    "action": "update_mcp_config",
                    "config": config,
                })
                .to_string();
                Self::push_to_mcp_endpoint_guest(inboxes, local_node_id, &guest_id, task_json)
                    .await;

                info!(endpoint_id, token_id, removed, "MCP token grant revoked.");

                IpcResponse::success(
                    "mcp_token_grant",
                    Some(serde_json::json!({
                        "endpoint_id": endpoint_id,
                        "token_id": token_id,
                        "removed_grants": removed,
                    })),
                )
            }

            // ── Governed outbound integration registry ───────────────────────
            IpcRequest::RegisterIntegrationBinding { binding } => {
                use ansible_mesh_core::integration::{IntegrationBinding, IntegrationTarget};

                let binding_id = binding.binding_id.clone();
                if !Self::mcp_owner_identity_ok(current_identity, &binding.owner_agent_id) {
                    return IpcResponse::error(
                        "integration_binding",
                        "FORBIDDEN",
                        format!(
                            "owner_agent_id '{}' does not match the registered guest identity",
                            binding.owner_agent_id
                        ),
                    );
                }
                if let Err(message) = binding.validate() {
                    return IpcResponse::error("integration_binding", "INVALID_BINDING", message);
                }
                if let IntegrationTarget::Mcp { upstream_id } = &binding.target {
                    let upstreams: std::collections::HashMap<
                        String,
                        ansible_mesh_core::mcp_upstream::McpUpstreamConfig,
                    > = graph
                        .get_config_value("__mcp_upstreams__")
                        .ok()
                        .flatten()
                        .and_then(|value| serde_json::from_str(&value).ok())
                        .unwrap_or_default();
                    if !upstreams.contains_key(upstream_id) {
                        return IpcResponse::error(
                            "integration_binding",
                            "MCP_UPSTREAM_NOT_FOUND",
                            format!("no MCP upstream is registered as '{upstream_id}'"),
                        );
                    }
                }

                let mut bindings: std::collections::HashMap<String, IntegrationBinding> = graph
                    .get_config_value("__integration_bindings__")
                    .ok()
                    .flatten()
                    .and_then(|value| serde_json::from_str(&value).ok())
                    .unwrap_or_default();
                if let Some(existing) = bindings.get(&binding_id) {
                    if existing.owner_agent_id != binding.owner_agent_id {
                        return IpcResponse::error(
                            "integration_binding",
                            "FORBIDDEN",
                            format!(
                                "binding '{binding_id}' is owned by '{}'",
                                existing.owner_agent_id
                            ),
                        );
                    }
                    if binding.updated_at < existing.updated_at {
                        return IpcResponse::error(
                            "integration_binding",
                            "STALE_UPDATE",
                            format!(
                                "binding update timestamp {} predates current {}",
                                binding.updated_at, existing.updated_at
                            ),
                        );
                    }
                }
                bindings.insert(binding_id.clone(), binding.clone());
                let serialized = match serde_json::to_string(&bindings) {
                    Ok(value) => value,
                    Err(error) => {
                        return IpcResponse::error(
                            "integration_binding",
                            "SERIALIZE_ERROR",
                            error.to_string(),
                        );
                    }
                };
                if let Err(error) = graph.set_config_value("__integration_bindings__", &serialized)
                {
                    return IpcResponse::error(
                        "integration_binding",
                        "CONFIG_STORE_ERROR",
                        error.to_string(),
                    );
                }

                let entry =
                    Self::integration_binding_entry(binding, registry, graph, local_node_id).await;
                let materialized_node_id = if matches!(
                    entry.binding.target,
                    IntegrationTarget::Http(_) | IntegrationTarget::Oidc(_)
                ) && !matches!(
                    entry.placement,
                    ansible_mesh_core::integration::EgressPlacementDecision::Deny { .. }
                ) {
                    Self::materialize_integration_runner(
                        &entry,
                        registry,
                        graph,
                        materialization_requester,
                        local_node_id,
                    )
                    .await
                } else {
                    None
                };
                info!(
                    binding_id,
                    execution_node_id = ?entry.execution_node_id,
                    materialized_node_id = ?materialized_node_id,
                    "outbound integration binding registered"
                );
                IpcResponse::IntegrationBindingRegistered {
                    binding_id,
                    materialized_node_id,
                }
            }

            IpcRequest::RevokeIntegrationBinding {
                binding_id,
                owner_agent_id,
            } => {
                use ansible_mesh_core::integration::IntegrationBinding;
                if !Self::mcp_owner_identity_ok(current_identity, &owner_agent_id) {
                    return IpcResponse::error(
                        "integration_binding",
                        "FORBIDDEN",
                        format!(
                            "owner_agent_id '{owner_agent_id}' does not match the registered guest identity"
                        ),
                    );
                }
                let mut bindings: std::collections::HashMap<String, IntegrationBinding> = graph
                    .get_config_value("__integration_bindings__")
                    .ok()
                    .flatten()
                    .and_then(|value| serde_json::from_str(&value).ok())
                    .unwrap_or_default();
                match bindings.get(&binding_id) {
                    Some(binding) if binding.owner_agent_id == owner_agent_id => {}
                    Some(binding) => {
                        return IpcResponse::error(
                            "integration_binding",
                            "FORBIDDEN",
                            format!(
                                "binding '{binding_id}' is owned by '{}'",
                                binding.owner_agent_id
                            ),
                        );
                    }
                    None => {
                        return IpcResponse::error(
                            "integration_binding",
                            "NOT_FOUND",
                            format!("no integration binding is registered as '{binding_id}'"),
                        );
                    }
                }
                bindings.remove(&binding_id);
                if let Err(error) = graph.set_config_value(
                    "__integration_bindings__",
                    &serde_json::to_string(&bindings).unwrap_or_else(|_| "{}".into()),
                ) {
                    return IpcResponse::error(
                        "integration_binding",
                        "CONFIG_STORE_ERROR",
                        error.to_string(),
                    );
                }
                info!(binding_id, "outbound integration binding revoked");
                IpcResponse::IntegrationBindingRegistered {
                    binding_id,
                    materialized_node_id: None,
                }
            }

            IpcRequest::GetIntegrationBindings {} => {
                use ansible_mesh_core::integration::IntegrationBinding;
                let bindings: std::collections::HashMap<String, IntegrationBinding> = graph
                    .get_config_value("__integration_bindings__")
                    .ok()
                    .flatten()
                    .and_then(|value| serde_json::from_str(&value).ok())
                    .unwrap_or_default();
                let mut entries = Vec::with_capacity(bindings.len());
                for binding in bindings.into_values() {
                    entries.push(
                        Self::integration_binding_entry(binding, registry, graph, local_node_id)
                            .await,
                    );
                }
                entries
                    .sort_by(|left, right| left.binding.binding_id.cmp(&right.binding.binding_id));
                IpcResponse::IntegrationBindingsState {
                    integration_bindings: entries,
                }
            }

            IpcRequest::ExchangeOperatorOidc {
                provider,
                authorization_code,
                code_verifier,
                redirect_uri,
            } => {
                let authorized = current_identity.as_ref().is_some_and(|identity| {
                    identity.role == "management" && identity.guest_id == "philotic-web-oidc"
                });
                if !authorized {
                    return IpcResponse::error(
                        "operator_oidc",
                        "FORBIDDEN",
                        "operator OIDC exchange requires the philotic-web-oidc management identity",
                    );
                }
                match Self::exchange_operator_oidc(
                    socket_path,
                    local_node_id,
                    graph,
                    &provider,
                    authorization_code,
                    code_verifier,
                    redirect_uri,
                )
                .await
                {
                    Ok(response) => IpcResponse::success(
                        "operator_oidc",
                        Some(
                            serde_json::to_value(response)
                                .unwrap_or_else(|_| serde_json::json!({})),
                        ),
                    ),
                    Err(error) => {
                        error!(provider, %error, "governed operator OIDC exchange failed");
                        IpcResponse::error(
                            "operator_oidc",
                            "OIDC_EXCHANGE_FAILED",
                            error.to_string(),
                        )
                    }
                }
            }

            IpcRequest::RecordIntegrationAudit { audit } => {
                let authorized = current_identity
                    .as_ref()
                    .is_some_and(|identity| identity.role == "egress-http-runner");
                if !authorized {
                    return IpcResponse::error(
                        "integration_audit",
                        "FORBIDDEN",
                        "only the egress-http-runner role may append integration audits",
                    );
                }
                if audit.finished_at_ms < audit.started_at_ms
                    || audit.binding_id.is_empty()
                    || audit.tool_name.is_empty()
                    || audit.agent_id.is_empty()
                    || audit.caller_role.is_empty()
                    || audit.session_id.is_empty()
                    || audit.turn_id.is_empty()
                    || audit.correlation_id.is_empty()
                    || audit.executor_node_id.is_empty()
                    || (audit.outcome == "failed" && audit.failure_code.is_none())
                {
                    return IpcResponse::error(
                        "integration_audit",
                        "INVALID_AUDIT",
                        "audit identity and time range are invalid",
                    );
                }
                let mut audits: Vec<ansible_mesh_core::integration::HttpIntegrationAudit> = graph
                    .get_config_value("__integration_audits__")
                    .ok()
                    .flatten()
                    .and_then(|value| serde_json::from_str(&value).ok())
                    .unwrap_or_default();
                audits.push(audit);
                if audits.len() > 2_000 {
                    let remove = audits.len() - 2_000;
                    audits.drain(..remove);
                }
                match serde_json::to_string(&audits)
                    .map_err(anyhow::Error::from)
                    .and_then(|value| graph.set_config_value("__integration_audits__", &value))
                {
                    Ok(()) => IpcResponse::success("integration_audit", None),
                    Err(error) => IpcResponse::error(
                        "integration_audit",
                        "CONFIG_STORE_ERROR",
                        error.to_string(),
                    ),
                }
            }

            IpcRequest::GetIntegrationAudit { binding_id, limit } => {
                let authorized = current_identity.as_ref().is_none_or(|identity| {
                    matches!(
                        identity.role.as_str(),
                        "operator" | "admin" | "management" | "desktop-membrane"
                    )
                });
                if !authorized {
                    return IpcResponse::error(
                        "integration_audit",
                        "FORBIDDEN",
                        "integration audit reads require an operator identity",
                    );
                }
                let audits: Vec<ansible_mesh_core::integration::HttpIntegrationAudit> = graph
                    .get_config_value("__integration_audits__")
                    .ok()
                    .flatten()
                    .and_then(|value| serde_json::from_str(&value).ok())
                    .unwrap_or_default();
                let mut selected: Vec<_> = audits
                    .into_iter()
                    .rev()
                    .filter(|audit| {
                        binding_id
                            .as_ref()
                            .is_none_or(|binding_id| audit.binding_id == *binding_id)
                    })
                    .take(limit.unwrap_or(100).clamp(1, 500) as usize)
                    .collect();
                selected.sort_by(|left, right| right.finished_at_ms.cmp(&left.finished_at_ms));
                IpcResponse::IntegrationAuditState {
                    integration_audits: selected,
                }
            }

            IpcRequest::ProvisionIntegrationCredential {
                binding_id,
                owner_agent_id,
                credential,
            } => {
                use ansible_mesh_core::integration::{IntegrationBinding, IntegrationTarget};
                if !Self::mcp_owner_identity_ok(current_identity, &owner_agent_id) {
                    return IpcResponse::error(
                        "integration_credential",
                        "FORBIDDEN",
                        format!(
                            "owner_agent_id '{owner_agent_id}' does not match the registered guest identity"
                        ),
                    );
                }
                if credential.trim().is_empty() {
                    return IpcResponse::error(
                        "integration_credential",
                        "EMPTY_CREDENTIAL",
                        "credential must be non-empty",
                    );
                }
                let mut bindings: std::collections::HashMap<String, IntegrationBinding> = graph
                    .get_config_value("__integration_bindings__")
                    .ok()
                    .flatten()
                    .and_then(|value| serde_json::from_str(&value).ok())
                    .unwrap_or_default();
                let Some(snapshot) = bindings.get(&binding_id).cloned() else {
                    return IpcResponse::error(
                        "integration_credential",
                        "NOT_FOUND",
                        format!("no integration binding is registered as '{binding_id}'"),
                    );
                };
                if snapshot.owner_agent_id != owner_agent_id {
                    return IpcResponse::error(
                        "integration_credential",
                        "FORBIDDEN",
                        format!("binding '{binding_id}' is not owned by '{owner_agent_id}'"),
                    );
                }
                let entry = Self::integration_binding_entry(
                    snapshot.clone(),
                    registry,
                    graph,
                    local_node_id,
                )
                .await;
                let Some(execution_node_id) = entry.execution_node_id.clone() else {
                    return IpcResponse::error(
                        "integration_credential",
                        "PLACEMENT_DENIED",
                        match entry.placement {
                            ansible_mesh_core::integration::EgressPlacementDecision::Deny {
                                reason,
                            } => reason,
                            _ => "binding has no executable placement".into(),
                        },
                    );
                };
                let existing_ref = match &snapshot.target {
                    IntegrationTarget::Http(target) => target
                        .credential
                        .as_ref()
                        .map(|binding| binding.secret_ref.clone()),
                    IntegrationTarget::Oidc(target) => target.client_secret_ref.clone(),
                    IntegrationTarget::Mcp { .. } => {
                        return IpcResponse::error(
                            "integration_credential",
                            "USE_MCP_CREDENTIAL_SURFACE",
                            "MCP bindings use ProvisionMcpUpstreamCredential",
                        );
                    }
                };

                let (vault_ref, rotated) = if execution_node_id == local_node_id {
                    match existing_ref {
                        Some(secret_ref)
                            if graph.get_secret(&secret_ref).ok().flatten().is_some() =>
                        {
                            if let Err(error) =
                                crate::vault::rotate_secret(graph, &secret_ref, &credential)
                            {
                                return IpcResponse::error(
                                    "integration_credential",
                                    "VAULT_ERROR",
                                    error.to_string(),
                                );
                            }
                            (secret_ref, true)
                        }
                        _ => match store_secret(
                            graph,
                            SecretInput {
                                secret_kind: "integration_http_credential".into(),
                                scope: "hotel".into(),
                                allowed_roles: vec!["egress-http-runner".into()],
                                allowed_guests: Vec::new(),
                                plaintext: credential,
                            },
                        ) {
                            Ok(secret_ref) => (secret_ref, false),
                            Err(error) => {
                                return IpcResponse::error(
                                    "integration_credential",
                                    "VAULT_ERROR",
                                    error.to_string(),
                                );
                            }
                        },
                    }
                } else {
                    let request = match existing_ref {
                        Some(secret_ref) if !secret_ref.starts_with("pending:") => {
                            IpcRequest::RotateOperatorTargetSecret {
                                target_node_id: execution_node_id.clone(),
                                secret_ref,
                                plaintext: credential,
                            }
                        }
                        _ => IpcRequest::AddOperatorTargetVaultEntry {
                            target_node_id: execution_node_id.clone(),
                            vault_name: format!("integration/{binding_id}"),
                            plaintext: credential,
                            allowed_roles: vec!["egress-http-runner".into()],
                        },
                    };
                    let response = Self::handle_operator_target_request(
                        request,
                        registry,
                        graph,
                        materialization_requester,
                        local_node_id,
                    )
                    .await;
                    match response {
                        IpcResponse::OperatorTargetSecretMutationAckView {
                            operator_target_secret_mutation,
                        } if operator_target_secret_mutation.ok => {
                            let Some(secret_ref) = operator_target_secret_mutation.secret_ref
                            else {
                                return IpcResponse::error(
                                    "integration_credential",
                                    "REMOTE_VAULT_ERROR",
                                    "remote vault mutation returned no secret_ref",
                                );
                            };
                            (
                                secret_ref,
                                operator_target_secret_mutation.operation == "rotate",
                            )
                        }
                        other => {
                            return IpcResponse::error(
                                "integration_credential",
                                "REMOTE_VAULT_ERROR",
                                format!("remote vault mutation failed: {other:?}"),
                            );
                        }
                    }
                };

                let binding = bindings
                    .get_mut(&binding_id)
                    .expect("binding snapshot came from this registry");
                match &mut binding.target {
                    IntegrationTarget::Http(target) => match &mut target.credential {
                        Some(credential_binding) => {
                            credential_binding.secret_ref = vault_ref.clone()
                        }
                        None => {
                            return IpcResponse::error(
                                "integration_credential",
                                "MISSING_CREDENTIAL_INJECTION",
                                "binding must declare credential header and format before provisioning",
                            );
                        }
                    },
                    IntegrationTarget::Oidc(target) => {
                        target.client_secret_ref = Some(vault_ref.clone());
                    }
                    IntegrationTarget::Mcp { .. } => unreachable!("MCP target returned above"),
                }
                binding.updated_at = unix_ts();
                if let Err(error) = graph.set_config_value(
                    "__integration_bindings__",
                    &serde_json::to_string(&bindings).unwrap_or_else(|_| "{}".into()),
                ) {
                    return IpcResponse::error(
                        "integration_credential",
                        "CONFIG_STORE_ERROR",
                        error.to_string(),
                    );
                }
                info!(
                    binding_id,
                    execution_node_id,
                    rotated,
                    "integration credential provisioned at execution hotel"
                );
                IpcResponse::success(
                    "integration_credential",
                    Some(serde_json::json!({
                        "binding_id": binding_id,
                        "vault_ref": vault_ref,
                        "execution_node_id": execution_node_id,
                        "rotated": rotated,
                    })),
                )
            }

            // ── MCP upstream (client fabric) registry ─────────────────────────
            IpcRequest::RegisterMcpUpstream { config } => {
                use ansible_mesh_core::mcp_upstream::{
                    McpEgressPolicy, McpUpstreamConfig, McpUpstreamTransport, host_from_http_url,
                };
                let upstream_id = config.upstream_id.clone();

                // Owner claim must match the registered guest identity on this
                // connection (hardening S4 pattern).
                if !Self::mcp_owner_identity_ok(current_identity, &config.owner_agent_id) {
                    return IpcResponse::error(
                        "mcp_upstream",
                        "FORBIDDEN",
                        format!(
                            "owner_agent_id '{}' does not match the registered guest identity",
                            config.owner_agent_id
                        ),
                    );
                }

                // Transport fence, by kind:
                // - HTTP: egress policy on the target host (loopback + tailnet
                //   by default; operator widens via `mcp_egress_policy`).
                // - Stdio: fail-closed command allowlist (operator widens via
                //   `mcp_stdio_allowlist` / `phil mcp allow-command`). The
                //   guest additionally spawns the child with a scrubbed env.
                match &config.transport {
                    McpUpstreamTransport::Stdio { command, args } => {
                        use ansible_mesh_core::mcp_upstream::McpStdioAllowlist;
                        let allowlist: McpStdioAllowlist = graph
                            .get_config_value("mcp_stdio_allowlist")
                            .ok()
                            .flatten()
                            .and_then(|j| serde_json::from_str(&j).ok())
                            .unwrap_or_default();
                        if !allowlist.command_allowed(command, args) {
                            return IpcResponse::error(
                                "mcp_upstream",
                                "STDIO_NOT_ALLOWED",
                                format!(
                                    "stdio command '{command}' (args {args:?}) is not on the \
                                     operator allowlist; an operator must add it via \
                                     `phil mcp allow-command` (config node mcp_stdio_allowlist)"
                                ),
                            );
                        }
                    }
                    McpUpstreamTransport::Http { url } => {
                        let url = url.clone();
                        // Egress fence: the target host must be loopback,
                        // tailnet, or explicitly allowlisted.
                        let policy: McpEgressPolicy = graph
                            .get_config_value("mcp_egress_policy")
                            .ok()
                            .flatten()
                            .and_then(|j| serde_json::from_str(&j).ok())
                            .unwrap_or_default();
                        match host_from_http_url(&url) {
                            Some(host) if policy.host_allowed(&host) => {}
                            Some(host) => {
                                return IpcResponse::error(
                                    "mcp_upstream",
                                    "EGRESS_DENIED",
                                    format!(
                                        "host '{host}' is outside the egress policy (loopback + \
                                         tailnet by default); an operator must add it to the \
                                         mcp_egress_policy config node"
                                    ),
                                );
                            }
                            None => {
                                return IpcResponse::error(
                                    "mcp_upstream",
                                    "INVALID_URL",
                                    format!("'{url}' is not a valid http(s) URL"),
                                );
                            }
                        }
                    }
                }

                // Ownership: an existing registration may only be updated by
                // its owner.
                let mut upstream_registry: std::collections::HashMap<String, McpUpstreamConfig> =
                    graph
                        .get_config_value("__mcp_upstreams__")
                        .ok()
                        .flatten()
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default();
                if let Some(existing) = upstream_registry.get(&upstream_id) {
                    if existing.owner_agent_id != config.owner_agent_id {
                        return IpcResponse::error(
                            "mcp_upstream",
                            "FORBIDDEN",
                            format!(
                                "upstream {upstream_id} is owned by {}",
                                existing.owner_agent_id
                            ),
                        );
                    }
                }
                upstream_registry.insert(upstream_id.clone(), config.clone());
                match serde_json::to_string(&upstream_registry) {
                    Ok(json) => {
                        if let Err(e) = graph.set_config_value("__mcp_upstreams__", &json) {
                            return IpcResponse::error(
                                "mcp_upstream",
                                "CONFIG_STORE_ERROR",
                                e.to_string(),
                            );
                        }
                    }
                    Err(e) => {
                        return IpcResponse::error(
                            "mcp_upstream",
                            "SERIALIZE_ERROR",
                            e.to_string(),
                        );
                    }
                }

                // Mirror MCP HTTP transport into the canonical integration
                // registry. The mcp-client keeps protocol/session ownership;
                // this binding only selects and materializes the hotel that
                // performs its network I/O through egress-http-runner.
                if matches!(config.transport, McpUpstreamTransport::Http { .. }) {
                    use ansible_mesh_core::integration::{
                        EgressTrafficClass, IntegrationBinding, IntegrationTarget,
                    };
                    let binding = IntegrationBinding {
                        binding_id: format!("mcp:{upstream_id}"),
                        owner_agent_id: config.owner_agent_id.clone(),
                        display_name: Some(format!("MCP upstream {upstream_id}")),
                        target: IntegrationTarget::Mcp {
                            upstream_id: upstream_id.clone(),
                        },
                        grant_agents: config.grant_agents.clone(),
                        grant_skills: vec![],
                        traffic_class: EgressTrafficClass::Mcp,
                        placement: config.placement.clone(),
                        requires_approval: true,
                        enabled: true,
                        updated_at: config.updated_at,
                    };
                    let mut bindings: std::collections::HashMap<String, IntegrationBinding> = graph
                        .get_config_value("__integration_bindings__")
                        .ok()
                        .flatten()
                        .and_then(|value| serde_json::from_str(&value).ok())
                        .unwrap_or_default();
                    bindings.insert(binding.binding_id.clone(), binding.clone());
                    if let Ok(serialized) = serde_json::to_string(&bindings) {
                        if let Err(error) =
                            graph.set_config_value("__integration_bindings__", &serialized)
                        {
                            warn!(
                                upstream_id,
                                %error,
                                "failed to persist MCP transport integration binding"
                            );
                        }
                    }
                    let entry =
                        Self::integration_binding_entry(binding, registry, graph, local_node_id)
                            .await;
                    let _ = Self::materialize_integration_runner(
                        &entry,
                        registry,
                        graph,
                        materialization_requester,
                        local_node_id,
                    )
                    .await;
                }

                // Fan out the config to the mcp-client guest inbox.
                let task_json = serde_json::json!({
                    "action": "update_mcp_upstream",
                    "config": config,
                })
                .to_string();
                Self::deliver_inbound_task(
                    inboxes,
                    local_node_id,
                    "mcp-client-runner",
                    None,
                    Uuid::new_v4(),
                    task_json,
                )
                .await;

                // Materialize the single mcp-client guest on first use.
                let materialized = if let Some(hotel_name) =
                    Self::local_hotel_name(graph, local_node_id)
                {
                    let socket_path = graph
                        .list_hotels()
                        .ok()
                        .and_then(|hs| {
                            hs.into_iter()
                                .find(|h| h.capabilities.node_id == local_node_id)
                                .map(|h| h.ipc_socket_path)
                        })
                        .unwrap_or_default();
                    let client_config = serde_json::json!({
                        "command": "membrane-mcp-client",
                        "args": [],
                        "env": {
                            "PHILOTIC_HOTEL_SOCKET": socket_path,
                            "PHILOTIC_GUEST_ID": "mcp-client",
                            "PHILOTIC_NODE_ID": local_node_id,
                        }
                    });
                    let record = ansible_mesh_core::storage::GuestRecord {
                        hotel_name: hotel_name.clone(),
                        guest_id: "mcp-client".into(),
                        role: "mcp-client-runner".into(),
                        config_json: client_config.to_string(),
                        is_active: true,
                        active_pid: None,
                        last_active_at: None,
                    };
                    match graph.upsert_guest(&record) {
                        Err(e) => {
                            warn!("RegisterMcpUpstream: failed to upsert mcp-client guest: {e}");
                            false
                        }
                        Ok(()) => {
                            if let Some(requester) = materialization_requester {
                                match requester.ensure_guest_active("mcp-client").await {
                                    Ok(spawned) => spawned,
                                    Err(e) => {
                                        warn!("mcp-client guest materialization error: {e}");
                                        false
                                    }
                                }
                            } else {
                                false
                            }
                        }
                    }
                } else {
                    warn!(
                        "RegisterMcpUpstream: hotel name not found for node [{}]; skipping guest spawn.",
                        local_node_id
                    );
                    false
                };

                info!(upstream_id, "MCP upstream registered and fanned out.");
                IpcResponse::McpUpstreamRegistered {
                    mcp_upstream_id: upstream_id,
                    mcp_upstream_materialized: materialized,
                }
            }

            IpcRequest::RevokeMcpUpstream {
                upstream_id,
                owner_agent_id,
            } => {
                use ansible_mesh_core::mcp_upstream::McpUpstreamConfig;
                if !Self::mcp_owner_identity_ok(current_identity, &owner_agent_id) {
                    return IpcResponse::error(
                        "mcp_upstream",
                        "FORBIDDEN",
                        format!(
                            "owner_agent_id '{owner_agent_id}' does not match the registered \
                             guest identity"
                        ),
                    );
                }
                let mut upstreams: std::collections::HashMap<String, McpUpstreamConfig> = graph
                    .get_config_value("__mcp_upstreams__")
                    .ok()
                    .flatten()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                match upstreams.get(&upstream_id) {
                    Some(existing) if existing.owner_agent_id != owner_agent_id => {
                        return IpcResponse::error(
                            "mcp_upstream",
                            "FORBIDDEN",
                            format!("upstream {upstream_id} is not owned by {owner_agent_id}"),
                        );
                    }
                    None => {
                        return IpcResponse::error(
                            "mcp_upstream",
                            "NOT_FOUND",
                            format!("no upstream registered as {upstream_id}"),
                        );
                    }
                    Some(_) => {}
                }
                upstreams.remove(&upstream_id);
                if let Ok(json) = serde_json::to_string(&upstreams) {
                    let _ = graph.set_config_value("__mcp_upstreams__", &json);
                }
                let mut integration_bindings: std::collections::HashMap<
                    String,
                    ansible_mesh_core::integration::IntegrationBinding,
                > = graph
                    .get_config_value("__integration_bindings__")
                    .ok()
                    .flatten()
                    .and_then(|value| serde_json::from_str(&value).ok())
                    .unwrap_or_default();
                integration_bindings.remove(&format!("mcp:{upstream_id}"));
                if let Ok(json) = serde_json::to_string(&integration_bindings) {
                    let _ = graph.set_config_value("__integration_bindings__", &json);
                }
                // Drop the stored catalog too.
                let mut catalogs: std::collections::HashMap<String, serde_json::Value> = graph
                    .get_config_value("__mcp_upstream_catalogs__")
                    .ok()
                    .flatten()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                catalogs.remove(&upstream_id);
                if let Ok(json) = serde_json::to_string(&catalogs) {
                    let _ = graph.set_config_value("__mcp_upstream_catalogs__", &json);
                }

                let task_json = serde_json::json!({
                    "action": "revoke_mcp_upstream",
                    "upstream_id": upstream_id,
                })
                .to_string();
                Self::deliver_inbound_task(
                    inboxes,
                    local_node_id,
                    "mcp-client-runner",
                    None,
                    Uuid::new_v4(),
                    task_json,
                )
                .await;

                info!(upstream_id, "MCP upstream revoked.");
                IpcResponse::McpUpstreamRegistered {
                    mcp_upstream_id: upstream_id,
                    mcp_upstream_materialized: false,
                }
            }

            IpcRequest::GetToolCatalog {} => match graph.list_abstract_tools() {
                Ok(tool_catalog) => IpcResponse::ToolCatalogState { tool_catalog },
                Err(err) => IpcResponse::Standard {
                    ok: false,
                    code: "tool_catalog_read_failed".into(),
                    message: format!("tool catalog read failed: {err}"),
                    corr_id: String::new(),
                    data: None,
                },
            },

            IpcRequest::GetMcpUpstreams {} => {
                use ansible_mesh_core::mcp_upstream::{McpUpstreamCatalog, McpUpstreamConfig};
                let registry: std::collections::HashMap<String, McpUpstreamConfig> = graph
                    .get_config_value("__mcp_upstreams__")
                    .ok()
                    .flatten()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                let mut catalogs: std::collections::HashMap<String, McpUpstreamCatalog> = graph
                    .get_config_value("__mcp_upstream_catalogs__")
                    .ok()
                    .flatten()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                let mut entries: Vec<philotic_client::McpUpstreamEntry> = registry
                    .into_values()
                    .map(|config| {
                        let catalog = catalogs.remove(&config.upstream_id);
                        philotic_client::McpUpstreamEntry { config, catalog }
                    })
                    .collect();
                entries.sort_by(|a, b| a.config.upstream_id.cmp(&b.config.upstream_id));
                IpcResponse::McpUpstreamsState {
                    mcp_upstreams: entries,
                }
            }

            IpcRequest::ReportMcpUpstreamCatalog { catalog } => {
                let upstream_id = catalog.upstream_id.clone();
                let mut catalogs: std::collections::HashMap<String, serde_json::Value> = graph
                    .get_config_value("__mcp_upstream_catalogs__")
                    .ok()
                    .flatten()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                match serde_json::to_value(&catalog) {
                    Ok(v) => {
                        catalogs.insert(upstream_id.clone(), v);
                    }
                    Err(e) => {
                        return IpcResponse::error(
                            "mcp_upstream",
                            "SERIALIZE_ERROR",
                            e.to_string(),
                        );
                    }
                }
                if let Err(e) = graph.set_config_value("__mcp_upstream_catalogs__", &{
                    match serde_json::to_string(&catalogs) {
                        Ok(j) => j,
                        Err(e) => {
                            return IpcResponse::error(
                                "mcp_upstream",
                                "SERIALIZE_ERROR",
                                e.to_string(),
                            );
                        }
                    }
                }) {
                    return IpcResponse::error("mcp_upstream", "CONFIG_STORE_ERROR", e.to_string());
                }
                info!(
                    upstream_id,
                    tool_count = catalog.tools.len(),
                    "MCP upstream catalog reported."
                );
                IpcResponse::success("mcp_upstream_catalog", None)
            }

            IpcRequest::ProvisionMcpUpstreamCredential {
                upstream_id,
                owner_agent_id,
                credential,
            } => {
                use ansible_mesh_core::mcp_upstream::McpUpstreamConfig;

                if !Self::mcp_owner_identity_ok(current_identity, &owner_agent_id) {
                    return IpcResponse::error(
                        "mcp_upstream_credential",
                        "FORBIDDEN",
                        format!(
                            "owner_agent_id '{owner_agent_id}' does not match the registered \
                             guest identity"
                        ),
                    );
                }
                if credential.trim().is_empty() {
                    return IpcResponse::error(
                        "mcp_upstream_credential",
                        "EMPTY_CREDENTIAL",
                        "credential must be non-empty",
                    );
                }

                let mut upstreams: std::collections::HashMap<String, McpUpstreamConfig> = graph
                    .get_config_value("__mcp_upstreams__")
                    .ok()
                    .flatten()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                let Some(config_snapshot) = upstreams.get(&upstream_id).cloned() else {
                    return IpcResponse::error(
                        "mcp_upstream_credential",
                        "NOT_FOUND",
                        format!("no upstream registered as {upstream_id}"),
                    );
                };
                if config_snapshot.owner_agent_id != owner_agent_id {
                    return IpcResponse::error(
                        "mcp_upstream_credential",
                        "FORBIDDEN",
                        format!("upstream {upstream_id} is not owned by {owner_agent_id}"),
                    );
                }

                let binding_id = format!("mcp:{upstream_id}");
                let integration_bindings: std::collections::HashMap<
                    String,
                    ansible_mesh_core::integration::IntegrationBinding,
                > = graph
                    .get_config_value("__integration_bindings__")
                    .ok()
                    .flatten()
                    .and_then(|value| serde_json::from_str(&value).ok())
                    .unwrap_or_default();
                let (execution_node_id, placement) = if let Some(binding) =
                    integration_bindings.get(&binding_id).cloned()
                {
                    let entry =
                        Self::integration_binding_entry(binding, registry, graph, local_node_id)
                            .await;
                    (entry.execution_node_id, entry.placement)
                } else {
                    (
                        Some(local_node_id.to_string()),
                        ansible_mesh_core::integration::EgressPlacementDecision::ExecuteLocal {
                            audit_fallback: false,
                        },
                    )
                };
                let Some(execution_node_id) = execution_node_id else {
                    return IpcResponse::error(
                        "mcp_upstream_credential",
                        "PLACEMENT_DENIED",
                        match placement {
                            ansible_mesh_core::integration::EgressPlacementDecision::Deny {
                                reason,
                            } => reason,
                            _ => "MCP transport has no execution node".into(),
                        },
                    );
                };

                let (vault_ref, rotated) = if execution_node_id == local_node_id {
                    match config_snapshot.credential_ref.clone() {
                        Some(existing_ref)
                            if graph.get_secret(&existing_ref).ok().flatten().is_some() =>
                        {
                            if let Err(error) =
                                crate::vault::rotate_secret(graph, &existing_ref, &credential)
                            {
                                return IpcResponse::error(
                                    "mcp_upstream_credential",
                                    "VAULT_ERROR",
                                    error.to_string(),
                                );
                            }
                            (existing_ref, true)
                        }
                        _ => match store_secret(
                            graph,
                            SecretInput {
                                secret_kind: "mcp_upstream_credential".into(),
                                scope: "hotel".into(),
                                allowed_roles: vec![
                                    "egress-http-runner".into(),
                                    "mcp-client-runner".into(),
                                ],
                                allowed_guests: Vec::new(),
                                plaintext: credential,
                            },
                        ) {
                            Ok(secret_ref) => (secret_ref, false),
                            Err(error) => {
                                return IpcResponse::error(
                                    "mcp_upstream_credential",
                                    "VAULT_ERROR",
                                    error.to_string(),
                                );
                            }
                        },
                    }
                } else {
                    let remote_request = match config_snapshot.credential_ref.clone() {
                        Some(secret_ref) => IpcRequest::RotateOperatorTargetSecret {
                            target_node_id: execution_node_id.clone(),
                            secret_ref,
                            plaintext: credential.clone(),
                        },
                        None => IpcRequest::AddOperatorTargetVaultEntry {
                            target_node_id: execution_node_id.clone(),
                            vault_name: format!("mcp/{upstream_id}"),
                            plaintext: credential.clone(),
                            allowed_roles: vec!["egress-http-runner".into()],
                        },
                    };
                    let mut response = Self::handle_operator_target_request(
                        remote_request,
                        registry,
                        graph,
                        materialization_requester,
                        local_node_id,
                    )
                    .await;
                    // A placement move leaves the old hotel's ref behind. If
                    // rotating that ref on the new exit fails, create a new
                    // execution-hotel secret instead of copying old material.
                    if !matches!(
                        response,
                        IpcResponse::OperatorTargetSecretMutationAckView {
                            ref operator_target_secret_mutation
                        } if operator_target_secret_mutation.ok
                    ) && config_snapshot.credential_ref.is_some()
                    {
                        response = Self::handle_operator_target_request(
                            IpcRequest::AddOperatorTargetVaultEntry {
                                target_node_id: execution_node_id.clone(),
                                vault_name: format!("mcp/{upstream_id}"),
                                plaintext: credential,
                                allowed_roles: vec!["egress-http-runner".into()],
                            },
                            registry,
                            graph,
                            materialization_requester,
                            local_node_id,
                        )
                        .await;
                    }
                    match response {
                        IpcResponse::OperatorTargetSecretMutationAckView {
                            operator_target_secret_mutation,
                        } if operator_target_secret_mutation.ok => {
                            let Some(secret_ref) = operator_target_secret_mutation.secret_ref
                            else {
                                return IpcResponse::error(
                                    "mcp_upstream_credential",
                                    "REMOTE_VAULT_ERROR",
                                    "remote vault mutation returned no secret_ref",
                                );
                            };
                            (
                                secret_ref,
                                operator_target_secret_mutation.operation == "rotate",
                            )
                        }
                        other => {
                            return IpcResponse::error(
                                "mcp_upstream_credential",
                                "REMOTE_VAULT_ERROR",
                                format!("remote vault mutation failed: {other:?}"),
                            );
                        }
                    }
                };
                let config = upstreams
                    .get_mut(&upstream_id)
                    .expect("config snapshot came from this registry");
                config.credential_ref = Some(vault_ref.clone());
                config.updated_at = unix_ts();
                let config_snapshot = config.clone();

                match serde_json::to_string(&upstreams) {
                    Ok(json) => {
                        if let Err(e) = graph.set_config_value("__mcp_upstreams__", &json) {
                            return IpcResponse::error(
                                "mcp_upstream_credential",
                                "CONFIG_STORE_ERROR",
                                e.to_string(),
                            );
                        }
                    }
                    Err(e) => {
                        return IpcResponse::error(
                            "mcp_upstream_credential",
                            "SERIALIZE_ERROR",
                            e.to_string(),
                        );
                    }
                }

                // Fan out so the MCP manager reconnects; the actual secret is
                // resolved later by egress-http-runner at the execution hotel.
                let task_json = serde_json::json!({
                    "action": "update_mcp_upstream",
                    "config": config_snapshot,
                })
                .to_string();
                Self::deliver_inbound_task(
                    inboxes,
                    local_node_id,
                    "mcp-client-runner",
                    None,
                    Uuid::new_v4(),
                    task_json,
                )
                .await;

                info!(
                    upstream_id,
                    execution_node_id,
                    rotated,
                    "MCP upstream credential provisioned at transport execution hotel"
                );
                IpcResponse::success(
                    "mcp_upstream_credential",
                    Some(serde_json::json!({
                        "upstream_id": upstream_id,
                        "vault_ref": vault_ref,
                        "rotated": rotated,
                        "execution_node_id": execution_node_id,
                    })),
                )
            }

            // ── User Task Engine ──────────────────────────────────────────────
            IpcRequest::CreateUserTask {
                task_id,
                session_id,
                agent_id,
                chat_id,
                goal,
                approved_risk_ceiling,
                planning_model_tier,
                quiet,
            } => {
                let now = unix_ts();
                let task_data = serde_json::json!({
                    "task_id": task_id,
                    "session_id": session_id,
                    "agent_id": agent_id,
                    "chat_id": chat_id,
                    "goal": goal,
                    "steps": [],
                    "status": "planning",
                    "approved_risk_ceiling": approved_risk_ceiling,
                    "planning_model_tier": planning_model_tier,
                    "quiet": quiet,
                    "created_at": now,
                    "updated_at": now,
                    "completed_at": null,
                    "next_step_idx": 0,
                    "approval_note": null,
                });
                match graph.upsert_user_task(task_data, &task_id) {
                    Ok(_) => IpcResponse::UserTaskCreated {
                        user_task_id: task_id,
                    },
                    Err(e) => {
                        IpcResponse::error("create_user_task", "STORAGE_ERROR", format!("{e}"))
                    }
                }
            }

            IpcRequest::UpdateUserTask {
                task_id,
                status,
                steps_json,
                next_step_idx,
                approval_note,
            } => match graph.get_user_task(&task_id) {
                Ok(Some(mut data)) => {
                    data["status"] = serde_json::Value::String(status);
                    data["updated_at"] = serde_json::json!(unix_ts());
                    if let Some(steps) = steps_json {
                        data["steps"] = serde_json::from_str(&steps)
                            .unwrap_or(serde_json::Value::Array(vec![]));
                    }
                    if let Some(idx) = next_step_idx {
                        data["next_step_idx"] = serde_json::json!(idx);
                    }
                    if let Some(note) = approval_note {
                        data["approval_note"] = serde_json::Value::String(note);
                    }
                    match graph.upsert_user_task(data, &task_id) {
                        Ok(_) => IpcResponse::UserTaskUpdated {
                            user_task_id: task_id,
                            user_task_updated: true,
                        },
                        Err(e) => {
                            IpcResponse::error("update_user_task", "STORAGE_ERROR", format!("{e}"))
                        }
                    }
                }
                Ok(None) => IpcResponse::error(
                    "update_user_task",
                    "NOT_FOUND",
                    format!("user task {task_id} not found"),
                ),
                Err(e) => IpcResponse::error("update_user_task", "STORAGE_ERROR", format!("{e}")),
            },

            IpcRequest::UpdateUserTaskStep {
                task_id,
                step_idx,
                status,
                output,
                error,
            } => match graph.get_user_task(&task_id) {
                Ok(Some(mut data)) => {
                    if let Some(steps) = data["steps"].as_array_mut() {
                        if let Some(step) = steps.get_mut(step_idx) {
                            step["status"] = serde_json::Value::String(status);
                            if let Some(out) = output {
                                step["output"] = serde_json::Value::String(out);
                            }
                            if let Some(err) = error {
                                step["error"] = serde_json::Value::String(err);
                            }
                        }
                    }
                    data["updated_at"] = serde_json::json!(unix_ts());
                    match graph.upsert_user_task(data, &task_id) {
                        Ok(_) => IpcResponse::UserTaskUpdated {
                            user_task_id: task_id,
                            user_task_updated: true,
                        },
                        Err(e) => IpcResponse::error(
                            "update_user_task_step",
                            "STORAGE_ERROR",
                            format!("{e}"),
                        ),
                    }
                }
                Ok(None) => IpcResponse::error(
                    "update_user_task_step",
                    "NOT_FOUND",
                    format!("user task {task_id} not found"),
                ),
                Err(e) => {
                    IpcResponse::error("update_user_task_step", "STORAGE_ERROR", format!("{e}"))
                }
            },

            IpcRequest::GetUserTask { task_id } => match graph.get_user_task(&task_id) {
                Ok(Some(data)) => IpcResponse::UserTaskData {
                    user_task_json: data.to_string(),
                },
                Ok(None) => IpcResponse::error(
                    "get_user_task",
                    "NOT_FOUND",
                    format!("user task {task_id} not found"),
                ),
                Err(e) => IpcResponse::error("get_user_task", "STORAGE_ERROR", format!("{e}")),
            },

            IpcRequest::ListUserTasks {
                session_id,
                agent_id,
            } => match graph.list_user_tasks(session_id.as_deref(), agent_id.as_deref()) {
                Ok(tasks) => IpcResponse::UserTaskList { user_tasks: tasks },
                Err(e) => IpcResponse::error("list_user_tasks", "STORAGE_ERROR", format!("{e}")),
            },

            IpcRequest::PushHealEntry { guest_id, raw_text } => match heal_queue.as_deref() {
                Some(hq) => match hq.push_error(&guest_id, &raw_text) {
                    Ok(id) => IpcResponse::HealEntryPushed { id },
                    Err(e) => {
                        IpcResponse::error("push_heal_entry", "STORAGE_ERROR", format!("{e}"))
                    }
                },
                None => IpcResponse::error(
                    "push_heal_entry",
                    "UNAVAILABLE",
                    "heal_queue not configured".to_string(),
                ),
            },

            IpcRequest::PushHealEvent {
                guest_id,
                severity,
                pattern_tag,
                detail,
            } => Self::handle_push_heal_event(
                heal_queue,
                &guest_id,
                &severity,
                &pattern_tag,
                &detail,
            ),

            IpcRequest::GetHealQueuePending { limit } => match heal_queue.as_deref() {
                Some(hq) => match hq.pending_errors(limit) {
                    Ok(rows) => IpcResponse::HealQueuePending { rows },
                    Err(e) => IpcResponse::error(
                        "get_heal_queue_pending",
                        "STORAGE_ERROR",
                        format!("{e}"),
                    ),
                },
                None => IpcResponse::HealQueuePending { rows: vec![] },
            },

            IpcRequest::TriageHealEntry {
                id,
                severity,
                pattern_tag,
                heal_action,
            } => match heal_queue.as_deref() {
                Some(hq) => match hq.update_triage(&id, &severity, &pattern_tag, &heal_action) {
                    Ok(()) => IpcResponse::success("triage_heal_entry", None),
                    Err(e) => {
                        IpcResponse::error("triage_heal_entry", "STORAGE_ERROR", format!("{e}"))
                    }
                },
                None => IpcResponse::error(
                    "triage_heal_entry",
                    "UNAVAILABLE",
                    "heal_queue not configured".to_string(),
                ),
            },

            IpcRequest::ResolveHealEntry { id, outcome } => {
                // Agent-originated calls (heal.resolve steward tool) require
                // operational admin authority; the heal-dispatcher and CLI
                // paths pass through unchanged.
                if let Err(refusal) =
                    steward_agent_admin_gate(graph, current_identity.as_ref(), "resolve_heal_entry")
                {
                    return refusal;
                }
                match heal_queue.as_deref() {
                    Some(hq) => match hq.resolve(&id, &outcome) {
                        Ok(()) => IpcResponse::success("resolve_heal_entry", None),
                        Err(e) => IpcResponse::error(
                            "resolve_heal_entry",
                            "STORAGE_ERROR",
                            format!("{e}"),
                        ),
                    },
                    None => IpcResponse::error(
                        "resolve_heal_entry",
                        "UNAVAILABLE",
                        "heal_queue not configured".to_string(),
                    ),
                }
            }

            IpcRequest::FileHealWorkItem {
                pattern_tag,
                guest_id,
                occurrence_count,
                window_secs,
                evidence_lines,
            } => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                Self::handle_file_heal_work_item(
                    graph,
                    heal_queue.as_deref(),
                    &pattern_tag,
                    &guest_id,
                    occurrence_count,
                    window_secs,
                    &evidence_lines,
                    now,
                    &|key| std::env::var(key).ok(),
                )
            }

            IpcRequest::CloseHealWorkItem { work_item_id } => {
                // Agent-originated calls (heal.close_work_item steward tool)
                // require operational admin authority; the autonomy-lane loop
                // and `phil heal close` CLI paths pass through unchanged.
                if let Err(refusal) = steward_agent_admin_gate(
                    graph,
                    current_identity.as_ref(),
                    "close_heal_work_item",
                ) {
                    return refusal;
                }
                // Closure path for a filed heal work item (finding F8). Wired
                // straight to the unit-tested domain method; closing a missing
                // id returns closed=false, and closing an already-closed item
                // returns closed=true (idempotent) so the autonomy-lane loop can
                // retry safely.
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                match graph.close_heal_work_item(&work_item_id, now) {
                    Ok(closed) => IpcResponse::success(
                        "close_heal_work_item",
                        Some(serde_json::json!({
                            "closed": closed,
                            "work_item_id": work_item_id,
                        })),
                    ),
                    Err(e) => IpcResponse::error(
                        "close_heal_work_item",
                        "STORAGE_ERROR",
                        format!("{e:#}"),
                    ),
                }
            }

            IpcRequest::ConsumeAutonomyAction {
                lane,
                action_summary,
                evidence,
                reversal_hint,
                filing,
            } => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                Self::handle_consume_autonomy_action_ext(
                    graph,
                    &lane,
                    &action_summary,
                    &evidence,
                    &reversal_hint,
                    filing,
                    now,
                    &|key| std::env::var(key).ok(),
                )
            }

            IpcRequest::RecordAutonomyOutcome { audit_id, outcome } => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                Self::handle_record_autonomy_outcome(graph, &audit_id, &outcome, now)
            }

            IpcRequest::QueryModelRoute {
                request_class,
                needs_tools,
                needs_structured,
                approx_context_tokens,
                latency_class,
                trust_ceiling,
                exclude_providers,
            } => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                Self::handle_query_model_route(
                    graph,
                    heal_queue.as_deref(),
                    local_node_id,
                    &request_class,
                    needs_tools,
                    needs_structured,
                    approx_context_tokens,
                    &latency_class,
                    &trust_ceiling,
                    &exclude_providers,
                    now,
                )
            }

            IpcRequest::AgentMigrateToHotel {
                agent_id,
                dest_hotel,
            } => {
                match Self::handle_agent_migrate_to_hotel(
                    &registry,
                    graph,
                    local_node_id,
                    socket_path,
                    &agent_id,
                    &dest_hotel,
                )
                .await
                {
                    Ok(()) => IpcResponse::success("agent_migrate_to_hotel", None),
                    Err(e) => IpcResponse::error(
                        "agent_migrate_to_hotel",
                        "MIGRATION_ERROR",
                        format!("{e:#}"),
                    ),
                }
            }

            IpcRequest::ApplyAgentBundle { bundle_json } => {
                match Self::handle_apply_agent_bundle(
                    graph,
                    &local_node_id,
                    materialization_requester.as_deref(),
                    &bundle_json,
                )
                .await
                {
                    Ok(_agent_id) => IpcResponse::success("apply_agent_bundle", None),
                    Err(e) => {
                        IpcResponse::error("apply_agent_bundle", "APPLY_ERROR", format!("{e:#}"))
                    }
                }
            }
        }
    }

    /// Handle [`IpcRequest::QueryModelRoute`] — the routing oracle's IPC face.
    ///
    /// Ranks the local hotel's live model profiles against the caller's need
    /// (pure `model_oracle::rank_models_with`), maps providers onto controller
    /// roles, and keeps only roles backed by a live guest — the same
    /// reachability rule `validate_fallback_ladders` enforces at config time.
    /// When the query carries `exclude_providers` (a failure-driven reroute),
    /// the switch is logged and pushed to the heal queue as an
    /// `oracle_reroute` info entry so reroute patterns surface in dev briefs.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn handle_query_model_route(
        graph: &GraphDomain,
        heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
        local_node_id: &str,
        request_class: &str,
        needs_tools: bool,
        needs_structured: bool,
        approx_context_tokens: u32,
        latency_class: &str,
        trust_ceiling: &str,
        exclude_providers: &[String],
        now_secs: u64,
    ) -> IpcResponse {
        use ansible_mesh_core::model_oracle as oracle;

        const CORR: &str = "query_model_route";

        if oracle::routing_oracle_disabled() {
            return IpcResponse::success(
                CORR,
                Some(serde_json::json!({ "ranked": [], "disabled": true })),
            );
        }

        let profiles = match graph.list_model_profiles() {
            Ok(p) => p,
            Err(e) => return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}")),
        };
        // Prefer profiles observed on this node; fall back to the full mesh
        // view when the local node has no profiles yet (fresh hotel).
        let local: Vec<_> = profiles
            .iter()
            .filter(|p| p.node_id == local_node_id)
            .cloned()
            .collect();
        let candidates = if local.is_empty() { profiles } else { local };

        let need = oracle::RouteNeed {
            request_class: request_class.to_string(),
            needs_tools,
            needs_structured,
            approx_context_tokens,
            latency_class: oracle::LatencyClass::parse(latency_class),
            trust_ceiling: trust_ceiling.to_string(),
        };
        let ranked = oracle::rank_models_with(
            &candidates,
            &need,
            now_secs,
            oracle::degrade_cooloff_secs_from_env(),
        );

        // A tier is reachable iff a live guest serves its role — same rule as
        // validate_fallback_ladders.
        let active_roles: std::collections::BTreeSet<String> =
            Self::local_hotel_name(graph, local_node_id)
                .and_then(|hotel| graph.list_guests(&hotel, true).ok())
                .map(|guests| guests.into_iter().map(|g| g.role).collect())
                .unwrap_or_default();

        let mut seen_roles: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let entries: Vec<serde_json::Value> = ranked
            .iter()
            .filter(|r| !exclude_providers.iter().any(|x| x == &r.provider))
            .filter_map(|r| {
                let role = oracle::controller_role_for_provider(&r.provider);
                if !active_roles.contains(&role) || !seen_roles.insert(role.clone()) {
                    return None;
                }
                Some(serde_json::json!({
                    "role": role,
                    "provider": r.provider,
                    "model_ref": r.model_ref,
                    "score": r.score,
                    "reasons": r.reasons,
                }))
            })
            .take(3)
            .collect();

        // Observability: a non-empty exclude list means a provider just
        // failed and the oracle is rerouting around it.
        if !exclude_providers.is_empty() {
            if let Some(first) = entries.first() {
                let provider_from = exclude_providers.join(",");
                let provider_to = first
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                info!(
                    provider_from = %provider_from,
                    provider_to = %provider_to,
                    reason = "fallback_ladder_exhausted",
                    "Routing oracle reroute"
                );
                if let Some(hq) = heal_queue {
                    let raw_text = format!(
                        "[model-oracle] reroute {provider_from} -> {provider_to} (fallback ladder exhausted, request_class={request_class})"
                    );
                    match hq.push_error("model-oracle", &raw_text) {
                        Ok(id) => {
                            if let Err(e) =
                                hq.update_triage(&id, "info", "oracle_reroute", "oracle_reroute")
                            {
                                warn!("oracle_reroute heal entry triage failed: {e}");
                            }
                        }
                        Err(e) => warn!("oracle_reroute heal entry push failed: {e}"),
                    }
                }
            }
        }

        IpcResponse::success(
            CORR,
            Some(serde_json::json!({ "ranked": entries, "disabled": false })),
        )
    }

    /// Turn-failure heal intake (self-heal): classify a FailTask's
    /// `error_code`/`reason` and, when it carries provider/model failure
    /// markers (`kind=provider_failure | component=model-router | provider=X`
    /// or a bare `MODEL_EMPTY_RESPONSE`), push a pre-triaged heal-queue entry
    /// so the heal-dispatcher and A3 recurrence counter see turn-level
    /// failures. Best-effort: never affects the FailTask response.
    ///
    /// Entry shape:
    /// - `guest_id`: `model-controller-{provider}` when the provider marker is
    ///   present (the model-controller guest naming convention), else
    ///   `turn:{caller_guest_id}`.
    /// - `severity`/`pattern_tag`: from the shared classifier
    ///   (`provider_4xx:{provider}`, `provider_timeout:{provider}`,
    ///   `model_empty_response`, …) so the dispatcher aggregates without
    ///   re-classifying.
    /// - `raw_text`: `[{error_code}] {reason}` capped to 2 KB.
    ///
    /// Flood control lives in `push_classified`: the same
    /// `(guest_id, pattern_tag)` within the flood window collapses.
    pub(crate) fn push_turn_failure_heal_entry(
        heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
        caller_guest_id: Option<&str>,
        error_code: &str,
        reason: &str,
    ) {
        use ansible_mesh_core::heal_queue::{cap_turn_failure_text, classify_turn_failure};

        let Some(hq) = heal_queue else {
            return;
        };
        // Classify on the full line (markers may sit at the end), then cap
        // what gets stored.
        let full_line = format!("[{error_code}] {reason}");
        let Some(class) = classify_turn_failure(&full_line) else {
            return;
        };
        let line = cap_turn_failure_text(&full_line);
        let guest_id = match class.provider.as_deref() {
            Some(provider) => format!("model-controller-{provider}"),
            None => format!("turn:{}", caller_guest_id.unwrap_or("unknown")),
        };
        match hq.push_classified(&guest_id, &line, &class.severity, &class.pattern_tag) {
            Ok(Some(id)) => info!(
                id = %id,
                guest_id = %guest_id,
                pattern_tag = %class.pattern_tag,
                "turn failure pushed to heal queue"
            ),
            Ok(None) => debug!(
                guest_id = %guest_id,
                pattern_tag = %class.pattern_tag,
                "turn failure collapsed into recent heal entry (flood window)"
            ),
            Err(e) => warn!("turn failure heal push failed: {e}"),
        }
    }

    /// Handle [`IpcRequest::PushHealEvent`] — a guest-reported, pre-classified
    /// turn-level failure (philote watchdog evictions, fallback-ladder
    /// exhaustion, paracrine budget breaches). Stored pre-triaged; flood
    /// control collapses the same `(guest_id, pattern_tag)` within the window.
    pub(crate) fn handle_push_heal_event(
        heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
        guest_id: &str,
        severity: &str,
        pattern_tag: &str,
        detail: &str,
    ) -> IpcResponse {
        use ansible_mesh_core::heal_queue::cap_turn_failure_text;

        const CORR: &str = "push_heal_event";
        let Some(hq) = heal_queue else {
            return IpcResponse::error(
                CORR,
                "UNAVAILABLE",
                "heal_queue not configured".to_string(),
            );
        };
        let detail = cap_turn_failure_text(detail);
        match hq.push_classified(guest_id, &detail, severity, pattern_tag) {
            Ok(Some(id)) => {
                info!(
                    id = %id,
                    guest_id = %guest_id,
                    pattern_tag = %pattern_tag,
                    "guest heal event pushed to heal queue"
                );
                IpcResponse::success(
                    CORR,
                    Some(serde_json::json!({ "collapsed": false, "id": id })),
                )
            }
            Ok(None) => IpcResponse::success(CORR, Some(serde_json::json!({ "collapsed": true }))),
            Err(e) => IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e}")),
        }
    }

    pub(crate) fn handle_file_heal_work_item(
        graph: &GraphDomain,
        heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
        pattern_tag: &str,
        guest_id: &str,
        occurrence_count: u32,
        window_secs: u64,
        evidence_lines: &[String],
        now: u64,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> IpcResponse {
        use ansible_mesh_core::autonomy::{
            AutonomyAuditRecord, AutonomyLane, LANE_FLEET_HEAL_SLICES, lane_enabled,
            try_consume_daily_action,
        };
        use ansible_mesh_core::heal_queue::{
            HEAL_WORK_ITEM_STATUS_OPEN, HealWorkItemRecord, cap_evidence_lines,
        };
        use ansible_mesh_core::provenance::{ProvenanceEnvelope, TrustTier};

        const CORR: &str = "file_heal_work_item";
        let lane = AutonomyLane::new(LANE_FLEET_HEAL_SLICES);

        // 1. Kill switch overrides everything, always (Autonomy Contract rule 3).
        if !lane_enabled(&lane, env) {
            debug!(
                pattern_tag,
                guest_id, "heal work item filing skipped: lane kill switch set"
            );
            return IpcResponse::success(
                CORR,
                Some(serde_json::json!({
                    "filed": false, "deduped": false, "reason": "lane_disabled"
                })),
            );
        }

        // 2. Dedup: one OPEN work item per (pattern_tag, guest_id). A re-breach
        // while open bumps count + last_seen — no budget consumed, no new audit.
        match graph.find_open_heal_work_item(pattern_tag, guest_id) {
            Ok(Some(mut item)) => {
                item.count = item.count.saturating_add(occurrence_count);
                item.last_seen = now;
                if let Err(e) = graph.upsert_heal_work_item(&item) {
                    return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}"));
                }
                info!(
                    pattern_tag,
                    guest_id,
                    work_item_id = %item.work_item_id,
                    count = item.count,
                    "heal work item re-breach: bumped open item"
                );
                return IpcResponse::success(
                    CORR,
                    Some(serde_json::json!({
                        "filed": false, "deduped": true,
                        "work_item_id": item.work_item_id,
                    })),
                );
            }
            Ok(None) => {}
            Err(e) => return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}")),
        }

        // 3. Grant + budget: frozen lanes and exhausted daily budgets refuse.
        let mut grant = match graph.get_or_create_autonomy_grant(LANE_FLEET_HEAL_SLICES, now) {
            Ok(grant) => grant,
            Err(e) => return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}")),
        };
        if !try_consume_daily_action(&mut grant, now) {
            let reason = if grant.frozen_until_operator_review {
                "lane_frozen"
            } else {
                "daily_budget_exhausted"
            };
            debug!(
                pattern_tag,
                guest_id, reason, "heal work item filing refused by autonomy grant"
            );
            return IpcResponse::success(
                CORR,
                Some(serde_json::json!({
                    "filed": false, "deduped": false, "reason": reason
                })),
            );
        }
        if let Err(e) = graph.upsert_autonomy_grant(&grant) {
            return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}"));
        }

        // 4. File: audit record first (the ledger), then the work item node.
        let evidence = cap_evidence_lines(evidence_lines);
        let work_item_id = Uuid::new_v4().to_string();
        let audit_id = format!("heal_filing:{work_item_id}");
        // Memory Transparency Slice M1: component-authored provenance for
        // the A3 heal filing — evidence pointers are the same evidence
        // lines already captured on the work item, so no new plumbing.
        let provenance = ProvenanceEnvelope::from_component("heal-dispatcher")
            .with_source(pattern_tag)
            .with_trust(TrustTier::Observed)
            .with_evidence(evidence.clone())
            .with_reversal(format!(
                "close_heal_work_item({work_item_id}) via GraphDomain::close_heal_work_item"
            ));
        let audit = AutonomyAuditRecord::new(
            audit_id.clone(),
            lane,
            format!(
                "filed heal work item {work_item_id}: pattern '{pattern_tag}' on guest \
                 '{guest_id}' recurred {occurrence_count}x within {window_secs}s"
            ),
            &evidence.join("\n"),
            "close the work item (GraphDomain::close_heal_work_item)",
            grant.posture,
            now,
        )
        .with_provenance(provenance);
        if let Err(e) = graph.record_autonomy_audit(&audit) {
            return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}"));
        }
        let item = HealWorkItemRecord {
            work_item_id: work_item_id.clone(),
            pattern_tag: pattern_tag.to_string(),
            guest_id: guest_id.to_string(),
            count: occurrence_count,
            window_secs,
            evidence,
            status: HEAL_WORK_ITEM_STATUS_OPEN.to_string(),
            filed_by: "heal-dispatcher".to_string(),
            audit_id: Some(audit_id.clone()),
            created_at: now,
            last_seen: now,
        };
        if let Err(e) = graph.upsert_heal_work_item(&item) {
            return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}"));
        }

        // 5. Operator visibility: one resolved info entry in the heal queue so
        // the filing surfaces in existing monitoring. Best-effort — the graph
        // nodes above are the durable record.
        if let Some(hq) = heal_queue {
            match hq.push_error(
                guest_id,
                &format!(
                    "work_item_filed: recurring pattern '{pattern_tag}' on {guest_id} \
                     ({occurrence_count}x/{window_secs}s) -> heal_work_item {work_item_id}"
                ),
            ) {
                Ok(entry_id) => {
                    if let Err(e) =
                        hq.update_triage(&entry_id, "info", pattern_tag, "work_item_filed")
                    {
                        warn!("heal work item info entry triage failed: {e:#}");
                    }
                    if let Err(e) = hq.resolve(&entry_id, "work_item_filed") {
                        warn!("heal work item info entry resolve failed: {e:#}");
                    }
                }
                Err(e) => warn!("heal work item info entry push failed: {e:#}"),
            }

            // 6. A9 Piece 3: an UNRESOLVED, throttled pending-outcome
            // notice — deliberately distinct from the `work_item_filed`
            // entry above, which is immediately `.resolve()`d and would
            // make a poor "still awaiting review" breadcrumb. Best-effort;
            // the audit record above is the durable one. Throttled per
            // (lane, pattern_tag) by `push_classified`'s own flood window.
            let notice = ansible_mesh_core::autonomy::pending_outcome_notice(
                &audit_id,
                LANE_FLEET_HEAL_SLICES,
                &audit.action_summary,
            );
            match hq.push_classified(
                LANE_FLEET_HEAL_SLICES,
                &notice,
                "info",
                "autonomy_outcome_pending",
            ) {
                Ok(Some(id)) => info!(
                    id,
                    audit_id = %audit_id,
                    "heal work item filing: pending-outcome notice pushed to heal queue"
                ),
                Ok(None) => debug!(
                    audit_id = %audit_id,
                    "heal work item filing: pending-outcome notice collapsed (flood window)"
                ),
                Err(e) => warn!("heal work item filing: pending-outcome notice push failed: {e:#}"),
            }
        }

        info!(
            pattern_tag,
            guest_id,
            work_item_id = %work_item_id,
            occurrence_count,
            window_secs,
            "heal work item filed via fleet.heal_slices lane"
        );
        IpcResponse::success(
            CORR,
            Some(serde_json::json!({
                "filed": true, "deduped": false,
                "work_item_id": work_item_id,
                "audit_id": audit_id,
            })),
        )
    }

    // ── Autonomy lane consult (Autopoiesis Slice A2) ──────────────────────────

    /// Handle [`IpcRequest::ConsumeAutonomyAction`] — a guest asking to take
    /// one autonomous action on `lane` (first consumer: the life-graph
    /// runner's feedback-to-action loop, lane `graph.bridge_edges`).
    ///
    /// Pipeline mirrors A3's `handle_file_heal_work_item`: lane kill switch →
    /// grant posture → daily budget (`try_consume_daily_action`, which also
    /// enforces the freeze flag) → Pending `autonomy_audit` record.
    ///
    /// Decision table (`data` in the Standard response):
    /// - kill switch set → `{allowed:false, reason:"lane_disabled"}`
    /// - posture ProposalOnly → `{allowed:false, posture:"proposal_only",
    ///   reason:"posture_proposal_only"}` — no budget, no audit; the caller
    ///   stays prose-only. This is every fresh lane's day-one answer.
    /// - frozen / budget exhausted → `{allowed:false, reason:...}`
    /// - posture ConfirmFirst → `{allowed:false, posture:"confirm_first",
    ///   audit_id}` — the caller files a ready-to-apply spec awaiting
    ///   operator confirmation.
    /// - posture AutoWithAudit → `{allowed:true, posture:"auto_with_audit",
    ///   audit_id}` — the caller acts now; the audit record is the ledger.
    ///
    /// Clock (`now`) and env reader are injected so tests run without
    /// wall-clock time or process environment.
    #[cfg(test)]
    pub(crate) fn handle_consume_autonomy_action(
        graph: &GraphDomain,
        lane: &str,
        action_summary: &str,
        evidence: &str,
        reversal_hint: &str,
        now: u64,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> IpcResponse {
        Self::handle_consume_autonomy_action_ext(
            graph,
            lane,
            action_summary,
            evidence,
            reversal_hint,
            false,
            now,
            env,
        )
    }

    /// [`Self::handle_consume_autonomy_action`] with the `filing` flag.
    ///
    /// A filing (a Draft skill, a proposal record) is what `ProposalOnly`
    /// *means* a lane may do, so `filing = true` is permitted at every
    /// posture — still kill-switch-gated, still budgeted, still audited
    /// `Pending` so the operator's outcome stamp trains the lane. `filing =
    /// false` keeps the original decision table (ProposalOnly refuses).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn handle_consume_autonomy_action_ext(
        graph: &GraphDomain,
        lane: &str,
        action_summary: &str,
        evidence: &str,
        reversal_hint: &str,
        filing: bool,
        now: u64,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> IpcResponse {
        use ansible_mesh_core::autonomy::{
            AutonomyAuditRecord, AutonomyLane, AutonomyPosture, lane_enabled,
            try_consume_daily_action,
        };

        const CORR: &str = "consume_autonomy_action";
        if lane.trim().is_empty() {
            return IpcResponse::error(CORR, "INVALID_LANE", "lane must not be empty");
        }
        let lane_id = AutonomyLane::new(lane);

        // 1. Kill switch overrides everything, always (Autonomy Contract rule 3).
        if !lane_enabled(&lane_id, env) {
            debug!(lane, "autonomy action refused: lane kill switch set");
            return IpcResponse::success(
                CORR,
                Some(serde_json::json!({
                    "allowed": false, "reason": "lane_disabled"
                })),
            );
        }

        // 2. Grant posture. ProposalOnly consumes nothing — the lane has not
        // earned filing actions yet, so the caller stays prose-only.
        let mut grant = match graph.get_or_create_autonomy_grant(lane, now) {
            Ok(grant) => grant,
            Err(e) => return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}")),
        };
        if grant.posture == AutonomyPosture::ProposalOnly && !filing {
            debug!(lane, "autonomy action refused: posture proposal_only");
            return IpcResponse::success(
                CORR,
                Some(serde_json::json!({
                    "allowed": false,
                    "posture": "proposal_only",
                    "reason": "posture_proposal_only",
                })),
            );
        }

        // 3. Budget: frozen lanes and exhausted daily budgets refuse.
        if !try_consume_daily_action(&mut grant, now) {
            let reason = if grant.frozen_until_operator_review {
                "lane_frozen"
            } else {
                "daily_budget_exhausted"
            };
            debug!(lane, reason, "autonomy action refused by grant");
            return IpcResponse::success(
                CORR,
                Some(serde_json::json!({
                    "allowed": false,
                    "posture": posture_str(grant.posture),
                    "reason": reason,
                })),
            );
        }
        if let Err(e) = graph.upsert_autonomy_grant(&grant) {
            return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}"));
        }

        // 4. Audit record first (the ledger) — Pending until the operator
        // confirms or reverses via RecordAutonomyOutcome.
        let audit_id = format!("autonomy:{}:{}", lane, Uuid::new_v4());
        let audit = AutonomyAuditRecord::new(
            audit_id.clone(),
            lane_id,
            action_summary,
            evidence,
            reversal_hint,
            grant.posture,
            now,
        );
        if let Err(e) = graph.record_autonomy_audit(&audit) {
            return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}"));
        }

        let allowed = filing || grant.posture == AutonomyPosture::AutoWithAudit;
        info!(
            lane,
            audit_id = %audit_id,
            posture = posture_str(grant.posture),
            allowed,
            filing,
            "autonomy action consulted"
        );
        IpcResponse::success(
            CORR,
            Some(serde_json::json!({
                "allowed": allowed,
                "posture": posture_str(grant.posture),
                "audit_id": audit_id,
            })),
        )
    }

    /// Handle [`IpcRequest::RecordAutonomyOutcome`] — the operator/steward
    /// reporting the reviewed outcome of an audited autonomous action
    /// (Autopoiesis Slice A9 — `trust-ledger`).
    ///
    /// `outcome`: `"confirmed_good"` → audit `ConfirmedGood` + grant
    /// `Outcome::ConfirmedGood` (counts toward promotion); `"reversed"` →
    /// audit `Reversed` + grant `Outcome::OperatorReversal` (demotes one
    /// posture level); `"neutral"` → audit `Neutral` only — the grant's
    /// earn/demote counters are untouched (a wash, not a signal). Idempotent
    /// per audit id: an already-reviewed audit refuses with
    /// `reason:"already_recorded"` so a double-confirm never double-counts
    /// toward promotion.
    pub(crate) fn handle_record_autonomy_outcome(
        graph: &GraphDomain,
        audit_id: &str,
        outcome: &str,
        now: u64,
    ) -> IpcResponse {
        use ansible_mesh_core::autonomy::{AuditOutcome, Outcome, Transition};

        const CORR: &str = "record_autonomy_outcome";
        let (audit_outcome, grant_outcome): (AuditOutcome, Option<Outcome>) = match outcome {
            "confirmed_good" => (AuditOutcome::ConfirmedGood, Some(Outcome::ConfirmedGood)),
            "reversed" => (AuditOutcome::Reversed, Some(Outcome::OperatorReversal)),
            "neutral" => (AuditOutcome::Neutral, None),
            other => {
                return IpcResponse::error(
                    CORR,
                    "INVALID_OUTCOME",
                    format!(
                        "unknown outcome '{other}' (expected confirmed_good | reversed | neutral)"
                    ),
                );
            }
        };

        let audit = match graph.get_autonomy_audit(audit_id) {
            Ok(Some(audit)) => audit,
            Ok(None) => {
                return IpcResponse::error(
                    CORR,
                    "AUDIT_NOT_FOUND",
                    format!("no autonomy_audit record with id '{audit_id}'"),
                );
            }
            Err(e) => return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}")),
        };
        if audit.outcome != AuditOutcome::Pending {
            return IpcResponse::success(
                CORR,
                Some(serde_json::json!({
                    "recorded": false,
                    "reason": "already_recorded",
                    "lane": audit.lane.as_str(),
                })),
            );
        }

        if let Err(e) = graph.set_autonomy_audit_outcome(audit_id, audit_outcome, now) {
            return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}"));
        }
        // Neutral carries no grant_outcome — it stamps the audit record and
        // stops there (see AuditOutcome::Neutral doc).
        let transition = match grant_outcome {
            Some(grant_outcome) => {
                match graph.record_autonomy_outcome(audit.lane.as_str(), grant_outcome, now) {
                    Ok(transition) => transition,
                    Err(e) => return IpcResponse::error(CORR, "STORAGE_ERROR", format!("{e:#}")),
                }
            }
            None => Transition::NoChange,
        };
        let transition_str = match transition {
            Transition::NoChange => "no_change",
            Transition::Promoted { .. } => "promoted",
            Transition::Demoted { .. } => "demoted",
            Transition::Frozen => "frozen",
        };
        let posture = graph
            .get_autonomy_grant(audit.lane.as_str())
            .ok()
            .flatten()
            .map(|g| posture_str(g.posture));

        info!(
            audit_id,
            lane = audit.lane.as_str(),
            outcome,
            transition = transition_str,
            "autonomy outcome recorded"
        );
        IpcResponse::success(
            CORR,
            Some(serde_json::json!({
                "recorded": true,
                "lane": audit.lane.as_str(),
                "transition": transition_str,
                "posture": posture,
            })),
        )
    }

    /// Handle `GetConfig("__autonomy_status__")` /
    /// `GetConfig("__autonomy_status__:{lane}")` — the per-lane trust-ledger
    /// report `phil autonomy status` reads (Autopoiesis Slice A9). Read-only:
    /// computed straight from the persisted [`AutonomyGrant`](ansible_mesh_core::autonomy::AutonomyGrant)s
    /// via [`ansible_mesh_core::autonomy::lane_status_report`], no new state.
    ///
    /// `lane = None` → JSON array of every granted lane's report (lanes
    /// never consulted have no grant yet and are omitted — there is nothing
    /// to report). `lane = Some(l)` → JSON of that lane's report, or JSON
    /// `null` if lane `l` has no grant yet.
    pub(crate) fn handle_query_autonomy_status(
        graph: &GraphDomain,
        lane: Option<&str>,
        now: u64,
    ) -> IpcResponse {
        use ansible_mesh_core::autonomy::lane_status_report;

        let key = match lane {
            Some(lane) => format!("__autonomy_status__:{lane}"),
            None => "__autonomy_status__".to_string(),
        };
        let value_json = match lane {
            Some(lane) => {
                let report = graph
                    .get_autonomy_grant(lane)
                    .unwrap_or(None)
                    .map(|g| lane_status_report(&g, now));
                serde_json::to_string(&report).ok()
            }
            None => {
                let reports: Vec<_> = graph
                    .list_autonomy_grants()
                    .unwrap_or_default()
                    .iter()
                    .map(|g| lane_status_report(g, now))
                    .collect();
                serde_json::to_string(&reports).ok()
            }
        };
        IpcResponse::ConfigData { key, value_json }
    }

    /// Handle `GetConfig("__autonomy_pending__")` — the A9 outcome-stamping
    /// follow-up slice's `phil autonomy pending` surface: every
    /// `autonomy_audit` record across all lanes still `Pending` an operator
    /// outcome, oldest first. Read-only, computed straight from
    /// [`ansible_mesh_core::domain::GraphDomain::list_all_autonomy_audits`] —
    /// no new state, and no autonomy grant is consulted (a read, not an
    /// action). Each entry carries `audit_id`, `lane`, `action_summary`,
    /// `created_at`, and `age_secs` (as of `now`) so an operator can eyeball
    /// how stale the backlog is before the timeout-to-Neutral sweep
    /// (`crate::autonomy_sweep`) catches up to it.
    pub(crate) fn handle_query_autonomy_pending(graph: &GraphDomain, now: u64) -> IpcResponse {
        use ansible_mesh_core::autonomy::AuditOutcome;

        const KEY: &str = "__autonomy_pending__";
        let records = graph.list_all_autonomy_audits().unwrap_or_default();
        let pending: Vec<_> = records
            .into_iter()
            .filter(|r| r.outcome == AuditOutcome::Pending)
            .map(|r| {
                serde_json::json!({
                    "audit_id": r.audit_id,
                    "lane": r.lane.as_str(),
                    "action_summary": r.action_summary,
                    "created_at": r.created_at,
                    "age_secs": now.saturating_sub(r.created_at),
                })
            })
            .collect();
        let value_json = serde_json::to_string(&pending).ok();
        IpcResponse::ConfigData {
            key: KEY.to_string(),
            value_json,
        }
    }

    // ── Agent migration ───────────────────────────────────────────────────────

    /// Build an `AgentMigrationBundle`, upload it to the local blob store, then
    /// dispatch it to `dest_hotel` via the OperatorSurface cross-hotel relay.
    async fn handle_agent_migrate_to_hotel(
        registry: &Arc<RwLock<NodeRegistry>>,
        graph: &GraphDomain,
        local_node_id: &str,
        socket_path: &str,
        agent_id: &str,
        dest_hotel: &str,
    ) -> anyhow::Result<()> {
        let hotel_name = Self::local_hotel_name(graph, local_node_id)
            .ok_or_else(|| anyhow::anyhow!("local hotel record missing"))?;

        // ── 1. Agent identity ────────────────────────────────────────────────
        let identity = graph
            .get_agent_identity(agent_id)?
            .ok_or_else(|| anyhow::anyhow!("agent_identity for '{}' not found", agent_id))?;

        // ── 2. Derive agent_key from role incarnation guest_id pattern ───────
        // Guest IDs follow: `{hotel_name}:philote-{agent_key}`
        let philote_prefix = format!("{hotel_name}:philote-");
        let agent_key = graph
            .list_role_incarnations(agent_id)?
            .iter()
            .find_map(|r| r.guest_id.strip_prefix(&philote_prefix).map(str::to_string))
            .or_else(|| {
                // Fallback: search all guests
                graph
                    .list_guests(&hotel_name, false)
                    .unwrap_or_default()
                    .into_iter()
                    .find_map(|g| g.guest_id.strip_prefix(&philote_prefix).map(str::to_string))
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "cannot derive agent_key for '{}': no philote guest found on hotel '{}'",
                    agent_id,
                    hotel_name
                )
            })?;

        // ── 3. Apartments ────────────────────────────────────────────────────
        let apartments: Vec<(String, serde_json::Value)> = graph
            .list_apartments(agent_id)?
            .into_iter()
            .filter_map(|memory_type| {
                graph
                    .get_apartment(agent_id, &memory_type)
                    .ok()
                    .flatten()
                    .map(|content| (memory_type, content))
            })
            .collect();

        // ── 4. Role incarnations ─────────────────────────────────────────────
        let role_incarnations = graph.list_role_incarnations(agent_id)?;

        // ── 5. Agent-specific guests ─────────────────────────────────────────
        let philote_guest_id = format!("{hotel_name}:philote-{agent_key}");
        let datasource_guest_id = format!("{hotel_name}:agent-graph-{agent_id}");
        let guests: Vec<GuestExport> = graph
            .list_guests(&hotel_name, false)?
            .into_iter()
            .filter(|g| g.guest_id == philote_guest_id || g.guest_id == datasource_guest_id)
            .map(|g| {
                let suffix = g
                    .guest_id
                    .strip_prefix(&format!("{hotel_name}:"))
                    .unwrap_or(&g.guest_id)
                    .to_string();
                GuestExport {
                    guest_id_suffix: suffix,
                    role: g.role,
                    config_json: g.config_json,
                    is_active: g.is_active,
                }
            })
            .collect();

        // ── 6. Vault entries: never exported (DEF-172) ────────────────────────
        // This bundle is written to the unauthenticated blob store and was
        // applied with an empty role ACL, so a plaintext secret in it was
        // readable by anyone who could reach either. Secrets move only with
        // the relocation ceremony, sealed to the target.
        let vault_entries: Vec<VaultEntryExport> = Vec::new();

        // ── 7. Non-vault agent config entries ────────────────────────────────
        let allowed_users_key = format!("telegram_allowed_users_{agent_key}");
        let mut config_entries: Vec<ConfigEntryExport> = Vec::new();
        if let Ok(Some(value_json)) = graph.get_config_value(&allowed_users_key) {
            config_entries.push(ConfigEntryExport {
                key: allowed_users_key,
                value_json,
            });
        }

        // ── 8. Build bundle ──────────────────────────────────────────────────
        let migration_id = Uuid::new_v4().to_string();
        let bundle = AgentMigrationBundle {
            migration_id: migration_id.clone(),
            source_hotel: hotel_name.clone(),
            agent_id: agent_id.to_string(),
            agent_key: agent_key.clone(),
            agent_identity: identity,
            apartments,
            role_incarnations,
            vault_entries,
            config_entries,
            guests,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };

        // ── 9. Upload bundle to local blob store ─────────────────────────────
        let hotel_record = graph
            .get_hotel(&hotel_name)?
            .ok_or_else(|| anyhow::anyhow!("hotel record for '{}' missing", hotel_name))?;
        let blob_port = hotel_record.blob_port;
        let blob_base = format!("http://127.0.0.1:{blob_port}");

        let bundle_bytes = serde_json::to_vec(&bundle)?;
        let part = reqwest::multipart::Part::bytes(bundle_bytes)
            .file_name("migration_bundle.json")
            .mime_str("application/json")?;
        let form = reqwest::multipart::Form::new().part("file", part);
        let upload_resp = reqwest::Client::new()
            .post(format!("{blob_base}/upload"))
            .multipart(form)
            .send()
            .await
            .context("blob store upload failed")?;
        if !upload_resp.status().is_success() {
            anyhow::bail!("blob store upload returned {}", upload_resp.status());
        }
        let upload_json: serde_json::Value = upload_resp.json().await?;
        let blob_id = upload_json["blob_ids"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("blob upload response missing blob_id"))?
            .to_string();

        // Use the hotel's mesh_host for the externally-reachable URL
        let source_mesh_host = hotel_record
            .mesh_host
            .as_deref()
            .filter(|h| !h.is_empty())
            .unwrap_or("127.0.0.1");
        let blob_url = format!("http://{source_mesh_host}:{blob_port}/download/{blob_id}");

        // ── 10. Get dest hotel node_id ───────────────────────────────────────
        let dest_record = graph.get_hotel(dest_hotel)?.ok_or_else(|| {
            anyhow::anyhow!("dest hotel '{}' not found in local graph", dest_hotel)
        })?;
        let dest_node_id = dest_record.capabilities.node_id;

        // Verify the dest node is reachable in the registry
        {
            let guard = registry.read().await;
            if guard.get_node(&dest_node_id).is_none() {
                anyhow::bail!(
                    "dest hotel '{}' (node {}) is not active in the mesh registry",
                    dest_hotel,
                    dest_node_id
                );
            }
        }

        // ── 11. Cross-hotel dispatch via OperatorSurfaceQueryHandoff ─────────
        let reply_guest_id = format!("agent-migration-reply-{}", Uuid::new_v4());
        let reply_role = OPERATOR_SURFACE_QUERY_REPLY_ROLE;
        let mut client = PhiloticClient::connect_at(
            socket_path,
            GuestIdentity {
                guest_id: reply_guest_id.clone(),
                role: reply_role.into(),
                supported_tools: Vec::new(),
            },
        )
        .await?;
        match client
            .send_request(IpcRequest::SubscribeInbox {
                role: reply_role.into(),
            })
            .await?
        {
            IpcResponse::Standard { ok: true, .. } => {}
            other => anyhow::bail!("subscribe inbox failed: {other:?}"),
        }

        let task_json = serde_json::to_string(&OperatorSurfaceQueryHandoff {
            handoff_kind: OPERATOR_SURFACE_QUERY_HANDOFF_KIND.into(),
            surface: "agent.deploy_bundle".into(),
            request_id: migration_id.clone(),
            source_hotel: hotel_name.clone(),
            target_hotel: dest_hotel.to_string(),
            target_node_id: dest_node_id.clone(),
            caller_kind: "agent_migration".into(),
            caller_id: local_node_id.to_string(),
            visibility_scope: "operator".into(),
            grant_scope: "default".into(),
            intent: format!(
                "migrate agent '{}' from '{}' to '{}'",
                agent_id, hotel_name, dest_hotel
            ),
            payload: serde_json::json!({ "blob_url": blob_url }),
            reply_to_node: local_node_id.to_string(),
            reply_to_role: reply_role.into(),
            reply_to_guest_id: Some(reply_guest_id),
            session_id: None,
            trace: None,
        })?;

        match client
            .send_request(IpcRequest::EmitTask {
                target_node: dest_node_id,
                target_role: OPERATOR_SURFACE_QUERY_ROLE.into(),
                target_guest_id: None,
                task_json,
            })
            .await?
        {
            IpcResponse::Standard { ok: true, .. } => {}
            other => anyhow::bail!("EmitTask for deploy_bundle failed: {other:?}"),
        }

        let reply_json = Self::recv_operator_surface_reply(
            &mut client,
            OPERATOR_SURFACE_QUERY_TIMEOUT_SECS,
            &format!("deploy_bundle from '{dest_hotel}'"),
        )
        .await?;
        let result: serde_json::Value = serde_json::from_str(&reply_json)?;
        if result.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let msg = result
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error");
            anyhow::bail!("deploy_bundle on '{}' failed: {}", dest_hotel, msg);
        }

        info!(
            migration_id = %migration_id,
            agent_id = %agent_id,
            dest_hotel = %dest_hotel,
            "Agent migration dispatched successfully"
        );
        Ok(())
    }

    /// Apply a serialised `AgentMigrationBundle` to the local hotel.
    ///
    /// Writes all agent state (identity, apartments, role incarnations, vault
    /// entries, config, guests) and materialises active guests.
    async fn handle_apply_agent_bundle(
        graph: &GraphDomain,
        local_node_id: &str,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        bundle_json: &str,
    ) -> anyhow::Result<String> {
        let bundle: AgentMigrationBundle = serde_json::from_str(bundle_json)
            .context("failed to deserialise AgentMigrationBundle")?;
        let dest_hotel = Self::local_hotel_name(graph, local_node_id)
            .ok_or_else(|| anyhow::anyhow!("local hotel record missing"))?;
        let source_hotel = &bundle.source_hotel;

        info!(
            migration_id = %bundle.migration_id,
            agent_id = %bundle.agent_id,
            source_hotel = %source_hotel,
            dest_hotel = %dest_hotel,
            "Applying agent migration bundle"
        );

        // ── 1. Agent identity ────────────────────────────────────────────────
        let mut identity = bundle.agent_identity.clone();
        identity.authority_hotel = dest_hotel.clone();
        graph
            .upsert_agent_identity(&identity)
            .context("upsert_agent_identity failed")?;

        // ── 2. Apartments ────────────────────────────────────────────────────
        for (memory_type, content) in &bundle.apartments {
            graph
                .sync_apartment(&bundle.agent_id, memory_type, content)
                .with_context(|| format!("sync_apartment '{}' failed", memory_type))?;
        }

        // ── 3. Role incarnations (rewrite guest_id hotel prefix) ─────────────
        let src_philote_prefix = format!("{source_hotel}:philote-");
        let dst_philote_prefix = format!("{dest_hotel}:philote-");
        for mut ri in bundle.role_incarnations.clone() {
            // Rewrite the home guest_id to point at dest hotel
            if ri.guest_id.starts_with(&src_philote_prefix) {
                ri.guest_id = ri.guest_id.replacen(source_hotel, &dest_hotel, 1);
            }
            // Clear home_node if it was pinned to the source hotel
            if ri.home_node.as_deref() == Some(source_hotel)
                || ri
                    .home_node
                    .as_deref()
                    .map_or(false, |n| n.contains(source_hotel))
            {
                ri.home_node = None;
            }
            graph
                .upsert_role_incarnation(&ri)
                .context("upsert_role_incarnation failed")?;
        }
        drop(src_philote_prefix);
        drop(dst_philote_prefix);

        // ── 4. Vault entries (re-encrypt with local vault key) ───────────────
        for ve in &bundle.vault_entries {
            // Keep the source vault_name as the kind — a migrated
            // `gemini_api_key` re-stored as generic `vault-token` loses its
            // identity in the secret_ref (same defect as AddVaultEntry).
            let new_secret_ref = store_secret(
                graph,
                SecretInput {
                    secret_kind: ve.vault_name.clone(),
                    scope: "hotel".to_string(),
                    allowed_roles: ve.allowed_roles.clone(),
                    allowed_guests: Vec::new(),
                    plaintext: ve.plaintext.clone(),
                },
            )
            .context("store_secret for vault entry failed")?;

            // Append to vault_registry
            let mut registry: Vec<serde_json::Value> = graph
                .get_config_value("vault_registry")?
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_default();
            registry.push(serde_json::json!({
                "vault_name": ve.vault_name,
                "secret_ref": new_secret_ref,
            }));
            graph.set_config_value("vault_registry", &serde_json::to_string(&registry)?)?;

            // Set config key to the new secret_ref (as a JSON string)
            graph.set_config_value(&ve.config_key, &serde_json::to_string(&new_secret_ref)?)?;
        }

        // ── 5. Non-vault config entries ──────────────────────────────────────
        for ce in &bundle.config_entries {
            graph
                .set_config_value(&ce.key, &ce.value_json)
                .with_context(|| format!("set_config_value '{}' failed", ce.key))?;
        }

        // ── 6. Guests (rewrite hotel prefix, materialize active ones) ────────
        for ge in &bundle.guests {
            let new_guest_id = format!("{dest_hotel}:{}", ge.guest_id_suffix);
            // Replace all occurrences of source hotel name in config_json env vars
            let new_config_json = ge
                .config_json
                .replace(source_hotel.as_str(), dest_hotel.as_str());
            let rec = GuestRecord {
                hotel_name: dest_hotel.clone(),
                guest_id: new_guest_id.clone(),
                role: ge.role.clone(),
                config_json: new_config_json,
                is_active: ge.is_active,
                active_pid: None,
                last_active_at: None,
            };
            graph.upsert_guest(&rec).context("upsert_guest failed")?;
            if ge.is_active {
                if let Some(mat_req) = materialization_requester {
                    let _ = mat_req.ensure_guest_active(&new_guest_id).await;
                }
            }
        }

        // ── 7. Update membrane-gateway AGENT_ROSTER if agent has telegram ────
        if !bundle.vault_entries.is_empty() {
            let membrane_guest_id = format!("{dest_hotel}:membrane-gateway");
            if let Ok(Some(mut gw)) = graph.get_guest(&dest_hotel, &membrane_guest_id) {
                if let Ok(mut cfg) = serde_json::from_str::<serde_json::Value>(&gw.config_json) {
                    let roster_key = "PHILOTIC_AGENT_ROSTER";
                    let mut roster: Vec<serde_json::Value> = cfg
                        .get("env")
                        .and_then(|e| e.get(roster_key))
                        .and_then(|v| v.as_str())
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or_default();
                    let already_present = roster.iter().any(|e| {
                        e.get("agent_id").and_then(|v| v.as_str()) == Some(&bundle.agent_id)
                    });
                    if !already_present {
                        roster.push(serde_json::json!({
                            "agent_id": bundle.agent_id,
                            "agent_key": bundle.agent_key,
                        }));
                        let new_roster_json = serde_json::to_string(&roster)?;
                        if let Some(env) = cfg.get_mut("env").and_then(|e| e.as_object_mut()) {
                            env.insert(
                                roster_key.to_string(),
                                serde_json::Value::String(new_roster_json),
                            );
                        }
                        gw.config_json = serde_json::to_string(&cfg)?;
                        graph.upsert_guest(&gw)?;
                        // Restart membrane-gateway so it picks up the new roster
                        if let Some(mat_req) = materialization_requester {
                            let _ = mat_req.ensure_guest_active(&membrane_guest_id).await;
                        }
                    }
                }
            }
        }

        info!(
            migration_id = %bundle.migration_id,
            agent_id = %bundle.agent_id,
            "Agent migration bundle applied on '{}'", dest_hotel
        );
        Ok(bundle.agent_id)
    }

    fn handle_patch_agent_bundle(
        graph: &GraphDomain,
        local_node_id: &str,
        agent_id: &str,
        persona_name: Option<String>,
        soul_text: Option<String>,
        identity_text: Option<String>,
        user_context_text: Option<String>,
        system_prompt: Option<String>,
        import_workspace: Option<String>,
        default_toolset: Option<Vec<String>>,
        default_skillset: Option<Vec<String>>,
        response_route_policy: Option<serde_json::Value>,
    ) -> anyhow::Result<DesktopMembraneAgentView> {
        let mut identity = graph
            .get_agent_identity(agent_id)?
            .ok_or_else(|| anyhow::anyhow!("agent [{agent_id}] not found"))?;

        // Verify the agent belongs to the local hotel
        let hotel_name = Self::local_hotel_name(graph, local_node_id)
            .ok_or_else(|| anyhow::anyhow!("local hotel record missing"))?;
        if identity.authority_hotel != hotel_name {
            anyhow::bail!("agent [{agent_id}] does not belong to local hotel [{hotel_name}]");
        }

        if let Some(name) = persona_name {
            identity.persona_name = name;
        }
        if let Some(value) = soul_text {
            identity.bundle_json["soul_text"] = serde_json::json!(value);
        }
        if let Some(value) = identity_text {
            identity.bundle_json["identity_text"] = serde_json::json!(value);
        }
        if let Some(value) = user_context_text {
            identity.bundle_json["user_context_text"] = serde_json::json!(value);
        }
        if let Some(value) = system_prompt {
            identity.bundle_json["system_prompt"] = serde_json::json!(value);
        }
        if let Some(value) = import_workspace {
            identity.bundle_json["import_workspace"] = serde_json::json!(value);
        }
        if let Some(toolset) = default_toolset {
            identity.bundle_json["default_toolset"] = serde_json::json!(toolset);
        }
        if let Some(skillset) = default_skillset {
            identity.bundle_json["default_skillset"] = serde_json::json!(skillset);
        }
        if let Some(policy) = response_route_policy {
            identity.bundle_json["response_route_policy"] = policy;
        }

        graph.upsert_agent_identity(&identity)?;
        Ok(Self::desktop_membrane_agent_view(identity))
    }

    pub(super) fn handle_add_vault_entry(
        graph: &GraphDomain,
        vault_name: String,
        plaintext: String,
        allowed_roles: Vec<String>,
        secret_kind: Option<String>,
    ) -> anyhow::Result<String> {
        // Store the encrypted secret. The kind defaults to the caller's
        // vault_name (e.g. `gemini_api_key`), not a generic label: the kind is
        // embedded in the secret_ref, and a `phil keys configure` entry stored
        // as `vault-token` is indistinguishable from an MCP token grant when
        // debugging ACL failures (2026-07-20 vps-jane provider-key incident).
        //
        // An explicit kind is required for vaults whose consumer filters on it
        // — `muninn_vault_token` is the only such kind today
        // (`memory::load_muninn_config` skips every registry entry that does
        // not carry it).
        let secret_kind = secret_kind.unwrap_or_else(|| vault_name.clone());
        let secret_ref = store_secret(
            graph,
            SecretInput {
                secret_kind,
                scope: "hotel".to_string(),
                allowed_roles,
                allowed_guests: Vec::new(),
                plaintext,
            },
        )?;

        // Append new entry to vault_registry in node_config.
        let mut registry: Vec<serde_json::Value> = graph
            .get_config_value("vault_registry")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        registry.push(serde_json::json!({ "vault_name": vault_name, "secret_ref": secret_ref }));
        graph.set_config_value("vault_registry", &serde_json::to_string(&registry)?)?;

        Ok(secret_ref)
    }

    fn mesh_private_key_ref_config_key(hotel_name: &str) -> String {
        format!("mesh_identity_private_key_ref:{hotel_name}")
    }

    fn mesh_public_key_config_key(hotel_name: &str) -> String {
        format!("mesh_identity_public_key:{hotel_name}")
    }

    fn mesh_transport_private_key_ref_config_key(hotel_name: &str) -> String {
        format!("mesh_transport_private_key_ref:{hotel_name}")
    }

    fn mesh_transport_public_key_config_key(hotel_name: &str) -> String {
        format!("mesh_transport_public_key:{hotel_name}")
    }

    fn mesh_pending_invite_config_key(nonce: &str) -> String {
        format!("mesh_pending_invite:{nonce}")
    }

    fn mesh_auth_key_config_key(node_id: &str) -> String {
        format!("mesh_auth_key:{node_id}")
    }

    fn read_string_config(graph: &GraphDomain, key: &str) -> anyhow::Result<Option<String>> {
        Ok(graph
            .get_config_value(key)?
            .and_then(|value| serde_json::from_str::<String>(&value).ok().or(Some(value)))
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()))
    }

    fn resolve_internal_secret(graph: &GraphDomain, secret_ref: &str) -> anyhow::Result<String> {
        resolve_secret(
            graph,
            secret_ref,
            &SecretAccess {
                role: "hotel.internal".into(),
                guest_id: "aiua".into(),
            },
        )?
        .ok_or_else(|| anyhow::anyhow!("vault secret not found: {secret_ref}"))
    }

    fn ensure_mesh_identity(
        graph: &GraphDomain,
        hotel_name: &str,
    ) -> anyhow::Result<(SigningKey, String, String)> {
        let private_ref_key = Self::mesh_private_key_ref_config_key(hotel_name);
        let public_key_key = Self::mesh_public_key_config_key(hotel_name);

        if let (Some(secret_ref), Some(public_key_b64)) = (
            Self::read_string_config(graph, &private_ref_key)?,
            Self::read_string_config(graph, &public_key_key)?,
        ) {
            let private_key_hex = Self::resolve_internal_secret(graph, &secret_ref)?;
            let signing_key = signing_key_from_hex(&private_key_hex)?;
            let fingerprint = fingerprint_from_base64url(&public_key_b64)?;
            return Ok((signing_key, public_key_b64, fingerprint));
        }

        let signing_key = SigningKey::generate(&mut rand::rngs::OsRng);
        let public_key_b64 = verifying_key_to_base64url(&signing_key.verifying_key());
        let fingerprint = fingerprint_from_base64url(&public_key_b64)?;
        let secret_ref = store_secret(
            graph,
            SecretInput {
                secret_kind: "mesh-hotel-ed25519-private-key".into(),
                scope: format!("hotel:{hotel_name}"),
                allowed_roles: vec!["hotel.internal".into()],
                allowed_guests: Vec::new(),
                plaintext: hex::encode(signing_key.to_bytes()),
            },
        )?;
        graph.set_config_value(&private_ref_key, &serde_json::to_string(&secret_ref)?)?;
        graph.set_config_value(&public_key_key, &serde_json::to_string(&public_key_b64)?)?;
        Ok((signing_key, public_key_b64, fingerprint))
    }

    fn ensure_mesh_transport_identity(
        graph: &GraphDomain,
        hotel_name: &str,
    ) -> anyhow::Result<(String, String)> {
        let private_ref_key = Self::mesh_transport_private_key_ref_config_key(hotel_name);
        let public_key_key = Self::mesh_transport_public_key_config_key(hotel_name);

        if let (Some(secret_ref), Some(public_key_b64)) = (
            Self::read_string_config(graph, &private_ref_key)?,
            Self::read_string_config(graph, &public_key_key)?,
        ) {
            let private_key_hex = Self::resolve_internal_secret(graph, &secret_ref)?;
            return Ok((private_key_hex, public_key_b64));
        }

        let (private_key_hex, public_key_b64) = generate_transport_keypair();
        let secret_ref = store_secret(
            graph,
            SecretInput {
                secret_kind: "mesh-hotel-x25519-private-key".into(),
                scope: format!("hotel:{hotel_name}"),
                allowed_roles: vec!["hotel.internal".into()],
                allowed_guests: Vec::new(),
                plaintext: private_key_hex.clone(),
            },
        )?;
        graph.set_config_value(&private_ref_key, &serde_json::to_string(&secret_ref)?)?;
        graph.set_config_value(&public_key_key, &serde_json::to_string(&public_key_b64)?)?;
        Ok((private_key_hex, public_key_b64))
    }

    fn local_hotel_record(graph: &GraphDomain, local_node_id: &str) -> anyhow::Result<HotelRecord> {
        let hotel_name = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;
        graph
            .get_hotel(&hotel_name)?
            .ok_or_else(|| anyhow::anyhow!("hotel record missing for hotel [{hotel_name}]"))
    }

    fn persist_hotel_mesh_host(
        graph: &GraphDomain,
        hotel_name: &str,
        mesh_host: &str,
    ) -> anyhow::Result<HotelRecord> {
        let mut hotel = graph
            .get_hotel(hotel_name)?
            .ok_or_else(|| anyhow::anyhow!("hotel '{hotel_name}' not found"))?;
        hotel.mesh_host = Some(mesh_host.to_string());
        graph.upsert_hotel(&hotel)?;
        Ok(hotel)
    }

    async fn handle_create_mesh_invite(
        graph: &GraphDomain,
        local_node_id: &str,
        hotel_name: String,
        mesh_host: String,
        ttl_secs: Option<u64>,
    ) -> anyhow::Result<serde_json::Value> {
        let local_hotel = Self::local_hotel_record(graph, local_node_id)?;
        if local_hotel.hotel_name != hotel_name {
            bail!(
                "mesh invites may only be created by the active local hotel [{}], not [{}]",
                local_hotel.hotel_name,
                hotel_name
            );
        }

        let hotel = Self::persist_hotel_mesh_host(graph, &hotel_name, &mesh_host)?;
        let (signing_key, public_key_b64, fingerprint) =
            Self::ensure_mesh_identity(graph, &hotel_name)?;
        let (_transport_private_key_hex, transport_public_key_b64) =
            Self::ensure_mesh_transport_identity(graph, &hotel_name)?;
        let now = now_epoch_secs();
        let nonce = generate_nonce();
        let expires_at = now + ttl_secs.unwrap_or(DEFAULT_INVITE_TTL_SECS);

        let invite = sign_invite(
            MeshInvitePayload {
                version: ansible_mesh_core::membership::MESH_INVITE_VERSION,
                hotel_name: hotel.hotel_name.clone(),
                capabilities: hotel.capabilities.clone(),
                mesh_host: hotel
                    .mesh_host
                    .clone()
                    .unwrap_or_else(|| "127.0.0.1".into()),
                mesh_port: hotel.mesh_port,
                blob_port: hotel.blob_port,
                execution_port: hotel.execution_port,
                inviter_pubkey_b64: public_key_b64,
                inviter_fingerprint: fingerprint.clone(),
                inviter_transport_pubkey_b64: transport_public_key_b64,
                nonce: nonce.clone(),
                created_at: now,
                expires_at,
            },
            &signing_key,
        )?;

        graph.set_config_value(
            &Self::mesh_pending_invite_config_key(&nonce),
            &serde_json::json!({
                "hotel_name": hotel.hotel_name,
                "created_at": now,
                "expires_at": expires_at,
                "status": "pending"
            })
            .to_string(),
        )?;

        Ok(serde_json::json!({
            "invite_json": serde_json::to_string_pretty(&invite)?,
            "fingerprint": fingerprint,
            "expires_at": expires_at,
            "nonce": nonce
        }))
    }

    async fn handle_accept_mesh_invite(
        graph: &GraphDomain,
        local_node_id: &str,
        hotel_name: String,
        mesh_host: String,
        invite_json: String,
    ) -> anyhow::Result<serde_json::Value> {
        let local_hotel = Self::local_hotel_record(graph, local_node_id)?;
        if local_hotel.hotel_name != hotel_name {
            bail!(
                "mesh invites may only be accepted by the active local hotel [{}], not [{}]",
                local_hotel.hotel_name,
                hotel_name
            );
        }

        let invite: MeshInvite =
            serde_json::from_str(&invite_json).context("parse signed mesh invite JSON")?;
        verify_invite(&invite, now_epoch_secs())?;

        let persisted_local_hotel = Self::persist_hotel_mesh_host(graph, &hotel_name, &mesh_host)?;
        let inviter_hotel = HotelRecord {
            hotel_name: invite.payload.hotel_name.clone(),
            capabilities: invite.payload.capabilities.clone(),
            mesh_host: Some(invite.payload.mesh_host.clone()),
            mesh_port: invite.payload.mesh_port,
            blob_port: invite.payload.blob_port,
            execution_port: invite.payload.execution_port,
            ipc_socket_path: String::new(),
            active_pid: None,
        };
        graph.upsert_hotel(&inviter_hotel)?;

        let (signing_key, public_key_b64, fingerprint) =
            Self::ensure_mesh_identity(graph, &hotel_name)?;
        let (transport_private_key_hex, transport_public_key_b64) =
            Self::ensure_mesh_transport_identity(graph, &hotel_name)?;
        let session_key = derive_transport_session_key(
            &invite.payload.nonce,
            &transport_private_key_hex,
            &invite.payload.inviter_transport_pubkey_b64,
        )?;
        graph.set_config_value(
            &Self::mesh_auth_key_config_key(&invite.payload.capabilities.node_id),
            &serde_json::to_string(&session_key)?,
        )?;
        let join_request = sign_join_request(
            MeshJoinRequestPayload {
                version: ansible_mesh_core::membership::MESH_INVITE_VERSION,
                invite_nonce: invite.payload.nonce.clone(),
                hotel_name: persisted_local_hotel.hotel_name.clone(),
                capabilities: persisted_local_hotel.capabilities.clone(),
                mesh_host: persisted_local_hotel
                    .mesh_host
                    .clone()
                    .unwrap_or_else(|| "127.0.0.1".into()),
                mesh_port: persisted_local_hotel.mesh_port,
                blob_port: persisted_local_hotel.blob_port,
                execution_port: persisted_local_hotel.execution_port,
                joiner_pubkey_b64: public_key_b64,
                joiner_fingerprint: fingerprint,
                joiner_transport_pubkey_b64: transport_public_key_b64,
                requested_at: now_epoch_secs(),
            },
            &signing_key,
        )?;

        let payload = serde_json::to_vec(&join_request)?;
        let msg_id = Uuid::new_v4();
        let timestamp = now_epoch_secs();
        let packet = ansible_mesh_core::BeaconMessage {
            version: ansible_mesh_core::membership::MESH_INVITE_VERSION,
            msg_id,
            src_node: persisted_local_hotel.capabilities.node_id.clone(),
            dest_node: invite.payload.capabilities.node_id.clone(),
            msg_type: ansible_mesh_core::MsgType::MeshMembershipAccept,
            seq: 0,
            total: 1,
            payload: payload.into(),
            timestamp,
            hmac: ansible_mesh_core::BeaconPayload::default(),
        };

        let target_addr = format!("{}:{}", invite.payload.mesh_host, invite.payload.mesh_port);
        let socket = UdpSocket::bind("0.0.0.0:0")
            .await
            .context("bind local UDP socket for mesh join request")?;
        socket
            .send_to(&serde_json::to_vec(&packet)?, &target_addr)
            .await
            .with_context(|| format!("send mesh join request to {target_addr}"))?;

        Ok(serde_json::json!({
            "inviter_hotel": invite.payload.hotel_name,
            "target_addr": target_addr,
            "nonce": invite.payload.nonce
        }))
    }

    fn upsert_tool_runner_registry_entry(
        graph: &GraphDomain,
        identity: &philotic_client::GuestIdentity,
    ) -> anyhow::Result<()> {
        let mut registry = load_tool_runner_registry(graph)?;
        registry.retain(|entry| entry.guest_id != identity.guest_id);
        registry.push(ToolRunnerRegistryEntry {
            guest_id: identity.guest_id.clone(),
            supported_tools: identity.supported_tools.clone(),
            last_seen_at: unix_ts(),
        });
        registry.sort_by(|a, b| a.guest_id.cmp(&b.guest_id));
        let registry_json = serde_json::Value::Array(
            registry
                .iter()
                .map(|entry| {
                    serde_json::json!({
                        "guest_id": entry.guest_id,
                        "supported_tools": entry.supported_tools,
                        "last_seen_at": entry.last_seen_at,
                    })
                })
                .collect(),
        );
        graph.set_config_value("tool_runner_registry", &registry_json.to_string())
    }

    async fn compose_session_snapshot(
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        registry: &Arc<RwLock<NodeRegistry>>,
        local_node_id: &str,
        session_id: &str,
        // When a role process requests its snapshot, it passes its role name so we
        // return the role-scoped checkpoint instead of the orchestrator's base one.
        role_name: Option<&str>,
    ) -> anyhow::Result<Option<serde_json::Value>> {
        let Some(session) = graph.get_session(session_id)? else {
            return Ok(None);
        };

        let turns = graph.list_session_turns(session_id, 8)?;
        let apartment_checkpoint = session.primary_agent_id.as_deref().and_then(|agent_id| {
            let memory_type = match role_name {
                Some(role) => format!("short_session:{session_id}:{role}"),
                None => format!("short_session:{session_id}"),
            };
            graph.get_apartment(agent_id, &memory_type).ok().flatten()
        });

        let session_index = session
            .primary_agent_id
            .as_deref()
            .and_then(|agent_id| graph.get_apartment(agent_id, "short").ok().flatten());
        let agent_profile = session
            .primary_agent_id
            .as_deref()
            .and_then(|agent_id| graph.get_agent_identity(agent_id).ok().flatten())
            .map(|identity| identity.bundle_json)
            .unwrap_or_else(|| serde_json::json!({}));

        let recent_turns = if let Some(checkpoint_turns) = apartment_checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.get("recent_turns"))
            .and_then(serde_json::Value::as_array)
        {
            checkpoint_turns.clone()
        } else {
            turns.iter()
                .map(|turn| {
                    serde_json::json!({
                        "turn_id": turn.turn_id,
                        "user_content": turn.user_message_json.get("content").and_then(serde_json::Value::as_str).unwrap_or_default(),
                        "assistant_content": turn.response_json.as_ref().and_then(|r| r.get("content")).and_then(serde_json::Value::as_str),
                    })
                })
                .collect::<Vec<_>>()
        };

        let active_turn = apartment_checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.get("active_turn"))
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let mut bindings = session
            .summary_json
            .get("bindings")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));

        let active_role_record = session
            .active_incarnation_id
            .as_deref()
            .and_then(|active_incarnation_id| {
                let agent_id = session.primary_agent_id.as_deref()?;
                let roles = graph.list_role_incarnations(agent_id).ok()?;
                roles
                    .into_iter()
                    .find(|role| role.guest_id == active_incarnation_id)
            })
            // When no explicit role is active, fall back to the orchestrator role record so the
            // agent always gets its full toolset and manifest from the first session turn.
            .or_else(|| {
                let agent_id = session.primary_agent_id.as_deref()?;
                graph
                    .get_role_incarnation(agent_id, "orchestrator")
                    .ok()
                    .flatten()
            });

        if let Some(role_record) = &active_role_record {
            if let Ok(Some(profile)) = graph.get_toolset_profile(&role_record.toolset_profile) {
                // Always union the current profile tools into the stored effective_toolset so that
                // profile additions flow through to existing sessions on hotel restart, without
                // discarding any tools the agent added itself above the profile baseline.
                let mut toolset: Vec<String> = bindings
                    .get("effective_toolset")
                    .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
                    .unwrap_or_default();
                for tool in &profile.allowed_tools {
                    if !toolset.contains(tool) {
                        toolset.push(tool.clone());
                    }
                }
                let mut skillset: Vec<String> = bindings
                    .get("effective_skillset")
                    .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
                    .unwrap_or_default();
                for skill in &profile.allowed_skills {
                    if !skillset.contains(skill) {
                        skillset.push(skill.clone());
                    }
                }
                // Merge on_demand_skills from profile (without expanding their tools).
                // These skills gate which tool schemas are visible per-turn in philote.
                let mut on_demand: Vec<String> = bindings
                    .get("on_demand_skills")
                    .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
                    .unwrap_or_default();
                for skill in &profile.on_demand_skills {
                    if !on_demand.contains(skill) {
                        on_demand.push(skill.clone());
                    }
                }

                // Carry allowed_classes so philote can expand class-tagged catalog tools.
                let allowed_classes = profile.allowed_classes.clone();

                if let Some(obj) = bindings.as_object_mut() {
                    obj.insert("effective_toolset".to_string(), serde_json::json!(toolset));
                    obj.insert(
                        "effective_skillset".to_string(),
                        serde_json::json!(skillset),
                    );
                    if !on_demand.is_empty() {
                        obj.insert("on_demand_skills".to_string(), serde_json::json!(on_demand));
                    }
                    if !allowed_classes.is_empty() {
                        obj.insert(
                            "allowed_classes".to_string(),
                            serde_json::json!(allowed_classes),
                        );
                    }
                } else {
                    bindings = serde_json::json!({
                        "effective_toolset": toolset,
                        "effective_skillset": skillset,
                        "on_demand_skills": on_demand,
                        "allowed_classes": allowed_classes,
                    });
                }

                // Merge profile-level remote_tool_runners into
                // allowed_tool_runner_incarnations.  Session-only runners
                // (incarnation_ids the profile does not know) are preserved,
                // but for an incarnation_id the profile DOES declare, the
                // profile entry replaces the stored one: sessions persist
                // their bindings snapshot, so append-only merging froze
                // `supported_tools` at whatever the session first saw — a
                // long-lived session never learned about tools added to the
                // runner later (live incident 2026-08-23: life.list was
                // projected from the fresh profile but unroutable against the
                // stale snapshot, hanging turns in WaitingTool).
                if !profile.remote_tool_runners.is_empty() {
                    let mut incarnations: Vec<serde_json::Value> = bindings
                        .get("allowed_tool_runner_incarnations")
                        .and_then(serde_json::Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    for runner in &profile.remote_tool_runners {
                        let runner_id = runner
                            .get("incarnation_id")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("");
                        if runner_id.is_empty() {
                            continue;
                        }
                        if let Some(existing) = incarnations.iter_mut().find(|existing| {
                            existing
                                .get("incarnation_id")
                                .and_then(serde_json::Value::as_str)
                                == Some(runner_id)
                        }) {
                            *existing = runner.clone();
                        } else {
                            incarnations.push(runner.clone());
                        }
                    }
                    if let Some(obj) = bindings.as_object_mut() {
                        obj.insert(
                            "allowed_tool_runner_incarnations".to_string(),
                            serde_json::Value::Array(incarnations),
                        );
                    }
                }
            }
        }

        if let Some(agent_id) = session.primary_agent_id.as_deref() {
            if let Some(routing_preferences) = load_agent_graph_routing_preferences(agent_id) {
                if let Some(obj) = bindings.as_object_mut() {
                    obj.insert(
                        "routing_preferences".to_string(),
                        serde_json::Value::Array(routing_preferences),
                    );
                } else {
                    bindings = serde_json::json!({
                        "routing_preferences": routing_preferences,
                    });
                }
            }
            if let Some((
                reflex_policy_agent_layers,
                reflex_policy_agent_suppressions,
                reflex_policy_agent_rewards,
            )) = load_agent_graph_reflex_preferences(graph, agent_id)
            {
                if let Some(obj) = bindings.as_object_mut() {
                    obj.insert(
                        "reflex_policy_agent_layers".to_string(),
                        serde_json::Value::Array(reflex_policy_agent_layers),
                    );
                    if !reflex_policy_agent_suppressions.is_empty() {
                        obj.insert(
                            "reflex_policy_agent_suppressions".to_string(),
                            serde_json::Value::Array(reflex_policy_agent_suppressions),
                        );
                    }
                    if !reflex_policy_agent_rewards.is_empty() {
                        obj.insert(
                            "reflex_policy_agent_rewards".to_string(),
                            serde_json::Value::Array(reflex_policy_agent_rewards),
                        );
                    }
                } else {
                    bindings = serde_json::json!({
                        "reflex_policy_agent_layers": reflex_policy_agent_layers,
                        "reflex_policy_agent_suppressions": reflex_policy_agent_suppressions,
                        "reflex_policy_agent_rewards": reflex_policy_agent_rewards,
                    });
                }
            }
        }

        if let Some(shared_model_markers) = load_shared_model_markers(graph) {
            if let Some(obj) = bindings.as_object_mut() {
                obj.insert(
                    "shared_model_markers".to_string(),
                    serde_json::Value::Array(shared_model_markers),
                );
            } else {
                bindings = serde_json::json!({
                    "shared_model_markers": shared_model_markers,
                });
            }
        }
        if let Some(shared_tool_markers) = load_shared_tool_markers(graph) {
            if let Some(obj) = bindings.as_object_mut() {
                obj.insert(
                    "shared_tool_markers".to_string(),
                    serde_json::Value::Array(shared_tool_markers),
                );
            } else {
                bindings = serde_json::json!({
                    "shared_tool_markers": shared_tool_markers,
                });
            }
        }
        if let Some(shared_skill_markers) = load_shared_skill_markers(graph) {
            if let Some(obj) = bindings.as_object_mut() {
                obj.insert(
                    "shared_skill_markers".to_string(),
                    serde_json::Value::Array(shared_skill_markers),
                );
            } else {
                bindings = serde_json::json!({
                    "shared_skill_markers": shared_skill_markers,
                });
            }
        }

        // Expand dynamic skill implied_tools into effective_toolset and carry prompt-facing
        // skill guidance so philote can project more than just skill names.
        // For each skill in effective_skillset, load its AbstractSkillRecord and merge
        // any implied_tools that are not already present. This runs hotel-side so philote
        // receives a fully-expanded toolset without needing DB access.
        {
            let skillset: Vec<String> = bindings
                .get("effective_skillset")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let on_demand_skills: Vec<String> = bindings
                .get("on_demand_skills")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();

            if !skillset.is_empty() || !on_demand_skills.is_empty() {
                let mut toolset: Vec<String> = bindings
                    .get("effective_toolset")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let mut skill_guidance: Vec<String> = bindings
                    .get("effective_skill_guidance")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let mut allowed_classes: Vec<String> = bindings
                    .get("allowed_classes")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                fn push_guidance(
                    skill_guidance: &mut Vec<String>,
                    skill_record: &AbstractSkillRecord,
                ) {
                    let guidance =
                        format!("{} — {}", skill_record.skill_name, skill_record.description);
                    if !skill_guidance.contains(&guidance) {
                        skill_guidance.push(guidance);
                    }
                }

                // Resolve the transitive SkillDAG closure over allowed_skills
                // edges. Cycles and dangling edges are tolerated (the
                // reachable set still resolves) but logged for the operator.
                let (resolved_skillset, dag_diagnostics) =
                    ansible_mesh_core::graph::resolve_transitive_skills(&skillset, |name| {
                        graph.get_abstract_skill(name).ok().flatten()
                    });
                if !dag_diagnostics.is_empty() {
                    warn!(
                        diagnostics = ?dag_diagnostics,
                        "SkillDAG resolution reported unresolvable edges"
                    );
                }

                // Administratively retired skills (suspended/deprecated) are
                // dropped from projection entirely: no name, no implied tools,
                // no guidance. Skills with no graph record pass through — they
                // may be legacy compiled-in skills philote still recognizes.
                let mut projected_skillset: Vec<String> = Vec::new();
                for skill_name in &resolved_skillset {
                    match graph.get_abstract_skill(skill_name) {
                        Ok(Some(skill_record)) => {
                            if !skill_record.validation_state.is_projectable() {
                                continue;
                            }
                            projected_skillset.push(skill_name.clone());
                            for implied in &skill_record.implied_tools {
                                if !toolset.contains(implied) {
                                    toolset.push(implied.clone());
                                }
                            }
                            for class in &skill_record.implied_classes {
                                if !allowed_classes.contains(class) {
                                    allowed_classes.push(class.clone());
                                }
                            }
                            push_guidance(&mut skill_guidance, &skill_record);
                        }
                        _ => projected_skillset.push(skill_name.clone()),
                    }
                }
                // On-demand skills project per-turn in philote, but their
                // doctrine text must still travel in effective_skill_guidance —
                // a skill projected without its guidance is a bare id (the
                // model never sees the outcome-note / escalation discipline the
                // catalog describes). Tools are deliberately NOT expanded here;
                // per-turn on-demand tool visibility stays philote's decision.
                // Administratively retired skills push no doctrine either.
                for skill_name in &on_demand_skills {
                    if skillset.contains(skill_name) {
                        continue;
                    }
                    if let Ok(Some(skill_record)) = graph.get_abstract_skill(skill_name) {
                        if skill_record.validation_state.is_projectable() {
                            push_guidance(&mut skill_guidance, &skill_record);
                        }
                    }
                }

                // Procedural graphs (doc:procedural-graphs P0): the full
                // records for every projectable procedure whose skill is in
                // play (projected or on-demand), plus standalone procedures
                // that carry a trigger. Records are tiny by construction, so
                // philote holds them on its bindings and localizes locally —
                // no IPC per step. Prompt-facing only; never a tool grant.
                let effective_procedures: Vec<serde_json::Value> = match graph.list_procedures() {
                    Ok(list) => list
                        .into_iter()
                        .filter(|p| p.validation_state.is_projectable())
                        .filter(|p| match p.skill_name.as_deref() {
                            Some(skill) => {
                                projected_skillset.iter().any(|s| s == skill)
                                    || on_demand_skills.iter().any(|s| s == skill)
                            }
                            None => p.trigger.is_some(),
                        })
                        .filter_map(|p| serde_json::to_value(p).ok())
                        .collect(),
                    Err(err) => {
                        warn!(
                            error = %err,
                            "procedure projection failed; binding session without procedures"
                        );
                        Vec::new()
                    }
                };

                // Skill RECORDS for every skill in play (projected closure +
                // on-demand), so philote's per-turn projection can read implied
                // tools, description and goal text instead of a compiled table
                // keyed by name (2026-09-15: runtime-registered skills could
                // never project). Retired states are already filtered above.
                let effective_skill_records: Vec<serde_json::Value> = projected_skillset
                    .iter()
                    .chain(
                        on_demand_skills
                            .iter()
                            .filter(|s| !projected_skillset.contains(s)),
                    )
                    .filter_map(|name| graph.get_abstract_skill(name).ok().flatten())
                    .filter(|record| record.validation_state.is_projectable())
                    .filter_map(|record| serde_json::to_value(record).ok())
                    .collect();

                if let Some(obj) = bindings.as_object_mut() {
                    obj.insert(
                        "effective_skill_records".to_string(),
                        serde_json::json!(effective_skill_records),
                    );
                    obj.insert(
                        "effective_procedures".to_string(),
                        serde_json::json!(effective_procedures),
                    );
                    obj.insert("effective_toolset".to_string(), serde_json::json!(toolset));
                    obj.insert(
                        "effective_skillset".to_string(),
                        serde_json::json!(projected_skillset),
                    );
                    obj.insert(
                        "effective_skill_guidance".to_string(),
                        serde_json::json!(skill_guidance),
                    );
                    if !allowed_classes.is_empty() {
                        obj.insert(
                            "allowed_classes".to_string(),
                            serde_json::json!(allowed_classes),
                        );
                    }
                }
            }
        }

        {
            let effective_rights = project_effective_rights(&bindings);
            if let Some(obj) = bindings.as_object_mut() {
                obj.insert(
                    "effective_rights".to_string(),
                    serde_json::json!(effective_rights),
                );
            }
        }

        {
            let placement_risk_level = session
                .summary_json
                .get("agent_runtime_provenance")
                .map(|provenance| {
                    infer_placement_risk_level(
                        provenance
                            .get("marker_kind")
                            .and_then(serde_json::Value::as_str),
                        provenance
                            .get("marker_source")
                            .and_then(serde_json::Value::as_str),
                        provenance
                            .get("marker_strength")
                            .and_then(serde_json::Value::as_str),
                    )
                })
                .unwrap_or("guarded");
            let effective_reflex_policy_layers = normalized_reflex_policy_records(
                &session.summary_json,
                &bindings,
                placement_risk_level,
            );
            if let Some(obj) = bindings.as_object_mut() {
                let effective_reflexes =
                    effective_reflexes_from_policy_records(&effective_reflex_policy_layers);
                let effective_right_policy = serde_json::json!({
                    "remote_tool_execution": effective_reflexes["remote_tool_reflex"],
                    "remote_component_execution": effective_reflexes["remote_component_reflex"],
                    "credential_scope": effective_reflexes["credential_scope_reflex"],
                });
                obj.insert(
                    "effective_posture".to_string(),
                    serde_json::json!({
                        "placement_risk_level": placement_risk_level,
                        "remote_execution_allowed": placement_risk_level != "elevated",
                    }),
                );
                obj.insert(
                    "effective_reflex_policy".to_string(),
                    serde_json::json!({
                        "precedence_model": "highest_precedence_wins",
                        "origin_classes": effective_reflex_policy_layers
                            .iter()
                            .filter_map(|layer| layer.get("origin_class").and_then(serde_json::Value::as_str))
                            .collect::<Vec<_>>(),
                        "layers": effective_reflex_policy_layers,
                        "evaluation_count": session
                            .summary_json
                            .get("reflex_evaluations")
                            .and_then(serde_json::Value::as_array)
                            .map(|items| items.len())
                            .unwrap_or(0),
                    }),
                );
                obj.insert("effective_reflexes".to_string(), effective_reflexes);
                obj.insert("effective_right_policy".to_string(), effective_right_policy);
            }
        }

        let role_activation = active_role_record
            .and_then(|role_record| {
                let effective_skillset = bindings
                    .get("effective_skillset")
                    .and_then(serde_json::Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let activation_reason = if session.active_incarnation_id.is_some() {
                    "session_active_incarnation"
                } else {
                    "default_identity_posture"
                };
                Some(serde_json::json!({
                    "role_name": role_record.role_name,
                    "active_incarnation_id": role_record.guest_id.clone(),
                    "activation_reason": activation_reason,
                    "requested_by": "hotel_runtime",
                    "activation_requester_class": "system",
                    "activation_policy_owner": "hotel_runtime",
                    "base_identity_ref": role_record.agent_id.clone(),
                    "role_addendum": role_record.role_identity_addendum,
                    "role_manifest": role_record.role_manifest,
                    "toolset_profile_ref": role_record.toolset_profile,
                    "skillset_profile_ref": role_record.toolset_profile,
                    "effective_skillset": effective_skillset,
                    "effective_skill_guidance": bindings
                        .get("effective_skill_guidance")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!([])),
                    "working_memory_policy": "role_local",
                    "memory_projection_policy": "shared_identity_role_scoped",
                    "turn_loop_config": serde_json::to_value(&role_record.turn_loop_config).unwrap_or_default(),
                }))
            })
            .unwrap_or(serde_json::Value::Null);
        let registered_runners = load_tool_runner_registry(graph)?;
        let tool_runners = live_tool_runners(inboxes).await;
        let live_model_subscribers = live_role_subscribers(inboxes, "model.").await;
        let local_guest_roles = Self::local_hotel_name(graph, local_node_id)
            .and_then(|hotel_name| graph.list_guests(&hotel_name, true).ok())
            .map(|guests| {
                guests
                    .into_iter()
                    .filter(|guest| guest.active_pid.is_some())
                    .map(|guest| guest.role)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let (remote_tool_ads, component_route_assembly) = {
            let guard = registry.read().await;
            (
                remote_tool_advertisements(&guard, local_node_id),
                compose_component_route_assembly(
                    &bindings,
                    &live_model_subscribers,
                    &local_guest_roles,
                    &guard,
                    local_node_id,
                ),
            )
        };
        let tool_assembly = compose_tool_assembly(
            &bindings,
            &registered_runners,
            &tool_runners,
            &remote_tool_ads,
            local_node_id,
        );
        let tool_runner_registry = merge_tool_runners(&registered_runners, &tool_runners);
        let mesh_registry = Self::compose_mesh_registry_snapshot(registry).await;

        // Always merge baseline approval classes into whatever policy is stored so that
        // stale sessions (which recorded an older policy before "config" was added) are
        // fixed automatically on the next hotel restart, without losing auto_approve_all.
        let approval_policy = {
            let mut policy = session
                .summary_json
                .get("approval_policy")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"auto_approve_all": false}));
            if let Some(obj) = policy.as_object_mut() {
                let classes = obj
                    .entry("preapproved_classes")
                    .or_insert_with(|| serde_json::json!([]));
                if let Some(arr) = classes.as_array_mut() {
                    for c in ["session", "utility", "capability", "config"] {
                        if !arr.iter().any(|v| v.as_str() == Some(c)) {
                            arr.push(serde_json::json!(c));
                        }
                    }
                }
                let tools = obj
                    .entry("preapproved_tools")
                    .or_insert_with(|| serde_json::json!([]));
                if let Some(arr) = tools.as_array_mut() {
                    if !arr.iter().any(|v| v.as_str() == Some("agent.configure")) {
                        arr.push(serde_json::json!("agent.configure"));
                    }
                }
            }
            policy
        };

        let mut snapshot = serde_json::json!({
            "session_id": session.session_id,
            "agent_id": session.primary_agent_id,
            "source": session.channel_kind,
            "active_incarnation_id": session.active_incarnation_id,
            "role_activation": role_activation,
            "agent_profile": agent_profile,
            "status": session.status,
            "summary": session.summary_json,
            "approval_policy": approval_policy,
            "bindings": bindings,
            "component_route_assembly": component_route_assembly,
            "tool_assembly": tool_assembly,
            "tool_runners": tool_runner_registry,
            "mesh_registry": mesh_registry,
            "recent_turns": recent_turns,
            "active_turn": active_turn,
            "session_index": session_index,
        });
        Self::overlay_philote_owned_checkpoint_fields(&mut snapshot, apartment_checkpoint.as_ref());
        Ok(Some(snapshot))
    }

    /// Carry every checkpoint field the snapshot does not compute itself —
    /// parked turns, the carryover plan, paracrine threads, watchdog clocks,
    /// the fallback override, the life-recall cache (DEF-167). Before this the
    /// snapshot projected only `recent_turns`/`active_turn` out of the
    /// apartment, so a philote restoring a session — after a restart, or on
    /// another hotel after a relocation — silently got defaults for all of
    /// it. Hotel-computed keys win: the session row is the truth for
    /// routing, status, and policy, and the hotel recomputes the profile and
    /// assemblies that `checkpoint_json` deliberately leaves out.
    fn overlay_philote_owned_checkpoint_fields(
        snapshot: &mut serde_json::Value,
        checkpoint: Option<&serde_json::Value>,
    ) {
        let (Some(snapshot), Some(checkpoint)) = (
            snapshot.as_object_mut(),
            checkpoint.and_then(serde_json::Value::as_object),
        ) else {
            return;
        };
        for (key, value) in checkpoint {
            if !snapshot.contains_key(key) {
                snapshot.insert(key.clone(), value.clone());
            }
        }
    }

    async fn compose_mesh_registry_snapshot(
        registry: &Arc<RwLock<NodeRegistry>>,
    ) -> serde_json::Value {
        let guard = registry.read().await;
        let nodes = guard
            .active_nodes()
            .map(|status| {
                serde_json::json!({
                    "node_id": status.capabilities.node_id,
                    "roles": status.capabilities.roles,
                    "models": status.capabilities.models,
                    "tools": status.capabilities.tools,
                    "execution_reachability": status.execution_reachability,
                    "advertisements": status.advertisements,
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({ "nodes": nodes })
    }

    /// May a role record a peer sent with a `session.handoff` be admitted, and
    /// as what? (DEF-183)
    ///
    /// The record arrives from the network and the handler goes on to
    /// materialize a guest from it, so it is checked like any other peer claim:
    /// - an existing local record is never overwritten — its `home_node`,
    ///   `is_admin` and toolset are this hotel's truth (the old code upserted
    ///   unconditionally although its own doc said "if not already present");
    /// - a record for an agent whose authority hotel this hotel knows is
    ///   accepted only from that hotel;
    /// - a peer never grants admin: `is_admin` is cleared unless the sender is
    ///   the agent's authority hotel.
    ///
    /// `Ok(None)` means "keep what is already here"; `Ok(Some(record))` is the
    /// record to upsert; `Err` refuses the handoff.
    pub(super) fn admit_handoff_role_record(
        graph: &GraphDomain,
        source_node_id: &str,
        role_value: &serde_json::Value,
    ) -> Result<Option<ansible_mesh_core::graph::RoleIncarnationRecord>, String> {
        let mut record = serde_json::from_value::<ansible_mesh_core::graph::RoleIncarnationRecord>(
            role_value.clone(),
        )
        .map_err(|err| format!("malformed role_record: {err}"))?;
        if graph
            .get_role_incarnation(&record.agent_id, &record.role_name)
            .ok()
            .flatten()
            .is_some()
        {
            return Ok(None);
        }
        let sender_hotel = graph.list_hotels().ok().and_then(|hotels| {
            hotels
                .into_iter()
                .find(|hotel| hotel.capabilities.node_id == source_node_id)
                .map(|hotel| hotel.hotel_name)
        });
        let sender_is_authority = match lookup_agent_authority_hotel(graph, &record.agent_id) {
            Some(authority) => {
                if sender_hotel.as_deref() != Some(authority.as_str()) {
                    return Err(format!(
                        "agent '{}' answers to hotel '{authority}', not the sending peer '{source_node_id}'",
                        record.agent_id
                    ));
                }
                true
            }
            None => false,
        };
        if !sender_is_authority {
            record.is_admin = false;
        }
        // Clear readiness — the remote hotel owns that state, not us.
        record.readiness_state = ansible_mesh_core::graph::RoleReadinessState::Configured;
        Ok(Some(record))
    }

    /// Handle a `session.handoff` mesh event received from a remote hotel.
    ///
    /// The payload carries the `RoleIncarnationRecord` and optional `ToolsetProfileRecord`
    /// from the authority hotel so the receiving hotel can upsert them if not already present,
    /// then materialize the role's guest and deliver the handoff bundle.
    pub(crate) async fn handle_remote_role_handoff(
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        parked_inbound: &ParkedInboundRegistry,
        materialization_requester: Option<Arc<dyn GuestMaterializationRequester>>,
        local_node_id: &str,
        source_node_id: &str,
        data: &str,
    ) {
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(data) else {
            warn!("handle_remote_role_handoff: failed to parse payload");
            return;
        };

        let Some(session_id) = payload.get("session_id").and_then(|v| v.as_str()) else {
            warn!("handle_remote_role_handoff: missing session_id");
            return;
        };
        let Some(role_name) = payload.get("role_name").and_then(|v| v.as_str()) else {
            warn!("handle_remote_role_handoff: missing role_name");
            return;
        };
        let handoff_bundle = payload.get("handoff_bundle").cloned().unwrap_or_default();

        // Admit the role_record into the local graph so ensure_role_materialized
        // can find it — but only as `admit_handoff_role_record` allows (DEF-183).
        if let Some(role_val) = payload.get("role_record") {
            match Self::admit_handoff_role_record(graph, source_node_id, role_val) {
                Ok(Some(role_record)) => {
                    if let Err(err) = graph.upsert_role_incarnation(&role_record) {
                        warn!(
                            "handle_remote_role_handoff: failed to upsert role_record for '{}': {}",
                            role_name, err
                        );
                    }
                }
                Ok(None) => {}
                Err(reason) => {
                    warn!(
                        "handle_remote_role_handoff: refusing handoff from '{}': {}",
                        source_node_id, reason
                    );
                    return;
                }
            }
        }

        // Upsert toolset_record if provided.
        if let Some(ts_val) = payload.get("toolset_record") {
            if ts_val.is_object() {
                if let Ok(profile) = serde_json::from_value::<
                    ansible_mesh_core::graph::ToolsetProfileRecord,
                >(ts_val.clone())
                {
                    let _ = graph.upsert_toolset_profile(&profile);
                }
            }
        }

        let readiness = match Self::ensure_role_materialized(
            graph,
            inboxes,
            materialization_requester.as_deref(),
            local_node_id,
            &payload
                .get("role_record")
                .and_then(|v| v.get("agent_id"))
                .and_then(|v| v.as_str())
                .unwrap_or(""),
            role_name,
        )
        .await
        {
            Ok(r) => r,
            Err(err) => {
                warn!(
                    "handle_remote_role_handoff: ensure_role_materialized failed for '{}': {}",
                    role_name, err
                );
                return;
            }
        };

        let agent_id = payload
            .get("role_record")
            .and_then(|v| v.get("agent_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let guest_id = payload
            .get("role_record")
            .and_then(|v| v.get("guest_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let routing_role = format!("role:{}:{}", agent_id, role_name);
        let task_id = Uuid::new_v4();
        let task_json = serde_json::json!({
            "action": "handoff_bundle",
            "agent_id": agent_id,
            "session_id": session_id,
            "handoff_bundle": handoff_bundle,
        })
        .to_string();

        if matches!(
            readiness,
            ansible_mesh_core::graph::RoleReadinessState::Configured
                | ansible_mesh_core::graph::RoleReadinessState::Materializing
                | ansible_mesh_core::graph::RoleReadinessState::Materialized
        ) {
            // Not yet live — park and wait for the guest to start up.
            let mut guard = parked_inbound.lock().await;
            guard.entry(guest_id).or_default().push(ParkedInboundTask {
                source_node: local_node_id.to_string(),
                task_id,
                task_json,
                activate_session_id: Some(session_id.to_string()),
                parked_at: unix_ts(),
            });
            return;
        }

        if let Err(err) = Self::deliver_live_guest_task(
            graph,
            inboxes,
            local_node_id,
            &routing_role,
            &guest_id,
            task_id,
            task_json,
            Some(session_id.to_string()),
        )
        .await
        {
            warn!(
                "handle_remote_role_handoff: deliver_live_guest_task failed for '{}': {}",
                role_name, err
            );
        }
    }

    /// Relocation Ceremony R3 (STANDBY phase), target side: receive a
    /// `MaterializeRequest` for a role_record this hotel doesn't own yet,
    /// bring it up locally, and reply with `MaterializeReady`.
    ///
    /// Deliberately mirrors `handle_remote_role_handoff`'s upsert step
    /// (readiness reset to `Configured`, `home_node` left exactly as the
    /// source sent it) — this hotel does not invent authority over the role
    /// just by pre-warming it; SWITCH (`role.set_home`) stays the only act
    /// that moves `home_node`. Idempotent: a retransmitted/duplicate request
    /// just re-upserts the same record and re-checks liveness.
    pub(crate) async fn handle_remote_materialize_request(
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        materialization_requester: Option<Arc<dyn GuestMaterializationRequester>>,
        dispatcher_tx: mpsc::Sender<LedgerCommand>,
        local_node_id: &str,
        source_node_id: &str,
        data: &str,
    ) {
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(data) else {
            warn!("handle_remote_materialize_request: failed to parse payload");
            return;
        };
        let Some(request_id) = payload
            .get("request_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
        else {
            warn!("handle_remote_materialize_request: missing request_id");
            return;
        };
        let Some(role_val) = payload.get("role_record") else {
            Self::reply_materialize_ready(
                &dispatcher_tx,
                local_node_id,
                source_node_id,
                &request_id,
                "",
                false,
                None,
                Some("missing role_record".into()),
            )
            .await;
            return;
        };
        let Ok(mut role_record) = serde_json::from_value::<
            ansible_mesh_core::graph::RoleIncarnationRecord,
        >(role_val.clone()) else {
            Self::reply_materialize_ready(
                &dispatcher_tx,
                local_node_id,
                source_node_id,
                &request_id,
                "",
                false,
                None,
                Some("malformed role_record".into()),
            )
            .await;
            return;
        };
        let agent_id = role_record.agent_id.clone();
        let role_name = role_record.role_name.clone();
        let guest_id = role_record.guest_id.clone();
        if let Err(reason) =
            Self::peer_may_place_role(graph, local_node_id, source_node_id, &agent_id, &role_name)
        {
            warn!("Materialize request [{}] refused: {}", request_id, reason);
            Self::reply_materialize_ready(
                &dispatcher_tx,
                local_node_id,
                source_node_id,
                &request_id,
                &guest_id,
                false,
                None,
                Some(reason),
            )
            .await;
            return;
        }
        let dry_run = payload
            .get("dry_run")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let requester_build_version = payload
            .get("requester_build_version")
            .and_then(|v| v.as_str());

        // Relocation Ceremony R4 (Feasibility and placement): decline loudly
        // before ever upserting or spawning anything, whether this is a
        // dry-run probe or a real STANDBY commit. `hotel_name` falls back to
        // `local_node_id` itself only if this hotel's own record can't be
        // found — a state so broken the checks below would be meaningless
        // anyway, so proceeding is no worse than declining here.
        let hotel_name = Self::local_hotel_name(graph, local_node_id)
            .unwrap_or_else(|| local_node_id.to_string());
        let decline_reasons = Self::evaluate_role_relocation_feasibility(
            graph,
            &hotel_name,
            &role_record,
            requester_build_version,
        );
        if !decline_reasons.is_empty() {
            info!(
                "Materialize request [{}] declined for role '{}' (agent '{}'): {}",
                request_id,
                role_name,
                agent_id,
                decline_reasons.join("; ")
            );
            Self::reply_materialize_ready(
                &dispatcher_tx,
                local_node_id,
                source_node_id,
                &request_id,
                &guest_id,
                false,
                None,
                Some(decline_reasons.join("; ")),
            )
            .await;
            return;
        }
        if dry_run {
            info!(
                "Materialize request [{}] feasible (dry_run) for role '{}' (agent '{}') — no changes made",
                request_id, role_name, agent_id
            );
            Self::reply_materialize_ready(
                &dispatcher_tx,
                local_node_id,
                source_node_id,
                &request_id,
                &guest_id,
                true,
                Some("feasible".to_string()),
                None,
            )
            .await;
            return;
        }

        role_record.readiness_state = ansible_mesh_core::graph::RoleReadinessState::Configured;
        if let Err(err) = graph.upsert_role_incarnation(&role_record) {
            warn!(
                "handle_remote_materialize_request: failed to upsert role_record for '{}': {}",
                role_name, err
            );
            Self::reply_materialize_ready(
                &dispatcher_tx,
                local_node_id,
                source_node_id,
                &request_id,
                &guest_id,
                false,
                None,
                Some(err.to_string()),
            )
            .await;
            return;
        }
        if let Some(ts_val) = payload.get("toolset_record") {
            if ts_val.is_object() {
                if let Ok(profile) = serde_json::from_value::<
                    ansible_mesh_core::graph::ToolsetProfileRecord,
                >(ts_val.clone())
                {
                    let _ = graph.upsert_toolset_profile(&profile);
                }
            }
        }

        let mut readiness = match Self::ensure_role_materialized(
            graph,
            inboxes,
            materialization_requester.as_deref(),
            local_node_id,
            &agent_id,
            &role_name,
        )
        .await
        {
            Ok(r) => r,
            Err(err) => {
                warn!(
                    "handle_remote_materialize_request: ensure_role_materialized failed for '{}': {}",
                    role_name, err
                );
                Self::reply_materialize_ready(
                    &dispatcher_tx,
                    local_node_id,
                    source_node_id,
                    &request_id,
                    &guest_id,
                    false,
                    None,
                    Some(err.to_string()),
                )
                .await;
                return;
            }
        };

        // Spawn is async; give it a bounded window to settle to a live state
        // before answering — same 250ms cadence as HandoffPending's existing
        // retry contract.
        let mut attempts = 0;
        while matches!(
            readiness,
            ansible_mesh_core::graph::RoleReadinessState::Configured
                | ansible_mesh_core::graph::RoleReadinessState::Materializing
        ) && attempts < 20
        {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            readiness = match Self::ensure_role_materialized(
                graph,
                inboxes,
                materialization_requester.as_deref(),
                local_node_id,
                &agent_id,
                &role_name,
            )
            .await
            {
                Ok(r) => r,
                Err(err) => {
                    warn!(
                        "handle_remote_materialize_request: re-check failed for '{}': {}",
                        role_name, err
                    );
                    break;
                }
            };
            attempts += 1;
        }

        let ok = matches!(
            readiness,
            ansible_mesh_core::graph::RoleReadinessState::Routable
                | ansible_mesh_core::graph::RoleReadinessState::Materialized
                | ansible_mesh_core::graph::RoleReadinessState::ActiveInSession
        );
        info!(
            "Materialize request [{}] for role '{}' (agent '{}') settled: readiness={:?} ok={}",
            request_id, role_name, agent_id, readiness, ok
        );
        // R7: a committed STANDBY mints this move's single-use key, so the
        // origin can seal the transport's secret to this hotel alone.
        let seal_public_key = ok.then(|| crate::service::continuity::issue_seal_key(&request_id));
        Self::reply_materialize_ready_with_seal_key(
            &dispatcher_tx,
            local_node_id,
            source_node_id,
            &request_id,
            &guest_id,
            ok,
            Some(readiness.as_str().to_string()),
            None,
            seal_public_key,
        )
        .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn reply_materialize_ready(
        dispatcher_tx: &mpsc::Sender<LedgerCommand>,
        local_node_id: &str,
        dest_node_id: &str,
        request_id: &str,
        guest_id: &str,
        ok: bool,
        readiness: Option<String>,
        error: Option<String>,
    ) {
        Self::reply_materialize_ready_with_seal_key(
            dispatcher_tx,
            local_node_id,
            dest_node_id,
            request_id,
            guest_id,
            ok,
            readiness,
            error,
            None,
        )
        .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn reply_materialize_ready_with_seal_key(
        dispatcher_tx: &mpsc::Sender<LedgerCommand>,
        local_node_id: &str,
        dest_node_id: &str,
        request_id: &str,
        guest_id: &str,
        ok: bool,
        readiness: Option<String>,
        error: Option<String>,
        seal_public_key: Option<String>,
    ) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let event = EventEnvelope {
            event_id: Uuid::new_v4(),
            seq: 0,
            source_node_id: local_node_id.to_string(),
            target_node_id: Some(dest_node_id.to_string()),
            source_agent_id: local_node_id.to_string(),
            target_agent_id: None,
            kind: ansible_mesh_core::event::EventKind::MaterializeReady,
            corr_id: request_id.to_string(),
            attempt: 0,
            created_at: ts,
            expires_at: None,
            payload: ansible_mesh_core::event::EventPayload::Inline {
                data: serde_json::json!({
                    "request_id": request_id,
                    "guest_id": guest_id,
                    "ok": ok,
                    "readiness": readiness,
                    "error": error,
                    // R5: this build imports continuity bundles. An origin
                    // only sends `ContinuityImport` to a target that says so,
                    // so an older peer never receives a kind it can't parse.
                    "supports_continuity": true,
                    // R7: the public half of this move's single-use seal key.
                    "sealed_secret_public_key": seal_public_key,
                })
                .to_string(),
            },
            trace: vec![],
        };
        let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(event)).await;
    }

    /// Relocation Ceremony R5, target side: import the origin's continuity
    /// bundle and ack. Runs before the event commits, and inbound events are
    /// not deduplicated, so the import itself is idempotent per ceremony.
    pub(crate) async fn handle_remote_continuity_import(
        graph: &GraphDomain,
        dispatcher_tx: mpsc::Sender<LedgerCommand>,
        local_node_id: &str,
        source_node_id: &str,
        data: &str,
    ) {
        let payload: serde_json::Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(err) => {
                warn!("handle_remote_continuity_import: failed to parse payload: {err}");
                return;
            }
        };
        let Some(request_id) = payload.get("request_id").and_then(|v| v.as_str()) else {
            warn!("handle_remote_continuity_import: missing request_id");
            return;
        };
        let outcome = payload
            .get("bundle")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("payload carries no bundle"))
            .and_then(|raw| {
                serde_json::from_value::<crate::service::continuity::ContinuityBundle>(raw)
                    .map_err(anyhow::Error::from)
            })
            .and_then(|bundle| {
                if bundle.target_node_id != local_node_id {
                    anyhow::bail!(
                        "bundle is addressed to '{}', not this hotel '{}'",
                        bundle.target_node_id,
                        local_node_id
                    );
                }
                if bundle.origin_node_id != source_node_id {
                    anyhow::bail!(
                        "bundle claims origin '{}' but was sent by '{}'",
                        bundle.origin_node_id,
                        source_node_id
                    );
                }
                Self::peer_may_place_role(
                    graph,
                    local_node_id,
                    source_node_id,
                    &bundle.agent_id,
                    &bundle.role_name,
                )
                .map_err(anyhow::Error::msg)?;
                let summary = crate::service::continuity::import_continuity_bundle(graph, &bundle)?;
                info!(
                    "Continuity import [{}] for ceremony [{}] (agent '{}', role '{}'): {:?}",
                    request_id, bundle.ceremony_id, bundle.agent_id, bundle.role_name, summary
                );
                Ok(summary)
            });
        let (ok, summary, error) = match outcome {
            Ok(summary) => (true, serde_json::to_value(summary).ok(), None),
            Err(err) => {
                warn!("Continuity import [{}] failed: {}", request_id, err);
                (false, None, Some(err.to_string()))
            }
        };
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let event = EventEnvelope {
            event_id: Uuid::new_v4(),
            seq: 0,
            source_node_id: local_node_id.to_string(),
            target_node_id: Some(source_node_id.to_string()),
            source_agent_id: local_node_id.to_string(),
            target_agent_id: None,
            kind: ansible_mesh_core::event::EventKind::ContinuityAck,
            corr_id: request_id.to_string(),
            attempt: 0,
            created_at: ts,
            expires_at: None,
            payload: ansible_mesh_core::event::EventPayload::Inline {
                data: serde_json::json!({
                    "request_id": request_id,
                    "ok": ok,
                    "summary": summary,
                    "error": error,
                })
                .to_string(),
            },
            trace: vec![],
        };
        let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(event)).await;
    }

    /// Relocation Ceremony R5, origin side: persist the target's
    /// `ContinuityAck` so the ceremony's CONTINUITY phase can poll it.
    pub(crate) fn handle_remote_continuity_ack(graph: &GraphDomain, data: &str) {
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(data) else {
            warn!("handle_remote_continuity_ack: failed to parse payload");
            return;
        };
        let Some(request_id) = payload.get("request_id").and_then(|v| v.as_str()) else {
            warn!("handle_remote_continuity_ack: missing request_id");
            return;
        };
        if let Err(err) = graph.set_config_value(&format!("continuity_ack:{request_id}"), data) {
            warn!(
                "handle_remote_continuity_ack: failed to persist ack for '{}': {}",
                request_id, err
            );
            return;
        }
        info!(
            "Continuity ack recorded for request [{}]: {}",
            request_id, data
        );
    }

    /// Relocation Ceremony R3, source side: receive the `MaterializeReady`
    /// reply and persist it so `hotel.materialize_status` can answer a poll.
    pub(crate) fn handle_remote_materialize_ready(
        graph: &GraphDomain,
        source_node_id: &str,
        data: &str,
    ) {
        let Ok(mut payload) = serde_json::from_str::<serde_json::Value>(data) else {
            warn!("handle_remote_materialize_ready: failed to parse payload");
            return;
        };
        let Some(request_id) = payload
            .get("request_id")
            .and_then(|v| v.as_str())
            .map(str::to_string)
        else {
            warn!("handle_remote_materialize_ready: missing request_id");
            return;
        };
        let key = format!("materialize_ready:{request_id}");
        // Which hotel answered, as the batch HMAC proved it (DEF-170): the
        // origin releases a sealed secret only to the ceremony's target.
        payload["__sender"] = serde_json::json!(source_node_id);
        let stored = payload.to_string();
        if let Err(err) = graph.set_config_value(&key, &stored) {
            warn!(
                "handle_remote_materialize_ready: failed to persist status for '{}': {}",
                request_id, err
            );
            return;
        }
        info!(
            "Materialize ready recorded for request [{}]: {}",
            request_id, data
        );
    }

    // ── Training data admin handlers ──────────────────────────────────────────

    // ── ASR provider lifecycle handlers ───────────────────────────────────────

    // ── Vision provider lifecycle handlers ────────────────────────────────────

    const VISION_GUEST_ID_SUFFIX: &'static str = "model-controller-vision-01";
}

/// Pick the inbox subscriber(s) a guest-targeted task should reach.
///
/// Exact guest ids match exactly. An UNSCOPED agent id (no `:role` suffix,
/// e.g. `agent-bjork-01`) never matches a materialized incarnation
/// (`agent-bjork-01:orchestrator`), so resolve it to exactly ONE live
/// incarnation: the orchestrator when present, else the lexically first.
/// Never more than one — a task addressed to one agent must not fan out
/// (live 2026-09-05: an MCP `tools/call` reached every philote on mac-jane).
pub(super) fn select_guest_targets(live_guest_ids: &[&str], target: &str) -> Vec<String> {
    if live_guest_ids.iter().any(|id| *id == target) {
        return vec![target.to_string()];
    }
    if target.contains(':') {
        return Vec::new();
    }
    let prefix = format!("{target}:");
    let mut incarnations: Vec<&str> = live_guest_ids
        .iter()
        .copied()
        .filter(|id| id.starts_with(&prefix))
        .collect();
    incarnations.sort_unstable();
    if let Some(orchestrator) = incarnations.iter().find(|id| id.ends_with(":orchestrator")) {
        return vec![(*orchestrator).to_string()];
    }
    incarnations
        .first()
        .map(|id| vec![(*id).to_string()])
        .unwrap_or_default()
}

pub(super) fn unix_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Wire string for an autonomy posture in ConsumeAutonomyAction /
/// RecordAutonomyOutcome response payloads (matches the grant's serde
/// snake_case representation).
fn posture_str(posture: ansible_mesh_core::autonomy::AutonomyPosture) -> &'static str {
    use ansible_mesh_core::autonomy::AutonomyPosture;
    match posture {
        AutonomyPosture::ProposalOnly => "proposal_only",
        AutonomyPosture::ConfirmFirst => "confirm_first",
        AutonomyPosture::AutoWithAudit => "auto_with_audit",
    }
}

mod tool_assembly;
#[allow(unused_imports)]
use self::tool_assembly::*;
#[allow(unused_imports)]
pub(super) use self::tool_assembly::{LiveToolRunner, compose_tool_assembly};

mod cron;
#[allow(unused_imports)]
use self::cron::*;
#[allow(unused_imports)]
pub(super) use self::cron::{
    cron_admin_identity, cron_forbidden, cron_job_mutation_allowed, cron_job_owned_by,
    cron_job_visible_to, cron_owner_agent_of_guest, cron_policy_authority, strip_forged_cron_keys,
};

mod surfaces;
#[allow(unused_imports)]
use self::surfaces::*;
#[allow(unused_imports)]
pub(super) use self::surfaces::{SurfaceAttribution, handle_apply_surface_messages};

mod procedures;
#[allow(unused_imports)]
use self::procedures::*;
#[allow(unused_imports)]
pub(super) use self::procedures::{
    evaluate_procedure_trials, handle_decide_procedure_patch, handle_propose_procedure_patch,
    handle_register_procedure,
};

mod skills;
#[cfg(test)]
#[allow(unused_imports)]
pub(super) use self::skills::handle_register_skill;
#[allow(unused_imports)]
use self::skills::*;
#[allow(unused_imports)]
pub(super) use self::skills::{
    guest_owns_agent, handle_register_skill_with_origin, handle_set_skill_state,
    record_skill_admin_audit, require_skill_admin, resolve_skill_delegation, skill_admin_role,
    skill_state_label,
};

mod agent_context;
#[allow(unused_imports)]
pub(crate) use self::agent_context::REPLY_OWNER_AGENT_ID_FIELD;
#[allow(unused_imports)]
use self::agent_context::*;
#[allow(unused_imports)]
pub(super) use self::agent_context::{
    AgentTaskContext, LocalDeliveryProvenanceHint, PlacementMarkerPolicy, ToolRunnerRegistryEntry,
    agent_graph_db_path, attach_agent_graph_snapshot, infer_agent_context_for_task,
    infer_marker_strength, infer_placement_risk_level, is_non_agent_infra_role,
    is_response_like_agent_action, lookup_agent_authority_hotel, peer_agent_node_from_roster,
    placement_marker_policy,
};

mod components;

mod memory;

mod media_setup;

mod hotel_status;

#[cfg(test)]
pub(crate) mod tests;
