use crate::LedgerCommand;
use crate::service::guest_manager::GuestMaterializationRequester;
use crate::service::lease::{
    LeaseAcquireOutcome, LeaseObserver, LeaseObserverEvent, LeaseObserverEventKind, LeaseProvider,
    LeaseRenewOutcome, RuntimeLeaseRegistry,
};
use crate::vault::{SecretAccess, resolve_secret};
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::event::{EventEnvelope, EventKind, EventPayload};
use ansible_mesh_core::graph::{AbstractSkillRecord, SkillValidationState};
use ansible_mesh_core::registry::{
    CapabilityAdvertisement, ExecutionReachability, NodeRegistry, NodeStatus,
};
use ansible_mesh_core::storage::{
    ComponentManifest, GuestRecord, HotelRecord, SessionEventRecord, SessionParticipantRecord,
    SessionRecord, SessionTurnRecord,
};
use ansible_mesh_core::validation::{
    SkillDraft, apply_validation_to_record, validate_skill_layer1,
};
use ansible_mesh_core::{NodeCapabilities, NodeConstraints};
use philotic_client::{
    DesktopMembraneAgentView, DesktopMembraneGuestView, DesktopMembraneStatusView,
    DesktopMembraneTargetGuestInventoryView, DesktopMembraneTargetReachabilityView,
    DesktopMembraneTargetStatusView, DesktopMembraneTargetView, GuestIdentity, HookRoute,
    HookSubscription, IpcRequest, IpcResponse, LeaseEnvelope, LeaseStatus,
    OPERATOR_CHAT_REPLY_ROLE, OPERATOR_SURFACE_QUERY_HANDOFF_KIND,
    OPERATOR_SURFACE_QUERY_REPLY_ROLE, OPERATOR_SURFACE_QUERY_ROLE, OperatorAgentView,
    OperatorChatTurnReply, OperatorSurfaceQueryHandoff, OperatorTargetAgentInventoryView,
    OperatorTargetGuestInventoryView, OperatorTargetStatusView, PhiloticClient,
    ResponseRoutePolicyView, SubagentDelegation,
};
use std::collections::HashMap;
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, RwLock, mpsc};
use tracing::{error, info, warn};
use uuid::Uuid;

pub(crate) type InboxRegistry = Arc<Mutex<HashMap<String, Vec<RoleSubscriber>>>>;

/// Per-subagent hook routing record stored in the hotel's in-memory registry.
/// Created at SpawnSubagent time; dropped on ReleaseSubagent or lease expiry.
#[derive(Clone)]
#[allow(dead_code)]
struct SubagentHookRecord {
    /// Which persona agent spawned this subagent (for PersonaAgent route).
    persona_guest_id: String,
    /// The role the persona agent registered under (for inbox lookup).
    persona_role: String,
    /// Hook subscriptions declared by the delegation skill.
    hook_subscriptions: Vec<HookSubscription>,
    /// Where to deliver `subagent.complete`.
    completion_route: HookRoute,
    /// Where to deliver `subagent.failed`.
    failure_route: HookRoute,
}

/// Maps `subagent_guest_id` → routing record.
type SubagentHookRegistry = Arc<Mutex<HashMap<String, SubagentHookRecord>>>;

#[derive(Clone)]
pub(crate) struct RoleSubscriber {
    conn_id: Uuid,
    pub(crate) guest_id: String,
    supported_tools: Vec<String>,
    pub(crate) tx: mpsc::UnboundedSender<IpcResponse>,
}

#[cfg(not(test))]
const TELEGRAM_POLL_LEASE_TTL_SECS: u64 = 45;
#[cfg(test)]
const TELEGRAM_POLL_LEASE_TTL_SECS: u64 = 1;

#[cfg(not(test))]
const DESKTOP_MEMBRANE_LEASE_TTL_SECS: u64 = 45;
#[cfg(test)]
const DESKTOP_MEMBRANE_LEASE_TTL_SECS: u64 = 1;

#[cfg(not(test))]
const DISCORD_GATEWAY_LEASE_TTL_SECS: u64 = 45;
#[cfg(test)]
const DISCORD_GATEWAY_LEASE_TTL_SECS: u64 = 1;

#[derive(Default)]
struct SessionEnvelope {
    session_id: Option<String>,
    turn_id: Option<String>,
    primary_agent_id: Option<String>,
    source: Option<String>,
    chat_id: Option<String>,
    action: Option<String>,
    content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolRunnerRegistryEntry {
    guest_id: String,
    supported_tools: Vec<String>,
    last_seen_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AllowedIncarnation {
    incarnation_id: String,
    runner_id: Option<String>,
    hotel_id: Option<String>,
    environment_id: Option<String>,
    target_node: Option<String>,
    target_role: Option<String>,
    supported_tools: Vec<String>,
    execution_mode: String,
    availability_state: String,
    selection_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct RoutingPreferences {
    preferred_tool_runner_incarnation: Option<String>,
    preferred_tool_runner: Option<String>,
    preferred_hotel_id: Option<String>,
    preferred_environment_id: Option<String>,
}

#[derive(Debug, Clone)]
struct ParkedInboundTask {
    source_node: String,
    task_id: Uuid,
    task_json: String,
    activate_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AgentRouteResolution {
    Deliver(Option<String>),
    Park { guest_id: String },
}

pub struct IpcServer {
    socket_path: String,
    local_node_id: String,
    dispatcher_tx: mpsc::Sender<LedgerCommand>,
    graph: Arc<GraphDomain>,
    inboxes: InboxRegistry,
    parked_inbound: Arc<Mutex<HashMap<String, Vec<ParkedInboundTask>>>>,
    materialization_requester: Option<Arc<dyn GuestMaterializationRequester>>,
    telegram_poll_leases: Arc<Mutex<RuntimeLeaseRegistry>>,
    desktop_membrane_leases: Arc<Mutex<RuntimeLeaseRegistry>>,
    discord_gateway_leases: Arc<Mutex<RuntimeLeaseRegistry>>,
    subagent_leases: Arc<Mutex<RuntimeLeaseRegistry>>,
    subagent_hooks: SubagentHookRegistry,
    registry: Arc<RwLock<NodeRegistry>>,
    /// Smoke-test peer socket map: node_id → UDS socket path for direct
    /// cross-hotel task forwarding without full mesh infrastructure.
    peer_sockets: Arc<RwLock<HashMap<String, String>>>,
    muninn_config: Option<Arc<memory_core::MuninnConfig>>,
    /// Broadcast channel for hotel-wide push events (e.g. NetworkState).
    /// The sender is cloned into each `handle_client` task for forwarding.
    network_broadcast: tokio::sync::broadcast::Sender<IpcResponse>,
}

struct LoggingLeaseObserver;

impl LeaseObserver for LoggingLeaseObserver {
    fn on_event(&mut self, event: &LeaseObserverEvent) {
        match event.kind {
            LeaseObserverEventKind::Granted => info!(
                "Telegram poll lease [{}] granted to guest [{}] epoch {}.",
                event.lease.lease_scope, event.lease.owner_guest_id, event.lease.lease_epoch
            ),
            LeaseObserverEventKind::Released => info!(
                "Telegram poll lease [{}] released by guest [{}].",
                event.lease.lease_scope, event.lease.owner_guest_id
            ),
            LeaseObserverEventKind::Renewed => info!(
                "Telegram poll lease [{}] renewed by guest [{}] epoch {}.",
                event.lease.lease_scope, event.lease.owner_guest_id, event.lease.lease_epoch
            ),
            LeaseObserverEventKind::Expired => info!(
                "Dropping expired Telegram poll lease [{}] for guest [{}] epoch {}.",
                event.lease.lease_scope, event.lease.owner_guest_id, event.lease.lease_epoch
            ),
            LeaseObserverEventKind::StaleOwnerDropped => info!(
                "Dropping stale Telegram poll lease [{}] for guest [{}] epoch {}.",
                event.lease.lease_scope, event.lease.owner_guest_id, event.lease.lease_epoch
            ),
            LeaseObserverEventKind::Revoked => info!(
                "Telegram poll lease [{}] revoked for guest [{}] epoch {}.",
                event.lease.lease_scope, event.lease.owner_guest_id, event.lease.lease_epoch
            ),
        }
    }
}

struct LoggingSubagentLeaseObserver;

impl LeaseObserver for LoggingSubagentLeaseObserver {
    fn on_event(&mut self, event: &LeaseObserverEvent) {
        match event.kind {
            LeaseObserverEventKind::Granted => info!(
                "Subagent lease [{}] granted to guest [{}] epoch {}.",
                event.lease.lease_scope, event.lease.owner_guest_id, event.lease.lease_epoch
            ),
            LeaseObserverEventKind::Released => info!(
                "Subagent lease [{}] released by guest [{}].",
                event.lease.lease_scope, event.lease.owner_guest_id
            ),
            LeaseObserverEventKind::Renewed => info!(
                "Subagent lease [{}] renewed by guest [{}] epoch {}.",
                event.lease.lease_scope, event.lease.owner_guest_id, event.lease.lease_epoch
            ),
            LeaseObserverEventKind::Expired => info!(
                "Subagent lease [{}] expired for guest [{}] epoch {}.",
                event.lease.lease_scope, event.lease.owner_guest_id, event.lease.lease_epoch
            ),
            LeaseObserverEventKind::StaleOwnerDropped => info!(
                "Stale subagent lease [{}] dropped for guest [{}].",
                event.lease.lease_scope, event.lease.owner_guest_id
            ),
            LeaseObserverEventKind::Revoked => info!(
                "Subagent lease [{}] revoked for guest [{}].",
                event.lease.lease_scope, event.lease.owner_guest_id
            ),
        }
    }
}

struct LoggingDesktopMembraneLeaseObserver;

impl LeaseObserver for LoggingDesktopMembraneLeaseObserver {
    fn on_event(&mut self, event: &LeaseObserverEvent) {
        match event.kind {
            LeaseObserverEventKind::Granted => info!(
                "Desktop membrane lease [{}] granted to guest [{}] epoch {}.",
                event.lease.lease_scope, event.lease.owner_guest_id, event.lease.lease_epoch
            ),
            LeaseObserverEventKind::Released => info!(
                "Desktop membrane lease [{}] released by guest [{}].",
                event.lease.lease_scope, event.lease.owner_guest_id
            ),
            LeaseObserverEventKind::Renewed => info!(
                "Desktop membrane lease [{}] renewed by guest [{}] epoch {}.",
                event.lease.lease_scope, event.lease.owner_guest_id, event.lease.lease_epoch
            ),
            LeaseObserverEventKind::Expired => info!(
                "Desktop membrane lease [{}] expired for guest [{}] epoch {}.",
                event.lease.lease_scope, event.lease.owner_guest_id, event.lease.lease_epoch
            ),
            LeaseObserverEventKind::StaleOwnerDropped => info!(
                "Stale desktop membrane lease [{}] dropped for guest [{}].",
                event.lease.lease_scope, event.lease.owner_guest_id
            ),
            LeaseObserverEventKind::Revoked => info!(
                "Desktop membrane lease [{}] revoked for guest [{}] epoch {}.",
                event.lease.lease_scope, event.lease.owner_guest_id, event.lease.lease_epoch
            ),
        }
    }
}

impl IpcServer {
    fn telegram_poll_lease(
        lease_key: &str,
        authority_hotel: &str,
        local_node_id: &str,
        owner_guest_id: &str,
        agent_id: &str,
    ) -> LeaseEnvelope {
        LeaseEnvelope {
            lease_type: "telegram_poll".into(),
            lease_scope: lease_key.to_string(),
            authority_hotel: authority_hotel.to_string(),
            authority_component: Some("aiua".into()),
            owner_guest_id: owner_guest_id.to_string(),
            owner_hotel: Some(authority_hotel.to_string()),
            owner_component_type: Some("membrane".into()),
            lease_epoch: 0,
            lease_expires_at: 0,
            last_heartbeat_at: 0,
            status: LeaseStatus::Active,
            delegated_from: None,
            metadata: serde_json::json!({
                "agent_id": agent_id,
                "authority_node_id": local_node_id,
            }),
        }
    }

    fn discord_gateway_lease(
        lease_key: &str,
        authority_hotel: &str,
        local_node_id: &str,
        owner_guest_id: &str,
        agent_id: &str,
    ) -> LeaseEnvelope {
        LeaseEnvelope {
            lease_type: "discord_gateway".into(),
            lease_scope: lease_key.to_string(),
            authority_hotel: authority_hotel.to_string(),
            authority_component: Some("aiua".into()),
            owner_guest_id: owner_guest_id.to_string(),
            owner_hotel: Some(authority_hotel.to_string()),
            owner_component_type: Some("membrane.discord".into()),
            lease_epoch: 0,
            lease_expires_at: 0,
            last_heartbeat_at: 0,
            status: LeaseStatus::Active,
            delegated_from: None,
            metadata: serde_json::json!({
                "agent_id": agent_id,
                "authority_node_id": local_node_id,
            }),
        }
    }

    fn desktop_membrane_lease(
        lease_key: &str,
        authority_hotel: &str,
        local_node_id: &str,
        owner_guest_id: &str,
        port: u16,
    ) -> LeaseEnvelope {
        LeaseEnvelope {
            lease_type: "desktop_membrane".into(),
            lease_scope: lease_key.to_string(),
            authority_hotel: authority_hotel.to_string(),
            authority_component: Some("aiua".into()),
            owner_guest_id: owner_guest_id.to_string(),
            owner_hotel: Some(authority_hotel.to_string()),
            owner_component_type: Some("membrane.desktop".into()),
            lease_epoch: 0,
            lease_expires_at: 0,
            last_heartbeat_at: 0,
            status: LeaseStatus::Active,
            delegated_from: None,
            metadata: serde_json::json!({
                "authority_node_id": local_node_id,
                "port": port,
                "surface": "operator-desktop",
            }),
        }
    }

    fn delegated_poll_hotels(
        agent_identity: &ansible_mesh_core::storage::AgentIdentityRecord,
    ) -> Vec<String> {
        agent_identity
            .bundle_json
            .get("telegram_poll_delegate_hotels")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect()
    }

    fn pid_exists(pid: u32) -> bool {
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

    fn local_hotel_name(graph: &GraphDomain, local_node_id: &str) -> Option<String> {
        graph.list_hotels().ok().and_then(|hotels| {
            hotels
                .into_iter()
                .find(|hotel| hotel.capabilities.node_id == local_node_id)
                .map(|hotel| hotel.hotel_name)
        })
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

    async fn desktop_membrane_target_status_view(
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

    async fn desktop_membrane_target_guest_inventory_view(
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

    async fn operator_target_agent_inventory_view(
        registry: &Arc<RwLock<NodeRegistry>>,
        graph: &GraphDomain,
        local_node_id: &str,
        target_node_id: &str,
    ) -> anyhow::Result<OperatorTargetAgentInventoryView> {
        let source_hotel = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;

        if target_node_id == local_node_id {
            return Ok(OperatorTargetAgentInventoryView {
                target_node_id: target_node_id.to_string(),
                target_hotel: source_hotel.clone(),
                source_hotel,
                observation_kind: "local-canonical".into(),
                available: true,
                pending_remote_query_state: "none".into(),
                agents: Self::operator_agent_views(graph, local_node_id)?,
                note: Some("derived from the local hotel's canonical agent identities".into()),
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

        match Self::query_remote_operator_target_agents(
            graph,
            local_node_id,
            target_node_id,
            &target_hotel,
        )
        .await
        {
            Ok(view) => Ok(view),
            Err(err) => Ok(OperatorTargetAgentInventoryView {
                target_node_id: target_node_id.to_string(),
                target_hotel,
                source_hotel,
                observation_kind: "remote-query-failed".into(),
                available: false,
                pending_remote_query_state: "error".into(),
                agents: Vec::new(),
                note: Some(format!(
                    "remote target agent inventory requires a target-hotel operator query: {}",
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
        let reply = tokio::time::timeout(std::time::Duration::from_secs(1), client.recv_task())
            .await
            .map_err(|_| anyhow::anyhow!("timed out waiting for remote guest inventory reply"))??;
        let IpcResponse::InboundTask { task_json, .. } = reply else {
            anyhow::bail!("unexpected remote guest inventory reply envelope: {reply:?}");
        };
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
        let reply = tokio::time::timeout(std::time::Duration::from_secs(1), client.recv_task())
            .await
            .map_err(|_| anyhow::anyhow!("timed out waiting for remote target status reply"))??;
        let IpcResponse::InboundTask { task_json, .. } = reply else {
            anyhow::bail!("unexpected remote target status reply envelope: {reply:?}");
        };
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

    async fn query_remote_operator_target_agents(
        graph: &GraphDomain,
        local_node_id: &str,
        target_node_id: &str,
        target_hotel: &str,
    ) -> anyhow::Result<OperatorTargetAgentInventoryView> {
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
            surface: "operator.targets.agents".into(),
            request_id: Uuid::new_v4().to_string(),
            source_hotel: source_hotel.clone(),
            target_hotel: target_hotel.to_string(),
            target_node_id: target_node_id.to_string(),
            caller_kind: "operator_surface_adapter".into(),
            caller_id: local_node_id.to_string(),
            visibility_scope: "operator".into(),
            grant_scope: "default".into(),
            intent: "query target agent inventory".into(),
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
            other => anyhow::bail!("unexpected remote agent query emit response: {other:?}"),
        }
        let reply = tokio::time::timeout(std::time::Duration::from_secs(1), client.recv_task())
            .await
            .map_err(|_| anyhow::anyhow!("timed out waiting for remote target agent reply"))??;
        let IpcResponse::InboundTask { task_json, .. } = reply else {
            anyhow::bail!("unexpected remote target agent reply envelope: {reply:?}");
        };
        let view: OperatorTargetAgentInventoryView = serde_json::from_str(&task_json)?;
        if view.target_node_id != target_node_id {
            anyhow::bail!(
                "remote target agents reply target mismatch: expected [{}], got [{}]",
                target_node_id,
                view.target_node_id
            );
        }
        if view.target_hotel != target_hotel {
            anyhow::bail!(
                "remote target agents reply hotel mismatch: expected [{}], got [{}]",
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

        match client
            .send_request(IpcRequest::EmitTask {
                target_node: target_node_id.to_string(),
                target_role: "agent".into(),
                target_guest_id: Some(target_agent_id.to_string()),
                task_json: serde_json::json!({
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

    fn operator_agent_views(
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

    async fn desktop_membrane_target_views(
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

    fn target_hotel_name(graph: &GraphDomain, status: &NodeStatus, source_hotel: &str) -> String {
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

    /// Inject a membrane binding into an agent's bundle_json reflex_context.
    /// Called on successful lease acquisition so the agent's next bundle fetch
    /// reflects the active membrane without any dynamic query.
    /// Bindings are never removed on release — the agent owns its membranes until
    /// it explicitly relinquishes them (operator-pinned by design).
    fn inject_membrane_binding(
        graph: &GraphDomain,
        agent_id: &str,
        kind: &str,
        membrane_guest_id: &str,
    ) {
        let Ok(Some(mut identity)) = graph.get_agent_identity(agent_id) else {
            return;
        };
        let new_binding = serde_json::json!({
            "kind": kind,
            "membrane_guest_id": membrane_guest_id,
        });
        let bindings = identity
            .bundle_json
            .get_mut("reflex_context")
            .and_then(|rc| rc.get_mut("membrane_bindings"))
            .and_then(|b| b.as_array_mut());
        if let Some(arr) = bindings {
            arr.retain(|b| {
                !(b.get("kind").and_then(|k| k.as_str()) == Some(kind)
                    && b.get("membrane_guest_id").and_then(|g| g.as_str())
                        == Some(membrane_guest_id))
            });
            arr.push(new_binding);
        } else {
            if let Some(obj) = identity.bundle_json.as_object_mut() {
                let rc = obj
                    .entry("reflex_context")
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(rc_obj) = rc.as_object_mut() {
                    rc_obj.insert(
                        "membrane_bindings".to_string(),
                        serde_json::json!([new_binding]),
                    );
                }
            }
        }
        if let Err(e) = graph.upsert_agent_identity(&identity) {
            warn!(
                "Failed to inject membrane binding [{kind}/{membrane_guest_id}] for agent [{agent_id}]: {e}"
            );
        }
    }

    fn hotel_may_poll_for_agent(
        agent_identity: &ansible_mesh_core::storage::AgentIdentityRecord,
        local_hotel_name: &str,
    ) -> bool {
        agent_identity.authority_hotel == local_hotel_name
            || Self::delegated_poll_hotels(agent_identity)
                .iter()
                .any(|hotel| hotel == local_hotel_name)
    }

    fn telegram_poll_lease_is_expired(lease: &LeaseEnvelope) -> bool {
        lease.lease_expires_at <= unix_ts()
    }

    fn telegram_poll_lease_owner_is_live(
        graph: &GraphDomain,
        local_node_id: &str,
        owner_guest_id: &str,
    ) -> bool {
        let Some(local_hotel_name) = Self::local_hotel_name(graph, local_node_id) else {
            return true;
        };
        // Only consider active guests — inactive/legacy records (e.g. old per-agent membrane
        // seats marked is_active=false during stale cleanup) must not trigger a false-dead result.
        let Ok(guests) = graph.list_guests(&local_hotel_name, true) else {
            return true;
        };
        if guests.is_empty() {
            return true;
        }
        let Some(owner_guest) = guests
            .into_iter()
            .find(|guest| guest.guest_id == owner_guest_id)
        else {
            // Guest not in DB (or not active) — it's an IPC-registered seat (e.g. multi-seat
            // membrane task), not a seeded subprocess. Trust the TTL-based expiry instead.
            return true;
        };
        let Some(pid_text) = owner_guest.active_pid.as_deref() else {
            // Guest is in DB but has no PID — it was never started or its PID was cleared
            // on shutdown. Treat as dead so the lease is dropped.
            // (IPC-only seats that are *not in the DB at all* are handled by the guard above.)
            return false;
        };
        let Ok(pid) = pid_text.parse::<u32>() else {
            return false;
        };
        Self::pid_exists(pid)
    }

    fn drop_stale_telegram_poll_lease_if_needed(
        guard: &mut RuntimeLeaseRegistry,
        graph: &GraphDomain,
        local_node_id: &str,
        lease_key: &str,
    ) {
        let mut observer = LoggingLeaseObserver;
        let _ = guard.drop_if_stale(
            lease_key,
            |existing| {
                Self::telegram_poll_lease_is_expired(existing)
                    || !Self::telegram_poll_lease_owner_is_live(
                        graph,
                        local_node_id,
                        &existing.owner_guest_id,
                    )
            },
            &mut observer,
        );
    }

    fn drop_stale_discord_gateway_lease_if_needed(
        guard: &mut RuntimeLeaseRegistry,
        graph: &GraphDomain,
        local_node_id: &str,
        lease_key: &str,
    ) {
        let mut observer = LoggingLeaseObserver;
        let _ = guard.drop_if_stale(
            lease_key,
            |existing| {
                Self::discord_gateway_lease_is_expired(existing)
                    || !Self::discord_gateway_lease_owner_is_live(
                        graph,
                        local_node_id,
                        &existing.owner_guest_id,
                    )
            },
            &mut observer,
        );
    }

    fn discord_gateway_lease_is_expired(lease: &LeaseEnvelope) -> bool {
        lease.lease_expires_at > 0 && unix_ts() > lease.lease_expires_at
    }

    fn discord_gateway_lease_owner_is_live(
        graph: &GraphDomain,
        local_node_id: &str,
        owner_guest_id: &str,
    ) -> bool {
        Self::telegram_poll_lease_owner_is_live(graph, local_node_id, owner_guest_id)
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
            socket_path: socket_path.into(),
            local_node_id: local_node_id.into(),
            dispatcher_tx,
            graph,
            inboxes: Arc::new(Mutex::new(HashMap::new())),
            parked_inbound: Arc::new(Mutex::new(HashMap::new())),
            materialization_requester: None,
            telegram_poll_leases: Arc::new(Mutex::new(RuntimeLeaseRegistry::default())),
            desktop_membrane_leases: Arc::new(Mutex::new(RuntimeLeaseRegistry::default())),
            discord_gateway_leases: Arc::new(Mutex::new(RuntimeLeaseRegistry::default())),
            subagent_leases: Arc::new(Mutex::new(RuntimeLeaseRegistry::default())),
            subagent_hooks: Arc::new(Mutex::new(HashMap::new())),
            registry: Arc::new(RwLock::new(NodeRegistry::new())),
            peer_sockets: Arc::new(RwLock::new(HashMap::new())),
            muninn_config: None,
            network_broadcast,
        }
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

    pub fn with_registry(mut self, registry: Arc<RwLock<NodeRegistry>>) -> Self {
        self.registry = registry;
        self
    }

    pub(crate) fn inboxes(&self) -> InboxRegistry {
        self.inboxes.clone()
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let path = Path::new(&self.socket_path);

        if path.exists() {
            std::fs::remove_file(path)?;
        }

        let listener = UnixListener::bind(path)?;
        info!("Hotel Front Desk (UDS) listening on: {}", self.socket_path);

        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let dispatcher = self.dispatcher_tx.clone();
                    let local_node_id = self.local_node_id.clone();
                    let graph = self.graph.clone();
                    let inboxes = self.inboxes.clone();
                    let parked_inbound = self.parked_inbound.clone();
                    let materialization_requester = self.materialization_requester.clone();
                    let telegram_poll_leases = self.telegram_poll_leases.clone();
                    let desktop_membrane_leases = self.desktop_membrane_leases.clone();
                    let discord_gateway_leases = self.discord_gateway_leases.clone();
                    let subagent_leases = self.subagent_leases.clone();
                    let subagent_hooks = self.subagent_hooks.clone();
                    let registry = self.registry.clone();
                    let peer_sockets = self.peer_sockets.clone();
                    let muninn_config = self.muninn_config.clone();
                    let network_broadcast_rx = self.network_broadcast.subscribe();
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_client(
                            stream,
                            local_node_id,
                            dispatcher,
                            graph,
                            inboxes,
                            parked_inbound,
                            materialization_requester,
                            telegram_poll_leases,
                            desktop_membrane_leases,
                            discord_gateway_leases,
                            subagent_leases,
                            subagent_hooks,
                            registry,
                            peer_sockets,
                            muninn_config,
                            network_broadcast_rx,
                        )
                        .await
                        {
                            error!("IPC client connection error: {}", e);
                        }
                    });
                }
                Err(e) => {
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
        materialization_requester: Option<Arc<dyn GuestMaterializationRequester>>,
        telegram_poll_leases: Arc<Mutex<RuntimeLeaseRegistry>>,
        desktop_membrane_leases: Arc<Mutex<RuntimeLeaseRegistry>>,
        discord_gateway_leases: Arc<Mutex<RuntimeLeaseRegistry>>,
        subagent_leases: Arc<Mutex<RuntimeLeaseRegistry>>,
        subagent_hooks: SubagentHookRegistry,
        registry: Arc<RwLock<NodeRegistry>>,
        peer_sockets: Arc<RwLock<HashMap<String, String>>>,
        muninn_config: Option<Arc<memory_core::MuninnConfig>>,
        network_broadcast_rx: tokio::sync::broadcast::Receiver<IpcResponse>,
    ) -> anyhow::Result<()> {
        let conn_id = Uuid::new_v4();
        let (mut reader, mut writer) = stream.into_split();
        let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<IpcResponse>();
        let write_task = tokio::spawn(async move {
            while let Some(response) = outbound_rx.recv().await {
                let res_bytes = match serde_json::to_vec(&response) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        error!("Failed to serialize IPC response: {}", e);
                        continue;
                    }
                };
                if let Err(e) = Self::write_frame(&mut writer, &res_bytes).await {
                    return Err(e);
                }
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

        let mut subscribed_roles = Vec::new();
        let mut current_identity: Option<GuestIdentity> = None;
        loop {
            match Self::read_frame(&mut reader).await {
                Ok(None) => {
                    Self::remove_subscriptions(&inboxes, conn_id, &subscribed_roles).await;
                    Self::remove_telegram_poll_leases(&telegram_poll_leases, conn_id).await;
                    Self::remove_desktop_membrane_leases(&desktop_membrane_leases, conn_id).await;
                    Self::remove_discord_gateway_leases(&discord_gateway_leases, conn_id).await;
                    let _ = write_task.await;
                    return Ok(());
                }
                Ok(Some(frame)) => match serde_json::from_slice::<IpcRequest>(&frame) {
                    Ok(IpcRequest::FetchMemoryConfig) => {
                        let config_json = muninn_config
                            .as_deref()
                            .and_then(|cfg| serde_json::to_string(cfg).ok());
                        info!(
                            has_config = config_json.is_some(),
                            "FetchMemoryConfig handled"
                        );
                        let _ = outbound_tx.send(IpcResponse::MemoryConfig { config_json });
                    }
                    Ok(req) => {
                        let mut follow_up_responses = Vec::new();
                        let response = Self::process_request(
                            req,
                            &local_node_id,
                            &dispatcher_tx,
                            graph.as_ref(),
                            &inboxes,
                            &parked_inbound,
                            materialization_requester.as_deref(),
                            &telegram_poll_leases,
                            &desktop_membrane_leases,
                            &discord_gateway_leases,
                            &subagent_leases,
                            &subagent_hooks,
                            &registry,
                            &peer_sockets,
                            conn_id,
                            &outbound_tx,
                            &mut subscribed_roles,
                            &mut current_identity,
                            &mut follow_up_responses,
                        )
                        .await;
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
                    let _ = write_task.await;
                    return Err(e.into());
                }
            }
        }
    }

    async fn add_subscription(
        inboxes: &InboxRegistry,
        role: &str,
        conn_id: Uuid,
        guest_id: &str,
        supported_tools: &[String],
        tx: &mpsc::UnboundedSender<IpcResponse>,
        subscribed_roles: &mut Vec<String>,
    ) {
        let mut guard = inboxes.lock().await;
        let entry = guard.entry(role.to_string()).or_default();
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

    async fn remove_telegram_poll_leases(
        telegram_poll_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        conn_id: Uuid,
    ) {
        let mut guard = telegram_poll_leases.lock().await;
        let scopes: Vec<String> = guard
            .active_leases_for_connection(conn_id)
            .into_iter()
            .map(|lease| lease.lease_scope)
            .collect();
        let mut observer = LoggingLeaseObserver;
        for scope in scopes {
            let _ = guard.release(&scope, conn_id, &mut observer);
        }
    }

    async fn remove_desktop_membrane_leases(
        desktop_membrane_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        conn_id: Uuid,
    ) {
        let mut guard = desktop_membrane_leases.lock().await;
        let scopes: Vec<String> = guard
            .active_leases_for_connection(conn_id)
            .into_iter()
            .map(|lease| lease.lease_scope)
            .collect();
        let mut observer = LoggingDesktopMembraneLeaseObserver;
        for scope in scopes {
            let _ = guard.release(&scope, conn_id, &mut observer);
        }
    }

    async fn remove_discord_gateway_leases(
        discord_gateway_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        conn_id: Uuid,
    ) {
        let mut guard = discord_gateway_leases.lock().await;
        let scopes: Vec<String> = guard
            .active_leases_for_connection(conn_id)
            .into_iter()
            .map(|lease| lease.lease_scope)
            .collect();
        let mut observer = LoggingLeaseObserver;
        for scope in scopes {
            let _ = guard.release(&scope, conn_id, &mut observer);
        }
    }

    // ── Subagent lease helpers ──────────────────────────────────────────────

    fn subagent_lease_scope(subagent_guest_id: &str) -> String {
        format!("subagent:{}", subagent_guest_id)
    }

    fn subagent_lease_candidate(
        local_node_id: &str,
        subagent_guest_id: &str,
        persona_guest_id: &str,
        ttl_seconds: u64,
    ) -> LeaseEnvelope {
        LeaseEnvelope {
            lease_type: "subagent".into(),
            lease_scope: Self::subagent_lease_scope(subagent_guest_id),
            authority_hotel: local_node_id.to_string(),
            authority_component: Some("aiua".into()),
            owner_guest_id: subagent_guest_id.to_string(),
            owner_hotel: Some(local_node_id.to_string()),
            owner_component_type: Some("philote-worker".into()),
            lease_epoch: 0,
            lease_expires_at: 0,
            last_heartbeat_at: 0,
            status: LeaseStatus::Active,
            delegated_from: Some(persona_guest_id.to_string()),
            metadata: serde_json::json!({
                "persona_guest_id": persona_guest_id,
                "ttl_seconds": ttl_seconds,
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_spawn_subagent(
        local_node_id: &str,
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        subagent_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        subagent_hooks: &SubagentHookRegistry,
        conn_id: Uuid,
        identity: &GuestIdentity,
        session_id: &str,
        delegation: SubagentDelegation,
    ) -> IpcResponse {
        let subagent_guest_id = Uuid::new_v4().to_string();
        let ttl = delegation.lease_terms.ttl_seconds;

        // 1. Register the subagent guest in the context graph so the materializer
        //    can spawn and supervise it.
        let config_json = serde_json::json!({
            "command": "philote-worker",
            "env": {
                "PHILOTIC_AGENT_ID":        &subagent_guest_id,
                "PHILOTIC_SESSION_ID":      session_id,
                "PHILOTIC_PARENT_GUEST_ID": identity.guest_id,
                "PHILOTIC_SUBAGENT_KIND":   &delegation.subagent_kind,
            },
        });
        let guest_record = ansible_mesh_core::storage::GuestRecord {
            hotel_name: local_node_id.to_string(),
            guest_id: subagent_guest_id.clone(),
            role: delegation.subagent_kind.clone(),
            config_json: config_json.to_string(),
            is_active: true,
            active_pid: None,
            last_active_at: None,
        };
        if let Err(e) = graph.seed_guests(local_node_id, &[guest_record]) {
            return IpcResponse::error(
                "spawn_subagent",
                "SUBAGENT_GUEST_REGISTER_FAILED",
                e.to_string(),
            );
        }

        // 2. Acquire a subagent lease on behalf of the new guest.
        let candidate = Self::subagent_lease_candidate(
            local_node_id,
            &subagent_guest_id,
            &identity.guest_id,
            ttl,
        );
        let lease = {
            let mut guard = subagent_leases.lock().await;
            let mut observer = LoggingSubagentLeaseObserver;
            match guard.acquire(conn_id, candidate, ttl, unix_ts(), &mut observer) {
                LeaseAcquireOutcome::Granted(lease) => lease,
                LeaseAcquireOutcome::Denied(existing) => {
                    return IpcResponse::error(
                        "spawn_subagent",
                        "SUBAGENT_LEASE_DENIED",
                        format!(
                            "Subagent lease scope already held by epoch {}",
                            existing.lease_epoch
                        ),
                    );
                }
            }
        };

        // 3. Register hook subscriptions and routing for this subagent.
        {
            let mut guard = subagent_hooks.lock().await;
            guard.insert(
                subagent_guest_id.clone(),
                SubagentHookRecord {
                    persona_guest_id: identity.guest_id.clone(),
                    persona_role: identity.role.clone(),
                    hook_subscriptions: delegation.hook_subscriptions.clone(),
                    completion_route: delegation.completion_route.clone(),
                    failure_route: delegation.failure_route.clone(),
                },
            );
        }

        // 4. Trigger materialization so the worker binary starts up.
        if let Some(requester) = materialization_requester {
            match requester.ensure_guest_active(&subagent_guest_id).await {
                Ok(activated) => {
                    if activated {
                        info!(
                            "Subagent guest [{}] materialization triggered for session [{}].",
                            subagent_guest_id, session_id
                        );
                    } else {
                        info!(
                            "Subagent guest [{}] was already active (session [{}]).",
                            subagent_guest_id, session_id
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        "Subagent guest [{}] materialization failed: {}. Lease granted; worker may self-start.",
                        subagent_guest_id, e
                    );
                }
            }
        } else {
            info!(
                "No materialization requester configured; subagent [{}] must connect independently.",
                subagent_guest_id
            );
        }

        // 5. Eagerly notify persona's inbox that spawning is underway.
        let notification_task_id = Uuid::new_v4();
        let notification_json = serde_json::json!({
            "kind": "subagent_spawned",
            "subagent_guest_id": subagent_guest_id,
            "session_id": session_id,
            "lease_epoch": lease.lease_epoch,
            "lease_expires_at": lease.lease_expires_at,
        })
        .to_string();
        Self::deliver_inbound_task(
            inboxes,
            local_node_id,
            &identity.role,
            Some(&identity.guest_id),
            notification_task_id,
            notification_json,
        )
        .await;

        info!(
            "SpawnSubagent complete: subagent_guest_id=[{}] session=[{}] kind=[{}] ttl={}s.",
            subagent_guest_id, session_id, delegation.subagent_kind, ttl
        );

        IpcResponse::SpawnSubagentOk {
            subagent_guest_id,
            confirmed_lease: lease,
        }
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

    pub(crate) async fn deliver_inbound_task(
        inboxes: &InboxRegistry,
        source_node: &str,
        target_role: &str,
        target_guest_id: Option<&str>,
        task_id: Uuid,
        task_json: String,
    ) {
        let subscribers = {
            let guard = inboxes.lock().await;
            let role_subscribers = guard.get(target_role).cloned().unwrap_or_default();
            match target_guest_id {
                Some(guest_id) => role_subscribers
                    .into_iter()
                    .filter(|subscriber| subscriber.guest_id == guest_id)
                    .collect(),
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
            return;
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
            if subscriber.tx.send(response.clone()).is_err() {
                warn!(
                    "Failed to deliver inbound task {} to local subscriber role='{}' guest='{}'. Removing stale inbox subscription.",
                    task_id, target_role, subscriber.guest_id
                );
                stale.push(subscriber.conn_id);
            }
        }

        if !stale.is_empty() {
            let mut guard = inboxes.lock().await;
            if let Some(entries) = guard.get_mut(target_role) {
                entries.retain(|subscriber| !stale.contains(&subscriber.conn_id));
            }
        }
    }

    fn configured_local_guest_exists(
        graph: &GraphDomain,
        local_node_id: &str,
        guest_id: &str,
    ) -> bool {
        let Some(local_hotel_name) = Self::local_hotel_name(graph, local_node_id) else {
            return false;
        };
        graph
            .list_guests(&local_hotel_name, false)
            .map(|guests| guests.into_iter().any(|guest| guest.guest_id == guest_id))
            .unwrap_or(false)
    }

    async fn resolve_agent_route(
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        local_node_id: &str,
        target_role: &str,
        target_guest_id: Option<String>,
        task_json: &str,
    ) -> AgentRouteResolution {
        if target_role != "agent" || target_guest_id.is_some() {
            return AgentRouteResolution::Deliver(target_guest_id);
        }
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(task_json) else {
            return AgentRouteResolution::Deliver(None);
        };
        let session_id = payload
            .get("session_id")
            .and_then(serde_json::Value::as_str);
        let Some(session_id) = session_id else {
            return AgentRouteResolution::Deliver(None);
        };
        let session = graph.get_session(session_id).ok().flatten();
        let Some(session) = session else {
            return AgentRouteResolution::Deliver(None);
        };

        let live_agent_guests: Vec<String> = {
            let guard = inboxes.lock().await;
            guard
                .get(target_role)
                .into_iter()
                .flatten()
                .map(|subscriber| subscriber.guest_id.clone())
                .collect()
        };

        let is_registered = |guest_id: &str| live_agent_guests.iter().any(|live| live == guest_id);

        if let Some(active_guest_id) = session.active_incarnation_id.clone() {
            if is_registered(&active_guest_id) {
                return AgentRouteResolution::Deliver(Some(active_guest_id));
            }

            if let Some(orchestrator_guest_id) =
                Self::resolve_orchestrator_guest_id(graph, &session, &live_agent_guests)
            {
                warn!(
                    "Active incarnation [{}] is not registered for session [{}]; falling back to orchestrator guest [{}].",
                    active_guest_id, session_id, orchestrator_guest_id
                );
                return AgentRouteResolution::Deliver(Some(orchestrator_guest_id));
            }

            if Self::configured_local_guest_exists(graph, local_node_id, &active_guest_id) {
                info!(
                    "Active incarnation [{}] is not registered for session [{}]; parking inbound and requesting materialization.",
                    active_guest_id, session_id
                );
                return AgentRouteResolution::Park {
                    guest_id: active_guest_id,
                };
            }

            warn!(
                "Active incarnation [{}] is not registered for session [{}], and no live orchestrator fallback was found.",
                active_guest_id, session_id
            );
            return AgentRouteResolution::Deliver(Some(active_guest_id));
        }

        let orchestrator_guest_id =
            Self::resolve_orchestrator_guest_id(graph, &session, &live_agent_guests);
        if let Some(orchestrator_guest_id) = orchestrator_guest_id {
            info!(
                "Session [{}] has no active incarnation; routing inbound task to orchestrator guest [{}].",
                session_id, orchestrator_guest_id
            );
            return AgentRouteResolution::Deliver(Some(orchestrator_guest_id));
        }

        if let Some(agent_id) = session.primary_agent_id.as_deref() {
            if let Ok(Some(role_record)) = graph.get_role_incarnation(agent_id, "orchestrator") {
                if Self::configured_local_guest_exists(graph, local_node_id, &role_record.guest_id)
                {
                    info!(
                        "Session [{}] has no active incarnation and no live orchestrator; parking inbound for orchestrator guest [{}] while materializing.",
                        session_id, role_record.guest_id
                    );
                    return AgentRouteResolution::Park {
                        guest_id: role_record.guest_id,
                    };
                }
            }
        }

        AgentRouteResolution::Deliver(None)
    }

    fn resolve_orchestrator_guest_id(
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

        live_agent_guests
            .iter()
            .find(|guest_id| guest_id.ends_with(":orchestrator"))
            .cloned()
    }

    fn update_session_active_incarnation(
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
        Ok(())
    }

    fn resolve_role_guest_id(
        graph: &GraphDomain,
        session_id: &str,
        role_name: &str,
    ) -> anyhow::Result<String> {
        let Some(session) = graph.get_session(session_id)? else {
            anyhow::bail!("session [{}] not found", session_id);
        };
        let Some(agent_id) = session.primary_agent_id else {
            anyhow::bail!("session [{}] has no primary_agent_id", session_id);
        };
        let Some(role_record) = graph.get_role_incarnation(&agent_id, role_name)? else {
            anyhow::bail!(
                "role [{}] is not configured for agent [{}]",
                role_name,
                agent_id
            );
        };
        Ok(role_record.guest_id)
    }

    async fn queue_or_deliver_guest_task(
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        parked_inbound: &Arc<Mutex<HashMap<String, Vec<ParkedInboundTask>>>>,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        local_node_id: &str,
        target_role: &str,
        guest_id: &str,
        task_id: Uuid,
        task_json: String,
        activate_session_id: Option<String>,
    ) -> anyhow::Result<bool> {
        let is_live = {
            let guard = inboxes.lock().await;
            guard
                .get(target_role)
                .into_iter()
                .flatten()
                .any(|subscriber| subscriber.guest_id == guest_id)
        };

        if is_live {
            if let Some(session_id) = activate_session_id.as_deref() {
                Self::update_session_active_incarnation(graph, session_id, guest_id)?;
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
            return Ok(true);
        }

        {
            let mut guard = parked_inbound.lock().await;
            guard
                .entry(guest_id.to_string())
                .or_default()
                .push(ParkedInboundTask {
                    source_node: local_node_id.to_string(),
                    task_id,
                    task_json,
                    activate_session_id,
                });
        }

        if let Some(requester) = materialization_requester {
            requester.ensure_guest_active(guest_id).await?;
        } else {
            warn!(
                "Task {} parked for guest [{}], but no materialization requester is configured.",
                task_id, guest_id
            );
        }
        Ok(false)
    }

    pub(crate) async fn deliver_event_envelope(
        inboxes: &InboxRegistry,
        event: &EventEnvelope,
    ) -> bool {
        match (&event.kind, &event.target_agent_id, &event.payload) {
            (
                EventKind::TaskInvoke | EventKind::TaskResult,
                Some(target_role),
                EventPayload::Inline { data },
            ) => {
                Self::deliver_inbound_task(
                    inboxes,
                    &event.source_node_id,
                    target_role,
                    None,
                    event.event_id,
                    data.clone(),
                )
                .await;
                true
            }
            _ => false,
        }
    }

    async fn process_request(
        req: IpcRequest,
        local_node_id: &str,
        dispatcher_tx: &mpsc::Sender<LedgerCommand>,
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        parked_inbound: &Arc<Mutex<HashMap<String, Vec<ParkedInboundTask>>>>,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        telegram_poll_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        desktop_membrane_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        discord_gateway_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        subagent_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        subagent_hooks: &SubagentHookRegistry,
        registry: &Arc<RwLock<NodeRegistry>>,
        peer_sockets: &Arc<RwLock<HashMap<String, String>>>,
        conn_id: Uuid,
        outbound_tx: &mpsc::UnboundedSender<IpcResponse>,
        subscribed_roles: &mut Vec<String>,
        current_identity: &mut Option<GuestIdentity>,
        follow_up_responses: &mut Vec<IpcResponse>,
    ) -> IpcResponse {
        match req {
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
            } => {
                info!("AddVaultEntry requested: {}", vault_name);
                match Self::handle_add_vault_entry(graph, vault_name, plaintext, allowed_roles) {
                    Ok(secret_ref) => IpcResponse::success(
                        "vault",
                        Some(serde_json::json!({ "secret_ref": secret_ref })),
                    ),
                    Err(e) => IpcResponse::error("vault", "VAULT_ERROR", e.to_string()),
                }
            }
            IpcRequest::PublishMessage {
                target_role,
                payload,
            } => {
                info!("PublishMessage for role: {}", target_role);
                let task_id = Uuid::new_v4();
                let payload_json = payload.to_string();
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
            IpcRequest::CreateTask {
                target_role,
                payload,
            } => {
                info!("CreateTask for role: {}", target_role);
                let task_id = Uuid::new_v4();
                let payload_json = payload.to_string();
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
            } => {
                info!("FailTask for: {} ({}): {}", task_id, error_code, reason);
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
                IpcResponse::success("sub", None)
            }
            IpcRequest::AcquireTelegramPollLease {
                lease_key,
                agent_id,
            } => {
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "telegram_poll_lease",
                        "LEASE_UNREGISTERED",
                        "guest must register before acquiring a Telegram poll lease",
                    );
                };
                let Some(agent_identity) = graph.get_agent_identity(&agent_id).ok().flatten()
                else {
                    return IpcResponse::error(
                        "telegram_poll_lease",
                        "LEASE_AGENT_UNKNOWN",
                        format!("no agent identity found for [{}]", agent_id),
                    );
                };
                let Some(local_hotel_name) = Self::local_hotel_name(graph, local_node_id) else {
                    return IpcResponse::error(
                        "telegram_poll_lease",
                        "LEASE_AUTHORITY_UNKNOWN",
                        format!(
                            "current hotel authority could not be resolved for node [{}]",
                            local_node_id
                        ),
                    );
                };
                if !Self::hotel_may_poll_for_agent(&agent_identity, &local_hotel_name) {
                    return IpcResponse::error(
                        "telegram_poll_lease",
                        "LEASE_FOREIGN_AUTHORITY",
                        format!(
                            "agent [{}] is owned by hotel [{}], and hotel [{}] is not in its telegram_poll_delegate_hotels",
                            agent_id, agent_identity.authority_hotel, local_hotel_name
                        ),
                    );
                }

                let mut guard = telegram_poll_leases.lock().await;
                Self::drop_stale_telegram_poll_lease_if_needed(
                    &mut guard,
                    graph,
                    local_node_id,
                    &lease_key,
                );
                let candidate = Self::telegram_poll_lease(
                    &lease_key,
                    &local_hotel_name,
                    local_node_id,
                    &identity.guest_id,
                    &agent_id,
                );
                let mut observer = LoggingLeaseObserver;
                match guard.acquire(
                    conn_id,
                    candidate,
                    TELEGRAM_POLL_LEASE_TTL_SECS,
                    unix_ts(),
                    &mut observer,
                ) {
                    LeaseAcquireOutcome::Granted(lease) => {
                        Self::inject_membrane_binding(
                            graph,
                            &agent_id,
                            "telegram",
                            &identity.guest_id,
                        );
                        IpcResponse::TelegramPollLease {
                            granted: true,
                            lease: Some(lease),
                        }
                    }
                    LeaseAcquireOutcome::Denied(lease) => {
                        info!(
                            "Telegram poll lease [{}] denied for guest [{}]; held by [{}] epoch {}.",
                            lease.lease_scope,
                            identity.guest_id,
                            lease.owner_guest_id,
                            lease.lease_epoch
                        );
                        IpcResponse::TelegramPollLease {
                            granted: false,
                            lease: Some(lease),
                        }
                    }
                }
            }
            IpcRequest::GetTelegramPollLeaseOwner { lease_key } => {
                let mut guard = telegram_poll_leases.lock().await;
                Self::drop_stale_telegram_poll_lease_if_needed(
                    &mut guard,
                    graph,
                    local_node_id,
                    &lease_key,
                );
                if let Some(existing) = guard.inspect(&lease_key) {
                    IpcResponse::TelegramPollLeaseStatus {
                        active: true,
                        lease: Some(existing),
                    }
                } else {
                    IpcResponse::TelegramPollLeaseStatus {
                        active: false,
                        lease: None,
                    }
                }
            }
            IpcRequest::RenewTelegramPollLease {
                lease_key,
                agent_id,
                lease_epoch,
            } => {
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "telegram_poll_lease",
                        "LEASE_UNREGISTERED",
                        "guest must register before renewing a Telegram poll lease",
                    );
                };
                let Some(agent_identity) = graph.get_agent_identity(&agent_id).ok().flatten()
                else {
                    return IpcResponse::error(
                        "telegram_poll_lease",
                        "LEASE_AGENT_UNKNOWN",
                        format!("no agent identity found for [{}]", agent_id),
                    );
                };
                let Some(local_hotel_name) = Self::local_hotel_name(graph, local_node_id) else {
                    return IpcResponse::error(
                        "telegram_poll_lease",
                        "LEASE_AUTHORITY_UNKNOWN",
                        format!(
                            "current hotel authority could not be resolved for node [{}]",
                            local_node_id
                        ),
                    );
                };
                if !Self::hotel_may_poll_for_agent(&agent_identity, &local_hotel_name) {
                    return IpcResponse::error(
                        "telegram_poll_lease",
                        "LEASE_FOREIGN_AUTHORITY",
                        format!(
                            "agent [{}] is owned by hotel [{}], and hotel [{}] is not in its telegram_poll_delegate_hotels",
                            agent_id, agent_identity.authority_hotel, local_hotel_name
                        ),
                    );
                }

                let mut guard = telegram_poll_leases.lock().await;
                Self::drop_stale_telegram_poll_lease_if_needed(
                    &mut guard,
                    graph,
                    local_node_id,
                    &lease_key,
                );
                let mut observer = LoggingLeaseObserver;
                match guard.renew(
                    &lease_key,
                    conn_id,
                    lease_epoch,
                    TELEGRAM_POLL_LEASE_TTL_SECS,
                    unix_ts(),
                    &mut observer,
                ) {
                    LeaseRenewOutcome::Renewed(lease) => IpcResponse::TelegramPollLease {
                        granted: true,
                        lease: Some(lease),
                    },
                    LeaseRenewOutcome::Lost(lease) => {
                        if let Some(ref lease) = lease {
                            info!(
                                "Telegram poll lease [{}] renew denied for guest [{}]; held by [{}] epoch {}.",
                                lease.lease_scope,
                                identity.guest_id,
                                lease.owner_guest_id,
                                lease.lease_epoch
                            );
                        }
                        IpcResponse::TelegramPollLease {
                            granted: false,
                            lease,
                        }
                    }
                }
            }
            IpcRequest::AcquireDesktopMembraneLease { lease_key, port } => {
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "desktop_membrane_lease",
                        "LEASE_UNREGISTERED",
                        "guest must register before acquiring a desktop membrane lease",
                    );
                };
                let Some(local_hotel_name) = Self::local_hotel_name(graph, local_node_id) else {
                    return IpcResponse::error(
                        "desktop_membrane_lease",
                        "LEASE_AUTHORITY_UNKNOWN",
                        format!(
                            "current hotel authority could not be resolved for node [{}]",
                            local_node_id
                        ),
                    );
                };

                let candidate = Self::desktop_membrane_lease(
                    &lease_key,
                    &local_hotel_name,
                    local_node_id,
                    &identity.guest_id,
                    port,
                );
                let mut guard = desktop_membrane_leases.lock().await;
                let mut observer = LoggingDesktopMembraneLeaseObserver;
                match guard.acquire(
                    conn_id,
                    candidate,
                    DESKTOP_MEMBRANE_LEASE_TTL_SECS,
                    unix_ts(),
                    &mut observer,
                ) {
                    LeaseAcquireOutcome::Granted(lease) => IpcResponse::DesktopMembraneLease {
                        desktop_granted: true,
                        desktop_lease: Some(lease),
                    },
                    LeaseAcquireOutcome::Denied(lease) => {
                        info!(
                            "Desktop membrane lease [{}] denied for guest [{}]; held by [{}] epoch {}.",
                            lease.lease_scope,
                            identity.guest_id,
                            lease.owner_guest_id,
                            lease.lease_epoch
                        );
                        IpcResponse::DesktopMembraneLease {
                            desktop_granted: false,
                            desktop_lease: Some(lease),
                        }
                    }
                }
            }
            IpcRequest::GetDesktopMembraneLeaseOwner { lease_key } => {
                let guard = desktop_membrane_leases.lock().await;
                if let Some(existing) = guard.inspect(&lease_key) {
                    IpcResponse::DesktopMembraneLeaseStatus {
                        desktop_active: true,
                        desktop_lease: Some(existing),
                    }
                } else {
                    IpcResponse::DesktopMembraneLeaseStatus {
                        desktop_active: false,
                        desktop_lease: None,
                    }
                }
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
            IpcRequest::QueryOperatorTargets => {
                match Self::desktop_membrane_target_views(registry, graph, local_node_id).await {
                    Ok(operator_targets) => IpcResponse::OperatorTargetsView { operator_targets },
                    Err(err) => IpcResponse::error(
                        "operator_targets",
                        "OPERATOR_TARGETS_ERROR",
                        err.to_string(),
                    ),
                }
            }
            IpcRequest::QueryOperatorTargetStatus { target_node_id } => {
                match Self::desktop_membrane_target_status_view(
                    registry,
                    graph,
                    local_node_id,
                    &target_node_id,
                )
                .await
                {
                    Ok(operator_target_status) => IpcResponse::OperatorTargetStatusView {
                        operator_target_status,
                    },
                    Err(err) => IpcResponse::error(
                        "operator_target_status",
                        "OPERATOR_TARGET_STATUS_ERROR",
                        err.to_string(),
                    ),
                }
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
            IpcRequest::QueryOperatorTargetGuests { target_node_id } => {
                match Self::desktop_membrane_target_guest_inventory_view(
                    registry,
                    graph,
                    local_node_id,
                    &target_node_id,
                )
                .await
                {
                    Ok(operator_target_guests) => IpcResponse::OperatorTargetGuestsView {
                        operator_target_guests,
                    },
                    Err(err) => IpcResponse::error(
                        "operator_target_guests",
                        "OPERATOR_TARGET_GUESTS_ERROR",
                        err.to_string(),
                    ),
                }
            }
            IpcRequest::QueryOperatorTargetAgents { target_node_id } => {
                match Self::operator_target_agent_inventory_view(
                    registry,
                    graph,
                    local_node_id,
                    &target_node_id,
                )
                .await
                {
                    Ok(operator_target_agents) => IpcResponse::OperatorTargetAgentsView {
                        operator_target_agents,
                    },
                    Err(err) => IpcResponse::error(
                        "operator_target_agents",
                        "OPERATOR_TARGET_AGENTS_ERROR",
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
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "desktop_membrane_lease",
                        "LEASE_UNREGISTERED",
                        "guest must register before renewing a desktop membrane lease",
                    );
                };

                let mut guard = desktop_membrane_leases.lock().await;
                let mut observer = LoggingDesktopMembraneLeaseObserver;
                match guard.renew(
                    &lease_key,
                    conn_id,
                    lease_epoch,
                    DESKTOP_MEMBRANE_LEASE_TTL_SECS,
                    unix_ts(),
                    &mut observer,
                ) {
                    LeaseRenewOutcome::Renewed(lease) => IpcResponse::DesktopMembraneLease {
                        desktop_granted: true,
                        desktop_lease: Some(lease),
                    },
                    LeaseRenewOutcome::Lost(lease) => {
                        if let Some(ref lease) = lease {
                            info!(
                                "Desktop membrane lease [{}] renew denied for guest [{}]; held by [{}] epoch {}.",
                                lease.lease_scope,
                                identity.guest_id,
                                lease.owner_guest_id,
                                lease.lease_epoch
                            );
                        }
                        IpcResponse::DesktopMembraneLease {
                            desktop_granted: false,
                            desktop_lease: lease,
                        }
                    }
                }
            }
            IpcRequest::ReleaseDesktopMembraneLease { lease_key } => {
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "desktop_membrane_lease",
                        "LEASE_UNREGISTERED",
                        "guest must register before releasing a desktop membrane lease",
                    );
                };

                let mut guard = desktop_membrane_leases.lock().await;
                let mut observer = LoggingDesktopMembraneLeaseObserver;
                match guard.inspect(&lease_key) {
                    Some(existing) if existing.owner_guest_id == identity.guest_id => {
                        if guard.release(&lease_key, conn_id, &mut observer).is_some() {
                            IpcResponse::success("desktop_membrane_lease_release", None)
                        } else {
                            IpcResponse::error(
                                "desktop_membrane_lease_release",
                                "LEASE_NOT_OWNER",
                                format!(
                                    "guest [{}] does not hold the active connection for lease [{}]",
                                    identity.guest_id, lease_key
                                ),
                            )
                        }
                    }
                    Some(existing) => IpcResponse::error(
                        "desktop_membrane_lease_release",
                        "LEASE_NOT_OWNER",
                        format!(
                            "guest [{}] cannot release lease [{}] owned by [{}]",
                            identity.guest_id, lease_key, existing.owner_guest_id
                        ),
                    ),
                    None => IpcResponse::success("desktop_membrane_lease_release", None),
                }
            }
            IpcRequest::AcquireDiscordGatewayLease {
                lease_key,
                agent_id,
            } => {
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "discord_gateway_lease",
                        "LEASE_UNREGISTERED",
                        "guest must register before acquiring a Discord gateway lease",
                    );
                };
                let Some(agent_identity) = graph.get_agent_identity(&agent_id).ok().flatten()
                else {
                    return IpcResponse::error(
                        "discord_gateway_lease",
                        "LEASE_AGENT_UNKNOWN",
                        format!("no agent identity found for [{}]", agent_id),
                    );
                };
                let Some(local_hotel_name) = Self::local_hotel_name(graph, local_node_id) else {
                    return IpcResponse::error(
                        "discord_gateway_lease",
                        "LEASE_AUTHORITY_UNKNOWN",
                        format!(
                            "current hotel authority could not be resolved for node [{}]",
                            local_node_id
                        ),
                    );
                };
                if !Self::hotel_may_poll_for_agent(&agent_identity, &local_hotel_name) {
                    return IpcResponse::error(
                        "discord_gateway_lease",
                        "LEASE_FOREIGN_AUTHORITY",
                        format!(
                            "agent [{}] is owned by hotel [{}] and hotel [{}] is not authorized",
                            agent_id, agent_identity.authority_hotel, local_hotel_name
                        ),
                    );
                }

                let mut guard = discord_gateway_leases.lock().await;
                Self::drop_stale_discord_gateway_lease_if_needed(
                    &mut guard,
                    graph,
                    local_node_id,
                    &lease_key,
                );
                let candidate = Self::discord_gateway_lease(
                    &lease_key,
                    &local_hotel_name,
                    local_node_id,
                    &identity.guest_id,
                    &agent_id,
                );
                let mut observer = LoggingLeaseObserver;
                match guard.acquire(
                    conn_id,
                    candidate,
                    DISCORD_GATEWAY_LEASE_TTL_SECS,
                    unix_ts(),
                    &mut observer,
                ) {
                    LeaseAcquireOutcome::Granted(lease) => {
                        Self::inject_membrane_binding(
                            graph,
                            &agent_id,
                            "discord_text",
                            &identity.guest_id,
                        );
                        IpcResponse::DiscordGatewayLease {
                            granted: true,
                            lease: Some(lease),
                        }
                    }
                    LeaseAcquireOutcome::Denied(lease) => {
                        info!(
                            "Discord gateway lease [{}] denied for guest [{}]; held by [{}] epoch {}.",
                            lease.lease_scope,
                            identity.guest_id,
                            lease.owner_guest_id,
                            lease.lease_epoch
                        );
                        IpcResponse::DiscordGatewayLease {
                            granted: false,
                            lease: Some(lease),
                        }
                    }
                }
            }
            IpcRequest::GetDiscordGatewayLeaseOwner { lease_key } => {
                let mut guard = discord_gateway_leases.lock().await;
                Self::drop_stale_discord_gateway_lease_if_needed(
                    &mut guard,
                    graph,
                    local_node_id,
                    &lease_key,
                );
                if let Some(existing) = guard.inspect(&lease_key) {
                    IpcResponse::DiscordGatewayLeaseStatus {
                        active: true,
                        lease: Some(existing),
                    }
                } else {
                    IpcResponse::DiscordGatewayLeaseStatus {
                        active: false,
                        lease: None,
                    }
                }
            }
            IpcRequest::RenewDiscordGatewayLease {
                lease_key,
                agent_id,
                lease_epoch,
            } => {
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "discord_gateway_lease",
                        "LEASE_UNREGISTERED",
                        "guest must register before renewing a Discord gateway lease",
                    );
                };
                let Some(agent_identity) = graph.get_agent_identity(&agent_id).ok().flatten()
                else {
                    return IpcResponse::error(
                        "discord_gateway_lease",
                        "LEASE_AGENT_UNKNOWN",
                        format!("no agent identity found for [{}]", agent_id),
                    );
                };
                let Some(local_hotel_name) = Self::local_hotel_name(graph, local_node_id) else {
                    return IpcResponse::error(
                        "discord_gateway_lease",
                        "LEASE_AUTHORITY_UNKNOWN",
                        format!(
                            "current hotel authority could not be resolved for node [{}]",
                            local_node_id
                        ),
                    );
                };
                if !Self::hotel_may_poll_for_agent(&agent_identity, &local_hotel_name) {
                    return IpcResponse::error(
                        "discord_gateway_lease",
                        "LEASE_FOREIGN_AUTHORITY",
                        format!(
                            "agent [{}] is owned by hotel [{}] and hotel [{}] is not authorized",
                            agent_id, agent_identity.authority_hotel, local_hotel_name
                        ),
                    );
                }

                let mut guard = discord_gateway_leases.lock().await;
                Self::drop_stale_discord_gateway_lease_if_needed(
                    &mut guard,
                    graph,
                    local_node_id,
                    &lease_key,
                );
                let mut observer = LoggingLeaseObserver;
                match guard.renew(
                    &lease_key,
                    conn_id,
                    lease_epoch,
                    DISCORD_GATEWAY_LEASE_TTL_SECS,
                    unix_ts(),
                    &mut observer,
                ) {
                    LeaseRenewOutcome::Renewed(lease) => IpcResponse::DiscordGatewayLease {
                        granted: true,
                        lease: Some(lease),
                    },
                    LeaseRenewOutcome::Lost(lease) => {
                        if let Some(ref lease) = lease {
                            info!(
                                "Discord gateway lease [{}] renew denied for guest [{}]; held by [{}] epoch {}.",
                                lease.lease_scope,
                                identity.guest_id,
                                lease.owner_guest_id,
                                lease.lease_epoch
                            );
                        }
                        IpcResponse::DiscordGatewayLease {
                            granted: false,
                            lease,
                        }
                    }
                }
            }
            IpcRequest::ReleaseDiscordGatewayLease { lease_key } => {
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "discord_gateway_lease",
                        "LEASE_UNREGISTERED",
                        "guest must register before releasing a Discord gateway lease",
                    );
                };

                let mut guard = discord_gateway_leases.lock().await;
                let mut observer = LoggingLeaseObserver;
                match guard.inspect(&lease_key) {
                    Some(existing) if existing.owner_guest_id == identity.guest_id => {
                        if guard.release(&lease_key, conn_id, &mut observer).is_some() {
                            IpcResponse::success("discord_gateway_lease_release", None)
                        } else {
                            IpcResponse::error(
                                "discord_gateway_lease_release",
                                "LEASE_NOT_OWNER",
                                format!(
                                    "guest [{}] does not hold the active connection for lease [{}]",
                                    identity.guest_id, lease_key
                                ),
                            )
                        }
                    }
                    Some(existing) => IpcResponse::error(
                        "discord_gateway_lease_release",
                        "LEASE_NOT_OWNER",
                        format!(
                            "guest [{}] cannot release lease [{}] owned by [{}]",
                            identity.guest_id, lease_key, existing.owner_guest_id
                        ),
                    ),
                    None => IpcResponse::success("discord_gateway_lease_release", None),
                }
            }
            IpcRequest::ReleaseTelegramPollLease { lease_key } => {
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "telegram_poll_lease",
                        "LEASE_UNREGISTERED",
                        "guest must register before releasing a Telegram poll lease",
                    );
                };

                let mut guard = telegram_poll_leases.lock().await;
                let mut observer = LoggingLeaseObserver;
                match guard.inspect(&lease_key) {
                    Some(existing) if existing.owner_guest_id == identity.guest_id => {
                        if guard.release(&lease_key, conn_id, &mut observer).is_some() {
                            IpcResponse::success("telegram_poll_lease_release", None)
                        } else {
                            IpcResponse::error(
                                "telegram_poll_lease_release",
                                "LEASE_NOT_OWNER",
                                format!(
                                    "guest [{}] does not hold the active connection for lease [{}]",
                                    identity.guest_id, lease_key
                                ),
                            )
                        }
                    }
                    Some(existing) => IpcResponse::error(
                        "telegram_poll_lease_release",
                        "LEASE_NOT_OWNER",
                        format!(
                            "guest [{}] cannot release lease [{}] owned by [{}]",
                            identity.guest_id, lease_key, existing.owner_guest_id
                        ),
                    ),
                    None => IpcResponse::success("telegram_poll_lease_release", None),
                }
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
                let route_resolution = Self::resolve_agent_route(
                    graph,
                    inboxes,
                    local_node_id,
                    &target_role,
                    target_guest_id.clone(),
                    &task_json,
                )
                .await;
                let resolved_target_guest_id = match &route_resolution {
                    AgentRouteResolution::Deliver(guest_id) => guest_id.clone(),
                    AgentRouteResolution::Park { guest_id } => Some(guest_id.clone()),
                };
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
                    payload: EventPayload::Inline {
                        data: task_json.clone(),
                    },
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
                            Self::deliver_inbound_task(
                                inboxes,
                                local_node_id,
                                &target_role,
                                target_guest_id.as_deref(),
                                task_id,
                                task_json,
                            )
                            .await;
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
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "handoff_to_role",
                        "HANDOFF_UNREGISTERED",
                        "guest must register before requesting a handoff",
                    );
                };
                if identity.role != "agent" {
                    return IpcResponse::error(
                        "handoff_to_role",
                        "HANDOFF_FORBIDDEN",
                        "only agent guests may initiate role handoff",
                    );
                }

                let target_guest_id = {
                    let resolved = match Self::resolve_role_guest_id(graph, &session_id, &role_name)
                    {
                        Ok(guest_id) => guest_id,
                        Err(err) => {
                            return IpcResponse::error(
                                "handoff_to_role",
                                "HANDOFF_ROLE_UNKNOWN",
                                err.to_string(),
                            );
                        }
                    };
                    // Single-process philote registers as the base agent_id. If the specific
                    // role guest is not live but the base agent is, deliver the handoff bundle
                    // to the base agent directly rather than parking it forever.
                    let is_live = {
                        let guard = inboxes.lock().await;
                        guard
                            .get("agent")
                            .into_iter()
                            .flatten()
                            .any(|s| s.guest_id == resolved)
                    };
                    if !is_live {
                        let base_id = resolved.split(':').next().unwrap_or(&resolved).to_string();
                        let base_is_live = {
                            let guard = inboxes.lock().await;
                            guard
                                .get("agent")
                                .into_iter()
                                .flatten()
                                .any(|s| s.guest_id == base_id)
                        };
                        if base_is_live {
                            warn!(
                                "Role guest [{}] is not registered; delivering handoff bundle to base agent [{}].",
                                resolved, base_id
                            );
                            base_id
                        } else {
                            resolved
                        }
                    } else {
                        resolved
                    }
                };
                let task_id = Uuid::new_v4();

                // Construct the SessionControl envelope for durable mesh ledger tracking
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                let event = EventEnvelope {
                    event_id: task_id,
                    seq: 0,
                    source_node_id: local_node_id.to_string(),
                    target_node_id: Some(local_node_id.to_string()),
                    source_agent_id: identity.guest_id.clone(),
                    target_agent_id: Some(target_guest_id.clone()),
                    kind: ansible_mesh_core::event::EventKind::SessionControl,
                    corr_id: session_id.clone(),
                    attempt: 0,
                    created_at: ts,
                    expires_at: None,
                    payload: ansible_mesh_core::event::EventPayload::Inline {
                        data: serde_json::json!({
                            "action": "session.handoff",
                            "session_id": session_id,
                            "role_name": role_name,
                            "handoff_bundle": handoff_bundle,
                        })
                        .to_string(),
                    },
                    trace: vec![],
                };
                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(event)).await;

                let task_json = serde_json::json!({
                    "action": "handoff_bundle",
                    "session_id": session_id,
                    "handoff_bundle": handoff_bundle,
                })
                .to_string();

                match Self::queue_or_deliver_guest_task(
                    graph,
                    inboxes,
                    parked_inbound,
                    materialization_requester,
                    local_node_id,
                    "agent",
                    &target_guest_id,
                    task_id,
                    task_json,
                    Some(session_id),
                )
                .await
                {
                    Ok(active) => IpcResponse::HandoffAck {
                        handoff_guest_id: target_guest_id,
                        became_active: active,
                    },
                    Err(err) => IpcResponse::error(
                        "handoff_to_role",
                        "HANDOFF_DELIVERY_FAILED",
                        err.to_string(),
                    ),
                }
            }
            IpcRequest::HandoffBack {
                session_id,
                summary,
                return_to,
            } => {
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "handoff_back",
                        "HANDOFF_UNREGISTERED",
                        "guest must register before handing back",
                    );
                };
                if identity.role != "agent" {
                    return IpcResponse::error(
                        "handoff_back",
                        "HANDOFF_FORBIDDEN",
                        "only agent guests may initiate role handoff",
                    );
                }
                let target_role = return_to.unwrap_or_else(|| "orchestrator".into());
                let target_guest_id =
                    match Self::resolve_role_guest_id(graph, &session_id, &target_role) {
                        Ok(guest_id) => guest_id,
                        Err(err) => {
                            return IpcResponse::error(
                                "handoff_back",
                                "HANDOFF_ROLE_UNKNOWN",
                                err.to_string(),
                            );
                        }
                    };
                let task_id = Uuid::new_v4();

                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                let event = EventEnvelope {
                    event_id: task_id,
                    seq: 0,
                    source_node_id: local_node_id.to_string(),
                    target_node_id: Some(local_node_id.to_string()),
                    source_agent_id: identity.guest_id.clone(),
                    target_agent_id: Some(target_guest_id.clone()),
                    kind: ansible_mesh_core::event::EventKind::SessionControl,
                    corr_id: session_id.clone(),
                    attempt: 0,
                    created_at: ts,
                    expires_at: None,
                    payload: ansible_mesh_core::event::EventPayload::Inline {
                        data: serde_json::json!({
                            "action": "session.handoff_back",
                            "session_id": session_id,
                            "summary": summary,
                            "from_incarnation_id": identity.guest_id,
                        })
                        .to_string(),
                    },
                    trace: vec![],
                };
                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(event)).await;

                let task_json = serde_json::json!({
                    "action": "handoff_return",
                    "session_id": session_id,
                    "summary": summary,
                    "from_incarnation_id": identity.guest_id,
                })
                .to_string();
                match Self::queue_or_deliver_guest_task(
                    graph,
                    inboxes,
                    parked_inbound,
                    materialization_requester,
                    local_node_id,
                    "agent",
                    &target_guest_id,
                    task_id,
                    task_json,
                    Some(session_id),
                )
                .await
                {
                    Ok(active) => IpcResponse::HandoffBackAck {
                        handoff_guest_id: target_guest_id,
                        became_active: active,
                    },
                    Err(err) => IpcResponse::error(
                        "handoff_back",
                        "HANDOFF_DELIVERY_FAILED",
                        err.to_string(),
                    ),
                }
            }
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

                // Build the mesh envelope for TaskInvoke
                let env = EventEnvelope {
                    event_id: delegation_id,
                    seq: 0,
                    source_node_id: local_node_id.to_string(),
                    target_node_id: None, // Router will resolve node for agent_id
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
                if identity.role != "agent" {
                    return IpcResponse::error(
                        "spawn_subagent",
                        "SUBAGENT_FORBIDDEN",
                        "only agent guests may request subagent delegation",
                    );
                }

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
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "renew_subagent_lease",
                        "SUBAGENT_UNREGISTERED",
                        "guest must register before renewing subagent leases",
                    );
                };
                let scope = Self::subagent_lease_scope(&subagent_guest_id);
                let ttl = {
                    // Read the registered TTL from hook record, fall back to default.
                    let guard = subagent_hooks.lock().await;
                    // We can't store ttl in SubagentHookRecord easily without adding a field —
                    // for now use the default 300s. Block F will wire this from the skill record.
                    let _ = guard.get(&subagent_guest_id);
                    300u64
                };
                let outcome = {
                    let mut guard = subagent_leases.lock().await;
                    let mut observer = LoggingSubagentLeaseObserver;
                    guard.renew(&scope, conn_id, lease_epoch, ttl, unix_ts(), &mut observer)
                };
                match outcome {
                    LeaseRenewOutcome::Renewed(lease) => IpcResponse::SubagentLeaseRenewed {
                        subagent_guest_id,
                        new_epoch: lease.lease_epoch,
                        expires_at: lease.lease_expires_at,
                    },
                    LeaseRenewOutcome::Lost(current) => IpcResponse::error(
                        "renew_subagent_lease",
                        "SUBAGENT_LEASE_LOST",
                        format!(
                            "Lease renewal failed for subagent [{}] at epoch {} by guest [{}]: {:?}",
                            subagent_guest_id,
                            lease_epoch,
                            identity.guest_id,
                            current.map(|l| l.lease_epoch),
                        ),
                    ),
                }
            }
            IpcRequest::ReleaseSubagent { subagent_guest_id } => {
                let scope = Self::subagent_lease_scope(&subagent_guest_id);
                let released = {
                    let mut guard = subagent_leases.lock().await;
                    let mut observer = LoggingSubagentLeaseObserver;
                    guard.release(&scope, conn_id, &mut observer)
                };
                // Clean up hook registry regardless of whether release succeeded.
                subagent_hooks.lock().await.remove(&subagent_guest_id);

                // Deactivate the guest record so the supervisor does not respawn
                // the worker process after it exits.
                if let Err(e) = graph.set_guest_active(local_node_id, &subagent_guest_id, false) {
                    warn!(
                        "ReleaseSubagent: failed to deactivate guest [{}] in graph: {}",
                        subagent_guest_id, e
                    );
                }

                if released.is_some() {
                    info!("Subagent lease released for guest [{}].", subagent_guest_id);
                    IpcResponse::success(
                        "release_subagent",
                        Some(serde_json::json!({ "subagent_guest_id": subagent_guest_id })),
                    )
                } else {
                    // Already expired or not owned — idempotent success.
                    IpcResponse::success(
                        "release_subagent",
                        Some(serde_json::json!({
                            "subagent_guest_id": subagent_guest_id,
                            "note": "lease not found or already released",
                        })),
                    )
                }
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
                // Worker calls this to acknowledge it has received and accepted the lease.
                // Verify the lease exists and mark acknowledgement in the hook record metadata.
                let scope = Self::subagent_lease_scope(&subagent_guest_id);
                let lease = subagent_leases.lock().await.inspect(&scope);
                if lease.is_some() {
                    info!("Subagent guest [{}] acknowledged lease.", subagent_guest_id);
                    IpcResponse::success(
                        "accept_subagent_lease",
                        Some(serde_json::json!({ "subagent_guest_id": subagent_guest_id })),
                    )
                } else {
                    IpcResponse::error(
                        "accept_subagent_lease",
                        "SUBAGENT_LEASE_NOT_FOUND",
                        format!("No active lease for subagent guest [{}]", subagent_guest_id),
                    )
                }
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
            } => {
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "configure_role",
                        "CONFIGURE_UNREGISTERED",
                        "guest must register before configuring roles",
                    );
                };
                let is_management = identity.role == "management";
                // Check the agent's active persona role, not the IPC process type.
                // The IPC process type is always "agent" for philote — what matters
                // is the session-level persona role ("orchestrator") passed explicitly.
                if !is_management && calling_role != "orchestrator" {
                    return IpcResponse::error(
                        "configure_role",
                        "CONFIGURE_FORBIDDEN",
                        "only agents operating in the orchestrator persona or management guests may configure role incarnations",
                    );
                }
                if !is_management && !identity.guest_id.starts_with(&agent_id) {
                    return IpcResponse::error(
                        "configure_role",
                        "CONFIGURE_FORBIDDEN",
                        "orchestrator guests may only configure roles for their own agent identity",
                    );
                }
                // Determine whether the calling guest has admin authority by looking up their
                // role incarnation record. Only admin roles may update operator-owned records.
                let caller_is_admin = if is_management {
                    true
                } else {
                    let caller_agent_id = identity
                        .guest_id
                        .strip_suffix(&format!(":{}", identity.role))
                        .unwrap_or(&identity.guest_id);
                    graph
                        .get_role_incarnation(caller_agent_id, &identity.role)
                        .ok()
                        .flatten()
                        .map(|r| r.is_admin)
                        .unwrap_or(false)
                };

                // The orchestrator role record is operator-owned. Only admin roles may update it.
                if role_name == "orchestrator" && !caller_is_admin {
                    return IpcResponse::error(
                        "configure_role",
                        "CONFIGURE_FORBIDDEN",
                        "the orchestrator role record is operator-owned; only admin roles may update it",
                    );
                }

                // Prevent privilege escalation: only admin roles may create other admin roles.
                if is_admin && !caller_is_admin {
                    return IpcResponse::error(
                        "configure_role",
                        "CONFIGURE_FORBIDDEN",
                        "only admin roles may create other admin roles",
                    );
                }

                let record = ansible_mesh_core::graph::RoleIncarnationRecord {
                    agent_id: agent_id.clone(),
                    role_name: role_name.clone(),
                    guest_id,
                    toolset_profile,
                    role_identity_addendum,
                    role_manifest,
                    is_admin,
                    inactive_ttl_seconds,
                    turn_loop_config: ansible_mesh_core::graph::TurnLoopConfig {
                        iteration_cap,
                        approval_policy,
                        model_profile,
                        context_window_policy,
                        loop_script: None,
                    },
                };

                if let Err(e) = graph.upsert_role_incarnation(&record) {
                    warn!("Failed to persist role config [{}]: {}", role_name, e);
                    return IpcResponse::error(
                        "configure_role",
                        "ROLE_PERSIST_FAILED",
                        format!("Failed to persist role config: {e}"),
                    );
                }

                info!(
                    agent_id = %agent_id,
                    role_name = %role_name,
                    "Role incarnation configured via IPC"
                );

                IpcResponse::ConfigureRoleOk { role_name }
            }
            IpcRequest::RegisterSkill {
                skill_name,
                description,
                subagent_kind,
                goal,
                allowed_tools,
                allowed_classes: _,
                hook_subscriptions: _,
                completion_route: _,
                failure_route: _,
                idle_behavior: _,
                lease_terms: _,
            } => {
                // Translate to a SkillDraft and run Layer 1 structural validation.
                let draft = SkillDraft {
                    skill_name: skill_name.clone(),
                    description: description.clone(),
                    subagent_kind,
                    goal_template: goal,
                    allowed_tools: allowed_tools.clone(),
                    allowed_skills: vec![],
                    iteration_budget: None,
                    lease_terms: ansible_mesh_core::validation::SkillLeaseTerms::default(),
                    hook_subscriptions: vec![],
                    completion_route: ansible_mesh_core::validation::HookRoute::default(),
                    failure_route: ansible_mesh_core::validation::HookRoute::default(),
                    completion_contract:
                        ansible_mesh_core::validation::SkillCompletionContract::default(),
                    // An empty object satisfies the "must be a JSON object" invariant.
                    field_sources: serde_json::json!({}),
                };

                let validation_result = validate_skill_layer1(&draft);

                let mut record = AbstractSkillRecord {
                    skill_name: skill_name.clone(),
                    description,
                    implied_tools: allowed_tools,
                    ..Default::default()
                };
                apply_validation_to_record(&mut record, validation_result);

                if let Err(e) = graph.upsert_abstract_skill(&record) {
                    warn!("Failed to persist skill [{}]: {}", skill_name, e);
                    return IpcResponse::error(
                        "register_skill",
                        "SKILL_PERSIST_FAILED",
                        format!("Failed to persist skill: {e}"),
                    );
                }

                let (state_str, errors) = match &record.validation_state {
                    SkillValidationState::Validated => ("validated".to_string(), vec![]),
                    SkillValidationState::Invalid { errors } => {
                        ("invalid".to_string(), errors.clone())
                    }
                    SkillValidationState::Draft => ("draft".to_string(), vec![]),
                    SkillValidationState::Registered => ("registered".to_string(), vec![]),
                    SkillValidationState::Suspended { reason } => {
                        ("suspended".to_string(), vec![reason.clone()])
                    }
                    SkillValidationState::Deprecated => ("deprecated".to_string(), vec![]),
                };

                info!(
                    skill_name = %skill_name,
                    validation_state = %state_str,
                    "Skill registered via IPC"
                );

                IpcResponse::SkillRegistered {
                    skill_name,
                    validation_state: state_str,
                    validation_errors: errors,
                }
            }
            IpcRequest::PatchAgentBundle {
                agent_id,
                persona_name,
                soul_text,
                identity_text,
                user_context_text,
                system_prompt,
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
            IpcRequest::AssignSkill {
                agent_id,
                role_name,
                skill_name,
            } => {
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "assign_skill",
                        "ASSIGN_UNREGISTERED",
                        "guest must register before assigning skills",
                    );
                };
                let is_management = identity.role == "management";
                if !is_management && identity.role != "orchestrator" {
                    return IpcResponse::error(
                        "assign_skill",
                        "ASSIGN_FORBIDDEN",
                        "only orchestrator or management guests may assign skills",
                    );
                }
                if !is_management && !identity.guest_id.starts_with(&agent_id) {
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
                let Some(identity) = current_identity.as_ref() else {
                    return IpcResponse::error(
                        "revoke_skill",
                        "REVOKE_UNREGISTERED",
                        "guest must register before revoking skills",
                    );
                };
                let is_management = identity.role == "management";
                if !is_management && identity.role != "orchestrator" {
                    return IpcResponse::error(
                        "revoke_skill",
                        "REVOKE_FORBIDDEN",
                        "only orchestrator or management guests may revoke skills",
                    );
                }
                if !is_management && !identity.guest_id.starts_with(&agent_id) {
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
            IpcRequest::ListSkills {} => {
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
                        let state_str = match &s.validation_state {
                            SkillValidationState::Validated => "validated",
                            SkillValidationState::Invalid { .. } => "invalid",
                            SkillValidationState::Draft => "draft",
                            SkillValidationState::Registered => "registered",
                            SkillValidationState::Suspended { .. } => "suspended",
                            SkillValidationState::Deprecated => "deprecated",
                        };
                        serde_json::json!({
                            "skill_name": s.skill_name,
                            "description": s.description,
                            "implied_tools": s.implied_tools,
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
            IpcRequest::FetchMemoryConfig => IpcResponse::error(
                "memory",
                "UNREACHABLE",
                "FetchMemoryConfig dispatched early",
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
            // Resource broker seam (agent-resource-broker). No callers yet;
            // hotel-side broker will be wired in the demand-derived-materialization seam.
            IpcRequest::ResourceRequest(_req) => IpcResponse::error(
                "resource_request",
                "NOT_IMPLEMENTED",
                "resource broker not yet wired",
            ),
            IpcRequest::ResourceReleased(_rel) => IpcResponse::error(
                "resource_released",
                "NOT_IMPLEMENTED",
                "resource broker not yet wired",
            ),
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
            IpcRequest::RestartComponent { guest_id } => {
                Self::handle_restart_component(
                    graph,
                    materialization_requester,
                    local_node_id,
                    &guest_id,
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
                registry.write().await.update_node(caps, vec![ad], None);
                if let Some(path) = socket_path {
                    peer_sockets.write().await.insert(node_id, path);
                }
                IpcResponse::success("seed_remote_incarnation", None)
            }
            // ── Cron scheduler ──────────────────────────────────────────────
            IpcRequest::RegisterCronJob { job } => {
                info!("RegisterCronJob: id={} role={}", job.id, job.target_role);
                match graph.upsert_cron_job(&job) {
                    Ok(_) => {
                        Self::broadcast_cron_sync_upsert(dispatcher_tx, local_node_id, &job).await;
                        IpcResponse::success(
                            "register_cron_job",
                            Some(serde_json::json!({ "job_id": job.id })),
                        )
                    }
                    Err(e) => IpcResponse::Error(format!("RegisterCronJob failed: {e}")),
                }
            }
            IpcRequest::RemoveCronJob { job_id } => {
                info!("RemoveCronJob: id={}", job_id);
                match graph.remove_cron_job(&job_id) {
                    Ok(_) => {
                        Self::broadcast_cron_sync_remove(dispatcher_tx, local_node_id, &job_id)
                            .await;
                        IpcResponse::success("remove_cron_job", None)
                    }
                    Err(e) => IpcResponse::Error(format!("RemoveCronJob failed: {e}")),
                }
            }
            IpcRequest::ListCronJobs => match graph.list_cron_jobs() {
                Ok(jobs) => IpcResponse::CronJobList { jobs },
                Err(e) => IpcResponse::Error(format!("ListCronJobs failed: {e}")),
            },
            IpcRequest::EnableCronJob { job_id } => match graph.get_cron_job(&job_id) {
                Ok(Some(mut job)) => {
                    job.enabled = true;
                    match graph.upsert_cron_job(&job) {
                        Ok(_) => {
                            Self::broadcast_cron_sync_upsert(dispatcher_tx, local_node_id, &job)
                                .await;
                            IpcResponse::success("enable_cron_job", None)
                        }
                        Err(e) => IpcResponse::Error(format!("EnableCronJob failed: {e}")),
                    }
                }
                Ok(None) => IpcResponse::Error(format!("cron job not found: {job_id}")),
                Err(e) => IpcResponse::Error(format!("EnableCronJob failed: {e}")),
            },
            IpcRequest::DisableCronJob { job_id } => match graph.get_cron_job(&job_id) {
                Ok(Some(mut job)) => {
                    job.enabled = false;
                    match graph.upsert_cron_job(&job) {
                        Ok(_) => {
                            Self::broadcast_cron_sync_upsert(dispatcher_tx, local_node_id, &job)
                                .await;
                            IpcResponse::success("disable_cron_job", None)
                        }
                        Err(e) => IpcResponse::Error(format!("DisableCronJob failed: {e}")),
                    }
                }
                Ok(None) => IpcResponse::Error(format!("cron job not found: {job_id}")),
                Err(e) => IpcResponse::Error(format!("DisableCronJob failed: {e}")),
            },
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
                });
                let task_json = paracrine_task.to_string();
                let task_id = Uuid::new_v4();

                // Check if the target role has a live inbox subscriber.
                let has_subscriber = {
                    let guard = inboxes.lock().await;
                    guard.get(&role).is_some_and(|subs| !subs.is_empty())
                };

                if has_subscriber {
                    Self::deliver_inbound_task(
                        inboxes,
                        local_node_id,
                        &role,
                        None,
                        task_id,
                        task_json,
                    )
                    .await;
                } else {
                    // No live subscriber — look up the role incarnation, park the task
                    // under the incarnation's guest_id, and trigger materialization of
                    // a dedicated role-philote so it can connect and flush the park.
                    match graph.find_role_incarnation_by_name(&role) {
                        Ok(Some(inc)) => {
                            // The philote that handles this role incarnation registers
                            // with guest_id = "{agent_id}:{role_name}".
                            let role_guest_id = inc.guest_id.clone();

                            // Park the task — flushed when the role philote connects.
                            {
                                let mut guard = parked_inbound.lock().await;
                                guard.entry(role_guest_id.clone()).or_default().push(
                                    ParkedInboundTask {
                                        source_node: local_node_id.to_string(),
                                        task_id,
                                        task_json,
                                        activate_session_id: None,
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

                                // Create the guest record if it doesn't already exist.
                                if graph
                                    .get_guest(&hotel_name, &hotel_guest_id)
                                    .ok()
                                    .flatten()
                                    .is_none()
                                {
                                    // Derive socket path from hotel record.
                                    let socket_path = graph
                                        .list_hotels()
                                        .ok()
                                        .and_then(|hs| {
                                            hs.into_iter()
                                                .find(|h| h.capabilities.node_id == local_node_id)
                                                .map(|h| h.ipc_socket_path)
                                        })
                                        .unwrap_or_default();

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

                                // Trigger materialization.
                                if let Some(requester) = materialization_requester {
                                    match requester.ensure_guest_active(&hotel_guest_id).await {
                                        Ok(true) => info!(
                                            "Role-philote [{}] materialization triggered.",
                                            hotel_guest_id
                                        ),
                                        Ok(false) => warn!(
                                            "Role-philote [{}] could not be materialized.",
                                            hotel_guest_id
                                        ),
                                        Err(e) => warn!(
                                            "Role-philote [{}] materialization error: {e}",
                                            hotel_guest_id
                                        ),
                                    }
                                }
                            } else {
                                warn!(
                                    "Cannot materialize role-philote for '{}': local hotel record missing.",
                                    role
                                );
                            }
                        }
                        Ok(None) => {
                            warn!(
                                role = %role,
                                "No role incarnation found for paracrine target '{}'; task dropped.",
                                role
                            );
                        }
                        Err(e) => {
                            warn!(
                                role = %role,
                                "Role incarnation lookup failed for '{}': {e}; task dropped.",
                                role
                            );
                        }
                    }
                }

                IpcResponse::success("paracrine_emit", None)
            }
        }
    }

    /// Broadcast a `CronJobSync` upsert envelope for a job definition change.
    async fn broadcast_cron_sync_upsert(
        dispatcher_tx: &mpsc::Sender<LedgerCommand>,
        local_node_id: &str,
        job: &ansible_mesh_core::cron::CronJob,
    ) {
        use ansible_mesh_core::event::{EventEnvelope, EventKind, EventPayload};
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let payload = serde_json::json!({ "op": "upsert", "job": job }).to_string();
        let env = EventEnvelope {
            event_id: Uuid::new_v4(),
            seq: 0,
            source_node_id: local_node_id.to_string(),
            target_node_id: None,
            source_agent_id: "ipc-server".into(),
            target_agent_id: None,
            kind: EventKind::CronJobSync,
            corr_id: format!("cron-sync:{}", job.id),
            attempt: 0,
            created_at: now_ms,
            expires_at: None,
            payload: EventPayload::Inline { data: payload },
            trace: vec!["ipc:cron-sync".into()],
        };
        let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;
    }

    /// Broadcast a `CronJobSync` remove envelope when a job is deleted.
    async fn broadcast_cron_sync_remove(
        dispatcher_tx: &mpsc::Sender<LedgerCommand>,
        local_node_id: &str,
        job_id: &str,
    ) {
        use ansible_mesh_core::event::{EventEnvelope, EventKind, EventPayload};
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let payload = serde_json::json!({ "op": "remove", "job_id": job_id }).to_string();
        let env = EventEnvelope {
            event_id: Uuid::new_v4(),
            seq: 0,
            source_node_id: local_node_id.to_string(),
            target_node_id: None,
            source_agent_id: "ipc-server".into(),
            target_agent_id: None,
            kind: EventKind::CronJobSync,
            corr_id: format!("cron-sync-remove:{job_id}"),
            attempt: 0,
            created_at: now_ms,
            expires_at: None,
            payload: EventPayload::Inline { data: payload },
            trace: vec!["ipc:cron-sync-remove".into()],
        };
        let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(env)).await;
    }

    async fn handle_register_component(
        graph: &GraphDomain,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        manifest: ComponentManifest,
    ) -> IpcResponse {
        let guest_id = manifest.guest_id.clone();
        let role = manifest.role.clone();

        // Build the spawn config blob expected by LocalProcessMaterializer.
        let config_json = serde_json::json!({
            "command": manifest.command,
            "args": manifest.args,
            "env": manifest.env,
        });

        let record = GuestRecord {
            hotel_name: manifest.hotel.clone(),
            guest_id: guest_id.clone(),
            role: role.clone(),
            config_json: config_json.to_string(),
            is_active: manifest.auto_start,
            active_pid: None,
            last_active_at: None,
        };

        if let Err(e) = graph.upsert_guest(&record) {
            error!(
                "RegisterComponent: failed to upsert guest {}: {}",
                guest_id, e
            );
            return IpcResponse::error("register_component", "UPSERT_FAILED", e.to_string());
        }

        // Store component-specific config for readback via GetConfig.
        if !manifest.component_config.is_null() {
            let config_key = format!("component:{}", guest_id);
            if let Err(e) =
                graph.set_config_value(&config_key, &manifest.component_config.to_string())
            {
                warn!(
                    "RegisterComponent: failed to store component config for {}: {}",
                    guest_id, e
                );
            }
        }

        info!(
            guest_id = %guest_id,
            role = %role,
            hotel = %manifest.hotel,
            auto_start = manifest.auto_start,
            "component registered",
        );

        // Trigger immediate materialization if auto_start.
        if manifest.auto_start {
            if let Some(requester) = materialization_requester {
                if let Err(e) = requester.ensure_guest_active(&guest_id).await {
                    warn!(
                        "RegisterComponent: ensure_guest_active failed for {}: {}",
                        guest_id, e
                    );
                }
            }
        }

        IpcResponse::ComponentRegistered {
            registered_guest_id: guest_id,
            registered_role: role,
        }
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

    fn handle_add_vault_entry(
        graph: &GraphDomain,
        vault_name: String,
        plaintext: String,
        allowed_roles: Vec<String>,
    ) -> anyhow::Result<String> {
        use crate::vault::{SecretInput, store_secret};

        // Store the encrypted secret.
        let secret_ref = store_secret(
            graph,
            SecretInput {
                secret_kind: "vault-token".to_string(),
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

    fn handle_list_components(graph: &GraphDomain, local_node_id: &str) -> IpcResponse {
        let hotel_name = match Self::local_hotel_name(graph, local_node_id) {
            Some(h) => h,
            None => {
                return IpcResponse::error(
                    "list_components",
                    "HOTEL_NOT_FOUND",
                    "local hotel record not found",
                );
            }
        };

        let guests = match graph.list_guests(&hotel_name, false) {
            Ok(g) => g,
            Err(e) => {
                return IpcResponse::error("list_components", "STORAGE_ERROR", e.to_string());
            }
        };

        // Load tool_runner_registry once to enrich tool-runner entries with capabilities.
        let tool_registry: Vec<serde_json::Value> = graph
            .get_config_value("tool_runner_registry")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok())
            .unwrap_or_default();

        let components: Vec<serde_json::Value> = guests
            .into_iter()
            .map(|g| {
                let spawn_config = serde_json::from_str::<serde_json::Value>(&g.config_json)
                    .unwrap_or(serde_json::Value::Null);
                let command = spawn_config
                    .get("command")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let args = spawn_config
                    .get("args")
                    .and_then(|value| value.as_array())
                    .cloned()
                    .unwrap_or_default();
                let env = spawn_config
                    .get("env")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));

                // Keep the compatibility component_type hint, but stop pretending
                // only model/tool-ish components exist on the authoring surface.
                let component_type = if g.role == "model"
                    || g.role.starts_with("model.")
                    || g.role.starts_with("model-controller")
                {
                    "model-controller"
                } else if g.role == "tool"
                    || g.role.starts_with("tool.")
                    || g.role.starts_with("tool-runner")
                {
                    "tool-runner"
                } else if g.role == "membrane" || g.role.starts_with("membrane.") {
                    "membrane"
                } else if g.role == "agent" || g.role.starts_with("agent.") {
                    "agent"
                } else if g.role == "graph-runner" || g.role.starts_with("graph.") {
                    "graph-runner"
                } else {
                    "other"
                };

                // Read per-component config blob.
                let component_config = {
                    let key = format!("component:{}", g.guest_id);
                    graph
                        .get_config_value(&key)
                        .ok()
                        .flatten()
                        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                        .unwrap_or(serde_json::Value::Null)
                };

                // Find capabilities from tool_runner_registry if this is a tool runner.
                let capabilities: Vec<String> = tool_registry
                    .iter()
                    .find(|entry| {
                        entry.get("guest_id").and_then(|v| v.as_str()) == Some(&g.guest_id)
                    })
                    .and_then(|entry| {
                        entry
                            .get("capabilities")
                            .and_then(|v| v.as_array())
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|c| c.as_str().map(String::from))
                                    .collect()
                            })
                    })
                    .unwrap_or_default();

                serde_json::json!({
                    "guest_id": g.guest_id,
                    "role": g.role,
                    "hotel": g.hotel_name,
                    "command": command,
                    "args": args,
                    "env": env,
                    "component_type": component_type,
                    "is_active": g.is_active,
                    "auto_start": g.is_active,
                    "active_pid": g.active_pid,
                    "last_active_at": g.last_active_at,
                    "component_config": component_config,
                    "capabilities": capabilities,
                })
            })
            .collect();

        IpcResponse::ComponentInventory { components }
    }

    async fn handle_set_component_active(
        graph: &GraphDomain,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        local_node_id: &str,
        guest_id: &str,
        active: bool,
    ) -> IpcResponse {
        let hotel_name = match Self::local_hotel_name(graph, local_node_id) {
            Some(h) => h,
            None => {
                return IpcResponse::error(
                    "set_component_active",
                    "HOTEL_NOT_FOUND",
                    "local hotel record not found",
                );
            }
        };

        // Verify guest exists.
        let guest_record = graph
            .list_guests(&hotel_name, false)
            .ok()
            .and_then(|guests| guests.into_iter().find(|g| g.guest_id == guest_id));

        if guest_record.is_none() {
            return IpcResponse::error(
                "set_component_active",
                "GUEST_NOT_FOUND",
                format!("No component registered with guest_id={guest_id}"),
            );
        }
        let guest = guest_record.unwrap();

        if !active {
            // Kill the process if running, then mark inactive.
            if let Some(ref pid_str) = guest.active_pid {
                if let Ok(pid) = pid_str.parse::<u32>() {
                    let _ = ProcessCommand::new("kill")
                        .args(["-15", &pid.to_string()])
                        .status();
                }
            }
            if let Err(e) = graph.set_guest_pid(&hotel_name, guest_id, None) {
                warn!("SetComponentActive: failed to clear PID for {guest_id}: {e}");
            }
            if let Err(e) = graph.set_guest_active(&hotel_name, guest_id, false) {
                return IpcResponse::error("set_component_active", "STORAGE_ERROR", e.to_string());
            }
            info!(guest_id = %guest_id, "component deactivated");
        } else {
            // Mark active then trigger materialization.
            if let Err(e) = graph.set_guest_active(&hotel_name, guest_id, true) {
                return IpcResponse::error("set_component_active", "STORAGE_ERROR", e.to_string());
            }
            if let Some(req) = materialization_requester {
                if let Err(e) = req.ensure_guest_active(guest_id).await {
                    warn!("SetComponentActive: ensure_guest_active failed for {guest_id}: {e}");
                }
            }
            info!(guest_id = %guest_id, "component activated");
        }

        IpcResponse::success(
            "set_component_active",
            Some(serde_json::json!({ "guest_id": guest_id, "active": active })),
        )
    }

    async fn handle_restart_component(
        graph: &GraphDomain,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        local_node_id: &str,
        guest_id: &str,
    ) -> IpcResponse {
        let hotel_name = match Self::local_hotel_name(graph, local_node_id) {
            Some(h) => h,
            None => {
                return IpcResponse::error(
                    "restart_component",
                    "HOTEL_NOT_FOUND",
                    "local hotel record not found",
                );
            }
        };

        let guest_record = graph
            .list_guests(&hotel_name, false)
            .ok()
            .and_then(|guests| guests.into_iter().find(|g| g.guest_id == guest_id));

        let Some(guest) = guest_record else {
            return IpcResponse::error(
                "restart_component",
                "GUEST_NOT_FOUND",
                format!("No component registered with guest_id={guest_id}"),
            );
        };

        if !guest.is_active {
            return IpcResponse::error(
                "restart_component",
                "COMPONENT_INACTIVE",
                format!("Component {guest_id} is marked inactive; enable it first"),
            );
        }

        // Terminate running process.
        if let Some(ref pid_str) = guest.active_pid {
            if let Ok(pid) = pid_str.parse::<u32>() {
                let _ = ProcessCommand::new("kill")
                    .args(["-15", &pid.to_string()])
                    .status();
            }
        }
        if let Err(e) = graph.set_guest_pid(&hotel_name, guest_id, None) {
            warn!("RestartComponent: failed to clear PID for {guest_id}: {e}");
        }

        // Respawn.
        if let Some(req) = materialization_requester {
            if let Err(e) = req.ensure_guest_active(guest_id).await {
                return IpcResponse::error("restart_component", "SPAWN_FAILED", e.to_string());
            }
        } else {
            return IpcResponse::error(
                "restart_component",
                "NO_MATERIALIZER",
                "no materialization requester available",
            );
        }

        info!(guest_id = %guest_id, "component restarted");
        IpcResponse::success(
            "restart_component",
            Some(serde_json::json!({ "guest_id": guest_id })),
        )
    }

    async fn handle_remove_component(
        graph: &GraphDomain,
        local_node_id: &str,
        guest_id: &str,
    ) -> IpcResponse {
        let hotel_name = match Self::local_hotel_name(graph, local_node_id) {
            Some(h) => h,
            None => {
                return IpcResponse::error(
                    "remove_component",
                    "HOTEL_NOT_FOUND",
                    "local hotel record not found",
                );
            }
        };

        let guest = match graph.get_guest(&hotel_name, guest_id) {
            Ok(Some(guest)) => guest,
            Ok(None) => {
                return IpcResponse::error(
                    "remove_component",
                    "GUEST_NOT_FOUND",
                    format!("No component registered with guest_id={guest_id}"),
                );
            }
            Err(e) => {
                return IpcResponse::error("remove_component", "STORAGE_ERROR", e.to_string());
            }
        };

        if let Some(ref pid_str) = guest.active_pid {
            if let Ok(pid) = pid_str.parse::<u32>() {
                let _ = ProcessCommand::new("kill")
                    .args(["-15", &pid.to_string()])
                    .status();
            }
        }

        if let Err(e) = graph.remove_guest(&hotel_name, guest_id) {
            return IpcResponse::error("remove_component", "STORAGE_ERROR", e.to_string());
        }

        let config_key = format!("component:{guest_id}");
        if let Err(e) = graph.remove_config_value(&config_key) {
            warn!("RemoveComponent: failed to remove component config for {guest_id}: {e}");
        }

        info!(guest_id = %guest_id, "component removed");
        IpcResponse::success(
            "remove_component",
            Some(serde_json::json!({ "guest_id": guest_id })),
        )
    }

    fn record_apartment_checkpoint(
        graph: &GraphDomain,
        agent_id: &str,
        memory_type: &str,
        content_json: &serde_json::Value,
    ) {
        if memory_type == "short" && content_json.get("active_sessions").is_some() {
            return;
        }

        let Some(session_id) = content_json
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            return;
        };

        let now = unix_ts();
        let mut session = graph
            .get_session(session_id)
            .ok()
            .flatten()
            .unwrap_or(SessionRecord {
                session_id: session_id.to_string(),
                session_kind: "conversation".into(),
                primary_agent_id: Some(agent_id.to_string()),
                active_incarnation_id: None,
                channel_kind: None,
                channel_session_key: None,
                status: "active".into(),
                lease_owner_component_id: Some(agent_id.to_string()),
                lease_expires_at: None,
                summary_json: serde_json::json!({}),
                created_at: now,
                updated_at: now,
            });

        session.primary_agent_id = Some(agent_id.to_string());
        session.updated_at = now;
        let mut summary_json = session.summary_json.clone();
        if !summary_json.is_object() {
            summary_json = serde_json::json!({});
        }
        summary_json["memory_checkpoint"] = serde_json::json!({
            "memory_type": memory_type,
            "checkpoint": content_json,
        });
        session.summary_json = summary_json;
        let _ = graph.upsert_session(&session);

        let _ = graph.append_session_event(&SessionEventRecord {
            event_id: Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            turn_id: content_json
                .get("active_turn")
                .and_then(|t| t.get("turn_id"))
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            component_id: agent_id.to_string(),
            kind: "apartment_checkpoint".into(),
            payload_json: serde_json::json!({
                "memory_type": memory_type,
            }),
            created_at: now,
        });
    }

    fn record_session_activity_from_value(
        graph: &GraphDomain,
        payload: &serde_json::Value,
        request_event_id: Option<Uuid>,
        turn_status: Option<&str>,
        participant_role: Option<&str>,
        event_kind: &str,
    ) {
        let envelope = Self::extract_session_envelope(payload);
        let Some(session_id) = envelope.session_id.clone() else {
            return;
        };

        let now = unix_ts();
        let mut session = graph
            .get_session(&session_id)
            .ok()
            .flatten()
            .unwrap_or(SessionRecord {
                session_id: session_id.clone(),
                session_kind: "conversation".into(),
                primary_agent_id: envelope.primary_agent_id.clone(),
                active_incarnation_id: None,
                channel_kind: envelope.source.clone(),
                channel_session_key: envelope.chat_id.clone(),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({}),
                created_at: now,
                updated_at: now,
            });

        if session.primary_agent_id.is_none() {
            session.primary_agent_id = envelope.primary_agent_id.clone();
        }
        if session.channel_kind.is_none() {
            session.channel_kind = envelope.source.clone();
        }
        if session.channel_session_key.is_none() {
            session.channel_session_key = envelope.chat_id.clone();
        }
        if let Some(session_status) = payload
            .get("session_status")
            .and_then(serde_json::Value::as_str)
        {
            session.status = session_status.to_string();
        }
        if let Some(approval_policy) = payload.get("approval_policy") {
            let mut summary_json = session.summary_json.clone();
            if !summary_json.is_object() {
                summary_json = serde_json::json!({});
            }
            summary_json["approval_policy"] = approval_policy.clone();
            session.summary_json = summary_json;
        }
        if let Some(bindings) = payload.get("bindings") {
            let mut summary_json = session.summary_json.clone();
            if !summary_json.is_object() {
                summary_json = serde_json::json!({});
            }
            summary_json["bindings"] = bindings.clone();
            if payload.get("tool_assembly").is_none() {
                summary_json["tool_assembly"] =
                    compose_tool_assembly(bindings, &[], &[], &[], "local-aiua-01");
            }
            session.summary_json = summary_json;
        }
        if let Some(tool_assembly) = payload.get("tool_assembly") {
            let mut summary_json = session.summary_json.clone();
            if !summary_json.is_object() {
                summary_json = serde_json::json!({});
            }
            summary_json["tool_assembly"] = tool_assembly.clone();
            session.summary_json = summary_json;
        }
        session.updated_at = now;
        let _ = graph.upsert_session(&session);

        if let (Some(component_id), Some(role)) = (participant_role, participant_role) {
            let _ = graph.upsert_session_participant(&SessionParticipantRecord {
                session_id: session_id.clone(),
                component_id: component_id.to_string(),
                role: role.to_string(),
                joined_at: now,
                last_seen_at: now,
            });
        }

        if let Some(turn_id) = envelope.turn_id.clone() {
            let existing = graph.get_session_turn(&session_id, &turn_id).ok().flatten();
            let mut turn = existing.unwrap_or(SessionTurnRecord {
                turn_id: turn_id.clone(),
                session_id: session_id.clone(),
                request_event_id: request_event_id.map(|id| id.to_string()),
                user_message_json: serde_json::json!({}),
                status: turn_status.unwrap_or("queued").to_string(),
                response_json: None,
                error_json: None,
                started_at: Some(now),
                completed_at: None,
            });

            if let Some(event_id) = request_event_id {
                turn.request_event_id = Some(event_id.to_string());
            }
            if turn.user_message_json == serde_json::json!({}) {
                turn.user_message_json = serde_json::json!({
                    "source": envelope.source,
                    "chat_id": envelope.chat_id,
                    "content": envelope.content,
                    "action": envelope.action,
                });
            }
            if let Some(status) = merge_turn_status(&turn.status, turn_status) {
                turn.status = status.clone();
                if matches!(status.as_str(), "completed" | "failed") {
                    turn.completed_at = Some(now);
                }
            }
            if envelope.action.as_deref() == Some("model_response")
                || envelope.action.as_deref() == Some("send_reply")
            {
                turn.response_json = Some(payload.clone());
            }
            let _ = graph.upsert_session_turn(&turn);
        }

        let turn_id = envelope.turn_id.clone();
        let _ = graph.append_session_event(&SessionEventRecord {
            event_id: Uuid::new_v4().to_string(),
            session_id,
            turn_id: turn_id.clone(),
            component_id: participant_role.unwrap_or("system").to_string(),
            kind: event_kind.to_string(),
            payload_json: payload.clone(),
            created_at: now,
        });

        Self::append_explicit_approval_events(
            graph,
            &session.session_id,
            turn_id.as_deref(),
            participant_role.unwrap_or("system"),
            payload,
            now,
        );
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

    fn append_explicit_approval_events(
        graph: &GraphDomain,
        session_id: &str,
        turn_id: Option<&str>,
        component_id: &str,
        payload: &serde_json::Value,
        now: u64,
    ) {
        if let Some(approval_request) = payload.get("approval_request") {
            let _ = graph.append_session_event(&SessionEventRecord {
                event_id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_id.map(str::to_string),
                component_id: component_id.to_string(),
                kind: "approval_requested".into(),
                payload_json: approval_request.clone(),
                created_at: now,
            });
        }

        if let Some(approval_resolution) = payload.get("approval_resolution") {
            let event_kind = match approval_resolution
                .get("decision")
                .and_then(serde_json::Value::as_str)
            {
                Some("approved") => "approval_resolved",
                Some("denied") => "approval_denied",
                _ => "approval_resolved",
            };
            let _ = graph.append_session_event(&SessionEventRecord {
                event_id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_id.map(str::to_string),
                component_id: component_id.to_string(),
                kind: event_kind.into(),
                payload_json: approval_resolution.clone(),
                created_at: now,
            });
        }

        if let Some(approval_policy) = payload.get("approval_policy") {
            let _ = graph.append_session_event(&SessionEventRecord {
                event_id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_id.map(str::to_string),
                component_id: component_id.to_string(),
                kind: "approval_policy_changed".into(),
                payload_json: approval_policy.clone(),
                created_at: now,
            });
        }

        if let Some(session_status) = payload.get("session_status") {
            let _ = graph.append_session_event(&SessionEventRecord {
                event_id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_id.map(str::to_string),
                component_id: component_id.to_string(),
                kind: "session_status_changed".into(),
                payload_json: session_status.clone(),
                created_at: now,
            });
        }

        if let Some(bindings) = payload.get("bindings") {
            let _ = graph.append_session_event(&SessionEventRecord {
                event_id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_id.map(str::to_string),
                component_id: component_id.to_string(),
                kind: "session_bindings_updated".into(),
                payload_json: bindings.clone(),
                created_at: now,
            });
        }

        if let Some(tool_assembly) = payload.get("tool_assembly").cloned().or_else(|| {
            payload
                .get("bindings")
                .map(|bindings| compose_tool_assembly(bindings, &[], &[], &[], "local-aiua-01"))
        }) {
            let _ = graph.append_session_event(&SessionEventRecord {
                event_id: Uuid::new_v4().to_string(),
                session_id: session_id.to_string(),
                turn_id: turn_id.map(str::to_string),
                component_id: component_id.to_string(),
                kind: "tool_assembly_updated".into(),
                payload_json: tool_assembly,
                created_at: now,
            });
        }
    }

    fn extract_session_envelope(payload: &serde_json::Value) -> SessionEnvelope {
        SessionEnvelope {
            session_id: payload
                .get("session_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    let source = payload.get("source").and_then(serde_json::Value::as_str)?;
                    let chat_id = payload.get("chat_id")?.as_str()?;
                    Some(format!("{source}:{chat_id}:agent-jane-01"))
                }),
            turn_id: payload
                .get("turn_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            primary_agent_id: payload
                .get("primary_agent_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .or_else(|| Some("agent-jane-01".to_string())),
            source: payload
                .get("source")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            chat_id: payload
                .get("chat_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            action: payload
                .get("action")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            content: payload
                .get("content")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        }
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
                if let Some(obj) = bindings.as_object_mut() {
                    obj.insert("effective_toolset".to_string(), serde_json::json!(toolset));
                    obj.insert(
                        "effective_skillset".to_string(),
                        serde_json::json!(skillset),
                    );
                } else {
                    bindings = serde_json::json!({
                        "effective_toolset": toolset,
                        "effective_skillset": skillset,
                    });
                }
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

            if !skillset.is_empty() {
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

                for skill_name in &skillset {
                    if let Ok(Some(skill_record)) = graph.get_abstract_skill(skill_name) {
                        for implied in &skill_record.implied_tools {
                            if !toolset.contains(implied) {
                                toolset.push(implied.clone());
                            }
                        }
                        let guidance =
                            format!("{} — {}", skill_record.skill_name, skill_record.description);
                        if !skill_guidance.contains(&guidance) {
                            skill_guidance.push(guidance);
                        }
                    }
                }

                if let Some(obj) = bindings.as_object_mut() {
                    obj.insert("effective_toolset".to_string(), serde_json::json!(toolset));
                    obj.insert(
                        "effective_skill_guidance".to_string(),
                        serde_json::json!(skill_guidance),
                    );
                }
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

        Ok(Some(serde_json::json!({
            "session_id": session.session_id,
            "agent_id": session.primary_agent_id,
            "source": session.channel_kind,
            "active_incarnation_id": session.active_incarnation_id,
            "role_activation": role_activation,
            "agent_profile": agent_profile,
            "status": session.status,
            "summary": session.summary_json,
            "approval_policy": session
                .summary_json
                .get("approval_policy")
                .cloned()
                .unwrap_or_else(|| {
                    // Bootstrap the orchestrator approval policy: governance and self-config
                    // tools run without per-action approval so the agent can operate
                    // autonomously from the first turn.
                    serde_json::json!({
                        "auto_approve_all": false,
                        "preapproved_classes": ["session", "utility", "capability"],
                        "preapproved_tools": ["agent.configure"]
                    })
                }),
            "bindings": bindings,
            "component_route_assembly": component_route_assembly,
            "tool_assembly": tool_assembly,
            "tool_runners": tool_runner_registry,
            "mesh_registry": mesh_registry,
            "recent_turns": recent_turns,
            "active_turn": active_turn,
            "session_index": session_index,
        })))
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
}

#[derive(Debug, Clone)]
struct LiveToolRunner {
    guest_id: String,
    supported_tools: Vec<String>,
}

#[derive(Debug, Clone)]
struct LiveRoleSubscriberView {
    guest_id: String,
    role: String,
}

async fn live_tool_runners(inboxes: &InboxRegistry) -> Vec<LiveToolRunner> {
    let guard = inboxes.lock().await;
    let mut runners = Vec::new();

    if let Some(subscribers) = guard.get("tool") {
        for subscriber in subscribers {
            if !runners
                .iter()
                .any(|existing: &LiveToolRunner| existing.guest_id == subscriber.guest_id)
            {
                runners.push(LiveToolRunner {
                    guest_id: subscriber.guest_id.clone(),
                    supported_tools: subscriber.supported_tools.clone(),
                });
            }
        }
    }

    runners
}

async fn live_role_subscribers(
    inboxes: &InboxRegistry,
    role_prefix: &str,
) -> Vec<LiveRoleSubscriberView> {
    let exact_role = role_prefix.strip_suffix('.').unwrap_or(role_prefix);
    let guard = inboxes.lock().await;
    let mut subscribers = guard
        .iter()
        .filter(|(role, _)| *role == exact_role || role.starts_with(role_prefix))
        .flat_map(|(role, entries)| {
            entries.iter().map(|entry| LiveRoleSubscriberView {
                guest_id: entry.guest_id.clone(),
                role: role.clone(),
            })
        })
        .collect::<Vec<_>>();
    subscribers.sort_by(|left, right| {
        left.role
            .cmp(&right.role)
            .then_with(|| left.guest_id.cmp(&right.guest_id))
    });
    subscribers.dedup_by(|left, right| left.role == right.role && left.guest_id == right.guest_id);
    subscribers
}

fn load_tool_runner_registry(graph: &GraphDomain) -> anyhow::Result<Vec<ToolRunnerRegistryEntry>> {
    let Some(raw) = graph.get_config_value("tool_runner_registry")? else {
        return Ok(Vec::new());
    };
    let value =
        serde_json::from_str::<serde_json::Value>(&raw).unwrap_or_else(|_| serde_json::json!([]));
    let entries = value
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| {
            Some(ToolRunnerRegistryEntry {
                guest_id: entry.get("guest_id")?.as_str()?.to_string(),
                supported_tools: entry
                    .get("supported_tools")
                    .and_then(serde_json::Value::as_array)
                    .map(|tools| {
                        tools
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default(),
                last_seen_at: entry
                    .get("last_seen_at")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0),
            })
        })
        .collect::<Vec<_>>();
    Ok(entries)
}

fn merge_tool_runners(
    registered_runners: &[ToolRunnerRegistryEntry],
    live_runners: &[LiveToolRunner],
) -> serde_json::Value {
    let merged = registered_runners
        .iter()
        .map(|runner| {
            let is_connected = live_runners
                .iter()
                .any(|live| live.guest_id == runner.guest_id);
            serde_json::json!({
                "guest_id": runner.guest_id,
                "supported_tools": runner.supported_tools,
                "last_seen_at": runner.last_seen_at,
                "is_connected": is_connected,
            })
        })
        .collect::<Vec<_>>();
    serde_json::Value::Array(merged)
}

fn compose_component_route_assembly(
    bindings: &serde_json::Value,
    local_subscribers: &[LiveRoleSubscriberView],
    local_guest_roles: &[String],
    registry: &NodeRegistry,
    local_node_id: &str,
) -> serde_json::Value {
    let execution_routes = default_component_capabilities(bindings)
        .into_iter()
        .filter_map(|capability| {
            select_component_route(
                bindings,
                &capability,
                local_subscribers,
                local_guest_roles,
                registry,
                local_node_id,
            )
            .map(|route| (capability, route))
        })
        .collect::<serde_json::Map<_, _>>();

    serde_json::json!({
        "execution_routes": execution_routes,
    })
}

fn default_component_capabilities(bindings: &serde_json::Value) -> Vec<String> {
    let mut capabilities = std::collections::BTreeSet::from([
        "text.generate".to_string(),
        "media.analyze".to_string(),
    ]);

    for route in bindings
        .get("component_routes")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(capability) = route.get("capability").and_then(serde_json::Value::as_str) {
            capabilities.insert(capability.to_string());
        }
    }

    capabilities.into_iter().collect()
}

fn select_component_route(
    bindings: &serde_json::Value,
    capability: &str,
    local_subscribers: &[LiveRoleSubscriberView],
    local_guest_roles: &[String],
    registry: &NodeRegistry,
    local_node_id: &str,
) -> Option<serde_json::Value> {
    let binding = bindings
        .get("component_routes")
        .and_then(serde_json::Value::as_array)
        .and_then(|routes| {
            routes.iter().find(|route| {
                route.get("capability").and_then(serde_json::Value::as_str) == Some(capability)
            })
        });
    let preferred_hotel_id = binding
        .and_then(|route| route.get("preferred_hotel_id"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            bindings
                .get("preferred_hotel_id")
                .and_then(serde_json::Value::as_str)
        });
    let preferred_environment_id = binding
        .and_then(|route| route.get("preferred_environment_id"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            bindings
                .get("preferred_environment_id")
                .and_then(serde_json::Value::as_str)
        });
    let target_role = binding
        .and_then(|route| route.get("implementation"))
        .and_then(serde_json::Value::as_str)
        .map(component_implementation_to_role)
        .or_else(|| {
            if capability == "text.generate" || capability == "media.analyze" {
                bindings
                    .get("effective_model_controller")
                    .and_then(serde_json::Value::as_str)
                    .map(component_implementation_to_role)
            } else {
                None
            }
        })
        .unwrap_or_else(|| default_component_role(capability).to_string());

    if let Some(incarnation_id) = binding
        .and_then(|route| route.get("incarnation"))
        .and_then(serde_json::Value::as_str)
    {
        if let Some(local) = local_subscribers.iter().find(|subscriber| {
            subscriber.role == target_role && subscriber.guest_id == incarnation_id
        }) {
            return Some(serde_json::json!({
                "target_node": local_node_id,
                "target_role": local.role,
                "incarnation_id": local.guest_id,
                "hotel_id": local_node_id,
                "environment_id": preferred_environment_id,
                "execution_mode": "preferred",
                "availability_state": "live",
                "selection_reason": "preferred_incarnation_live",
            }));
        }

        if let Some(remote) = registry
            .advertisements_for_role(&target_role)
            .filter(|advertisement| {
                advertisement.node_id != local_node_id
                    && advertisement.availability_state == "live"
                    && advertisement.incarnation_id == incarnation_id
            })
            .next()
        {
            return Some(serde_json::json!({
                "target_node": remote.node_id,
                "target_role": remote.target_role,
                "incarnation_id": remote.incarnation_id,
                "hotel_id": remote.hotel_id,
                "environment_id": preferred_environment_id,
                "execution_mode": "preferred",
                "availability_state": remote.availability_state,
                "selection_reason": "preferred_incarnation_live",
            }));
        }
    }

    if let Some(local) = local_subscribers
        .iter()
        .find(|subscriber| subscriber.role == target_role)
    {
        return Some(serde_json::json!({
            "target_node": local_node_id,
            "target_role": local.role,
            "incarnation_id": local.guest_id,
            "hotel_id": local_node_id,
            "environment_id": preferred_environment_id,
            "execution_mode": "capability",
            "availability_state": "live",
            "selection_reason": if binding.is_some() {
                "live_local_capability"
            } else {
                "live_local_fallback"
            },
        }));
    }

    if local_guest_roles.iter().any(|role| role == &target_role) {
        return Some(serde_json::json!({
            "target_node": local_node_id,
            "target_role": target_role,
            "incarnation_id": serde_json::Value::Null,
            "hotel_id": local_node_id,
            "environment_id": preferred_environment_id,
            "execution_mode": "capability",
            "availability_state": "live",
            "selection_reason": if binding.is_some() {
                "local_active_guest_fallback"
            } else {
                "local_active_guest_fallback"
            },
        }));
    }

    if let Some(remote) = select_remote_component_advertisement(
        registry,
        &target_role,
        preferred_hotel_id,
        local_node_id,
    ) {
        return Some(serde_json::json!({
            "target_node": remote.node_id,
            "target_role": remote.target_role,
            "incarnation_id": remote.incarnation_id,
            "hotel_id": remote.hotel_id,
            "environment_id": preferred_environment_id,
            "execution_mode": "capability",
            "availability_state": remote.availability_state,
            "selection_reason": remote.selection_hint.unwrap_or_else(|| "remote_latency_capacity".into()),
        }));
    }

    Some(serde_json::json!({
        "target_node": local_node_id,
        "target_role": target_role,
        "incarnation_id": serde_json::Value::Null,
        "hotel_id": local_node_id,
        "environment_id": preferred_environment_id,
        "execution_mode": "capability",
        "availability_state": "materialization_required",
        "selection_reason": "local_requires_materialization",
    }))
}

fn component_implementation_to_role(implementation: &str) -> String {
    let normalized = implementation.trim().to_ascii_lowercase();
    if normalized.starts_with("model.") {
        return normalized;
    }
    let prefix = normalized
        .split(['.', '-', '@', '/'])
        .find(|segment| !segment.is_empty())
        .unwrap_or("gemini");

    if prefix == "elevenlabs" {
        "model.elevenlabs".into()
    } else {
        "model".into()
    }
}

fn default_component_role(capability: &str) -> &'static str {
    match capability {
        "voice.synthesize" => "model.elevenlabs",
        _ => "model",
    }
}

fn select_remote_component_advertisement(
    registry: &NodeRegistry,
    target_role: &str,
    preferred_hotel_id: Option<&str>,
    local_node_id: &str,
) -> Option<CapabilityAdvertisement> {
    let mut candidates = registry
        .advertisements_for_role(target_role)
        .filter(|advertisement| {
            advertisement.node_id != local_node_id && advertisement.availability_state == "live"
        })
        .cloned()
        .collect::<Vec<_>>();

    candidates.sort_by(|left, right| {
        let left_pref = preferred_hotel_id == Some(left.hotel_id.as_str());
        let right_pref = preferred_hotel_id == Some(right.hotel_id.as_str());
        right_pref
            .cmp(&left_pref)
            .then_with(|| {
                left.latency_hint_ms
                    .unwrap_or(u32::MAX)
                    .cmp(&right.latency_hint_ms.unwrap_or(u32::MAX))
            })
            .then_with(|| remote_available_capacity(right).cmp(&remote_available_capacity(left)))
            .then_with(|| left.incarnation_id.cmp(&right.incarnation_id))
    });

    candidates.into_iter().next()
}

fn compose_tool_assembly(
    bindings: &serde_json::Value,
    registered_runners: &[ToolRunnerRegistryEntry],
    live_runners: &[LiveToolRunner],
    remote_tool_ads: &[CapabilityAdvertisement],
    local_node_id: &str,
) -> serde_json::Value {
    let allowed_incarnations =
        parse_allowed_incarnations(bindings, registered_runners, live_runners);
    if !allowed_incarnations.is_empty() {
        return compose_tool_assembly_from_incarnations(bindings, &allowed_incarnations);
    }

    let toolset = default_visible_toolset(bindings);

    let tools_for_model = toolset
        .iter()
        .map(|tool_name| {
            serde_json::json!({
                "tool_name": tool_name,
                "description": format!("Execute the {} tool.", tool_name),
                "input_schema": {
                    "type": "object"
                }
            })
        })
        .collect::<Vec<_>>();

    let execution_routes = toolset
        .iter()
        .map(|tool_name| {
            if is_local_agent_tool(tool_name) {
                return Some((
                    tool_name.to_string(),
                    serde_json::json!({
                        "target_node": "agent-jane-01",
                        "target_role": "agent",
                        "runner_id": serde_json::Value::Null,
                        "incarnation_id": serde_json::Value::Null,
                        "hotel_id": serde_json::Value::Null,
                        "environment_id": serde_json::Value::Null,
                        "task_runner_kind": serde_json::Value::Null,
                        "execution_mode": "local_agent",
                        "availability_state": "live",
                        "selection_reason": "agent_local_tool",
                    }),
                ));
            }
            let execution_mode = if is_pinned_tool(tool_name) {
                "pinned"
            } else {
                "capability"
            };
            let registered = registered_runners.iter().find(|runner| {
                runner.supported_tools.is_empty()
                    || runner
                        .supported_tools
                        .iter()
                        .any(|supported| supported == tool_name)
            });
            let live_runner = live_runners.iter().find(|runner| {
                runner.supported_tools.is_empty()
                    || runner
                        .supported_tools
                        .iter()
                        .any(|supported| supported == tool_name)
            });
            if registered.is_none() && live_runner.is_none() {
                let remote = select_remote_tool_advertisement(remote_tool_ads, tool_name, bindings)?;
                return Some((
                    tool_name.to_string(),
                    serde_json::json!({
                        "target_node": remote.node_id,
                        "target_role": remote.target_role,
                        "runner_id": remote.incarnation_id,
                        "incarnation_id": remote.incarnation_id,
                        "hotel_id": remote.hotel_id,
                        "environment_id": serde_json::Value::Null,
                        "task_runner_kind": task_runner_kind_for_tool(tool_name),
                        "task_runner_config": task_runner_base_config_for_tool(bindings, tool_name),
                        "execution_mode": "capability",
                        "availability_state": remote.availability_state,
                        "selection_reason": remote.selection_hint.unwrap_or_else(|| "remote_latency_capacity".into()),
                    }),
                ));
            }
            let registered = registered?;
            Some((
                tool_name.to_string(),
                serde_json::json!({
                    "target_node": local_node_id,
                    "target_role": format!("tool.{}", tool_name),
                    "runner_id": live_runner
                        .map(|runner| runner.guest_id.clone())
                        .unwrap_or_else(|| registered.guest_id.clone()),
                    "incarnation_id": live_runner
                        .map(|runner| runner.guest_id.clone())
                        .unwrap_or_else(|| registered.guest_id.clone()),
                    "hotel_id": local_node_id,
                    "environment_id": serde_json::Value::Null,
                    "task_runner_kind": task_runner_kind_for_tool(tool_name),
                    "task_runner_config": task_runner_base_config_for_tool(bindings, tool_name),
                    "execution_mode": execution_mode,
                    "availability_state": if live_runner.is_some() {
                        "live"
                    } else {
                        "materialization_required"
                    },
                    "selection_reason": if live_runner.is_some() {
                        if execution_mode == "pinned" {
                            "live_pinned_runner"
                        } else {
                            "live_capability_runner"
                        }
                    } else {
                        if execution_mode == "pinned" {
                            "registered_pinned_runner_requires_materialization"
                        } else {
                            "registered_capability_runner_requires_materialization"
                        }
                    },
                }),
            ))
        })
        .flatten()
        .collect::<serde_json::Map<_, _>>();

    let policy_annotations = toolset
        .iter()
        .map(|tool_name| {
            (
                tool_name.to_string(),
                serde_json::json!({
                    "policy_class": format!("tool:{tool_name}"),
                    "approval_required": false
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();

    serde_json::json!({
        "tools_for_model": tools_for_model,
        "execution_routes": execution_routes,
        "policy_annotations": policy_annotations,
    })
}

fn default_visible_toolset(bindings: &serde_json::Value) -> Vec<String> {
    let mut toolset = bindings
        .get("effective_toolset")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if toolset.is_empty() {
        toolset.push("echo".into());
    }
    toolset
}

fn remote_tool_advertisements(
    registry: &NodeRegistry,
    local_node_id: &str,
) -> Vec<CapabilityAdvertisement> {
    registry
        .active_nodes()
        .filter(|status| status.capabilities.node_id != local_node_id)
        .flat_map(|status| status.advertisements.iter().cloned())
        .filter(|advertisement| advertisement.target_role.starts_with("tool."))
        .collect()
}

fn select_remote_tool_advertisement(
    remote_tool_ads: &[CapabilityAdvertisement],
    tool_name: &str,
    bindings: &serde_json::Value,
) -> Option<CapabilityAdvertisement> {
    let target_role = format!("tool.{tool_name}");
    let preferred_hotel_id = bindings
        .get("preferred_hotel_id")
        .and_then(serde_json::Value::as_str);
    let mut candidates = remote_tool_ads
        .iter()
        .filter(|advertisement| {
            advertisement.target_role == target_role && advertisement.availability_state == "live"
        })
        .cloned()
        .collect::<Vec<_>>();

    candidates.sort_by(|left, right| {
        let left_pref = preferred_hotel_id == Some(left.hotel_id.as_str());
        let right_pref = preferred_hotel_id == Some(right.hotel_id.as_str());
        right_pref
            .cmp(&left_pref)
            .then_with(|| {
                left.latency_hint_ms
                    .unwrap_or(u32::MAX)
                    .cmp(&right.latency_hint_ms.unwrap_or(u32::MAX))
            })
            .then_with(|| remote_available_capacity(right).cmp(&remote_available_capacity(left)))
            .then_with(|| left.incarnation_id.cmp(&right.incarnation_id))
    });

    candidates.into_iter().next()
}

fn remote_available_capacity(advertisement: &CapabilityAdvertisement) -> i64 {
    i64::from(advertisement.max_concurrent_jobs.unwrap_or(0))
        - i64::from(advertisement.active_jobs)
        - i64::from(advertisement.queue_depth)
}

fn is_local_agent_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "session.status"
            | "agent.configure"
            | "skill.register"
            | "subagent.spawn"
            | "role.configure"
    )
}

fn is_pinned_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "workspace.list" | "workspace.read" | "workspace.search" | "workspace.write"
    )
}

fn task_runner_kind_for_tool(tool_name: &str) -> Option<&'static str> {
    if tool_name.starts_with("workspace.") {
        return Some("workspace");
    }

    if tool_name.starts_with("shell.") {
        return Some("shell");
    }

    None
}

fn task_runner_base_config_for_tool(
    bindings: &serde_json::Value,
    tool_name: &str,
) -> serde_json::Value {
    if !tool_name.starts_with("workspace.") {
        return serde_json::Value::Null;
    }

    let mut config = bindings
        .get("workspace_runner_config")
        .cloned()
        .filter(|value| value.is_object())
        .unwrap_or_else(|| serde_json::json!({}));

    if config.get("default_workspace_ref").is_none() {
        if let Some(workspace_ref) = bindings.get("effective_workspace_ref").cloned() {
            config["default_workspace_ref"] = workspace_ref;
        }
    }

    if config.get("allowed_tools").is_none() {
        let workspace_tools = bindings
            .get("effective_toolset")
            .and_then(serde_json::Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .filter(|tool| tool.starts_with("workspace."))
                    .map(|tool| serde_json::Value::String(tool.to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !workspace_tools.is_empty() {
            config["allowed_tools"] = serde_json::Value::Array(workspace_tools);
        }
    }

    config
}

fn parse_allowed_incarnations(
    bindings: &serde_json::Value,
    registered_runners: &[ToolRunnerRegistryEntry],
    live_runners: &[LiveToolRunner],
) -> Vec<AllowedIncarnation> {
    bindings
        .get("allowed_tool_runner_incarnations")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| {
            let incarnation_id = entry.get("incarnation_id")?.as_str()?.to_string();
            let runner_id = entry
                .get("runner_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let target_node = entry
                .get("target_node")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let target_role = entry
                .get("target_role")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let supported_tools = entry
                .get("supported_tools")
                .and_then(serde_json::Value::as_array)
                .map(|tools| {
                    tools
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let is_live = live_runners.iter().any(|runner| {
                runner.guest_id == incarnation_id
                    || runner_id.as_deref() == Some(runner.guest_id.as_str())
            });
            let is_registered = registered_runners.iter().any(|runner| {
                runner.guest_id == incarnation_id
                    || runner_id.as_deref() == Some(runner.guest_id.as_str())
            });
            Some(AllowedIncarnation {
                incarnation_id,
                runner_id,
                hotel_id: entry
                    .get("hotel_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                environment_id: entry
                    .get("environment_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                target_node,
                target_role,
                supported_tools,
                execution_mode: entry
                    .get("execution_mode")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("capability")
                    .to_string(),
                availability_state: if is_live {
                    "live".into()
                } else if is_registered {
                    "materialization_required".into()
                } else {
                    entry
                        .get("availability_state")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("materialization_required")
                        .to_string()
                },
                selection_hint: entry
                    .get("selection_hint")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect()
}

fn parse_routing_preferences(bindings: &serde_json::Value) -> RoutingPreferences {
    RoutingPreferences {
        preferred_tool_runner_incarnation: bindings
            .get("preferred_tool_runner_incarnation")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        preferred_tool_runner: bindings
            .get("preferred_tool_runner")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        preferred_hotel_id: bindings
            .get("preferred_hotel_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        preferred_environment_id: bindings
            .get("preferred_environment_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    }
}

fn compose_tool_assembly_from_incarnations(
    bindings: &serde_json::Value,
    incarnations: &[AllowedIncarnation],
) -> serde_json::Value {
    let preferences = parse_routing_preferences(bindings);
    let toolset = {
        let filtered = default_visible_toolset(bindings);
        if bindings
            .get("effective_toolset")
            .and_then(serde_json::Value::as_array)
            .is_some()
        {
            filtered
        } else {
            incarnations
                .iter()
                .flat_map(|incarnation| incarnation.supported_tools.iter().cloned())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>()
        }
    };

    let tools_for_model = toolset
        .iter()
        .map(|tool_name| {
            serde_json::json!({
                "tool_name": tool_name,
                "description": format!("Execute the {} tool.", tool_name),
                "input_schema": {
                    "type": "object"
                }
            })
        })
        .collect::<Vec<_>>();

    let execution_routes = toolset
        .iter()
        .filter_map(|tool_name| {
            select_allowed_incarnation(incarnations, tool_name, &preferences).map(|incarnation| {
                (
                    tool_name.to_string(),
                    serde_json::json!({
                        "target_node": incarnation.target_node.clone().or_else(|| incarnation.hotel_id.clone()).unwrap_or_else(|| "local-aiua-01".into()),
                        "target_role": incarnation.target_role.clone().unwrap_or_else(|| format!("tool.{tool_name}")),
                        "runner_id": incarnation.runner_id.clone().unwrap_or_else(|| incarnation.incarnation_id.clone()),
                        "incarnation_id": incarnation.incarnation_id,
                        "hotel_id": incarnation.hotel_id,
                        "environment_id": incarnation.environment_id,
                        "task_runner_kind": task_runner_kind_for_tool(tool_name),
                        "task_runner_config": task_runner_base_config_for_tool(bindings, tool_name),
                        "execution_mode": incarnation.execution_mode,
                        "availability_state": incarnation.availability_state,
                        "selection_reason": selection_reason_for_incarnation(incarnation, &preferences),
                    }),
                )
            })
        })
        .collect::<serde_json::Map<_, _>>();

    let policy_annotations = toolset
        .iter()
        .map(|tool_name| {
            (
                tool_name.to_string(),
                serde_json::json!({
                    "policy_class": format!("tool:{tool_name}"),
                    "approval_required": false
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();

    serde_json::json!({
        "tools_for_model": tools_for_model,
        "execution_routes": execution_routes,
        "policy_annotations": policy_annotations,
    })
}

fn select_allowed_incarnation<'a>(
    incarnations: &'a [AllowedIncarnation],
    tool_name: &str,
    preferences: &RoutingPreferences,
) -> Option<&'a AllowedIncarnation> {
    let mut candidates = incarnations
        .iter()
        .filter(|incarnation| {
            incarnation
                .supported_tools
                .iter()
                .any(|supported| supported == tool_name)
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        incarnation_preference_rank(preferences, right)
            .cmp(&incarnation_preference_rank(preferences, left))
            .then_with(|| {
                let left_live = left.availability_state == "live";
                let right_live = right.availability_state == "live";
                right_live.cmp(&left_live)
            })
            .then_with(|| {
                let left_local = left.hotel_id.as_deref() == Some("local-aiua-01");
                let right_local = right.hotel_id.as_deref() == Some("local-aiua-01");
                right_local.cmp(&left_local)
            })
            .then_with(|| left.incarnation_id.cmp(&right.incarnation_id))
    });
    candidates.into_iter().next()
}

fn incarnation_preference_rank(
    preferences: &RoutingPreferences,
    incarnation: &AllowedIncarnation,
) -> u8 {
    if preferences.preferred_tool_runner_incarnation.as_deref()
        == Some(incarnation.incarnation_id.as_str())
    {
        return 4;
    }
    if preferences.preferred_tool_runner.as_deref() == incarnation.runner_id.as_deref() {
        return 3;
    }
    if preferences.preferred_environment_id.as_deref() == incarnation.environment_id.as_deref() {
        return 2;
    }
    if preferences.preferred_hotel_id.as_deref() == incarnation.hotel_id.as_deref() {
        return 1;
    }
    0
}

fn selection_reason_for_incarnation(
    incarnation: &AllowedIncarnation,
    preferences: &RoutingPreferences,
) -> String {
    let suffix = if incarnation.availability_state == "live" {
        "live"
    } else {
        "requires_materialization"
    };

    let computed = if preferences.preferred_tool_runner_incarnation.as_deref()
        == Some(incarnation.incarnation_id.as_str())
    {
        format!("preferred_incarnation_{suffix}")
    } else if preferences.preferred_tool_runner.as_deref() == incarnation.runner_id.as_deref() {
        format!("preferred_runner_{suffix}")
    } else if preferences.preferred_environment_id.as_deref()
        == incarnation.environment_id.as_deref()
    {
        format!("preferred_environment_{suffix}")
    } else if preferences.preferred_hotel_id.as_deref() == incarnation.hotel_id.as_deref() {
        format!("preferred_hotel_{suffix}")
    } else if incarnation.availability_state == "live"
        && incarnation.hotel_id.as_deref() == Some("local-aiua-01")
    {
        "live_local_fallback".into()
    } else if incarnation.availability_state == "live" {
        "live_allowed_incarnation".into()
    } else {
        "allowed_incarnation_requires_materialization".into()
    };

    let used_preference = incarnation_preference_rank(preferences, incarnation) > 0;
    if used_preference {
        computed
    } else {
        incarnation.selection_hint.clone().unwrap_or(computed)
    }
}

fn unix_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn merge_turn_status(current: &str, incoming: Option<&str>) -> Option<String> {
    let incoming = incoming?;
    if matches!(current, "completed" | "failed") && !matches!(incoming, "completed" | "failed") {
        return Some(current.to_string());
    }
    Some(incoming.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::guest_manager::GuestMaterializationRequester;
    use crate::vault::{SecretInput, store_secret};
    use ansible_mesh_core::NodeCapabilities;
    use ansible_mesh_core::graph::{RoleIncarnationRecord, TurnLoopConfig};
    use ansible_mesh_core::registry::{CapabilityAdvertisement, NodeRegistry};
    use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
    use ansible_mesh_core::storage::{
        AgentIdentityRecord, GuestRecord, HotelRecord, SessionRecord, SessionTurnRecord,
    };
    use base64::Engine;
    use philotic_client::{
        GuestIdentity, HandoffBundle, OperatorTargetView, PhiloticClient,
        SubagentCompletionContract, SubagentContextPacket, SubagentDelegation,
    };
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{LazyLock, Mutex as StdMutex};

    fn expect_telegram_poll_lease(response: IpcResponse) -> (bool, Option<LeaseEnvelope>) {
        match response {
            IpcResponse::TelegramPollLease { granted, lease } => (granted, lease),
            other => panic!("unexpected Telegram poll lease response: {other:?}"),
        }
    }

    fn expect_telegram_poll_status(response: IpcResponse) -> (bool, Option<LeaseEnvelope>) {
        match response {
            IpcResponse::TelegramPollLeaseStatus { active, lease } => (active, lease),
            other => panic!("unexpected Telegram poll lease status response: {other:?}"),
        }
    }

    fn expect_desktop_membrane_lease(response: IpcResponse) -> (bool, Option<LeaseEnvelope>) {
        match response {
            IpcResponse::DesktopMembraneLease {
                desktop_granted,
                desktop_lease,
            } => (desktop_granted, desktop_lease),
            other => panic!("unexpected desktop membrane lease response: {other:?}"),
        }
    }

    fn expect_desktop_membrane_status(response: IpcResponse) -> (bool, Option<LeaseEnvelope>) {
        match response {
            IpcResponse::DesktopMembraneLeaseStatus {
                desktop_active,
                desktop_lease,
            } => (desktop_active, desktop_lease),
            other => panic!("unexpected desktop membrane lease status response: {other:?}"),
        }
    }

    fn expect_desktop_membrane_view_status(response: IpcResponse) -> DesktopMembraneStatusView {
        match response {
            IpcResponse::DesktopMembraneStatusView { membrane_status } => membrane_status,
            other => panic!("unexpected desktop membrane status view response: {other:?}"),
        }
    }

    fn expect_desktop_membrane_target_status(
        response: IpcResponse,
    ) -> DesktopMembraneTargetStatusView {
        match response {
            IpcResponse::DesktopMembraneTargetStatusView {
                membrane_target_status,
            } => membrane_target_status,
            other => panic!("unexpected desktop membrane target status response: {other:?}"),
        }
    }

    fn expect_desktop_membrane_target_guest_inventory(
        response: IpcResponse,
    ) -> DesktopMembraneTargetGuestInventoryView {
        match response {
            IpcResponse::DesktopMembraneTargetGuestsView {
                membrane_target_guests,
            } => membrane_target_guests,
            other => panic!("unexpected desktop membrane target guests response: {other:?}"),
        }
    }

    fn expect_desktop_membrane_guest_views(response: IpcResponse) -> Vec<DesktopMembraneGuestView> {
        match response {
            IpcResponse::DesktopMembraneGuestsView { membrane_guests } => membrane_guests,
            other => panic!("unexpected desktop membrane guests view response: {other:?}"),
        }
    }

    fn expect_desktop_membrane_agent_views(response: IpcResponse) -> Vec<DesktopMembraneAgentView> {
        match response {
            IpcResponse::DesktopMembraneAgentsView { membrane_agents } => membrane_agents,
            other => panic!("unexpected desktop membrane agents view response: {other:?}"),
        }
    }

    fn expect_desktop_membrane_target_views(
        response: IpcResponse,
    ) -> Vec<DesktopMembraneTargetView> {
        match response {
            IpcResponse::DesktopMembraneTargetsView { membrane_targets } => membrane_targets,
            other => panic!("unexpected desktop membrane targets view response: {other:?}"),
        }
    }

    fn expect_operator_target_views(response: IpcResponse) -> Vec<OperatorTargetView> {
        match response {
            IpcResponse::OperatorTargetsView { operator_targets } => operator_targets,
            other => panic!("unexpected operator targets view response: {other:?}"),
        }
    }

    fn expect_operator_target_status(response: IpcResponse) -> OperatorTargetStatusView {
        match response {
            IpcResponse::OperatorTargetStatusView {
                operator_target_status,
            } => operator_target_status,
            other => panic!("unexpected operator target status response: {other:?}"),
        }
    }

    fn expect_operator_target_guests(response: IpcResponse) -> OperatorTargetGuestInventoryView {
        match response {
            IpcResponse::OperatorTargetGuestsView {
                operator_target_guests,
            } => operator_target_guests,
            other => panic!("unexpected operator target guests response: {other:?}"),
        }
    }

    fn expect_operator_target_agents(response: IpcResponse) -> OperatorTargetAgentInventoryView {
        match response {
            IpcResponse::OperatorTargetAgentsView {
                operator_target_agents,
            } => operator_target_agents,
            other => panic!("unexpected operator target agents response: {other:?}"),
        }
    }

    #[test]
    fn list_components_returns_manifest_relevant_fields_for_registered_components() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: "/tmp/test.sock".into(),
                active_pid: None,
            })
            .expect("seed local hotel");
        graph
            .upsert_guest(&GuestRecord {
                hotel_name: "local-hotel".into(),
                guest_id: "membrane-discord-01".into(),
                role: "membrane.discord".into(),
                config_json: serde_json::json!({
                    "command": "membrane-discord",
                    "args": ["--agent-id", "agent-01"],
                    "env": {
                        "DISCORD_BOT_TOKEN": "token"
                    }
                })
                .to_string(),
                is_active: true,
                active_pid: Some("4242".into()),
                last_active_at: Some(123),
            })
            .expect("seed component guest");
        graph
            .set_config_value(
                "component:membrane-discord-01",
                &serde_json::json!({
                    "guild_id": "1234"
                })
                .to_string(),
            )
            .expect("seed component config");

        let response = IpcServer::handle_list_components(&graph, "local-aiua-01");
        let components = match response {
            IpcResponse::ComponentInventory { components } => components,
            other => panic!("unexpected list_components response: {other:?}"),
        };

        assert_eq!(components.len(), 1);
        assert_eq!(components[0]["guest_id"], "membrane-discord-01");
        assert_eq!(components[0]["role"], "membrane.discord");
        assert_eq!(components[0]["hotel"], "local-hotel");
        assert_eq!(components[0]["command"], "membrane-discord");
        assert_eq!(components[0]["args"][0], "--agent-id");
        assert_eq!(components[0]["env"]["DISCORD_BOT_TOKEN"], "token");
        assert_eq!(components[0]["component_type"], "membrane");
        assert_eq!(components[0]["auto_start"], true);
        assert_eq!(components[0]["component_config"]["guild_id"], "1234");
    }

    #[test]
    fn remove_component_deletes_guest_and_component_config() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: "/tmp/test.sock".into(),
                active_pid: None,
            })
            .expect("seed local hotel");
        graph
            .upsert_guest(&GuestRecord {
                hotel_name: "local-hotel".into(),
                guest_id: "tool-echo-01".into(),
                role: "tool.echo".into(),
                config_json: serde_json::json!({
                    "command": "tool-runner",
                    "args": ["--help"],
                    "env": {}
                })
                .to_string(),
                is_active: false,
                active_pid: None,
                last_active_at: None,
            })
            .expect("seed component guest");
        graph
            .set_config_value(
                "component:tool-echo-01",
                &serde_json::json!({ "description": "temporary" }).to_string(),
            )
            .expect("seed component config");

        let runtime = tokio::runtime::Runtime::new().expect("create tokio runtime");
        let response = runtime.block_on(IpcServer::handle_remove_component(
            &graph,
            "local-aiua-01",
            "tool-echo-01",
        ));

        match response {
            IpcResponse::Standard { ok: true, .. } => {}
            other => panic!("unexpected remove_component response: {other:?}"),
        }

        assert!(
            graph
                .get_guest("local-hotel", "tool-echo-01")
                .expect("load guest")
                .is_none()
        );
        assert!(
            graph
                .get_config_value("component:tool-echo-01")
                .expect("load component config")
                .is_none()
        );
    }

    fn expect_operator_chat_reply(response: IpcResponse) -> OperatorChatTurnReply {
        match response {
            IpcResponse::OperatorChatTurnReply {
                operator_chat_reply,
            } => operator_chat_reply,
            other => panic!("unexpected operator chat reply response: {other:?}"),
        }
    }

    #[derive(Default)]
    struct TestGraphAdapter;

    impl ansible_mesh_core::storage::GraphAdapter for TestGraphAdapter {
        fn upsert_node(&self, _node: &ansible_mesh_core::graph::GraphNode) -> anyhow::Result<()> {
            Ok(())
        }
        fn get_node(
            &self,
            _node_key: &str,
        ) -> anyhow::Result<Option<ansible_mesh_core::graph::GraphNode>> {
            Ok(None)
        }
        fn delete_node(&self, _node_key: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn list_nodes_by_kind(
            &self,
            _kind: &str,
        ) -> anyhow::Result<Vec<ansible_mesh_core::graph::GraphNode>> {
            Ok(vec![])
        }
        fn upsert_edge(&self, _edge: &ansible_mesh_core::graph::GraphEdge) -> anyhow::Result<()> {
            Ok(())
        }
        fn delete_edge(&self, _edge_key: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn list_edges_from(
            &self,
            _src_node_key: &str,
            _edge_kind: Option<&str>,
        ) -> anyhow::Result<Vec<ansible_mesh_core::graph::GraphEdge>> {
            Ok(vec![])
        }
    }

    #[derive(Default)]
    struct MockMaterializationRequester {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl GuestMaterializationRequester for MockMaterializationRequester {
        async fn ensure_guest_active(&self, _guest_id: &str) -> anyhow::Result<bool> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        }
    }

    fn test_socket_path() -> String {
        format!("/tmp/ipc-e2e-{}.sock", Uuid::new_v4().simple())
    }

    static IPC_TEST_ENV_LOCK: LazyLock<StdMutex<()>> = LazyLock::new(|| StdMutex::new(()));

    fn ipc_env_guard() -> std::sync::MutexGuard<'static, ()> {
        IPC_TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    #[tokio::test]
    async fn emit_task_is_delivered_to_registered_local_role() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, mut dispatcher_rx) = mpsc::channel(8);
        let graph = Arc::new(GraphDomain::new(Arc::new(TestGraphAdapter)));
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let agent_identity = GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let membrane_identity = GuestIdentity {
            guest_id: "membrane-local".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        };

        let mut agent = PhiloticClient::connect(agent_identity)
            .await
            .expect("agent connect");
        let mut membrane = PhiloticClient::connect(membrane_identity)
            .await
            .expect("membrane connect");

        let task_payload = serde_json::json!({
            "source": "telegram",
            "chat_id": "12345",
            "content": "hello from telegram"
        })
        .to_string();

        let response = membrane
            .send_request(IpcRequest::EmitTask {
                target_node: "local-aiua-01".into(),
                target_role: "agent".into(),
                target_guest_id: None,
                task_json: task_payload.clone(),
            })
            .await
            .expect("emit task");

        assert!(matches!(response, IpcResponse::Standard { ok: true, .. }));

        let delivered =
            tokio::time::timeout(tokio::time::Duration::from_secs(1), agent.recv_task())
                .await
                .expect("agent should receive task before timeout")
                .expect("agent recv should succeed");

        match delivered {
            IpcResponse::InboundTask {
                source_node,
                task_json,
                ..
            } => {
                assert_eq!(source_node, "local-aiua-01");
                assert_eq!(task_json, task_payload);
            }
            other => panic!("unexpected inbound response: {other:?}"),
        }

        let ledger_msg = dispatcher_rx
            .recv()
            .await
            .expect("ledger command should be emitted");
        match ledger_msg {
            LedgerCommand::AppendLocal(env) => {
                assert_eq!(env.source_node_id, "local-aiua-01");
                assert_eq!(env.target_node_id.as_deref(), Some("local-aiua-01"));
            }
            _ => panic!("unexpected ledger command"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn emit_task_can_target_specific_guest_within_shared_role() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph = Arc::new(GraphDomain::new(Arc::new(TestGraphAdapter)));
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut sender = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("sender connect");
        let mut telegram_membrane = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-telegram-01".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("telegram membrane connect");
        let mut whatsapp_membrane = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-whatsapp-01".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("whatsapp membrane connect");

        sender
            .send_request(IpcRequest::EmitTask {
                target_node: "local-aiua-01".into(),
                target_role: "membrane".into(),
                target_guest_id: Some("membrane-telegram-01".into()),
                task_json: serde_json::json!({
                    "action": "send_reply",
                    "session_id": "telegram:123:agent-jane-01",
                    "turn_id": "turn-1",
                    "chat_id": "123",
                    "content": "hello targeted membrane"
                })
                .to_string(),
            })
            .await
            .expect("emit targeted task");

        let targeted = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            telegram_membrane.recv_task(),
        )
        .await
        .expect("telegram membrane should receive targeted task")
        .expect("telegram membrane recv should succeed");
        match targeted {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("payload should decode");
                assert_eq!(payload["content"], "hello targeted membrane");
            }
            other => panic!("unexpected telegram membrane response: {other:?}"),
        }

        assert!(
            tokio::time::timeout(
                tokio::time::Duration::from_millis(150),
                whatsapp_membrane.recv_task()
            )
            .await
            .is_err(),
            "non-target membrane should not receive guest-targeted task"
        );

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn emit_task_routes_agent_work_to_active_incarnation_from_session() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-role-route".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: Some("agent-jane:developer".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({}),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut orchestrator = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-jane:orchestrator".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("orchestrator connect");
        let mut developer = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-jane:developer".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("developer connect");
        let mut membrane = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-local".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("membrane connect");

        let task_payload = serde_json::json!({
            "session_id": "sess-role-route",
            "source": "telegram",
            "chat_id": "123",
            "content": "route to developer"
        })
        .to_string();

        let response = membrane
            .send_request(IpcRequest::EmitTask {
                target_node: "local-aiua-01".into(),
                target_role: "agent".into(),
                target_guest_id: None,
                task_json: task_payload.clone(),
            })
            .await
            .expect("emit task");

        assert!(matches!(response, IpcResponse::Standard { ok: true, .. }));

        let delivered =
            tokio::time::timeout(tokio::time::Duration::from_secs(1), developer.recv_task())
                .await
                .expect("developer should receive task before timeout")
                .expect("developer recv should succeed");
        match delivered {
            IpcResponse::InboundTask { task_json, .. } => {
                assert_eq!(task_json, task_payload);
            }
            other => panic!("unexpected developer inbound response: {other:?}"),
        }

        assert!(
            tokio::time::timeout(
                tokio::time::Duration::from_millis(200),
                orchestrator.recv_task()
            )
            .await
            .is_err(),
            "orchestrator should not receive task when developer is active incarnation"
        );

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn emit_task_falls_back_to_orchestrator_when_active_incarnation_is_unregistered() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-jane-01".into(),
                role_name: "orchestrator".into(),
                guest_id: "agent-jane:orchestrator".into(),
                toolset_profile: "orchestrator".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
            })
            .expect("orchestrator role should seed");
        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-role-fallback".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: Some("agent-jane:developer".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({}),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut orchestrator = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-jane:orchestrator".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("orchestrator connect");
        let mut membrane = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-local".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("membrane connect");

        let task_payload = serde_json::json!({
            "session_id": "sess-role-fallback",
            "source": "telegram",
            "chat_id": "123",
            "content": "route to fallback orchestrator"
        })
        .to_string();

        let response = membrane
            .send_request(IpcRequest::EmitTask {
                target_node: "local-aiua-01".into(),
                target_role: "agent".into(),
                target_guest_id: None,
                task_json: task_payload.clone(),
            })
            .await
            .expect("emit task");

        assert!(matches!(response, IpcResponse::Standard { ok: true, .. }));

        let delivered = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            orchestrator.recv_task(),
        )
        .await
        .expect("orchestrator should receive fallback task before timeout")
        .expect("orchestrator recv should succeed");
        match delivered {
            IpcResponse::InboundTask { task_json, .. } => {
                assert_eq!(task_json, task_payload);
            }
            other => panic!("unexpected orchestrator inbound response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn emit_task_defaults_to_orchestrator_when_session_has_no_active_incarnation() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-jane-01".into(),
                role_name: "orchestrator".into(),
                guest_id: "agent-jane:orchestrator".into(),
                toolset_profile: "orchestrator".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
            })
            .expect("orchestrator role should seed");
        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-role-default".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: None,
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({}),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut orchestrator = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-jane:orchestrator".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("orchestrator connect");
        let mut membrane = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-local".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("membrane connect");

        let task_payload = serde_json::json!({
            "session_id": "sess-role-default",
            "source": "telegram",
            "chat_id": "123",
            "content": "route to default orchestrator"
        })
        .to_string();

        let response = membrane
            .send_request(IpcRequest::EmitTask {
                target_node: "local-aiua-01".into(),
                target_role: "agent".into(),
                target_guest_id: None,
                task_json: task_payload.clone(),
            })
            .await
            .expect("emit task");

        assert!(matches!(response, IpcResponse::Standard { ok: true, .. }));

        let delivered = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            orchestrator.recv_task(),
        )
        .await
        .expect("orchestrator should receive default task before timeout")
        .expect("orchestrator recv should succeed");
        match delivered {
            IpcResponse::InboundTask { task_json, .. } => {
                assert_eq!(task_json, task_payload);
            }
            other => panic!("unexpected orchestrator inbound response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn emit_task_parks_for_missing_active_incarnation_and_flushes_after_register() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
            })
            .expect("seed local hotel");
        graph
            .seed_guests(
                "local-hotel",
                &[GuestRecord {
                    hotel_name: "local-hotel".into(),
                    guest_id: "agent-jane:developer".into(),
                    role: "agent".into(),
                    config_json: "{}".into(),
                    is_active: true,
                    active_pid: None,
                    last_active_at: None,
                }],
            )
            .expect("seed developer guest");
        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-role-park".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: Some("agent-jane:developer".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({}),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");

        let requester = Arc::new(MockMaterializationRequester::default());
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph)
            .with_materialization_requester(requester.clone());

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut membrane = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-local".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("membrane connect");

        let task_payload = serde_json::json!({
            "session_id": "sess-role-park",
            "source": "telegram",
            "chat_id": "123",
            "content": "park until developer registers"
        })
        .to_string();

        let response = membrane
            .send_request(IpcRequest::EmitTask {
                target_node: "local-aiua-01".into(),
                target_role: "agent".into(),
                target_guest_id: None,
                task_json: task_payload.clone(),
            })
            .await
            .expect("emit task");

        assert!(matches!(response, IpcResponse::Standard { ok: true, .. }));
        assert_eq!(requester.calls.load(Ordering::SeqCst), 1);

        let mut developer = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-jane:developer".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("developer connect");

        let delivered =
            tokio::time::timeout(tokio::time::Duration::from_secs(1), developer.recv_task())
                .await
                .expect("developer should receive parked task after register")
                .expect("developer recv should succeed");
        match delivered {
            IpcResponse::InboundTask { task_json, .. } => {
                assert_eq!(task_json, task_payload);
            }
            other => panic!("unexpected developer inbound response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn handoff_to_live_role_switches_active_incarnation_and_delivers_bundle() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-handoff-live".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: Some("agent-jane:orchestrator".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({}),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-jane-01".into(),
                role_name: "developer".into(),
                guest_id: "agent-jane:developer".into(),
                toolset_profile: "codex".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
            })
            .expect("developer role should seed");
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut orchestrator = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-jane:orchestrator".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("orchestrator connect");
        let mut developer = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-jane:developer".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("developer connect");

        let response = orchestrator
            .send_request(IpcRequest::HandoffToRole {
                session_id: "sess-handoff-live".into(),
                role_name: "developer".into(),
                handoff_bundle: HandoffBundle {
                    goal: "implement the fix".into(),
                    context_excerpt: "need code changes".into(),
                    session_id: "sess-handoff-live".into(),
                    initiating_turn_id: "turn-1".into(),
                    return_to: Some("orchestrator".into()),
                    handoff_reason: Some("manual_role_switch".into()),
                    active_goal: Some("implement the fix".into()),
                    active_constraints: vec!["same_identity_role_handoff".into()],
                    relevant_session_facts: vec!["session_status=active".into()],
                    working_summary: Some(
                        "phase=waiting_model, iteration=1, pending_tool=false, pending_approval=false"
                            .into(),
                    ),
                    from_role: Some("orchestrator".into()),
                    to_role: Some("developer".into()),
                    suggested_memory_refs: Vec::new(),
                    expected_return_mode: Some("required".into()),
                    cleanup_actions: vec!["switch_active_role".into()],
                },
            })
            .await
            .expect("handoff request");

        match response {
            IpcResponse::HandoffAck {
                handoff_guest_id,
                became_active,
            } => {
                assert_eq!(handoff_guest_id, "agent-jane:developer");
                assert!(became_active);
            }
            other => panic!("unexpected handoff response: {other:?}"),
        }

        let session = graph
            .get_session("sess-handoff-live")
            .expect("session lookup")
            .expect("session exists");
        assert_eq!(
            session.active_incarnation_id.as_deref(),
            Some("agent-jane:developer")
        );

        let delivered =
            tokio::time::timeout(tokio::time::Duration::from_secs(1), developer.recv_task())
                .await
                .expect("developer should receive handoff bundle")
                .expect("developer recv should succeed");
        match delivered {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("handoff payload should decode");
                assert_eq!(payload["action"], "handoff_bundle");
                assert_eq!(payload["handoff_bundle"]["goal"], "implement the fix");
                assert_eq!(
                    payload["handoff_bundle"]["handoff_reason"],
                    "manual_role_switch"
                );
                assert_eq!(
                    payload["handoff_bundle"]["expected_return_mode"],
                    "required"
                );
            }
            other => panic!("unexpected developer inbound response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn handoff_to_missing_role_parks_until_register_then_switches_active_incarnation() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
            })
            .expect("seed local hotel");
        graph
            .seed_guests(
                "local-hotel",
                &[GuestRecord {
                    hotel_name: "local-hotel".into(),
                    guest_id: "agent-jane:developer".into(),
                    role: "agent".into(),
                    config_json: "{}".into(),
                    is_active: true,
                    active_pid: None,
                    last_active_at: None,
                }],
            )
            .expect("seed developer guest");
        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-handoff-park".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: Some("agent-jane:orchestrator".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({}),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-jane-01".into(),
                role_name: "developer".into(),
                guest_id: "agent-jane:developer".into(),
                toolset_profile: "codex".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
            })
            .expect("developer role should seed");

        let requester = Arc::new(MockMaterializationRequester::default());
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        )
        .with_materialization_requester(requester.clone());

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut orchestrator = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-jane:orchestrator".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("orchestrator connect");

        let response = orchestrator
            .send_request(IpcRequest::HandoffToRole {
                session_id: "sess-handoff-park".into(),
                role_name: "developer".into(),
                handoff_bundle: HandoffBundle {
                    goal: "implement later".into(),
                    context_excerpt: "waiting for startup".into(),
                    session_id: "sess-handoff-park".into(),
                    initiating_turn_id: "turn-1".into(),
                    return_to: Some("orchestrator".into()),
                    handoff_reason: Some("manual_role_switch".into()),
                    active_goal: Some("implement later".into()),
                    active_constraints: vec!["same_identity_role_handoff".into()],
                    relevant_session_facts: vec!["session_status=active".into()],
                    working_summary: Some(
                        "phase=waiting_model, iteration=1, pending_tool=false, pending_approval=false"
                            .into(),
                    ),
                    from_role: Some("orchestrator".into()),
                    to_role: Some("developer".into()),
                    suggested_memory_refs: Vec::new(),
                    expected_return_mode: Some("required".into()),
                    cleanup_actions: vec!["switch_active_role".into()],
                },
            })
            .await
            .expect("handoff request");

        match response {
            IpcResponse::HandoffAck {
                handoff_guest_id,
                became_active,
            } => {
                assert_eq!(handoff_guest_id, "agent-jane:developer");
                assert!(!became_active);
            }
            other => panic!("unexpected handoff response: {other:?}"),
        }

        assert_eq!(requester.calls.load(Ordering::SeqCst), 1);
        let session_before = graph
            .get_session("sess-handoff-park")
            .expect("session lookup")
            .expect("session exists");
        assert_eq!(
            session_before.active_incarnation_id.as_deref(),
            Some("agent-jane:orchestrator")
        );

        let mut developer = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-jane:developer".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("developer connect");

        let delivered =
            tokio::time::timeout(tokio::time::Duration::from_secs(1), developer.recv_task())
                .await
                .expect("developer should receive parked handoff bundle")
                .expect("developer recv should succeed");
        match delivered {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("handoff payload should decode");
                assert_eq!(payload["action"], "handoff_bundle");
                assert_eq!(payload["handoff_bundle"]["goal"], "implement later");
                assert_eq!(
                    payload["handoff_bundle"]["expected_return_mode"],
                    "required"
                );
            }
            other => panic!("unexpected developer inbound response: {other:?}"),
        }

        let session_after = graph
            .get_session("sess-handoff-park")
            .expect("session lookup")
            .expect("session exists");
        assert_eq!(
            session_after.active_incarnation_id.as_deref(),
            Some("agent-jane:developer")
        );

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn spawn_subagent_acquires_lease_and_returns_ok() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-jane:developer".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");

        let response = agent
            .send_request(IpcRequest::SpawnSubagent {
                session_id: "sess-subagent-1".into(),
                delegation: SubagentDelegation {
                    parent_agent_id: "agent-jane-01".into(),
                    parent_role: "developer".into(),
                    subagent_kind: "research_worker".into(),
                    goal: "Read files and report risks.".into(),
                    context_packet: SubagentContextPacket {
                        summary: "Bounded file review requested by parent role.".into(),
                        session_facts: vec!["session_status=active".into()],
                        constraints: vec!["subagent_lightweight_default".into()],
                        memory_refs: Vec::new(),
                    },
                    allowed_tools: vec!["workspace.read".into()],
                    allowed_skills: vec!["research".into()],
                    memory_allowance: Some("none_by_default".into()),
                    writeback_allowance: Some("summary_only_parent_mediated".into()),
                    iteration_budget: Some(6),
                    ttl_seconds: Some(900),
                    completion_contract: SubagentCompletionContract {
                        summary_required: true,
                        artifact_refs_expected: false,
                        failure_summary_required: true,
                        requires_parent_ack: true,
                    },
                    ..Default::default()
                },
            })
            .await
            .expect("spawn subagent request");

        match response {
            IpcResponse::SpawnSubagentOk {
                subagent_guest_id,
                confirmed_lease,
            } => {
                assert!(
                    !subagent_guest_id.is_empty(),
                    "subagent_guest_id should be a UUID"
                );
                assert!(confirmed_lease.is_active());
                assert_eq!(confirmed_lease.lease_epoch, 1);
                assert!(confirmed_lease.lease_expires_at > 0);
                assert_eq!(
                    confirmed_lease.owner_component_type.as_deref(),
                    Some("philote-worker")
                );
            }
            other => panic!("unexpected spawn_subagent response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn emit_task_can_target_specific_guest_with_large_audio_payload() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph = Arc::new(GraphDomain::new(Arc::new(TestGraphAdapter)));
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut sender = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("sender connect");
        let mut telegram_membrane = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-telegram-01".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("telegram membrane connect");

        let large_audio = "A".repeat(256 * 1024);
        sender
            .send_request(IpcRequest::EmitTask {
                target_node: "local-aiua-01".into(),
                target_role: "membrane".into(),
                target_guest_id: Some("membrane-telegram-01".into()),
                task_json: serde_json::json!({
                    "action": "send_reply",
                    "session_id": "telegram:voice:agent-jane-01",
                    "turn_id": "turn-voice-1",
                    "chat_id": "123",
                    "content": "voice reply",
                    "audio_artifact": serde_json::json!({
                        "mime_type": "audio/ogg",
                        "audio_base64": large_audio
                    }).to_string(),
                    "send_text_caption": false
                })
                .to_string(),
            })
            .await
            .expect("emit targeted large task");

        let delivered = tokio::time::timeout(
            tokio::time::Duration::from_secs(2),
            telegram_membrane.recv_task(),
        )
        .await
        .expect("telegram membrane should receive targeted large task")
        .expect("telegram membrane recv should succeed");

        match delivered {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("payload should decode");
                let audio_artifact = payload["audio_artifact"]
                    .as_str()
                    .expect("audio_artifact should be a string");
                assert!(audio_artifact.len() > 256 * 1024);
                assert_eq!(payload["chat_id"], "123");
            }
            other => panic!("unexpected telegram membrane response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn emit_task_for_remote_node_stays_off_local_inbox() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, mut dispatcher_rx) = mpsc::channel(8);
        let graph = Arc::new(GraphDomain::new(Arc::new(TestGraphAdapter)));
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut sender = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("sender connect");
        let mut local_agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-receiver".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("receiver connect");

        sender
            .send_request(IpcRequest::EmitTask {
                target_node: "remote-ansible-02".into(),
                target_role: "agent".into(),
                target_guest_id: None,
                task_json: serde_json::json!({"content":"remote task"}).to_string(),
            })
            .await
            .expect("emit remote task");

        let recv = tokio::time::timeout(
            tokio::time::Duration::from_millis(200),
            local_agent.recv_task(),
        )
        .await;
        assert!(
            recv.is_err(),
            "remote-targeted task should not be delivered locally"
        );

        match dispatcher_rx.recv().await.expect("ledger command") {
            LedgerCommand::AppendLocal(env) => {
                assert_eq!(env.target_node_id.as_deref(), Some("remote-ansible-02"));
                assert_eq!(env.target_agent_id.as_deref(), Some("agent"));
            }
            _ => panic!("unexpected ledger command"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn get_config_can_return_live_mesh_registry_snapshot() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph = Arc::new(GraphDomain::new(Arc::new(TestGraphAdapter)));
        let registry = Arc::new(RwLock::new(NodeRegistry::new()));
        registry.write().await.update_node(
            NodeCapabilities {
                node_id: "aria-node".into(),
                roles: vec![ansible_mesh_core::NodeRole::AnsibleNode],
                models: vec![],
                tools: vec![],
                constraints: Default::default(),
            },
            vec![CapabilityAdvertisement {
                hotel_id: "aria-architect-hotel".into(),
                node_id: "aria-node".into(),
                incarnation_id: "aria-architect-hotel:model-controller-gemini".into(),
                target_role: "model".into(),
                availability_state: "live".into(),
                selection_hint: Some("remote_fallback".into()),
                latency_hint_ms: Some(12),
                max_concurrent_jobs: Some(4),
                active_jobs: 1,
                queue_depth: 0,
            }],
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "aria-vps".into(),
                port: 9002,
            }),
        );
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph)
            .with_registry(registry);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut client = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("client connect");

        let response = client
            .send_request(IpcRequest::GetConfig {
                key: "__mesh_registry__".into(),
            })
            .await
            .expect("mesh registry request");

        match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => {
                let snapshot: serde_json::Value =
                    serde_json::from_str(&value_json).expect("snapshot should decode");
                assert_eq!(snapshot["nodes"][0]["node_id"], "aria-node");
                assert_eq!(
                    snapshot["nodes"][0]["execution_reachability"]["host"],
                    "aria-vps"
                );
                assert_eq!(
                    snapshot["nodes"][0]["advertisements"][0]["target_role"],
                    "model"
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn get_secret_returns_vault_secret_for_authorized_guest() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        let vault_key = base64::engine::general_purpose::STANDARD.encode([5u8; 32]);
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
            std::env::set_var("PHILOTIC_VAULT_MASTER_KEY", vault_key);
        }

        let secret_ref = store_secret(
            &graph,
            SecretInput {
                secret_kind: "gemini-access-token".into(),
                scope: "hotel".into(),
                allowed_roles: vec!["model".into()],
                allowed_guests: Vec::new(),
                plaintext: "top-secret".into(),
            },
        )
        .expect("store secret");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let mut guest = PhiloticClient::connect(GuestIdentity {
            guest_id: "model-gemini-guest".into(),
            role: "model".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("guest connect");

        let response = guest
            .send_request(IpcRequest::GetSecret { secret_ref })
            .await
            .expect("get secret");

        match response {
            IpcResponse::SecretData {
                value_json: Some(value_json),
                ..
            } => {
                assert_eq!(
                    serde_json::from_str::<String>(&value_json).unwrap(),
                    "top-secret"
                );
            }
            other => panic!("unexpected secret response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
            std::env::remove_var("PHILOTIC_VAULT_MASTER_KEY");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn emit_task_persists_session_and_turn_metadata() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let membrane_identity = GuestIdentity {
            guest_id: "membrane-local".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        };
        let mut membrane = PhiloticClient::connect(membrane_identity)
            .await
            .expect("membrane connect");

        membrane
            .send_request(IpcRequest::EmitTask {
                target_node: "local-aiua-01".into(),
                target_role: "agent".into(),
                target_guest_id: None,
                task_json: serde_json::json!({
                    "source": "telegram",
                    "session_id": "telegram:123:agent-jane-01",
                    "turn_id": "telegram-update-1",
                    "chat_id": "123",
                    "content": "hello from telegram"
                })
                .to_string(),
            })
            .await
            .expect("emit task");

        let session = graph
            .get_session("telegram:123:agent-jane-01")
            .expect("session lookup should work")
            .expect("session should exist");
        assert_eq!(session.channel_kind.as_deref(), Some("telegram"));

        let turns = graph
            .list_session_turns("telegram:123:agent-jane-01", 10)
            .expect("turn listing should work");
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].turn_id, "telegram-update-1");
        assert_eq!(turns[0].user_message_json["content"], "hello from telegram");

        let events = graph
            .list_session_events("telegram:123:agent-jane-01", 10)
            .expect("event listing should work");
        assert!(!events.is_empty(), "session events should be recorded");

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn get_config_can_return_canonical_session_snapshot() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        graph
            .upsert_agent_identity(&AgentIdentityRecord {
                agent_id: "agent-jane-01".into(),
                persona_name: "Jane".into(),
                authority_hotel: "local-hotel".into(),
                bundle_json: serde_json::json!({
                    "soul_text": "Soul anchor",
                    "identity_text": "Identity anchor",
                    "user_context_text": "User anchor"
                }),
            })
            .expect("agent identity should seed");
        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-1".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: Some("agent-jane:developer".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({"summary": "hello summary"}),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");
        graph
            .upsert_session_turn(&SessionTurnRecord {
                turn_id: "turn-1".into(),
                session_id: "sess-1".into(),
                request_event_id: Some("req-1".into()),
                user_message_json: serde_json::json!({"content": "hello"}),
                status: "completed".into(),
                response_json: Some(serde_json::json!({"content": "hi"})),
                error_json: None,
                started_at: Some(1),
                completed_at: Some(2),
            })
            .expect("turn should seed");
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-jane-01".into(),
                role_name: "developer".into(),
                guest_id: "agent-jane:developer".into(),
                toolset_profile: "codex".into(),
                role_identity_addendum: Some("Focus on implementation and code changes.".into()),
                role_manifest: None,
                is_admin: false,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
            })
            .expect("developer role should seed");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let agent_identity = GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let mut agent = PhiloticClient::connect(agent_identity)
            .await
            .expect("agent connect");

        let response = agent
            .send_request(IpcRequest::GetConfig {
                key: "__session_snapshot__:sess-1".into(),
            })
            .await
            .expect("snapshot request should succeed");

        match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => {
                let snapshot: serde_json::Value =
                    serde_json::from_str(&value_json).expect("snapshot should decode");
                assert_eq!(snapshot["session_id"], "sess-1");
                assert_eq!(snapshot["source"], "telegram");
                assert_eq!(snapshot["active_incarnation_id"], "agent-jane:developer");
                assert_eq!(snapshot["role_activation"]["role_name"], "developer");
                assert_eq!(snapshot["role_activation"]["toolset_profile_ref"], "codex");
                assert_eq!(
                    snapshot["role_activation"]["role_addendum"],
                    "Focus on implementation and code changes."
                );
                assert_eq!(snapshot["agent_profile"]["soul_text"], "Soul anchor");
                assert_eq!(
                    snapshot["agent_profile"]["identity_text"],
                    "Identity anchor"
                );
                assert_eq!(snapshot["recent_turns"][0]["user_content"], "hello");
                assert_eq!(snapshot["recent_turns"][0]["assistant_content"], "hi");
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn session_snapshot_seeds_bindings_from_toolset_profile_on_fresh_role_session() {
        let _guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        // Seed profile with known allowed_tools and allowed_skills.
        graph
            .upsert_toolset_profile(&ansible_mesh_core::graph::ToolsetProfileRecord {
                profile_name: "codex".into(),
                allowed_tools: vec!["session.status".into(), "workspace.read".into()],
                allowed_classes: vec!["session".into()],
                allowed_skills: vec!["handoff.back".into()],
                description: None,
            })
            .expect("toolset profile should seed");
        graph
            .upsert_agent_identity(&AgentIdentityRecord {
                agent_id: "agent-jane-01".into(),
                persona_name: "Jane".into(),
                authority_hotel: "local-hotel".into(),
                bundle_json: serde_json::json!({}),
            })
            .expect("agent identity");
        // Session with active_incarnation_id but NO bindings in summary_json.
        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-seed".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: Some("agent-jane:codex".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("456".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({}),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session");
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-jane-01".into(),
                role_name: "codex".into(),
                guest_id: "agent-jane:codex".into(),
                toolset_profile: "codex".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
            })
            .expect("role incarnation");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local-2".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("connect");

        let response = agent
            .send_request(IpcRequest::GetConfig {
                key: "__session_snapshot__:sess-seed".into(),
            })
            .await
            .expect("snapshot request");

        match response {
            IpcResponse::ConfigData {
                value_json: Some(vj),
                ..
            } => {
                let snap: serde_json::Value = serde_json::from_str(&vj).expect("decode snapshot");
                let toolset = snap["bindings"]["effective_toolset"]
                    .as_array()
                    .expect("effective_toolset should be an array");
                assert!(
                    toolset.iter().any(|t| t == "session.status"),
                    "expected session.status in effective_toolset, got {toolset:?}"
                );
                assert!(
                    toolset.iter().any(|t| t == "workspace.read"),
                    "expected workspace.read in effective_toolset, got {toolset:?}"
                );
                let skillset = snap["bindings"]["effective_skillset"]
                    .as_array()
                    .expect("effective_skillset should be an array");
                assert!(
                    skillset.iter().any(|s| s == "handoff.back"),
                    "expected handoff.back in effective_skillset, got {skillset:?}"
                );
                assert_eq!(snap["role_activation"]["toolset_profile_ref"], "codex");
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn session_snapshot_includes_approval_policy_from_session_summary() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-approval".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: None,
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({
                    "approval_policy": {
                        "auto_approve_all": true
                    }
                }),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");

        let response = agent
            .send_request(IpcRequest::GetConfig {
                key: "__session_snapshot__:sess-approval".into(),
            })
            .await
            .expect("snapshot request should succeed");

        match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => {
                let snapshot: serde_json::Value =
                    serde_json::from_str(&value_json).expect("snapshot should decode");
                assert_eq!(snapshot["approval_policy"]["auto_approve_all"], true);
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn session_snapshot_includes_bindings_and_status_from_session_summary() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-bindings".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: None,
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "paused".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({
                    "bindings": {
                        "effective_toolset": ["echo"],
                        "effective_skillset": ["planning"],
                        "effective_workspace_ref": "workspace://main",
                        "effective_model_controller": "gemini-flash"
                    }
                }),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");
        let mut tool = PhiloticClient::connect(GuestIdentity {
            guest_id: "tool-runner-local".into(),
            role: "tool".into(),
            supported_tools: vec!["echo".into()],
        })
        .await
        .expect("tool connect");
        tool.send_request(IpcRequest::SubscribeInbox {
            role: "tool.echo".into(),
        })
        .await
        .expect("tool subscribe");

        let response = agent
            .send_request(IpcRequest::GetConfig {
                key: "__session_snapshot__:sess-bindings".into(),
            })
            .await
            .expect("snapshot request should succeed");

        match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => {
                let snapshot: serde_json::Value =
                    serde_json::from_str(&value_json).expect("snapshot should decode");
                assert_eq!(snapshot["status"], "paused");
                assert_eq!(snapshot["bindings"]["effective_toolset"][0], "echo");
                assert_eq!(
                    snapshot["tool_assembly"]["tools_for_model"][0]["tool_name"],
                    "echo"
                );
                assert_eq!(
                    snapshot["tool_assembly"]["execution_routes"]["echo"]["target_role"],
                    "tool.echo"
                );
                assert_eq!(snapshot["tool_runners"][0]["guest_id"], "tool-runner-local");
                assert_eq!(snapshot["tool_runners"][0]["is_connected"], true);
                assert_eq!(
                    snapshot["bindings"]["effective_workspace_ref"],
                    "workspace://main"
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn session_snapshot_can_route_model_capability_to_remote_advertisement_when_local_model_missing()
     {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let registry = Arc::new(RwLock::new(NodeRegistry::new()));
        registry.write().await.update_node(
            NodeCapabilities {
                node_id: "aria-node".into(),
                roles: vec![ansible_mesh_core::NodeRole::AnsibleNode],
                models: vec!["gemini".into()],
                tools: vec![],
                constraints: Default::default(),
            },
            vec![CapabilityAdvertisement {
                hotel_id: "aria-architect-hotel".into(),
                node_id: "aria-node".into(),
                incarnation_id: "aria-architect-hotel:model-controller-gemini".into(),
                target_role: "model".into(),
                availability_state: "live".into(),
                selection_hint: Some("remote_latency_capacity".into()),
                latency_hint_ms: Some(8),
                max_concurrent_jobs: Some(8),
                active_jobs: 1,
                queue_depth: 0,
            }],
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "aria-vps".into(),
                port: 9002,
            }),
        );
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        )
        .with_registry(registry);

        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-remote-model".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: None,
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({
                    "bindings": {
                        "effective_model_controller": "gemini-flash",
                        "preferred_hotel_id": "aria-architect-hotel"
                    }
                }),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");

        let response = agent
            .send_request(IpcRequest::GetConfig {
                key: "__session_snapshot__:sess-remote-model".into(),
            })
            .await
            .expect("snapshot request should succeed");

        match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => {
                let snapshot: serde_json::Value =
                    serde_json::from_str(&value_json).expect("snapshot should decode");
                assert_eq!(
                    snapshot["component_route_assembly"]["execution_routes"]["text.generate"]["target_node"],
                    "aria-node"
                );
                assert_eq!(
                    snapshot["component_route_assembly"]["execution_routes"]["text.generate"]["target_role"],
                    "model"
                );
                assert_eq!(
                    snapshot["component_route_assembly"]["execution_routes"]["text.generate"]["incarnation_id"],
                    "aria-architect-hotel:model-controller-gemini"
                );
                assert_eq!(
                    snapshot["component_route_assembly"]["execution_routes"]["text.generate"]["selection_reason"],
                    "remote_latency_capacity"
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn session_snapshot_prefers_live_local_generic_model_over_remote_advertisement() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let registry = Arc::new(RwLock::new(NodeRegistry::new()));
        registry.write().await.update_node(
            NodeCapabilities {
                node_id: "aria-node".into(),
                roles: vec![ansible_mesh_core::NodeRole::AnsibleNode],
                models: vec!["gemini".into()],
                tools: vec![],
                constraints: Default::default(),
            },
            vec![CapabilityAdvertisement {
                hotel_id: "aria-architect-hotel".into(),
                node_id: "aria-node".into(),
                incarnation_id: "aria-architect-hotel:model-controller-gemini".into(),
                target_role: "model".into(),
                availability_state: "live".into(),
                selection_hint: Some("remote_latency_capacity".into()),
                latency_hint_ms: Some(8),
                max_concurrent_jobs: Some(8),
                active_jobs: 1,
                queue_depth: 0,
            }],
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "aria-vps".into(),
                port: 9002,
            }),
        );
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        )
        .with_registry(registry);

        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-local-model".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: None,
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({
                    "bindings": {
                        "effective_model_controller": "gemini-flash"
                    }
                }),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");
        let mut local_model = PhiloticClient::connect(GuestIdentity {
            guest_id: "local-aiua-01:model-controller-gemini".into(),
            role: "model".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("local model connect");
        local_model
            .send_request(IpcRequest::SubscribeInbox {
                role: "model".into(),
            })
            .await
            .expect("local model subscribe");

        let response = agent
            .send_request(IpcRequest::GetConfig {
                key: "__session_snapshot__:sess-local-model".into(),
            })
            .await
            .expect("snapshot request should succeed");

        match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => {
                let snapshot: serde_json::Value =
                    serde_json::from_str(&value_json).expect("snapshot should decode");
                assert_eq!(
                    snapshot["component_route_assembly"]["execution_routes"]["text.generate"]["target_node"],
                    "local-aiua-01"
                );
                assert_eq!(
                    snapshot["component_route_assembly"]["execution_routes"]["text.generate"]["target_role"],
                    "model"
                );
                assert_eq!(
                    snapshot["component_route_assembly"]["execution_routes"]["text.generate"]["incarnation_id"],
                    "local-aiua-01:model-controller-gemini"
                );
                assert_eq!(
                    snapshot["component_route_assembly"]["execution_routes"]["text.generate"]["selection_reason"],
                    "live_local_fallback"
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn session_snapshot_prefers_local_active_guest_when_model_subscriber_visibility_is_missing()
     {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let registry = Arc::new(RwLock::new(NodeRegistry::new()));
        registry.write().await.update_node(
            NodeCapabilities {
                node_id: "aria-node".into(),
                roles: vec![ansible_mesh_core::NodeRole::AnsibleNode],
                models: vec!["gemini".into()],
                tools: vec![],
                constraints: Default::default(),
            },
            vec![CapabilityAdvertisement {
                hotel_id: "aria-architect-hotel".into(),
                node_id: "aria-node".into(),
                incarnation_id: "aria-architect-hotel:model-controller-gemini".into(),
                target_role: "model".into(),
                availability_state: "live".into(),
                selection_hint: Some("remote_latency_capacity".into()),
                latency_hint_ms: Some(8),
                max_concurrent_jobs: Some(8),
                active_jobs: 1,
                queue_depth: 0,
            }],
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "aria-vps".into(),
                port: 9002,
            }),
        );
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        )
        .with_registry(registry);

        let mut hotel = crate::default_hotel_record("local");
        hotel.ipc_socket_path = socket_path.clone();
        graph.upsert_hotel(&hotel).expect("hotel should seed");
        graph
            .seed_guests("local", &crate::default_guest_seed("local"))
            .expect("local guests should seed");
        graph
            .set_guest_pid("local", "local:model-controller-gemini", Some("4242"))
            .expect("local model guest pid should seed");
        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-local-active-guest".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: None,
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({
                    "bindings": {
                        "effective_model_controller": "gemini-flash"
                    }
                }),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");

        let response = agent
            .send_request(IpcRequest::GetConfig {
                key: "__session_snapshot__:sess-local-active-guest".into(),
            })
            .await
            .expect("snapshot request should succeed");

        match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => {
                let snapshot: serde_json::Value =
                    serde_json::from_str(&value_json).expect("snapshot should decode");
                assert_eq!(
                    snapshot["component_route_assembly"]["execution_routes"]["text.generate"]["target_node"],
                    "local-aiua-01"
                );
                assert_eq!(
                    snapshot["component_route_assembly"]["execution_routes"]["text.generate"]["target_role"],
                    "model"
                );
                assert!(snapshot["component_route_assembly"]["execution_routes"]["text.generate"]["incarnation_id"].is_null());
                assert_eq!(
                    snapshot["component_route_assembly"]["execution_routes"]["text.generate"]["selection_reason"],
                    "local_active_guest_fallback"
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn session_snapshot_includes_workspace_runner_base_config() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-workspace-policy".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: None,
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({
                    "bindings": {
                        "effective_toolset": ["workspace.read"],
                        "effective_workspace_ref": "workspace://main",
                        "workspace_runner_config": {
                            "default_workspace_ref": "workspace://policy",
                            "allowed_tools": ["workspace.read"],
                            "max_read_bytes": 8192,
                            "max_search_results": 25
                        }
                    }
                }),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");
        graph
            .set_config_value(
                "tool_runner_registry",
                &serde_json::json!([
                    {
                        "guest_id": "tool-runner-local",
                        "supported_tools": ["workspace.read"],
                        "last_seen_at": 42
                    }
                ])
                .to_string(),
            )
            .expect("registry should seed");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");
        let mut tool = PhiloticClient::connect(GuestIdentity {
            guest_id: "tool-runner-local".into(),
            role: "tool".into(),
            supported_tools: vec!["workspace.read".into()],
        })
        .await
        .expect("tool connect");
        tool.send_request(IpcRequest::SubscribeInbox {
            role: "tool.workspace.read".into(),
        })
        .await
        .expect("tool subscribe");

        let response = agent
            .send_request(IpcRequest::GetConfig {
                key: "__session_snapshot__:sess-workspace-policy".into(),
            })
            .await
            .expect("snapshot request should succeed");

        match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => {
                let snapshot: serde_json::Value =
                    serde_json::from_str(&value_json).expect("snapshot should decode");
                assert_eq!(
                    snapshot["tool_assembly"]["execution_routes"]["workspace.read"]["task_runner_config"]
                        ["default_workspace_ref"],
                    "workspace://policy"
                );
                assert_eq!(
                    snapshot["tool_assembly"]["execution_routes"]["workspace.read"]["task_runner_config"]
                        ["max_read_bytes"],
                    8192
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn session_snapshot_derives_visible_tools_from_allowed_incarnations() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-incarnations".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: None,
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({
                    "bindings": {
                        "allowed_tool_runner_incarnations": [
                            {
                                "incarnation_id": "tool-runner-remote",
                                "runner_id": "tool-runner-remote",
                                "hotel_id": "remote-hotel",
                                "environment_id": "env://remote",
                                "target_node": "remote-hotel",
                                "target_role": "tool.echo",
                                "supported_tools": ["echo"],
                                "execution_mode": "capability",
                                "selection_hint": "remote_fallback"
                            },
                            {
                                "incarnation_id": "tool-runner-local",
                                "runner_id": "tool-runner-local",
                                "hotel_id": "local-aiua-01",
                                "environment_id": "env://local",
                                "target_node": "local-aiua-01",
                                "target_role": "tool.echo",
                                "supported_tools": ["echo"],
                                "execution_mode": "capability",
                                "selection_hint": "local_live_preferred"
                            }
                        ]
                    }
                }),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");
        graph
            .set_config_value(
                "tool_runner_registry",
                &serde_json::json!([
                    {
                        "guest_id": "tool-runner-remote",
                        "supported_tools": ["echo"],
                        "last_seen_at": 41
                    },
                    {
                        "guest_id": "tool-runner-local",
                        "supported_tools": ["echo"],
                        "last_seen_at": 42
                    }
                ])
                .to_string(),
            )
            .expect("registry should seed");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");
        let mut local_tool = PhiloticClient::connect(GuestIdentity {
            guest_id: "tool-runner-local".into(),
            role: "tool".into(),
            supported_tools: vec!["echo".into()],
        })
        .await
        .expect("local tool connect");
        local_tool
            .send_request(IpcRequest::SubscribeInbox {
                role: "tool.echo".into(),
            })
            .await
            .expect("local tool subscribe");

        let response = agent
            .send_request(IpcRequest::GetConfig {
                key: "__session_snapshot__:sess-incarnations".into(),
            })
            .await
            .expect("snapshot request should succeed");

        match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => {
                let snapshot: serde_json::Value =
                    serde_json::from_str(&value_json).expect("snapshot should decode");
                assert_eq!(
                    snapshot["tool_assembly"]["tools_for_model"][0]["tool_name"],
                    "echo"
                );
                assert_eq!(
                    snapshot["tool_assembly"]["execution_routes"]["echo"]["incarnation_id"],
                    "tool-runner-local"
                );
                assert_eq!(
                    snapshot["tool_assembly"]["execution_routes"]["echo"]["selection_reason"],
                    "local_live_preferred"
                );
                assert_eq!(
                    snapshot["tool_assembly"]["execution_routes"]["echo"]["availability_state"],
                    "live"
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn session_snapshot_prefers_requested_environment_even_when_local_is_live() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-pref-env".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: None,
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({
                    "bindings": {
                        "preferred_environment_id": "env://remote",
                        "allowed_tool_runner_incarnations": [
                            {
                                "incarnation_id": "tool-runner-local",
                                "runner_id": "tool-runner-local",
                                "hotel_id": "local-aiua-01",
                                "environment_id": "env://local",
                                "target_node": "local-aiua-01",
                                "target_role": "tool.echo",
                                "supported_tools": ["echo"],
                                "execution_mode": "capability",
                                "selection_hint": "local_live_preferred"
                            },
                            {
                                "incarnation_id": "tool-runner-remote",
                                "runner_id": "tool-runner-remote",
                                "hotel_id": "remote-hotel",
                                "environment_id": "env://remote",
                                "target_node": "remote-hotel",
                                "target_role": "tool.echo",
                                "supported_tools": ["echo"],
                                "execution_mode": "capability",
                                "selection_hint": "remote_fallback"
                            }
                        ]
                    }
                }),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");
        graph
            .set_config_value(
                "tool_runner_registry",
                &serde_json::json!([
                    {
                        "guest_id": "tool-runner-remote",
                        "supported_tools": ["echo"],
                        "last_seen_at": 41
                    },
                    {
                        "guest_id": "tool-runner-local",
                        "supported_tools": ["echo"],
                        "last_seen_at": 42
                    }
                ])
                .to_string(),
            )
            .expect("registry should seed");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");
        let mut local_tool = PhiloticClient::connect(GuestIdentity {
            guest_id: "tool-runner-local".into(),
            role: "tool".into(),
            supported_tools: vec!["echo".into()],
        })
        .await
        .expect("local tool connect");
        local_tool
            .send_request(IpcRequest::SubscribeInbox {
                role: "tool.echo".into(),
            })
            .await
            .expect("local tool subscribe");

        let response = agent
            .send_request(IpcRequest::GetConfig {
                key: "__session_snapshot__:sess-pref-env".into(),
            })
            .await
            .expect("snapshot request should succeed");

        match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => {
                let snapshot: serde_json::Value =
                    serde_json::from_str(&value_json).expect("snapshot should decode");
                assert_eq!(
                    snapshot["tool_assembly"]["execution_routes"]["echo"]["incarnation_id"],
                    "tool-runner-remote"
                );
                assert_eq!(
                    snapshot["tool_assembly"]["execution_routes"]["echo"]["selection_reason"],
                    "preferred_environment_requires_materialization"
                );
                assert_eq!(
                    snapshot["tool_assembly"]["execution_routes"]["echo"]["availability_state"],
                    "materialization_required"
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn session_snapshot_can_route_tool_to_remote_advertisement_when_local_runner_missing() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let registry = Arc::new(RwLock::new(NodeRegistry::new()));
        registry.write().await.update_node(
            NodeCapabilities {
                node_id: "aria-node".into(),
                roles: vec![ansible_mesh_core::NodeRole::AnsibleNode],
                models: vec![],
                tools: vec![],
                constraints: ansible_mesh_core::NodeConstraints {
                    max_concurrent_jobs: Some(8),
                    latency_hint_ms: Some(10),
                    trust_level: None,
                },
            },
            vec![CapabilityAdvertisement {
                hotel_id: "aria-architect-hotel".into(),
                node_id: "aria-node".into(),
                incarnation_id: "aria-architect-hotel:tool-runner-echo".into(),
                target_role: "tool.echo".into(),
                availability_state: "live".into(),
                selection_hint: Some("remote_latency_capacity".into()),
                latency_hint_ms: Some(10),
                max_concurrent_jobs: Some(8),
                active_jobs: 1,
                queue_depth: 0,
            }],
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "aria-vps".into(),
                port: 9002,
            }),
        );
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        )
        .with_registry(registry);

        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-remote-tool".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: None,
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({
                    "bindings": {
                        "effective_toolset": ["echo"]
                    }
                }),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");

        let response = agent
            .send_request(IpcRequest::GetConfig {
                key: "__session_snapshot__:sess-remote-tool".into(),
            })
            .await
            .expect("snapshot request should succeed");

        match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => {
                let snapshot: serde_json::Value =
                    serde_json::from_str(&value_json).expect("snapshot should decode");
                assert_eq!(
                    snapshot["tool_assembly"]["execution_routes"]["echo"]["target_node"],
                    "aria-node"
                );
                assert_eq!(
                    snapshot["tool_assembly"]["execution_routes"]["echo"]["incarnation_id"],
                    "aria-architect-hotel:tool-runner-echo"
                );
                assert_eq!(
                    snapshot["tool_assembly"]["execution_routes"]["echo"]["selection_reason"],
                    "remote_latency_capacity"
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn tool_runner_registration_persists_durable_registry() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let _tool = PhiloticClient::connect(GuestIdentity {
            guest_id: "tool-runner-local".into(),
            role: "tool".into(),
            supported_tools: vec!["echo".into()],
        })
        .await
        .expect("tool connect");

        let raw = graph
            .get_config_value("tool_runner_registry")
            .expect("registry lookup should work")
            .expect("registry should exist");
        let registry: serde_json::Value =
            serde_json::from_str(&raw).expect("registry should decode");
        assert_eq!(registry[0]["guest_id"], "tool-runner-local");
        assert_eq!(registry[0]["supported_tools"][0], "echo");

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn session_snapshot_marks_registered_but_offline_tools_as_materialization_required() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-dormant-runner".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: None,
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({
                    "bindings": {
                        "effective_toolset": ["echo"]
                    }
                }),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");
        graph
            .set_config_value(
                "tool_runner_registry",
                &serde_json::json!([
                    {
                        "guest_id": "tool-runner-local",
                        "supported_tools": ["echo"],
                        "last_seen_at": 42
                    }
                ])
                .to_string(),
            )
            .expect("registry should seed");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");

        let response = agent
            .send_request(IpcRequest::GetConfig {
                key: "__session_snapshot__:sess-dormant-runner".into(),
            })
            .await
            .expect("snapshot request should succeed");

        match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => {
                let snapshot: serde_json::Value =
                    serde_json::from_str(&value_json).expect("snapshot should decode");
                assert_eq!(
                    snapshot["tool_assembly"]["tools_for_model"][0]["tool_name"],
                    "echo"
                );
                assert_eq!(
                    snapshot["tool_assembly"]["execution_routes"]["echo"]["availability_state"],
                    "materialization_required"
                );
                assert_eq!(snapshot["tool_runners"][0]["is_connected"], false);
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn session_snapshot_uses_per_session_checkpoint_when_agent_has_multiple_sessions() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        for session_id in ["sess-1", "sess-2"] {
            graph
                .upsert_session(&SessionRecord {
                    session_id: session_id.into(),
                    session_kind: "conversation".into(),
                    primary_agent_id: Some("agent-jane-01".into()),
                    active_incarnation_id: None,
                    channel_kind: Some("telegram".into()),
                    channel_session_key: Some(format!("chat-{session_id}")),
                    status: "active".into(),
                    lease_owner_component_id: None,
                    lease_expires_at: None,
                    summary_json: serde_json::json!({}),
                    created_at: 1,
                    updated_at: 2,
                })
                .expect("session should seed");
        }
        graph_store
            .raw_conn()
            .lock()
            .expect("sqlite lock")
            .execute(
                "INSERT INTO agent_identities (agent_id, persona_name, bundle_json) VALUES (?1, ?2, ?3)",
                rusqlite::params!["agent-jane-01", "Jane", "{}"],
            )
            .expect("agent identity should seed");

        graph
            .sync_apartment(
                "agent-jane-01",
                "short",
                &serde_json::json!({
                    "agent_id": "agent-jane-01",
                    "active_sessions": [
                        {"session_id": "sess-2", "updated_at": 200, "has_active_turn": false},
                        {"session_id": "sess-1", "updated_at": 100, "has_active_turn": true}
                    ]
                }),
            )
            .expect("session index should seed");
        graph
            .sync_apartment(
                "agent-jane-01",
                "short_session:sess-1",
                &serde_json::json!({
                    "session_id": "sess-1",
                    "agent_id": "agent-jane-01",
                    "source": "telegram",
                    "active_turn": {
                        "turn_id": "turn-1a",
                        "task_id": Uuid::nil().to_string(),
                        "chat_id": "chat-sess-1",
                        "user_content": "hello from sess-1",
                        "final_reply_to": "local-aiua-01",
                        "final_reply_role": "membrane"
                    },
                    "recent_turns": [{
                        "turn_id": "turn-1z",
                        "user_content": "older sess-1",
                        "assistant_content": "older reply"
                    }]
                }),
            )
            .expect("session checkpoint should seed");
        graph
            .sync_apartment(
                "agent-jane-01",
                "short_session:sess-2",
                &serde_json::json!({
                    "session_id": "sess-2",
                    "agent_id": "agent-jane-01",
                    "source": "telegram",
                    "active_turn": null,
                    "recent_turns": [{
                        "turn_id": "turn-2z",
                        "user_content": "latest sess-2",
                        "assistant_content": "reply 2"
                    }]
                }),
            )
            .expect("other session checkpoint should seed");

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");

        let response = agent
            .send_request(IpcRequest::GetConfig {
                key: "__session_snapshot__:sess-1".into(),
            })
            .await
            .expect("snapshot request should succeed");

        match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => {
                let snapshot: serde_json::Value =
                    serde_json::from_str(&value_json).expect("snapshot should decode");
                assert_eq!(snapshot["session_id"], "sess-1");
                assert_eq!(snapshot["active_turn"]["turn_id"], "turn-1a");
                assert_eq!(snapshot["recent_turns"][0]["user_content"], "older sess-1");
                assert_eq!(
                    snapshot["session_index"]["active_sessions"]
                        .as_array()
                        .unwrap()
                        .len(),
                    2
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn update_task_with_approval_metadata_writes_explicit_approval_events() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");

        agent
            .send_request(IpcRequest::UpdateTask {
                task_id: Uuid::new_v4(),
                state: "approval_preapproved".into(),
                payload: serde_json::json!({
                    "session_id": "sess-approval-events",
                    "turn_id": "turn-approval-1",
                    "chat_id": "123",
                    "approval_request": {
                        "approval_id": "appr-1",
                        "reason": "deploy the thing",
                        "approved_response": "Approved: deploy the thing"
                    },
                    "approval_resolution": {
                        "approval_id": "appr-1",
                        "decision": "approved",
                        "reason": "deploy the thing",
                        "resolution_mode": "preapproved"
                    }
                }),
            })
            .await
            .expect("update task should succeed");

        let events = graph
            .list_session_events("sess-approval-events", 20)
            .expect("event listing should work");
        assert!(
            events
                .iter()
                .any(|event| event.kind == "approval_requested")
        );
        assert!(events.iter().any(|event| event.kind == "approval_resolved"));
        assert!(events.iter().any(|event| {
            event.kind == "approval_resolved"
                && event.payload_json["resolution_mode"] == "preapproved"
        }));

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn update_task_with_approval_policy_updates_session_summary_and_event_log() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");

        agent
            .send_request(IpcRequest::UpdateTask {
                task_id: Uuid::new_v4(),
                state: "session_policy_updated".into(),
                payload: serde_json::json!({
                    "session_id": "sess-policy-events",
                    "turn_id": "turn-policy-1",
                    "chat_id": "123",
                    "approval_policy": {
                        "auto_approve_all": true,
                        "preapproved_tools": [],
                        "preapproved_classes": []
                    },
                    "action": "approval_policy_update"
                }),
            })
            .await
            .expect("update task should succeed");

        let session = graph
            .get_session("sess-policy-events")
            .expect("session lookup should work")
            .expect("session should exist");
        assert_eq!(
            session.summary_json["approval_policy"]["auto_approve_all"],
            true
        );

        let events = graph
            .list_session_events("sess-policy-events", 20)
            .expect("event listing should work");
        assert!(
            events
                .iter()
                .any(|event| event.kind == "approval_policy_changed")
        );

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn update_task_with_session_status_and_bindings_updates_session_summary_and_event_log() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");

        agent
            .send_request(IpcRequest::UpdateTask {
                task_id: Uuid::new_v4(),
                state: "session_status_updated".into(),
                payload: serde_json::json!({
                    "session_id": "sess-lifecycle",
                    "turn_id": "turn-lifecycle-1",
                    "chat_id": "123",
                    "session_status": "paused",
                    "bindings": {
                        "effective_toolset": ["echo"],
                        "effective_skillset": ["planning"],
                        "effective_workspace_ref": "workspace://main",
                        "effective_model_controller": "gemini-flash"
                    },
                    "action": "session_status_update"
                }),
            })
            .await
            .expect("update task should succeed");

        let session = graph
            .get_session("sess-lifecycle")
            .expect("session lookup should work")
            .expect("session should exist");
        assert_eq!(session.status, "paused");
        assert_eq!(
            session.summary_json["bindings"]["effective_toolset"][0],
            "echo"
        );
        assert!(session.summary_json["tool_assembly"]["execution_routes"]["echo"].is_null());

        let events = graph
            .list_session_events("sess-lifecycle", 20)
            .expect("event listing should work");
        assert!(
            events
                .iter()
                .any(|event| event.kind == "session_status_changed")
        );
        assert!(
            events
                .iter()
                .any(|event| event.kind == "session_bindings_updated")
        );
        assert!(
            events
                .iter()
                .any(|event| event.kind == "tool_assembly_updated")
        );

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn e2e_session_round_trip_persists_and_delivers_reply() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, mut dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut membrane = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-local".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("membrane connect");
        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");
        let mut model = PhiloticClient::connect(GuestIdentity {
            guest_id: "model-local".into(),
            role: "model".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("model connect");

        let session_id = "telegram:123:agent-jane-01";
        let turn_id = "telegram-update-1";

        membrane
            .send_request(IpcRequest::EmitTask {
                target_node: "local-aiua-01".into(),
                target_role: "agent".into(),
                target_guest_id: None,
                task_json: serde_json::json!({
                    "source": "telegram",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "123",
                    "content": "hello from telegram",
                    "final_reply_to": "local-aiua-01",
                    "final_reply_role": "membrane"
                })
                .to_string(),
            })
            .await
            .expect("emit user task");

        let inbound_to_agent =
            tokio::time::timeout(tokio::time::Duration::from_secs(1), agent.recv_task())
                .await
                .expect("agent should receive task")
                .expect("agent recv should succeed");

        let task_id = match inbound_to_agent {
            IpcResponse::InboundTask {
                task_id, task_json, ..
            } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("payload should decode");
                assert_eq!(payload["session_id"], session_id);
                assert_eq!(payload["turn_id"], turn_id);
                task_id
            }
            other => panic!("unexpected inbound response to agent: {other:?}"),
        };

        agent
            .send_request(IpcRequest::UpdateTask {
                task_id,
                state: "waiting_model".into(),
                payload: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "123",
                    "content": "hello from telegram"
                }),
            })
            .await
            .expect("update task");
        agent
            .send_request(IpcRequest::EmitTask {
                target_node: "local-aiua-01".into(),
                target_role: "model".into(),
                target_guest_id: None,
                task_json: serde_json::json!({
                    "action": "generate_text",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "prompt": "hello from telegram",
                    "chat_id": "123",
                    "reply_to": "local-aiua-01",
                    "reply_role": "agent",
                    "final_reply_to": "local-aiua-01",
                    "final_reply_role": "membrane"
                })
                .to_string(),
            })
            .await
            .expect("emit model request");

        let inbound_to_model =
            tokio::time::timeout(tokio::time::Duration::from_secs(1), model.recv_task())
                .await
                .expect("model should receive task")
                .expect("model recv should succeed");

        match inbound_to_model {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("payload should decode");
                assert_eq!(payload["reply_role"], "agent");
            }
            other => panic!("unexpected inbound response to model: {other:?}"),
        }

        model
            .send_request(IpcRequest::EmitTask {
                target_node: "local-aiua-01".into(),
                target_role: "agent".into(),
                target_guest_id: None,
                task_json: serde_json::json!({
                    "action": "model_response",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "123",
                    "content": "hi back",
                    "final_reply_to": "local-aiua-01",
                    "final_reply_role": "membrane"
                })
                .to_string(),
            })
            .await
            .expect("emit model response");

        let inbound_model_response =
            tokio::time::timeout(tokio::time::Duration::from_secs(1), agent.recv_task())
                .await
                .expect("agent should receive model response")
                .expect("agent recv should succeed");

        match inbound_model_response {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("payload should decode");
                assert_eq!(payload["action"], "model_response");
                assert_eq!(payload["content"], "hi back");
            }
            other => panic!("unexpected model response to agent: {other:?}"),
        }

        agent
            .send_request(IpcRequest::CompleteTask {
                task_id,
                result: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "123",
                    "content": "hi back"
                }),
            })
            .await
            .expect("complete task");
        agent
            .send_request(IpcRequest::EmitTask {
                target_node: "local-aiua-01".into(),
                target_role: "membrane".into(),
                target_guest_id: None,
                task_json: serde_json::json!({
                    "action": "send_reply",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "123",
                    "content": "hi back"
                })
                .to_string(),
            })
            .await
            .expect("emit final reply");

        let final_reply =
            tokio::time::timeout(tokio::time::Duration::from_secs(1), membrane.recv_task())
                .await
                .expect("membrane should receive final reply")
                .expect("membrane recv should succeed");

        match final_reply {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("payload should decode");
                assert_eq!(payload["action"], "send_reply");
                assert_eq!(payload["content"], "hi back");
            }
            other => panic!("unexpected final response to membrane: {other:?}"),
        }

        let turn = graph
            .get_session_turn(session_id, turn_id)
            .expect("turn lookup should work")
            .expect("turn should exist");
        assert_eq!(turn.status, "completed");
        assert_eq!(
            turn.response_json
                .as_ref()
                .and_then(|json| json.get("content"))
                .and_then(serde_json::Value::as_str),
            Some("hi back")
        );

        let mut ledger_count = 0usize;
        while tokio::time::timeout(tokio::time::Duration::from_millis(10), dispatcher_rx.recv())
            .await
            .ok()
            .flatten()
            .is_some()
        {
            ledger_count += 1;
            if ledger_count > 10 {
                break;
            }
        }
        assert!(
            ledger_count >= 4,
            "expected multiple ledger writes, got {ledger_count}"
        );

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn e2e_structured_tool_call_round_trip_persists_and_delivers_reply() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, mut dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut membrane = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-local".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("membrane connect");
        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-local".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");
        let mut model = PhiloticClient::connect(GuestIdentity {
            guest_id: "model-local".into(),
            role: "model".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("model connect");

        let session_id = "telegram:456:agent-jane-01";
        let turn_id = "telegram-update-tool-1";

        membrane
            .send_request(IpcRequest::EmitTask {
                target_node: "local-aiua-01".into(),
                target_role: "agent".into(),
                target_guest_id: None,
                task_json: serde_json::json!({
                    "source": "telegram",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "456",
                    "content": "use echo hello structured tool",
                    "final_reply_to": "local-aiua-01",
                    "final_reply_role": "membrane"
                })
                .to_string(),
            })
            .await
            .expect("emit user task");

        let inbound_to_agent =
            tokio::time::timeout(tokio::time::Duration::from_secs(1), agent.recv_task())
                .await
                .expect("agent should receive task")
                .expect("agent recv should succeed");

        let task_id = match inbound_to_agent {
            IpcResponse::InboundTask {
                task_id, task_json, ..
            } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("payload should decode");
                assert_eq!(payload["session_id"], session_id);
                assert_eq!(payload["turn_id"], turn_id);
                task_id
            }
            other => panic!("unexpected inbound response to agent: {other:?}"),
        };

        agent
            .send_request(IpcRequest::UpdateTask {
                task_id,
                state: "waiting_model".into(),
                payload: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "456",
                    "content": "use echo hello structured tool"
                }),
            })
            .await
            .expect("update task");
        agent
            .send_request(IpcRequest::EmitTask {
                target_node: "local-aiua-01".into(),
                target_role: "model".into(),
                target_guest_id: None,
                task_json: serde_json::json!({
                    "action": "generate_text",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "prompt": "use echo hello structured tool",
                    "user_content": "use echo hello structured tool",
                    "chat_id": "456",
                    "reply_to": "local-aiua-01",
                    "reply_role": "agent",
                    "final_reply_to": "local-aiua-01",
                    "final_reply_role": "membrane"
                })
                .to_string(),
            })
            .await
            .expect("emit model request");

        let inbound_to_model =
            tokio::time::timeout(tokio::time::Duration::from_secs(1), model.recv_task())
                .await
                .expect("model should receive task")
                .expect("model recv should succeed");

        match inbound_to_model {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("payload should decode");
                assert_eq!(payload["user_content"], "use echo hello structured tool");
            }
            other => panic!("unexpected inbound response to model: {other:?}"),
        }

        model
            .send_request(IpcRequest::EmitTask {
                target_node: "local-aiua-01".into(),
                target_role: "agent".into(),
                target_guest_id: None,
                task_json: serde_json::json!({
                    "action": "model_response",
                    "agent_action": {
                        "kind": "tool_call",
                        "tool_name": "echo",
                        "arguments": {
                            "text": "hello structured tool"
                        }
                    },
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "456",
                    "content": "tool_call: echo hello structured tool",
                    "final_reply_to": "local-aiua-01",
                    "final_reply_role": "membrane"
                })
                .to_string(),
            })
            .await
            .expect("emit model tool call response");

        let inbound_tool_response =
            tokio::time::timeout(tokio::time::Duration::from_secs(1), agent.recv_task())
                .await
                .expect("agent should receive model response")
                .expect("agent recv should succeed");

        match inbound_tool_response {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("payload should decode");
                assert_eq!(payload["agent_action"]["kind"], "tool_call");
                assert_eq!(payload["agent_action"]["tool_name"], "echo");
            }
            other => panic!("unexpected model response to agent: {other:?}"),
        }

        agent
            .send_request(IpcRequest::CompleteTask {
                task_id,
                result: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "456",
                    "content": "Tool echo says: hello structured tool"
                }),
            })
            .await
            .expect("complete task");
        agent
            .send_request(IpcRequest::EmitTask {
                target_node: "local-aiua-01".into(),
                target_role: "membrane".into(),
                target_guest_id: None,
                task_json: serde_json::json!({
                    "action": "send_reply",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": "456",
                    "content": "Tool echo says: hello structured tool"
                })
                .to_string(),
            })
            .await
            .expect("emit final reply");

        let final_reply =
            tokio::time::timeout(tokio::time::Duration::from_secs(1), membrane.recv_task())
                .await
                .expect("membrane should receive final reply")
                .expect("membrane recv should succeed");

        match final_reply {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("payload should decode");
                assert_eq!(payload["action"], "send_reply");
                assert_eq!(payload["content"], "Tool echo says: hello structured tool");
            }
            other => panic!("unexpected final response to membrane: {other:?}"),
        }

        let turn = graph
            .get_session_turn(session_id, turn_id)
            .expect("turn lookup should work")
            .expect("turn should exist");
        assert_eq!(turn.status, "completed");
        assert_eq!(
            turn.response_json
                .as_ref()
                .and_then(|json| json.get("content"))
                .and_then(serde_json::Value::as_str),
            Some("Tool echo says: hello structured tool")
        );

        let events = graph
            .list_session_events(session_id, 20)
            .expect("event listing should work");
        assert!(
            events
                .iter()
                .any(|event| event.payload_json.get("agent_action").is_some()),
            "expected structured agent action to be captured in session events"
        );

        let mut ledger_count = 0usize;
        while tokio::time::timeout(tokio::time::Duration::from_millis(10), dispatcher_rx.recv())
            .await
            .ok()
            .flatten()
            .is_some()
        {
            ledger_count += 1;
            if ledger_count > 10 {
                break;
            }
        }
        assert!(
            ledger_count >= 4,
            "expected multiple ledger writes, got {ledger_count}"
        );

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn telegram_poll_lease_is_single_owner_and_released_on_disconnect() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
            })
            .expect("seed local hotel");
        graph
            .upsert_agent_identity(&AgentIdentityRecord {
                agent_id: "agent-jane-01".into(),
                persona_name: "Jane".into(),
                authority_hotel: "local-hotel".into(),
                bundle_json: serde_json::json!({}),
            })
            .expect("seed local agent identity");
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let lease_key = "telegram:telegram_bot_token:deadbeefcafebabe";

        let mut primary = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-telegram-01".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("primary connect");

        let mut secondary = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-telegram-02".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("secondary connect");

        let granted = primary
            .send_request(IpcRequest::AcquireTelegramPollLease {
                lease_key: lease_key.into(),
                agent_id: "agent-jane-01".into(),
            })
            .await
            .expect("primary lease request");
        let (granted, lease) = expect_telegram_poll_lease(granted);
        let lease = lease.expect("lease envelope");
        assert!(granted);
        assert_eq!(lease.metadata["agent_id"], "agent-jane-01");
        assert_eq!(lease.lease_epoch, 1);
        assert_eq!(lease.owner_guest_id, "membrane-telegram-01");

        let denied = secondary
            .send_request(IpcRequest::AcquireTelegramPollLease {
                lease_key: lease_key.into(),
                agent_id: "agent-jane-01".into(),
            })
            .await
            .expect("secondary lease request");
        let (granted, lease) = expect_telegram_poll_lease(denied);
        let lease = lease.expect("lease envelope");
        assert!(!granted);
        assert_eq!(lease.metadata["agent_id"], "agent-jane-01");
        assert_eq!(lease.lease_epoch, 1);
        assert_eq!(lease.owner_guest_id, "membrane-telegram-01");

        drop(primary);
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let after_disconnect = secondary
            .send_request(IpcRequest::AcquireTelegramPollLease {
                lease_key: lease_key.into(),
                agent_id: "agent-jane-01".into(),
            })
            .await
            .expect("secondary re-acquire after disconnect");
        let (granted, lease) = expect_telegram_poll_lease(after_disconnect);
        let lease = lease.expect("lease envelope");
        assert!(granted);
        assert_eq!(lease.metadata["agent_id"], "agent-jane-01");
        assert_eq!(lease.lease_epoch, 2);
        assert_eq!(lease.owner_guest_id, "membrane-telegram-02");

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn telegram_poll_lease_denies_foreign_authority_hotel() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
            })
            .expect("seed local hotel");
        graph
            .upsert_agent_identity(&AgentIdentityRecord {
                agent_id: "agent-aria-01".into(),
                persona_name: "Aria".into(),
                authority_hotel: "remote-hotel".into(),
                bundle_json: serde_json::json!({}),
            })
            .expect("seed remote-owned agent identity");
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut poller = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-telegram-01".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("poller connect");

        let response = poller
            .send_request(IpcRequest::AcquireTelegramPollLease {
                lease_key: "telegram:telegram_bot_token:deadbeefcafebabe".into(),
                agent_id: "agent-aria-01".into(),
            })
            .await
            .expect("foreign authority request");

        match response {
            IpcResponse::Standard {
                ok, code, message, ..
            } => {
                assert!(!ok);
                assert_eq!(code, "LEASE_FOREIGN_AUTHORITY");
                assert!(message.contains("remote-hotel"));
                assert!(message.contains("local-hotel"));
            }
            other => panic!("unexpected response: {other:?}"),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn telegram_poll_lease_allows_delegated_foreign_hotel() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
            })
            .expect("seed local hotel");
        graph
            .upsert_agent_identity(&AgentIdentityRecord {
                agent_id: "agent-aria-01".into(),
                persona_name: "Aria".into(),
                authority_hotel: "remote-hotel".into(),
                bundle_json: serde_json::json!({
                    "telegram_poll_delegate_hotels": ["local-hotel"]
                }),
            })
            .expect("seed delegated remote-owned agent identity");
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut poller = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-telegram-01".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("poller connect");

        let response = poller
            .send_request(IpcRequest::AcquireTelegramPollLease {
                lease_key: "telegram:telegram_bot_token:deadbeefcafebabe".into(),
                agent_id: "agent-aria-01".into(),
            })
            .await
            .expect("delegated foreign authority request");

        let (granted, lease) = expect_telegram_poll_lease(response);
        let lease = lease.expect("lease envelope");
        assert!(granted);
        assert_eq!(lease.lease_epoch, 1);
        assert_eq!(lease.owner_guest_id, "membrane-telegram-01");

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn telegram_poll_lease_can_be_renewed_by_owner() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
            })
            .expect("seed local hotel");
        graph
            .upsert_agent_identity(&AgentIdentityRecord {
                agent_id: "agent-jane-01".into(),
                persona_name: "Jane".into(),
                authority_hotel: "local-hotel".into(),
                bundle_json: serde_json::json!({}),
            })
            .expect("seed local agent identity");
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut poller = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-telegram-01".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("poller connect");

        let acquired = poller
            .send_request(IpcRequest::AcquireTelegramPollLease {
                lease_key: "telegram:telegram_bot_token:deadbeefcafebabe".into(),
                agent_id: "agent-jane-01".into(),
            })
            .await
            .expect("acquire lease");
        let (granted, lease) = expect_telegram_poll_lease(acquired);
        let lease = lease.expect("lease envelope");
        assert!(granted);
        let epoch = lease.lease_epoch;

        let renewed = poller
            .send_request(IpcRequest::RenewTelegramPollLease {
                lease_key: "telegram:telegram_bot_token:deadbeefcafebabe".into(),
                agent_id: "agent-jane-01".into(),
                lease_epoch: epoch,
            })
            .await
            .expect("renew lease");
        let (granted, lease) = expect_telegram_poll_lease(renewed);
        let lease = lease.expect("lease envelope");
        assert!(granted);
        assert_eq!(lease.lease_epoch, epoch);
        assert_eq!(lease.owner_guest_id, "membrane-telegram-01");

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn telegram_poll_lease_expires_and_allows_takeover() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
            })
            .expect("seed local hotel");
        graph
            .upsert_agent_identity(&AgentIdentityRecord {
                agent_id: "agent-jane-01".into(),
                persona_name: "Jane".into(),
                authority_hotel: "local-hotel".into(),
                bundle_json: serde_json::json!({}),
            })
            .expect("seed local agent identity");
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut primary = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-telegram-01".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("primary connect");
        let mut secondary = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-telegram-02".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("secondary connect");

        let acquired = primary
            .send_request(IpcRequest::AcquireTelegramPollLease {
                lease_key: "telegram:telegram_bot_token:deadbeefcafebabe".into(),
                agent_id: "agent-jane-01".into(),
            })
            .await
            .expect("acquire lease");
        let (granted, lease) = expect_telegram_poll_lease(acquired);
        assert!(granted);
        assert_eq!(lease.expect("lease envelope").lease_epoch, 1);

        tokio::time::sleep(tokio::time::Duration::from_millis(1100)).await;

        let takeover = secondary
            .send_request(IpcRequest::AcquireTelegramPollLease {
                lease_key: "telegram:telegram_bot_token:deadbeefcafebabe".into(),
                agent_id: "agent-jane-01".into(),
            })
            .await
            .expect("takeover acquire");
        let (granted, lease) = expect_telegram_poll_lease(takeover);
        let lease = lease.expect("lease envelope");
        assert!(granted);
        assert_eq!(lease.lease_epoch, 2);
        assert_eq!(lease.owner_guest_id, "membrane-telegram-02");

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn telegram_poll_lease_release_allows_immediate_takeover() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
            })
            .expect("seed local hotel");
        graph
            .upsert_agent_identity(&AgentIdentityRecord {
                agent_id: "agent-jane-01".into(),
                persona_name: "Jane".into(),
                authority_hotel: "local-hotel".into(),
                bundle_json: serde_json::json!({}),
            })
            .expect("seed local agent identity");
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut primary = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-telegram-01".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("primary connect");
        let mut secondary = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-telegram-02".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("secondary connect");

        primary
            .send_request(IpcRequest::AcquireTelegramPollLease {
                lease_key: "telegram:telegram_bot_token:deadbeefcafebabe".into(),
                agent_id: "agent-jane-01".into(),
            })
            .await
            .expect("acquire lease");

        let release = primary
            .send_request(IpcRequest::ReleaseTelegramPollLease {
                lease_key: "telegram:telegram_bot_token:deadbeefcafebabe".into(),
            })
            .await
            .expect("release lease");
        match release {
            IpcResponse::Standard { ok, .. } => assert!(ok),
            other => panic!("unexpected release response: {other:?}"),
        }

        let takeover = secondary
            .send_request(IpcRequest::AcquireTelegramPollLease {
                lease_key: "telegram:telegram_bot_token:deadbeefcafebabe".into(),
                agent_id: "agent-jane-01".into(),
            })
            .await
            .expect("takeover acquire");
        let (granted, lease) = expect_telegram_poll_lease(takeover);
        let lease = lease.expect("lease envelope");
        assert!(granted);
        assert_eq!(lease.lease_epoch, 2);
        assert_eq!(lease.owner_guest_id, "membrane-telegram-02");

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn telegram_poll_lease_owner_status_drops_dead_guest_owner() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
            })
            .expect("seed local hotel");
        graph
            .upsert_agent_identity(&AgentIdentityRecord {
                agent_id: "agent-jane-01".into(),
                persona_name: "Jane".into(),
                authority_hotel: "local-hotel".into(),
                bundle_json: serde_json::json!({}),
            })
            .expect("seed local agent identity");
        graph
            .seed_guests(
                "local-hotel",
                &[GuestRecord {
                    hotel_name: "local-hotel".into(),
                    guest_id: "membrane-telegram-01".into(),
                    role: "membrane".into(),
                    config_json: serde_json::json!({ "command": "membrane" }).to_string(),
                    is_active: true,
                    active_pid: Some(std::process::id().to_string()),
                    last_active_at: None,
                }],
            )
            .expect("seed membrane guest");
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut poller = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-telegram-01".into(),
            role: "membrane".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("poller connect");

        let acquired = poller
            .send_request(IpcRequest::AcquireTelegramPollLease {
                lease_key: "telegram:telegram_bot_token:deadbeefcafebabe".into(),
                agent_id: "agent-jane-01".into(),
            })
            .await
            .expect("acquire lease");
        let (granted, _lease) = expect_telegram_poll_lease(acquired);
        assert!(granted);

        graph
            .set_guest_pid("local-hotel", "membrane-telegram-01", None)
            .expect("clear membrane guest pid");

        let status = poller
            .send_request(IpcRequest::GetTelegramPollLeaseOwner {
                lease_key: "telegram:telegram_bot_token:deadbeefcafebabe".into(),
            })
            .await
            .expect("query lease owner");
        let (active, lease) = expect_telegram_poll_status(status);
        assert!(!active);
        assert!(lease.is_none());

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn desktop_membrane_lease_disconnect_allows_takeover() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
            })
            .expect("seed local hotel");
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let lease_key = "desktop:local-hotel:operator-surface";

        let mut primary = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-desktop-01".into(),
            role: "management".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("primary connect");

        let mut secondary = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-desktop-02".into(),
            role: "management".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("secondary connect");

        let granted = primary
            .send_request(IpcRequest::AcquireDesktopMembraneLease {
                lease_key: lease_key.into(),
                port: 7700,
            })
            .await
            .expect("primary lease request");
        let (granted, lease) = expect_desktop_membrane_lease(granted);
        let lease = lease.expect("lease envelope");
        assert!(granted);
        assert_eq!(lease.metadata["port"], 7700);
        assert_eq!(lease.lease_epoch, 1);
        assert_eq!(lease.owner_guest_id, "membrane-desktop-01");

        let denied = secondary
            .send_request(IpcRequest::AcquireDesktopMembraneLease {
                lease_key: lease_key.into(),
                port: 7701,
            })
            .await
            .expect("secondary lease request");
        let (granted, lease) = expect_desktop_membrane_lease(denied);
        let lease = lease.expect("lease envelope");
        assert!(!granted);
        assert_eq!(lease.metadata["port"], 7700);
        assert_eq!(lease.lease_epoch, 1);
        assert_eq!(lease.owner_guest_id, "membrane-desktop-01");

        drop(primary);
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let after_disconnect = secondary
            .send_request(IpcRequest::AcquireDesktopMembraneLease {
                lease_key: lease_key.into(),
                port: 7701,
            })
            .await
            .expect("secondary re-acquire after disconnect");
        let (granted, lease) = expect_desktop_membrane_lease(after_disconnect);
        let lease = lease.expect("lease envelope");
        assert!(granted);
        assert_eq!(lease.metadata["port"], 7701);
        assert_eq!(lease.lease_epoch, 2);
        assert_eq!(lease.owner_guest_id, "membrane-desktop-02");

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn desktop_membrane_lease_can_be_renewed_by_owner() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
            })
            .expect("seed local hotel");
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut client = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-desktop-01".into(),
            role: "management".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("desktop connect");

        let acquired = client
            .send_request(IpcRequest::AcquireDesktopMembraneLease {
                lease_key: "desktop:local-hotel:operator-surface".into(),
                port: 7700,
            })
            .await
            .expect("acquire lease");
        let (granted, lease) = expect_desktop_membrane_lease(acquired);
        let lease = lease.expect("lease envelope");
        assert!(granted);
        let epoch = lease.lease_epoch;

        let renewed = client
            .send_request(IpcRequest::RenewDesktopMembraneLease {
                lease_key: "desktop:local-hotel:operator-surface".into(),
                lease_epoch: epoch,
            })
            .await
            .expect("renew lease");
        let (granted, lease) = expect_desktop_membrane_lease(renewed);
        let lease = lease.expect("lease envelope");
        assert!(granted);
        assert_eq!(lease.lease_epoch, epoch);
        assert_eq!(lease.owner_guest_id, "membrane-desktop-01");

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn desktop_membrane_lease_release_allows_immediate_takeover() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
            })
            .expect("seed local hotel");
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut primary = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-desktop-01".into(),
            role: "management".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("primary connect");
        let mut secondary = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-desktop-02".into(),
            role: "management".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("secondary connect");

        primary
            .send_request(IpcRequest::AcquireDesktopMembraneLease {
                lease_key: "desktop:local-hotel:operator-surface".into(),
                port: 7700,
            })
            .await
            .expect("acquire lease");

        let release = primary
            .send_request(IpcRequest::ReleaseDesktopMembraneLease {
                lease_key: "desktop:local-hotel:operator-surface".into(),
            })
            .await
            .expect("release lease");
        match release {
            IpcResponse::Standard { ok, .. } => assert!(ok),
            other => panic!("unexpected release response: {other:?}"),
        }

        let takeover = secondary
            .send_request(IpcRequest::AcquireDesktopMembraneLease {
                lease_key: "desktop:local-hotel:operator-surface".into(),
                port: 7701,
            })
            .await
            .expect("takeover acquire");
        let (granted, lease) = expect_desktop_membrane_lease(takeover);
        let lease = lease.expect("lease envelope");
        assert!(granted);
        assert_eq!(lease.lease_epoch, 2);
        assert_eq!(lease.metadata["port"], 7701);
        assert_eq!(lease.owner_guest_id, "membrane-desktop-02");

        let status = secondary
            .send_request(IpcRequest::GetDesktopMembraneLeaseOwner {
                lease_key: "desktop:local-hotel:operator-surface".into(),
            })
            .await
            .expect("query lease owner");
        let (active, lease) = expect_desktop_membrane_status(status);
        assert!(active);
        assert_eq!(
            lease.expect("lease envelope").owner_guest_id,
            "membrane-desktop-02"
        );

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn desktop_membrane_status_view_comes_from_hotel_record() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: Some(std::process::id().to_string()),
            })
            .expect("seed local hotel");
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut membrane = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-local".into(),
            role: "management".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("membrane connect");

        let response = membrane
            .send_request(IpcRequest::GetDesktopMembraneStatus)
            .await
            .expect("desktop membrane status request");
        let status = expect_desktop_membrane_view_status(response);
        assert_eq!(status.hotel, "local-hotel");
        assert_eq!(status.daemon, "running");

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn desktop_membrane_guest_views_come_from_graph_storage() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: Some(std::process::id().to_string()),
            })
            .expect("seed local hotel");
        graph
            .seed_guests(
                "local-hotel",
                &[
                    GuestRecord {
                        hotel_name: "local-hotel".into(),
                        guest_id: "local-hotel:membrane-gateway".into(),
                        role: "membrane".into(),
                        config_json: "{}".into(),
                        is_active: true,
                        active_pid: Some(std::process::id().to_string()),
                        last_active_at: Some(50),
                    },
                    GuestRecord {
                        hotel_name: "local-hotel".into(),
                        guest_id: "local-hotel:model-router-gemini".into(),
                        role: "model.gemini".into(),
                        config_json: "{}".into(),
                        is_active: true,
                        active_pid: None,
                        last_active_at: Some(25),
                    },
                ],
            )
            .expect("seed guests");
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut membrane = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-local".into(),
            role: "management".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("membrane connect");

        let response = membrane
            .send_request(IpcRequest::ListDesktopMembraneGuests)
            .await
            .expect("desktop membrane guests request");
        let guests = expect_desktop_membrane_guest_views(response);
        assert_eq!(guests.len(), 2);
        assert_eq!(guests[0].guest_id, "local-hotel:membrane-gateway");
        assert_eq!(guests[0].name, "Membrane");
        assert_eq!(guests[0].status, "running");
        assert_eq!(guests[1].guest_id, "local-hotel:model-router-gemini");
        assert_eq!(guests[1].name, "Gemini");
        assert_eq!(guests[1].status, "stopped");

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn desktop_membrane_agent_views_are_redacted_and_local_hotel_scoped() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: Some(std::process::id().to_string()),
            })
            .expect("seed local hotel");
        graph
            .upsert_agent_identity(&AgentIdentityRecord {
                agent_id: "agent-jane-01".into(),
                persona_name: "Jane".into(),
                authority_hotel: "local-hotel".into(),
                bundle_json: serde_json::json!({
                    "system_prompt": "top secret",
                    "toolset_tags": ["orchestrator", "desktop"]
                }),
            })
            .expect("seed local agent");
        graph
            .upsert_agent_identity(&AgentIdentityRecord {
                agent_id: "agent-remote-01".into(),
                persona_name: "Remote".into(),
                authority_hotel: "remote-hotel".into(),
                bundle_json: serde_json::json!({
                    "system_prompt": "should not leak",
                    "toolset_tags": ["remote"]
                }),
            })
            .expect("seed remote agent");
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut membrane = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-local".into(),
            role: "management".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("membrane connect");

        let response = membrane
            .send_request(IpcRequest::ListDesktopMembraneAgents)
            .await
            .expect("desktop membrane agents request");
        let agents = expect_desktop_membrane_agent_views(response);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].agent_id, "agent-jane-01");
        assert_eq!(agents[0].persona_name, "Jane");
        assert_eq!(agents[0].authority_hotel, "local-hotel");
        assert_eq!(
            agents[0].toolset_tags,
            vec!["orchestrator".to_string(), "desktop".to_string()]
        );

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn desktop_membrane_target_views_include_source_and_freshness_attribution() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: Some(std::process::id().to_string()),
            })
            .expect("seed local hotel");
        let registry = Arc::new(RwLock::new(NodeRegistry::new()));
        registry.write().await.update_node(
            NodeCapabilities {
                node_id: "local-aiua-01".into(),
                roles: vec![ansible_mesh_core::NodeRole::PersonalDevice],
                models: vec![],
                tools: vec!["tool.local.status@1".into()],
                constraints: Default::default(),
            },
            vec![CapabilityAdvertisement {
                hotel_id: "local-hotel".into(),
                node_id: "local-aiua-01".into(),
                incarnation_id: "local-hotel:membrane".into(),
                target_role: "management".into(),
                availability_state: "live".into(),
                selection_hint: Some("local".into()),
                latency_hint_ms: Some(2),
                max_concurrent_jobs: Some(8),
                active_jobs: 0,
                queue_depth: 0,
            }],
            Some(ExecutionReachability {
                protocol: "unix".into(),
                host: "127.0.0.1".into(),
                port: 0,
            }),
        );
        registry.write().await.update_node(
            NodeCapabilities {
                node_id: "remote-aiua-01".into(),
                roles: vec![ansible_mesh_core::NodeRole::AnsibleNode],
                models: vec!["model.gemini-2.5-pro@2026.1".into()],
                tools: vec!["tool.remote.restart@1".into()],
                constraints: Default::default(),
            },
            vec![CapabilityAdvertisement {
                hotel_id: "remote-hotel".into(),
                node_id: "remote-aiua-01".into(),
                incarnation_id: "remote-hotel:model-router".into(),
                target_role: "model".into(),
                availability_state: "live".into(),
                selection_hint: Some("remote_fallback".into()),
                latency_hint_ms: Some(12),
                max_concurrent_jobs: Some(4),
                active_jobs: 1,
                queue_depth: 0,
            }],
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "remote.mesh".into(),
                port: 9002,
            }),
        );
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph)
            .with_registry(registry);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut membrane = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-local".into(),
            role: "management".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("membrane connect");

        let response = membrane
            .send_request(IpcRequest::ListDesktopMembraneTargets)
            .await
            .expect("desktop membrane targets request");
        let targets = expect_desktop_membrane_target_views(response);
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].target_node_id, "local-aiua-01");
        assert_eq!(targets[0].target_hotel, "local-hotel");
        assert_eq!(targets[0].source_hotel, "local-hotel");
        assert!(targets[0].is_local);
        assert_eq!(targets[0].roles, vec!["personal-device".to_string()]);
        assert_eq!(targets[0].advertised_roles, vec!["management".to_string()]);
        assert_eq!(targets[0].freshness_state, "heartbeat-fresh");
        assert!(targets[0].freshness_age_secs <= targets[0].freshness_ttl_secs);

        assert_eq!(targets[1].target_node_id, "remote-aiua-01");
        assert_eq!(targets[1].target_hotel, "remote-hotel");
        assert_eq!(targets[1].source_hotel, "local-hotel");
        assert!(!targets[1].is_local);
        assert_eq!(targets[1].roles, vec!["ansible-node".to_string()]);
        assert_eq!(
            targets[1]
                .reachability
                .as_ref()
                .expect("remote reachability")
                .host,
            "remote.mesh"
        );

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn desktop_membrane_target_status_distinguishes_local_from_remote_observation() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: Some(std::process::id().to_string()),
            })
            .expect("seed local hotel");
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "remote-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "remote-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9100,
                blob_port: 9101,
                execution_port: 9102,
                ipc_socket_path: "/tmp/remote-aiua.sock".into(),
                active_pid: None,
            })
            .expect("seed remote hotel");
        let registry = Arc::new(RwLock::new(NodeRegistry::new()));
        registry.write().await.update_node(
            NodeCapabilities {
                node_id: "remote-aiua-01".into(),
                roles: vec![ansible_mesh_core::NodeRole::AnsibleNode],
                models: vec![],
                tools: vec![],
                constraints: Default::default(),
            },
            vec![CapabilityAdvertisement {
                hotel_id: "remote-hotel".into(),
                node_id: "remote-aiua-01".into(),
                incarnation_id: "remote-hotel:model-router".into(),
                target_role: "model".into(),
                availability_state: "live".into(),
                selection_hint: Some("remote_fallback".into()),
                latency_hint_ms: Some(12),
                max_concurrent_jobs: Some(4),
                active_jobs: 1,
                queue_depth: 0,
            }],
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "remote.mesh".into(),
                port: 9102,
            }),
        );
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph)
            .with_registry(registry);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut membrane = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-local".into(),
            role: "management".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("membrane connect");

        let local_response = membrane
            .send_request(IpcRequest::GetDesktopMembraneTargetStatus {
                target_node_id: "local-aiua-01".into(),
            })
            .await
            .expect("local target status request");
        let local_status = expect_desktop_membrane_target_status(local_response);
        assert_eq!(local_status.observation_kind, "local-canonical");
        assert_eq!(local_status.daemon_status, "running");
        assert_eq!(local_status.target_hotel, "local-hotel");

        let remote_response = membrane
            .send_request(IpcRequest::GetDesktopMembraneTargetStatus {
                target_node_id: "remote-aiua-01".into(),
            })
            .await
            .expect("remote target status request");
        let remote_status = expect_desktop_membrane_target_status(remote_response);
        assert_eq!(remote_status.observation_kind, "remote-heartbeat-observed");
        assert_eq!(remote_status.daemon_status, "observed-reachable");
        assert_eq!(remote_status.target_hotel, "remote-hotel");
        assert_eq!(remote_status.source_hotel, "local-hotel");
        assert_eq!(
            remote_status
                .reachability
                .as_ref()
                .expect("remote reachability")
                .host,
            "remote.mesh"
        );

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn desktop_membrane_target_guest_inventory_reports_failed_remote_query_when_unreachable()
    {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: Some(std::process::id().to_string()),
            })
            .expect("seed local hotel");
        graph
            .seed_guests(
                "local-hotel",
                &[GuestRecord {
                    hotel_name: "local-hotel".into(),
                    guest_id: "local-hotel:membrane-gateway".into(),
                    role: "membrane".into(),
                    config_json: "{}".into(),
                    is_active: true,
                    active_pid: Some(std::process::id().to_string()),
                    last_active_at: Some(50),
                }],
            )
            .expect("seed local guests");
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "remote-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "remote-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9100,
                blob_port: 9101,
                execution_port: 9102,
                ipc_socket_path: "/tmp/remote-aiua.sock".into(),
                active_pid: None,
            })
            .expect("seed remote hotel");
        let registry = Arc::new(RwLock::new(NodeRegistry::new()));
        registry.write().await.update_node(
            NodeCapabilities {
                node_id: "remote-aiua-01".into(),
                roles: vec![ansible_mesh_core::NodeRole::AnsibleNode],
                models: vec![],
                tools: vec![],
                constraints: Default::default(),
            },
            vec![CapabilityAdvertisement {
                hotel_id: "remote-hotel".into(),
                node_id: "remote-aiua-01".into(),
                incarnation_id: "remote-hotel:model-router".into(),
                target_role: "model".into(),
                availability_state: "live".into(),
                selection_hint: Some("remote_fallback".into()),
                latency_hint_ms: Some(12),
                max_concurrent_jobs: Some(4),
                active_jobs: 1,
                queue_depth: 0,
            }],
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "remote.mesh".into(),
                port: 9102,
            }),
        );
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph)
            .with_registry(registry);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut membrane = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-local".into(),
            role: "management".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("membrane connect");

        let local_response = membrane
            .send_request(IpcRequest::ListDesktopMembraneTargetGuests {
                target_node_id: "local-aiua-01".into(),
            })
            .await
            .expect("local target guests request");
        let local_inventory = expect_desktop_membrane_target_guest_inventory(local_response);
        assert!(local_inventory.available);
        assert_eq!(local_inventory.observation_kind, "local-canonical");
        assert_eq!(local_inventory.guests.len(), 1);

        let remote_response = membrane
            .send_request(IpcRequest::ListDesktopMembraneTargetGuests {
                target_node_id: "remote-aiua-01".into(),
            })
            .await
            .expect("remote target guests request");
        let remote_inventory = expect_desktop_membrane_target_guest_inventory(remote_response);
        assert!(!remote_inventory.available);
        assert_eq!(remote_inventory.observation_kind, "remote-query-failed");
        assert_eq!(remote_inventory.pending_remote_query_state, "error");
        assert!(remote_inventory.guests.is_empty());
        assert_eq!(remote_inventory.target_hotel, "remote-hotel");

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn operator_target_surface_requests_reuse_membrane_target_logic() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: Some(std::process::id().to_string()),
            })
            .expect("seed local hotel");
        graph
            .seed_guests(
                "local-hotel",
                &[GuestRecord {
                    hotel_name: "local-hotel".into(),
                    guest_id: "local-hotel:membrane-gateway".into(),
                    role: "membrane".into(),
                    config_json: "{}".into(),
                    is_active: true,
                    active_pid: Some(std::process::id().to_string()),
                    last_active_at: Some(50),
                }],
            )
            .expect("seed local guests");
        graph
            .upsert_agent_identity(&AgentIdentityRecord {
                agent_id: "agent-jane-01".into(),
                persona_name: "Jane".into(),
                authority_hotel: "local-hotel".into(),
                bundle_json: serde_json::json!({
                    "toolset_tags": ["shell", "memory"]
                }),
            })
            .expect("seed local agent identity");
        let registry = Arc::new(RwLock::new(NodeRegistry::new()));
        registry.write().await.update_node(
            NodeCapabilities {
                node_id: "local-aiua-01".into(),
                roles: vec![ansible_mesh_core::NodeRole::PersonalDevice],
                models: vec![],
                tools: vec![],
                constraints: Default::default(),
            },
            vec![CapabilityAdvertisement {
                hotel_id: "local-hotel".into(),
                node_id: "local-aiua-01".into(),
                incarnation_id: "local-hotel:membrane".into(),
                target_role: "management".into(),
                availability_state: "live".into(),
                selection_hint: Some("local".into()),
                latency_hint_ms: Some(1),
                max_concurrent_jobs: Some(4),
                active_jobs: 0,
                queue_depth: 0,
            }],
            Some(ExecutionReachability {
                protocol: "unix".into(),
                host: "127.0.0.1".into(),
                port: 0,
            }),
        );
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph)
            .with_registry(registry);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut client = PhiloticClient::connect(GuestIdentity {
            guest_id: "operator-surface-test".into(),
            role: "management".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("management connect");

        let targets = expect_operator_target_views(
            client
                .send_request(IpcRequest::QueryOperatorTargets)
                .await
                .expect("operator targets request"),
        );
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].target_node_id, "local-aiua-01");

        let status = expect_operator_target_status(
            client
                .send_request(IpcRequest::QueryOperatorTargetStatus {
                    target_node_id: "local-aiua-01".into(),
                })
                .await
                .expect("operator target status request"),
        );
        assert_eq!(status.observation_kind, "local-canonical");
        assert_eq!(status.target_hotel, "local-hotel");

        let guests = expect_operator_target_guests(
            client
                .send_request(IpcRequest::QueryOperatorTargetGuests {
                    target_node_id: "local-aiua-01".into(),
                })
                .await
                .expect("operator target guests request"),
        );
        assert!(guests.available);
        assert_eq!(guests.guests.len(), 1);
        assert_eq!(guests.guests[0].guest_id, "local-hotel:membrane-gateway");

        let agents = expect_operator_target_agents(
            client
                .send_request(IpcRequest::QueryOperatorTargetAgents {
                    target_node_id: "local-aiua-01".into(),
                })
                .await
                .expect("operator target agents request"),
        );
        assert!(agents.available);
        assert_eq!(agents.observation_kind, "local-canonical");
        assert_eq!(agents.agents.len(), 1);
        assert_eq!(agents.agents[0].agent_id, "agent-jane-01");
        assert_eq!(agents.agents[0].authority_hotel, "local-hotel");

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn operator_chat_turn_reuses_agent_conversation_path() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(16);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: Some(std::process::id().to_string()),
            })
            .expect("seed local hotel");
        let registry = Arc::new(RwLock::new(NodeRegistry::new()));
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph)
            .with_registry(registry);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut management = PhiloticClient::connect(GuestIdentity {
            guest_id: "operator-chat-test".into(),
            role: "management".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("management connect");
        let mut agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-jane-01".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("agent connect");

        let agent_task = tokio::spawn(async move {
            let inbound = agent.recv_task().await.expect("agent recv task");
            let IpcResponse::InboundTask { task_json, .. } = inbound else {
                panic!("unexpected inbound response to agent");
            };
            let payload: serde_json::Value =
                serde_json::from_str(&task_json).expect("agent payload should decode");
            assert_eq!(payload["source"], "operator_chat");
            assert_eq!(payload["transport"], "operator_chat");
            assert_eq!(payload["content"], "hello from desktop operator");
            let final_reply_to = payload["final_reply_to"]
                .as_str()
                .expect("final_reply_to should exist")
                .to_string();
            let final_reply_role = payload["final_reply_role"]
                .as_str()
                .expect("final_reply_role should exist")
                .to_string();
            let final_reply_guest_id = payload["final_reply_guest_id"]
                .as_str()
                .expect("final_reply_guest_id should exist")
                .to_string();
            let session_id = payload["session_id"]
                .as_str()
                .expect("session_id should exist")
                .to_string();
            let turn_id = payload["turn_id"]
                .as_str()
                .expect("turn_id should exist")
                .to_string();
            let chat_id = payload["chat_id"]
                .as_str()
                .expect("chat_id should exist")
                .to_string();

            agent
                .send_request(IpcRequest::EmitTask {
                    target_node: final_reply_to.clone(),
                    target_role: final_reply_role.clone(),
                    target_guest_id: Some(final_reply_guest_id.clone()),
                    task_json: serde_json::json!({
                        "action": "turn_event",
                        "event": "waiting_model",
                        "session_id": session_id,
                        "turn_id": turn_id,
                        "chat_id": chat_id
                    })
                    .to_string(),
                })
                .await
                .expect("agent emit turn event");

            agent
                .send_request(IpcRequest::EmitTask {
                    target_node: final_reply_to.clone(),
                    target_role: final_reply_role.clone(),
                    target_guest_id: Some(final_reply_guest_id.clone()),
                    task_json: serde_json::json!({
                        "action": "partial_reply",
                        "session_id": session_id,
                        "turn_id": turn_id,
                        "chat_id": chat_id,
                        "content": "hello from partial"
                    })
                    .to_string(),
                })
                .await
                .expect("agent emit partial reply");

            agent
                .send_request(IpcRequest::EmitTask {
                    target_node: final_reply_to,
                    target_role: final_reply_role,
                    target_guest_id: Some(final_reply_guest_id),
                    task_json: serde_json::json!({
                        "action": "send_reply",
                        "session_id": session_id,
                        "turn_id": turn_id,
                        "chat_id": chat_id,
                        "content": "hello back from agent"
                    })
                    .to_string(),
                })
                .await
                .expect("agent emit final reply");
        });

        let reply = expect_operator_chat_reply(
            management
                .send_request(IpcRequest::SendOperatorChatTurn {
                    target_node_id: "local-aiua-01".into(),
                    target_agent_id: "agent-jane-01".into(),
                    operator_session_id: "desktop-operator-session-1".into(),
                    conversation_id: None,
                    content: "hello from desktop operator".into(),
                })
                .await
                .expect("operator chat request"),
        );
        assert_eq!(reply.target_node_id, "local-aiua-01");
        assert_eq!(reply.target_agent_id, "agent-jane-01");
        assert_eq!(reply.delivery_kind, "local-direct");
        assert_eq!(reply.reply_action, "send_reply");
        assert_eq!(reply.observed_events, vec!["waiting_model"]);
        assert_eq!(reply.observed_partial_replies, vec!["hello from partial"]);
        assert_eq!(reply.content, "hello back from agent");

        agent_task.await.expect("agent task should finish");

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn operator_chat_turn_can_round_trip_through_remote_hotel_bridge() {
        let _env_guard = ipc_env_guard();
        let local_socket_path = test_socket_path();
        let remote_socket_path = format!("{local_socket_path}-remote");
        let (local_dispatcher_tx, mut local_dispatcher_rx) = mpsc::channel(16);
        let (remote_dispatcher_tx, mut remote_dispatcher_rx) = mpsc::channel(16);

        let local_graph_store =
            SqliteGraphStorage::open(":memory:").expect("open local sqlite graph store");
        let local_graph = Arc::new(GraphDomain::new(Arc::new(local_graph_store.adapter())));
        local_graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "local-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "local-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: local_socket_path.clone(),
                active_pid: Some(std::process::id().to_string()),
            })
            .expect("seed local hotel");
        local_graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "remote-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "remote-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9100,
                blob_port: 9101,
                execution_port: 9102,
                ipc_socket_path: remote_socket_path.clone(),
                active_pid: None,
            })
            .expect("seed remote hotel");
        let local_registry = Arc::new(RwLock::new(NodeRegistry::new()));
        local_registry.write().await.update_node(
            NodeCapabilities {
                node_id: "remote-aiua-01".into(),
                roles: vec![ansible_mesh_core::NodeRole::AnsibleNode],
                models: vec![],
                tools: vec![],
                constraints: Default::default(),
            },
            vec![CapabilityAdvertisement {
                hotel_id: "remote-hotel".into(),
                node_id: "remote-aiua-01".into(),
                incarnation_id: "remote-hotel:agent-runtime".into(),
                target_role: "agent".into(),
                availability_state: "live".into(),
                selection_hint: Some("remote_operator_chat".into()),
                latency_hint_ms: Some(12),
                max_concurrent_jobs: Some(4),
                active_jobs: 0,
                queue_depth: 0,
            }],
            Some(ExecutionReachability {
                protocol: "tcp-framed-v1".into(),
                host: "remote.mesh".into(),
                port: 9102,
            }),
        );

        let remote_graph_store =
            SqliteGraphStorage::open(":memory:").expect("open remote sqlite graph store");
        let remote_graph = Arc::new(GraphDomain::new(Arc::new(remote_graph_store.adapter())));
        remote_graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "remote-hotel".into(),
                capabilities: NodeCapabilities {
                    node_id: "remote-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                },
                mesh_port: 9100,
                blob_port: 9101,
                execution_port: 9102,
                ipc_socket_path: remote_socket_path.clone(),
                active_pid: Some(std::process::id().to_string()),
            })
            .expect("seed remote hotel");

        let local_server = IpcServer::new(
            local_socket_path.clone(),
            "local-aiua-01",
            local_dispatcher_tx,
            local_graph,
        )
        .with_registry(local_registry);
        let remote_server = IpcServer::new(
            remote_socket_path.clone(),
            "remote-aiua-01",
            remote_dispatcher_tx,
            remote_graph,
        );

        let local_server_task = tokio::spawn(async move {
            local_server
                .run()
                .await
                .expect("local ipc server should run");
        });
        let remote_server_task = tokio::spawn(async move {
            remote_server
                .run()
                .await
                .expect("remote ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &local_socket_path);
        }

        let local_to_remote_bridge = tokio::spawn({
            let remote_socket_path = remote_socket_path.clone();
            async move {
                let mut bridge = PhiloticClient::connect_at(
                    &remote_socket_path,
                    GuestIdentity {
                        guest_id: "bridge-local-to-remote".into(),
                        role: "management".into(),
                        supported_tools: Vec::new(),
                    },
                )
                .await
                .expect("connect local->remote bridge");

                while let Some(command) = local_dispatcher_rx.recv().await {
                    let LedgerCommand::AppendLocal(env) = command else {
                        continue;
                    };
                    if env.target_node_id.as_deref() != Some("remote-aiua-01") {
                        continue;
                    }
                    let target_role = env
                        .target_agent_id
                        .clone()
                        .unwrap_or_else(|| "agent".into());
                    let EventPayload::Inline { data } = env.payload else {
                        continue;
                    };
                    bridge
                        .send_request(IpcRequest::EmitTask {
                            target_node: "remote-aiua-01".into(),
                            target_role,
                            target_guest_id: None,
                            task_json: data,
                        })
                        .await
                        .expect("relay local->remote operator chat task");
                }
            }
        });

        let remote_to_local_bridge = tokio::spawn({
            let local_socket_path = local_socket_path.clone();
            async move {
                let mut bridge = PhiloticClient::connect_at(
                    &local_socket_path,
                    GuestIdentity {
                        guest_id: "bridge-remote-to-local".into(),
                        role: "management".into(),
                        supported_tools: Vec::new(),
                    },
                )
                .await
                .expect("connect remote->local bridge");

                while let Some(command) = remote_dispatcher_rx.recv().await {
                    let LedgerCommand::AppendLocal(env) = command else {
                        continue;
                    };
                    if env.target_node_id.as_deref() != Some("local-aiua-01") {
                        continue;
                    }
                    let target_role = env
                        .target_agent_id
                        .clone()
                        .unwrap_or_else(|| OPERATOR_CHAT_REPLY_ROLE.into());
                    let EventPayload::Inline { data } = env.payload else {
                        continue;
                    };
                    bridge
                        .send_request(IpcRequest::EmitTask {
                            target_node: "local-aiua-01".into(),
                            target_role,
                            target_guest_id: None,
                            task_json: data,
                        })
                        .await
                        .expect("relay remote->local operator chat reply");
                }
            }
        });

        let mut management = PhiloticClient::connect(GuestIdentity {
            guest_id: "operator-chat-test-remote".into(),
            role: "management".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("management connect");
        let mut remote_agent = PhiloticClient::connect_at(
            &remote_socket_path,
            GuestIdentity {
                guest_id: "agent-jane-remote".into(),
                role: "agent".into(),
                supported_tools: Vec::new(),
            },
        )
        .await
        .expect("remote agent connect");

        let remote_agent_task = tokio::spawn(async move {
            let inbound = remote_agent
                .recv_task()
                .await
                .expect("remote agent recv task");
            let IpcResponse::InboundTask { task_json, .. } = inbound else {
                panic!("unexpected inbound response to remote agent");
            };
            let payload: serde_json::Value =
                serde_json::from_str(&task_json).expect("remote agent payload should decode");
            assert_eq!(payload["source"], "operator_chat");
            assert_eq!(payload["transport"], "operator_chat");
            assert_eq!(payload["content"], "hello across the mesh");
            let final_reply_to = payload["final_reply_to"]
                .as_str()
                .expect("final_reply_to should exist")
                .to_string();
            let final_reply_role = payload["final_reply_role"]
                .as_str()
                .expect("final_reply_role should exist")
                .to_string();
            let session_id = payload["session_id"]
                .as_str()
                .expect("session_id should exist")
                .to_string();
            let turn_id = payload["turn_id"]
                .as_str()
                .expect("turn_id should exist")
                .to_string();
            let chat_id = payload["chat_id"]
                .as_str()
                .expect("chat_id should exist")
                .to_string();

            remote_agent
                .send_request(IpcRequest::EmitTask {
                    target_node: final_reply_to.clone(),
                    target_role: final_reply_role.clone(),
                    target_guest_id: None,
                    task_json: serde_json::json!({
                        "action": "turn_event",
                        "event": "waiting_remote_model",
                        "session_id": session_id,
                        "turn_id": turn_id,
                        "chat_id": chat_id
                    })
                    .to_string(),
                })
                .await
                .expect("remote agent emit turn event");

            remote_agent
                .send_request(IpcRequest::EmitTask {
                    target_node: final_reply_to,
                    target_role: final_reply_role,
                    target_guest_id: None,
                    task_json: serde_json::json!({
                        "action": "send_reply",
                        "session_id": session_id,
                        "turn_id": turn_id,
                        "chat_id": chat_id,
                        "content": "hello back from remote agent"
                    })
                    .to_string(),
                })
                .await
                .expect("remote agent emit final reply");
        });

        let reply = expect_operator_chat_reply(
            management
                .send_request(IpcRequest::SendOperatorChatTurn {
                    target_node_id: "remote-aiua-01".into(),
                    target_agent_id: "agent-jane-remote".into(),
                    operator_session_id: "desktop-operator-session-remote".into(),
                    conversation_id: None,
                    content: "hello across the mesh".into(),
                })
                .await
                .expect("remote operator chat request"),
        );
        assert_eq!(reply.target_node_id, "remote-aiua-01");
        assert_eq!(reply.target_agent_id, "agent-jane-remote");
        assert_eq!(reply.target_hotel, "remote-hotel");
        assert_eq!(reply.delivery_kind, "router-routed");
        assert_eq!(reply.reply_action, "send_reply");
        assert_eq!(reply.observed_events, vec!["waiting_remote_model"]);
        assert_eq!(reply.content, "hello back from remote agent");

        remote_agent_task
            .await
            .expect("remote agent task should finish");

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        local_to_remote_bridge.abort();
        let _ = local_to_remote_bridge.await;
        remote_to_local_bridge.abort();
        let _ = remote_to_local_bridge.await;
        local_server_task.abort();
        let _ = local_server_task.await;
        remote_server_task.abort();
        let _ = remote_server_task.await;
        if Path::new(&local_socket_path).exists() {
            let _ = std::fs::remove_file(&local_socket_path);
        }
        if Path::new(&remote_socket_path).exists() {
            let _ = std::fs::remove_file(&remote_socket_path);
        }
    }

    #[tokio::test]
    async fn configure_role_persists_config_successfully() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _) = mpsc::channel(8);
        let graph = Arc::new(GraphDomain::new(Arc::new(TestGraphAdapter)));
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut orchestrator = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-jane-01:orchestrator".into(),
            role: "orchestrator".into(),
            supported_tools: vec![],
        })
        .await
        .expect("orchestrator connect");

        let resp = orchestrator
            .send_request(IpcRequest::ConfigureRole {
                agent_id: "agent-jane-01".into(),
                role_name: "developer".into(),
                guest_id: "agent-jane-01:developer".into(),
                calling_role: "orchestrator".into(),
                toolset_profile: "developer".into(),
                role_identity_addendum: Some("Addendum".into()),
                role_manifest: None,
                is_admin: false,
                inactive_ttl_seconds: Some(60),
                iteration_cap: Some(10),
                approval_policy: Some("auto".into()),
                model_profile: Some("fast".into()),
                context_window_policy: Some("standard".into()),
            })
            .await
            .expect("configure request");

        match resp {
            IpcResponse::ConfigureRoleOk { role_name } => assert_eq!(role_name, "developer"),
            other => panic!("expected ConfigureRoleOk, got {:?}", other),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn configure_role_forbids_configuring_other_identities() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _) = mpsc::channel(8);
        let graph = Arc::new(GraphDomain::new(Arc::new(TestGraphAdapter)));
        let server = IpcServer::new(socket_path.clone(), "local-aiua-01", dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut orchestrator = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-jane-01:orchestrator".into(),
            role: "orchestrator".into(),
            supported_tools: vec![],
        })
        .await
        .expect("orchestrator connect");

        let resp = orchestrator
            .send_request(IpcRequest::ConfigureRole {
                agent_id: "agent-bob-01".into(), // Different agent!
                role_name: "developer".into(),
                guest_id: "agent-bob-01:developer".into(),
                calling_role: "orchestrator".into(),
                toolset_profile: "developer".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                inactive_ttl_seconds: None,
                iteration_cap: None,
                approval_policy: None,
                model_profile: None,
                context_window_policy: None,
            })
            .await
            .expect("configure request");

        match resp {
            IpcResponse::Standard { ok, code, .. } => {
                assert!(!ok);
                assert_eq!(code, "CONFIGURE_FORBIDDEN");
            }
            other => panic!("expected Error, got {:?}", other),
        }

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }
}
