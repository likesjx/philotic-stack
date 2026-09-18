//! Role routing and materialization: agent-route resolution
//! (`resolve_agent_route`), on-demand role-incarnation materialization
//! (`ensure_role_materialized`), the unified park-and-materialize primitive
//! (`park_and_materialize` / [`ParkTarget`]), mesh envelope delivery/parking
//! (`deliver_event_envelope_or_park`), and the HandoffToRole / HandoffBack /
//! ConfigureRole / SetRoleHome handlers.
//!
//! The IPC dispatch match arms remain in `ipc.rs` and delegate here via `Self::`.
//!
//! Extracted verbatim from `ipc.rs` — no behavior change.

use super::ipc::{
    AgentRouteResolution, DeliveryClaimRegistry, InboxRegistry, IpcServer, ParkTarget,
    ParkedInboundTask, attach_agent_graph_snapshot, claim_delivery, lookup_agent_authority_hotel,
    placement_marker_policy, unix_ts,
};
use crate::LedgerCommand;
use crate::service::guest_manager::GuestMaterializationRequester;
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::event::{EventEnvelope, EventKind, EventPayload};
use ansible_mesh_core::graph::{
    MembraneTransportHomeRecord, MembraneTransportHomeStatus, RoleReadinessState,
};
use ansible_mesh_core::relocation_ceremony::{RelocationCeremonyPhase, RelocationCeremonyRecord};
use philotic_client::{GuestIdentity, HandoffBundle, IpcResponse};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tracing::{info, warn};
use uuid::Uuid;

impl IpcServer {
    /// Park a task for a dormant target and trigger its materialization, flushed when the
    /// target philote connects and registers under the parked guest_id.
    ///
    /// The [`ParkTarget`] enum forces the caller to state which materialization semantics
    /// apply (see its docs — the two arms are intentionally *not* interchangeable):
    /// - [`ParkTarget::LocalRoleIncarnation`]: local single-process role incarnation,
    ///   parked under `role_record.guest_id`, woken via [`Self::ensure_role_materialized`].
    /// - [`ParkTarget::CrossHotelGuest`]: cross-hotel `TaskInvoke` addressed to
    ///   `delivery_target_guest_id` with no live inbox subscriber, parked under the
    ///   agent-centric guest id, materialized as a dedicated `{hotel}:philote-{role}`
    ///   process guest.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn park_and_materialize(
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        parked_inbound: &Arc<Mutex<HashMap<String, Vec<ParkedInboundTask>>>>,
        mat_req: Option<&dyn GuestMaterializationRequester>,
        local_node_id: &str,
        source_node: &str,
        task_id: Uuid,
        task_json: String,
        target: ParkTarget<'_>,
    ) {
        match target {
            ParkTarget::LocalRoleIncarnation { role_record } => {
                {
                    let mut guard = parked_inbound.lock().await;
                    guard.entry(role_record.guest_id.clone()).or_default().push(
                        ParkedInboundTask {
                            source_node: source_node.to_string(),
                            task_id,
                            task_json,
                            activate_session_id: None,
                            parked_at: unix_ts(),
                        },
                    );
                }
                info!(
                    guest_id = %role_record.guest_id,
                    task_id = %task_id,
                    "Local role-incarnation task parked; triggering on-demand materialization."
                );

                match Self::ensure_role_materialized(
                    graph,
                    inboxes,
                    mat_req,
                    local_node_id,
                    &role_record.agent_id,
                    &role_record.role_name,
                )
                .await
                {
                    Ok(readiness) => info!(
                        guest_id = %role_record.guest_id,
                        ?readiness,
                        "Local role-incarnation materialization requested."
                    ),
                    Err(e) => warn!(
                        guest_id = %role_record.guest_id,
                        "Local role-incarnation materialization failed: {e}"
                    ),
                }
            }
            ParkTarget::CrossHotelGuest { agent_guest_id } => {
                // Park the task — flushed when the role philote connects and registers.
                {
                    let mut guard = parked_inbound.lock().await;
                    guard
                        .entry(agent_guest_id.to_string())
                        .or_default()
                        .push(ParkedInboundTask {
                            source_node: source_node.to_string(),
                            task_id,
                            task_json,
                            activate_session_id: None,
                            parked_at: unix_ts(),
                        });
                }
                info!(
                    agent_guest_id,
                    task_id = %task_id,
                    "Cross-hotel TaskInvoke parked; triggering role-philote materialization."
                );

                // Resolve the hotel guest record ID from the agent-centric guest_id.
                let incarnations = graph
                    .list_role_incarnations_by_guest_id(agent_guest_id)
                    .unwrap_or_default();
                let Some(inc) = incarnations.into_iter().next() else {
                    warn!(
                        agent_guest_id,
                        "No role incarnation found for cross-hotel guest; cannot materialize."
                    );
                    return;
                };
                let Some(hotel_name) = Self::local_hotel_name(graph, local_node_id) else {
                    warn!(
                        agent_guest_id,
                        "Cannot determine local hotel name; cannot materialize role philote."
                    );
                    return;
                };
                let hotel_guest_id = format!("{}:philote-{}", hotel_name, inc.role_name);
                let socket_path = graph
                    .list_hotels()
                    .ok()
                    .and_then(|hs| {
                        hs.into_iter()
                            .find(|h| h.capabilities.node_id == local_node_id)
                            .map(|h| h.ipc_socket_path)
                    })
                    .unwrap_or_default();

                // Create the hotel guest record if it doesn't already exist.
                if graph
                    .get_guest(&hotel_name, &hotel_guest_id)
                    .ok()
                    .flatten()
                    .is_none()
                {
                    // Mirror role_worker_manifest's env exactly (ipc.rs, Self::role_worker_manifest):
                    // PHILOTIC_ROLE_INBOX/PHILOTIC_GUEST_ID/PHILOTIC_HOTEL_NAME are what let the
                    // spawned philote self-report the canonical role_incarnation identity
                    // ("role:{agent_id}:{role_name}" / "{agent_id}:{role_name}") instead of
                    // falling back to a bare role name that can never pass
                    // Self::is_agent_handoff_caller — omitting them here previously produced a
                    // process that could materialize into a role but never hand back out of it.
                    let config_json = serde_json::json!({
                        "command": "philote",
                        "args": [],
                        "env": {
                            "PHILOTIC_AGENT_ID": inc.agent_id,
                            "PHILOTIC_GUEST_ID": inc.guest_id,
                            "PHILOTIC_ROLE_NAME": inc.role_name,
                            "PHILOTIC_ROLE_INBOX": inc.routing_role(),
                            "PHILOTIC_HOTEL_SOCKET": socket_path,
                            "PHILOTIC_HOTEL_NAME": hotel_name,
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
                        info!("Created role-philote guest record: {}", hotel_guest_id);
                    }
                }

                if let Some(req) = mat_req {
                    match req.ensure_guest_active(&hotel_guest_id).await {
                        Ok(true) => info!(
                            "Role-philote [{}] materialization triggered for cross-hotel task.",
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
            }
        }
    }

    pub(super) async fn resolve_agent_route(
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        local_node_id: &str,
        target_role: &str,
        target_guest_id: Option<String>,
        task_json: &str,
    ) -> AgentRouteResolution {
        if target_role != "agent" {
            return AgentRouteResolution::Deliver(target_guest_id);
        }
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(task_json) else {
            return AgentRouteResolution::Deliver(target_guest_id);
        };
        let session_id = payload
            .get("session_id")
            .and_then(serde_json::Value::as_str);
        let Some(session_id) = session_id else {
            return AgentRouteResolution::Deliver(target_guest_id);
        };
        let session = graph.get_session(session_id).ok().flatten();
        let Some(session) = session else {
            return AgentRouteResolution::Deliver(target_guest_id);
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

        if let Some(explicit_guest_id) = target_guest_id.as_deref() {
            let targets_base_agent = session
                .primary_agent_id
                .as_deref()
                .map(|agent_id| explicit_guest_id == agent_id)
                .unwrap_or(false);
            if !targets_base_agent {
                // Explicit incarnation target — e.g. a paracrine_response addressed
                // back to the orchestrator as "{agent_id}:{role_name}". This used to
                // early-return Deliver(incarnation); deliver_inbound_task then dropped
                // it "ledger-only" whenever that incarnation was not subscribed under
                // its exact id (the async-whisper reply drop). Resolve liveness the same
                // way the active-incarnation path below does so the reply survives an
                // ephemeral orchestrator or one registered under its bare agent id.
                //
                // 1. Incarnation is itself live → deliver straight to it.
                if is_registered(explicit_guest_id) {
                    return AgentRouteResolution::Deliver(target_guest_id);
                }
                // 2. Its base agent is live (a single-process philote registers under
                //    the bare agent id, not the incarnation id) → normalize to the live
                //    base, which handles the incarnation internally.
                if let Some((base_agent_id, _role_name)) = explicit_guest_id.split_once(':') {
                    let base_is_this_agent = session.primary_agent_id.as_deref()
                        == Some(base_agent_id)
                        || graph
                            .list_role_incarnations_by_guest_id(explicit_guest_id)
                            .ok()
                            .into_iter()
                            .flatten()
                            .any(|record| record.agent_id == base_agent_id);
                    if base_is_this_agent && is_registered(base_agent_id) {
                        info!(
                            "Explicit incarnation [{}] not registered for session [{}]; delivering to its live base agent [{}].",
                            explicit_guest_id, session_id, base_agent_id
                        );
                        return AgentRouteResolution::Deliver(Some(base_agent_id.to_string()));
                    }
                }
                // 3. Nothing live, but the incarnation is configured on this hotel →
                //    park + materialize instead of dropping, so a respawn can flush the
                //    reply. For a remote/unknown guest, fall through to Deliver so the
                //    mesh reroute in EmitTask can forward it.
                if Self::configured_local_guest_exists(graph, local_node_id, explicit_guest_id) {
                    info!(
                        "Explicit incarnation [{}] not live for session [{}]; parking + requesting materialization instead of dropping.",
                        explicit_guest_id, session_id
                    );
                    return AgentRouteResolution::Park {
                        guest_id: explicit_guest_id.to_string(),
                    };
                }
                return AgentRouteResolution::Deliver(target_guest_id);
            }
        }
        let local_hotel_name = Self::local_hotel_name(graph, local_node_id);
        let mut provenance_hint =
            Self::local_delivery_provenance_hint(&session, local_hotel_name.as_deref());
        // Guard (2026-07-06 parked-tool-result incident): an agent-role task must never
        // be delivered to — or parked for — a guest whose role is not an agent role.
        // A tool dispatch that leaked into `agent_runtime_provenance` would otherwise
        // park the agent's tool RESULT for the runner that produced it (e.g.
        // vps-jane:life-graph-runner), a guest that never consumes agent tasks, and the
        // turn dies at the watchdog. Reject such hints loudly so the next poisoning is
        // self-diagnosing.
        if let Some(hint) = provenance_hint.as_ref() {
            if !Self::guest_can_fill_agent_placement(graph, local_node_id, &hint.guest_id) {
                warn!(
                    "Session [{}] persisted local delivery guest [{}] has a non-agent role; \
                     rejecting poisoned placement provenance (tool/datasource dispatches must \
                     not set agent placement — parked-tool-result incident).",
                    session_id, hint.guest_id
                );
                provenance_hint = None;
            }
        }
        if let (Some(active_guest_id), Some(hint)) = (
            session.active_incarnation_id.as_deref(),
            provenance_hint.as_ref(),
        ) {
            let policy = placement_marker_policy(
                hint.marker_kind.as_deref(),
                hint.marker_strength.as_deref(),
            );
            if policy.supersede_on_newer_active_incarnation_conflict
                && active_guest_id != hint.guest_id
                && session.updated_at > hint.updated_at
            {
                provenance_hint = None;
            }
        }

        if let Some(active_guest_id) = session.active_incarnation_id.clone() {
            if is_registered(&active_guest_id) {
                return AgentRouteResolution::Deliver(Some(active_guest_id));
            }

            // Registration-name mismatch (enabler of the parked-tool-result incident):
            // a single-process philote registers under its bare agent id while the
            // session's active_incarnation_id stores "{agent_id}:{role_name}", so the
            // live registry lookup above misses and routing used to fall through to the
            // provenance-hint / park paths. Normalize: if the incarnation's base agent
            // (its ":"-prefix) is the session's primary agent or the incarnation's own
            // agent_id, and that base is live, deliver to it directly.
            if let Some((base_agent_id, _role_name)) = active_guest_id.split_once(':') {
                let base_is_this_agent = session.primary_agent_id.as_deref() == Some(base_agent_id)
                    || graph
                        .list_role_incarnations_by_guest_id(&active_guest_id)
                        .ok()
                        .into_iter()
                        .flatten()
                        .any(|record| record.agent_id == base_agent_id);
                if base_is_this_agent && is_registered(base_agent_id) {
                    info!(
                        "Active incarnation [{}] is not registered for session [{}]; delivering to its live base agent registration [{}].",
                        active_guest_id, session_id, base_agent_id
                    );
                    return AgentRouteResolution::Deliver(Some(base_agent_id.to_string()));
                }
            }

            if let Some(hint) = provenance_hint.as_ref() {
                let provenance_guest_id = hint.guest_id.as_str();
                if provenance_guest_id != active_guest_id {
                    if is_registered(provenance_guest_id) {
                        warn!(
                            "Active incarnation [{}] is not registered for session [{}]; preferring persisted local delivery guest [{}].",
                            active_guest_id, session_id, provenance_guest_id
                        );
                        return AgentRouteResolution::Deliver(Some(
                            provenance_guest_id.to_string(),
                        ));
                    }

                    let policy = placement_marker_policy(
                        hint.marker_kind.as_deref(),
                        hint.marker_strength.as_deref(),
                    );
                    if policy.permit_parking_when_unregistered
                        && Self::configured_local_guest_exists(
                            graph,
                            local_node_id,
                            provenance_guest_id,
                        )
                    {
                        info!(
                            "Active incarnation [{}] is not registered for session [{}]; parking inbound for persisted local delivery guest [{}].",
                            active_guest_id, session_id, provenance_guest_id
                        );
                        return AgentRouteResolution::Park {
                            guest_id: provenance_guest_id.to_string(),
                        };
                    }
                }
            }

            if !Self::configured_local_guest_exists(graph, local_node_id, &active_guest_id) {
                // Active incarnation is not configured on this hotel — it may live on a
                // remote hotel. Return Deliver directly so EmitTask can reroute via the
                // mesh registry (HotelStateSync). Do NOT fall back to the local orchestrator,
                // which would silently drop the intent to use the remote role.
                return AgentRouteResolution::Deliver(Some(active_guest_id));
            }

            // Active incarnation is configured locally but not running; try orchestrator
            // fallback so the user isn't stuck waiting for a respawn.
            if let Some(orchestrator_guest_id) =
                Self::resolve_orchestrator_guest_id(graph, &session, &live_agent_guests)
            {
                warn!(
                    "Active incarnation [{}] is not registered for session [{}]; falling back to orchestrator guest [{}].",
                    active_guest_id, session_id, orchestrator_guest_id
                );
                return AgentRouteResolution::Deliver(Some(orchestrator_guest_id));
            }

            info!(
                "Active incarnation [{}] is not registered for session [{}]; parking inbound and requesting materialization.",
                active_guest_id, session_id
            );
            return AgentRouteResolution::Park {
                guest_id: active_guest_id,
            };
        }

        if let Some(provenance_guest_id) =
            provenance_hint.as_ref().map(|hint| hint.guest_id.as_str())
        {
            if is_registered(provenance_guest_id) {
                info!(
                    "Session [{}] has no active incarnation; routing inbound task to persisted local delivery guest [{}].",
                    session_id, provenance_guest_id
                );
                return AgentRouteResolution::Deliver(Some(provenance_guest_id.to_string()));
            }

            let policy = placement_marker_policy(
                provenance_hint
                    .as_ref()
                    .and_then(|hint| hint.marker_kind.as_deref()),
                provenance_hint
                    .as_ref()
                    .and_then(|hint| hint.marker_strength.as_deref()),
            );
            if policy.permit_parking_when_unregistered
                && Self::configured_local_guest_exists(graph, local_node_id, provenance_guest_id)
            {
                info!(
                    "Session [{}] has no active incarnation; parking inbound for persisted local delivery guest [{}] while materializing.",
                    session_id, provenance_guest_id
                );
                return AgentRouteResolution::Park {
                    guest_id: provenance_guest_id.to_string(),
                };
            }
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

    /// Normalize a freshly-registered cron job's `target_role` to the inbox routing key
    /// (`role:{agent_id}:{role_name}`, matching `RoleIncarnationRecord::routing_role`) that
    /// `deliver_inbound_task`/`SubscribeInbox` actually key on. Agents calling `cron.register`
    /// almost always mean "my own role of that name" when they pass a bare role name like
    /// `"orchestrator"` — resolve it against the registering guest's own role incarnations so
    /// the job is deliverable, instead of silently persisting a key that can never match.
    pub(super) fn normalize_cron_target_role(
        graph: &GraphDomain,
        job: &mut ansible_mesh_core::cron::CronJob,
    ) {
        if job.target_role.starts_with("role:") {
            return;
        }
        let ansible_mesh_core::cron::CronJobSource::Guest(agent_id) = &job.created_by else {
            return;
        };
        if graph
            .get_role_incarnation(agent_id, &job.target_role)
            .ok()
            .flatten()
            .is_some()
        {
            job.target_role = format!("role:{agent_id}:{}", job.target_role);
        }
    }

    pub(crate) async fn ensure_role_materialized(
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        local_node_id: &str,
        agent_id: &str,
        role_name: &str,
    ) -> anyhow::Result<RoleReadinessState> {
        let role_record = graph
            .get_role_incarnation(agent_id, role_name)?
            .ok_or_else(|| {
                anyhow::anyhow!("role [{role_name}] is not configured for agent [{agent_id}]")
            })?;

        if Self::role_route_is_live(inboxes, &role_record.routing_role(), &role_record.guest_id)
            .await
        {
            let readiness = if matches!(
                role_record.readiness_state,
                RoleReadinessState::ActiveInSession
            ) {
                RoleReadinessState::ActiveInSession
            } else {
                RoleReadinessState::Routable
            };
            graph.set_role_incarnation_readiness(agent_id, role_name, readiness.clone())?;
            return Ok(readiness);
        }

        // If a role worker process is already running (but not yet registered to its inbox),
        // skip re-registering. handle_register_component resets active_pid=None in its upsert,
        // which causes ensure_guest_active to re-spawn unconditionally — creating a spawn storm
        // on each 250ms HandoffPending retry.
        if Self::role_guest_process_is_live(graph, local_node_id, &role_record.guest_id)? {
            graph.set_role_incarnation_readiness(
                agent_id,
                role_name,
                RoleReadinessState::Materializing,
            )?;
            return Ok(RoleReadinessState::Materializing);
        }

        let manifest = Self::role_worker_manifest(graph, local_node_id, &role_record)?;
        match Self::handle_register_component(graph, materialization_requester, manifest).await {
            IpcResponse::ComponentRegistered { .. } => {}
            IpcResponse::Standard { ok: true, .. } => {}
            IpcResponse::Error(msg) => anyhow::bail!(msg),
            other => anyhow::bail!("unexpected role materialization response: {other:?}"),
        }

        let readiness = if Self::role_route_is_live(
            inboxes,
            &role_record.routing_role(),
            &role_record.guest_id,
        )
        .await
        {
            RoleReadinessState::Routable
        } else if Self::role_guest_process_is_live(graph, local_node_id, &role_record.guest_id)? {
            RoleReadinessState::Materialized
        } else {
            RoleReadinessState::Materializing
        };
        graph.set_role_incarnation_readiness(agent_id, role_name, readiness.clone())?;
        Ok(readiness)
    }

    pub(super) async fn configure_role_record(
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        local_node_id: &str,
        current_identity: Option<&GuestIdentity>,
        agent_id: String,
        role_name: String,
        guest_id: String,
        calling_role: String,
        toolset_profile: String,
        role_identity_addendum: Option<String>,
        role_manifest: Option<String>,
        is_admin: bool,
        inactive_ttl_seconds: Option<u64>,
        iteration_cap: Option<u32>,
        approval_policy: Option<String>,
        model_profile: Option<String>,
        context_window_policy: Option<String>,
        fallback_tiers: Option<Vec<String>>,
        // Per-agent model NAME binding (Layer 1). Same preserve-on-None
        // contract as `fallback_tiers`: `None` preserves whatever is already
        // on the record (empty for a brand-new role); `Some(map)` sets it
        // explicitly. Mirrors the #179/#213 preserve-or-source contract so
        // `aiua load`'s reseed (`seed_orchestrator_roles`) never wipes an
        // operator-set binding.
        model_bindings: Option<std::collections::BTreeMap<String, String>>,
        // Content-filtering posture for this role. `None` PRESERVES whatever is
        // already on the record (or defaults a brand-new role to `"standard"`) —
        // mirrors the `fallback_tiers` preserve-on-None fix so reconfiguring a
        // role for an unrelated field (e.g. toolset_profile) never silently
        // resets an operator-set `"unrestricted"` policy back to `"standard"`.
        // `Some(value)` must be one of `unrestricted` | `standard` | `strict`.
        content_policy: Option<String>,
    ) -> IpcResponse {
        let Some(identity) = current_identity else {
            return IpcResponse::error(
                "configure_role",
                "CONFIGURE_UNREGISTERED",
                "guest must register before configuring roles",
            );
        };
        // Model-selection self-service: an agent may retune its model routing
        // (`fallback_tiers` / `model_bindings`) without admin rights. Choosing
        // which model answers is lower-stakes than changing toolset, manifest,
        // TTL, or admin status, and this backs the operator's one-tap `/model`
        // swap command (philote `SlashCommand::ModelPreset`). Gated tightly:
        // ONLY when no privileged field is being changed. The toolset the
        // caller passed is IGNORED for this path (force-preserved to the
        // existing record below), so it can never escalate privilege or alter
        // capabilities — only the model routing changes.
        let is_model_selection_only = (fallback_tiers.is_some() || model_bindings.is_some())
            && !is_admin
            && role_identity_addendum.is_none()
            && role_manifest.is_none()
            && approval_policy.is_none()
            && model_profile.is_none()
            && context_window_policy.is_none()
            && content_policy.is_none()
            && inactive_ttl_seconds.is_none()
            && iteration_cap.is_none();

        // A non-orchestrator role may pass ONLY as model-selection-only
        // self-service on its own record (philote sends `calling_role =
        // <active role>` for `/model`, so a session in e.g. vixen posture
        // retunes vixen, not orchestrator — anything broader stays
        // orchestrator-gated). Two extra guards keep the claim honest: the
        // caller's registered guest identity must actually BE that role
        // incarnation (or the agent's single-process base guest, which hosts
        // every role in-process), and the record must already exist —
        // self-retune may never CREATE a role, since a brand-new record
        // would take the caller-supplied toolset instead of preserving one.
        let is_model_selection_self_service = is_model_selection_only
            && role_name == calling_role
            && (identity.guest_id == format!("{agent_id}:{role_name}")
                || identity.guest_id == agent_id)
            && graph
                .get_role_incarnation(&agent_id, &role_name)
                .ok()
                .flatten()
                .is_some();
        if calling_role != "orchestrator" && !is_model_selection_self_service {
            return IpcResponse::error(
                "configure_role",
                "CONFIGURE_FORBIDDEN",
                "only agents operating in the orchestrator persona may configure role incarnations \
                 (exception: any role may apply a model-selection-only change to itself)",
            );
        }
        if !identity.guest_id.starts_with(&agent_id) {
            return IpcResponse::error(
                "configure_role",
                "CONFIGURE_FORBIDDEN",
                "guests may only configure roles for their own agent identity",
            );
        }
        let caller_agent_id = identity
            .guest_id
            .strip_suffix(&format!(":{}", identity.role))
            .unwrap_or(&identity.guest_id);
        let caller_is_admin = graph
            .get_role_incarnation(caller_agent_id, &identity.role)
            .ok()
            .flatten()
            .map(|r| r.has_full_admin_authority())
            .unwrap_or(false);

        if role_name == "orchestrator" && !caller_is_admin && !is_model_selection_only {
            return IpcResponse::error(
                "configure_role",
                "CONFIGURE_FORBIDDEN",
                "the orchestrator role record is operator-owned; only admin roles may update it",
            );
        }

        if is_admin && !caller_is_admin {
            return IpcResponse::error(
                "configure_role",
                "CONFIGURE_FORBIDDEN",
                "only admin roles may create other admin roles",
            );
        }

        let previous = graph
            .get_role_incarnation(&agent_id, &role_name)
            .ok()
            .flatten();
        let is_new_role = previous.is_none();

        // Model-selection-only self-service (see the gate exemption above):
        // force-preserve the existing toolset so a `/model`-style change can only
        // touch model routing, never capabilities — regardless of what toolset
        // the caller passed. Non-model-selection callers keep the passed value.
        let toolset_profile = if is_model_selection_only {
            previous
                .as_ref()
                .map(|p| p.toolset_profile.clone())
                .unwrap_or(toolset_profile)
        } else {
            toolset_profile
        };

        // Ladder resolution: `None` PRESERVES the existing record's ladder — this is
        // the fix for the bug where every ConfigureRole call unconditionally wiped
        // `fallback_tiers` to empty, silently erasing DB-edited ladders on every
        // reconfigure (there is no IPC path to set one, so the wipe was permanent).
        // `Some(tiers)` sets the ladder explicitly, validated for shape (non-empty
        // list of non-empty tier names). A brand-new role with `None` gets
        // `DEFAULT_FALLBACK_TIERS` rather than empty.
        let resolved_fallback_tiers = match fallback_tiers {
            Some(tiers) => {
                if tiers.is_empty() || tiers.iter().any(|t| t.trim().is_empty()) {
                    return IpcResponse::error(
                        "configure_role",
                        "CONFIGURE_INVALID_FALLBACK_TIERS",
                        "fallback_tiers must be a non-empty list of non-empty tier role names",
                    );
                }
                tiers
            }
            None => match previous.as_ref() {
                Some(prev) => prev.turn_loop_config.fallback_tiers.clone(),
                None => ansible_mesh_core::model_routing::DEFAULT_FALLBACK_TIERS
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            },
        };

        // Model-binding resolution (Layer 1): same preserve-on-None contract
        // as `fallback_tiers` above — `None` preserves the existing record's
        // bindings (empty for a brand-new role); `Some(map)` sets them
        // explicitly. Keys/values are trimmed and empty entries rejected so a
        // malformed IPC/tool call can't silently persist a dead binding.
        let resolved_model_bindings = match model_bindings {
            Some(bindings) => {
                if bindings
                    .iter()
                    .any(|(k, v)| k.trim().is_empty() || v.trim().is_empty())
                {
                    return IpcResponse::error(
                        "configure_role",
                        "CONFIGURE_INVALID_MODEL_BINDINGS",
                        "model_bindings keys and values must be non-empty",
                    );
                }
                bindings
            }
            None => match previous.as_ref() {
                Some(prev) => prev.turn_loop_config.model_bindings.clone(),
                None => Default::default(),
            },
        };

        // Content-policy resolution: `None` preserves the existing record's policy
        // (or defaults a brand-new role to `"standard"`) — same preserve-on-None
        // contract as `fallback_tiers` above. `Some(value)` must be a known policy.
        let resolved_content_policy = match content_policy {
            Some(policy) => {
                if !ansible_mesh_core::graph::is_valid_content_policy(&policy) {
                    return IpcResponse::error(
                        "configure_role",
                        "CONFIGURE_INVALID_CONTENT_POLICY",
                        "content_policy must be one of: unrestricted, standard, strict",
                    );
                }
                policy
            }
            None => match previous.as_ref() {
                Some(prev) => prev.content_policy.clone(),
                None => ansible_mesh_core::graph::default_content_policy(),
            },
        };

        // For a new role, check if the base agent is currently live. Single-process philote
        // registers as the base agent_id and handles all roles internally, so any new role
        // it creates is immediately routable via the base guest.
        let initial_readiness = if let Some(prev) = previous.as_ref() {
            prev.readiness_state.clone()
        } else {
            let base_guest_live = {
                let guard = inboxes.lock().await;
                guard
                    .get("agent")
                    .into_iter()
                    .flatten()
                    .any(|s| s.guest_id == agent_id)
            };
            if base_guest_live {
                ansible_mesh_core::graph::RoleReadinessState::Routable
            } else {
                ansible_mesh_core::graph::RoleReadinessState::Configured
            }
        };
        // Model-selection-only self-service force-preserves EVERY non-model
        // field from the existing record, not just the toolset: the philote's
        // `/model` swap sends `None` for fields it doesn't touch, and writing
        // those `None`s through would wipe the role's identity addendum,
        // manifest, TTL, admin flag, home pin, and non-model turn-loop config
        // (a vixen `/model` swap would silently strip the register identity).
        // `is_model_selection_only` already guarantees the corresponding
        // request args are all `None`/false, so preserving is never a conflict.
        let preserved = if is_model_selection_only {
            previous.as_ref()
        } else {
            None
        };
        let record = ansible_mesh_core::graph::RoleIncarnationRecord {
            agent_id: agent_id.clone(),
            role_name: role_name.clone(),
            guest_id,
            toolset_profile,
            role_identity_addendum: preserved
                .map(|p| p.role_identity_addendum.clone())
                .unwrap_or(role_identity_addendum),
            role_manifest: preserved
                .map(|p| p.role_manifest.clone())
                .unwrap_or(role_manifest),
            content_policy: resolved_content_policy,
            is_admin: preserved.map(|p| p.is_admin).unwrap_or(is_admin),
            readiness_state: initial_readiness,
            inactive_ttl_seconds: preserved
                .map(|p| p.inactive_ttl_seconds)
                .unwrap_or(inactive_ttl_seconds),
            turn_loop_config: match preserved {
                // Start from the existing turn-loop config and override ONLY
                // the model routing, so paracrine budgets, context-window
                // overrides, and loop scripts survive a `/model` swap too.
                Some(prev) => ansible_mesh_core::graph::TurnLoopConfig {
                    model_bindings: resolved_model_bindings,
                    fallback_tiers: resolved_fallback_tiers,
                    ..prev.turn_loop_config.clone()
                },
                None => ansible_mesh_core::graph::TurnLoopConfig {
                    iteration_cap,
                    approval_policy,
                    model_profile,
                    model_bindings: resolved_model_bindings,
                    context_window_policy,
                    loop_script: None,
                    fallback_tiers: resolved_fallback_tiers,
                    paracrine_hop_budget: None,
                    paracrine_chain_budget_secs: None,
                    context_window: None,
                    plan_continuation_budget: None,
                },
            },
            home_node: preserved.and_then(|p| p.home_node.clone()),
            placement_updated_unix: preserved.map(|p| p.placement_updated_unix).unwrap_or(0),
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

        let breaking_change = previous.as_ref().is_some_and(|existing| {
            existing.guest_id != record.guest_id
                || existing.toolset_profile != record.toolset_profile
                || existing.role_manifest != record.role_manifest
                || existing.turn_loop_config.model_profile != record.turn_loop_config.model_profile
                || existing.turn_loop_config.model_bindings
                    != record.turn_loop_config.model_bindings
        });

        if is_new_role || breaking_change {
            if let Err(err) = graph.set_role_incarnation_readiness(
                &agent_id,
                &role_name,
                RoleReadinessState::Configured,
            ) {
                warn!(
                    "Failed to reset readiness for role [{}] before materialization: {}",
                    role_name, err
                );
            }
            let manifest = match Self::role_worker_manifest(graph, local_node_id, &record) {
                Ok(manifest) => manifest,
                Err(err) => {
                    return IpcResponse::error(
                        "configure_role",
                        "ROLE_COMPONENT_CONFIG_FAILED",
                        err.to_string(),
                    );
                }
            };
            match Self::handle_register_component(graph, materialization_requester, manifest).await
            {
                IpcResponse::ComponentRegistered { .. }
                | IpcResponse::Standard { ok: true, .. } => {}
                IpcResponse::Error(msg) => {
                    return IpcResponse::error(
                        "configure_role",
                        "ROLE_COMPONENT_REGISTER_FAILED",
                        msg,
                    );
                }
                other => {
                    return IpcResponse::error(
                        "configure_role",
                        "ROLE_COMPONENT_REGISTER_FAILED",
                        format!("unexpected role worker registration response: {other:?}"),
                    );
                }
            }
            if breaking_change {
                match Self::handle_restart_component(
                    graph,
                    materialization_requester,
                    local_node_id,
                    &record.guest_id,
                    // Deliberate role reconfiguration (breaking change) — an operator
                    // action, never budget-limited.
                    philotic_client::RestartReason::Operator,
                )
                .await
                {
                    IpcResponse::Standard { ok: true, .. } => {}
                    IpcResponse::Error(msg) => {
                        return IpcResponse::error(
                            "configure_role",
                            "ROLE_COMPONENT_RESTART_FAILED",
                            msg,
                        );
                    }
                    other => {
                        return IpcResponse::error(
                            "configure_role",
                            "ROLE_COMPONENT_RESTART_FAILED",
                            format!("unexpected role worker restart response: {other:?}"),
                        );
                    }
                }
            }
            if let Err(err) = Self::ensure_role_materialized(
                graph,
                inboxes,
                materialization_requester,
                local_node_id,
                &agent_id,
                &role_name,
            )
            .await
            {
                warn!(
                    "Role [{}] was configured but eager materialization failed: {}",
                    role_name, err
                );
            }
        }

        IpcResponse::ConfigureRoleOk { role_name }
    }

    /// Deliver a mesh event envelope to local inbox subscribers, parking + materializing
    /// the role philote when `delivery_target_guest_id` is set and no subscriber is
    /// currently connected. Called from the mesh inbox loop where the full hotel context
    /// is available.
    ///
    /// This is the *only* envelope delivery entry point. A park-less twin
    /// (`deliver_event_envelope`) used to exist alongside it; it had no remaining callers
    /// and lacked both the target-node guard and the park path, so it was retired rather
    /// than left as a footgun.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn deliver_event_envelope_or_park(
        inboxes: &InboxRegistry,
        event: &EventEnvelope,
        operator_surface_tx: Option<&mpsc::Sender<String>>,
        graph: &GraphDomain,
        local_node_id: &str,
        parked_inbound: &Arc<Mutex<HashMap<String, Vec<ParkedInboundTask>>>>,
        mat_req: Option<&dyn GuestMaterializationRequester>,
        delivery_claims: &DeliveryClaimRegistry,
    ) -> bool {
        // An event explicitly addressed to a different node arrived in this hotel's mesh
        // inbox (gossiped/relayed batch). It is not ours to deliver or park here — doing so
        // previously caused a hotel to try materializing a remote hotel's infrastructure
        // guest (e.g. another hotel's life-graph-runner) as a dormant role incarnation. That
        // can never succeed since such guest_ids have no role_incarnation record, leaving the
        // task permanently parked until the turn watchdog timed it out ~90s later.
        if let Some(target_node) = event.target_node_id.as_deref() {
            if target_node != local_node_id {
                return false;
            }
        }
        match (&event.kind, &event.target_agent_id, &event.payload) {
            (
                EventKind::TaskInvoke | EventKind::TaskResult,
                Some(target_role),
                EventPayload::Inline { data },
            ) => {
                // Single-delivery ownership: if another consumer (e.g. CronTicker::fire's
                // direct delivery, or an earlier arrival of this same envelope in a
                // retransmitted mesh batch) already claimed this event, it is not ours
                // to deliver or park — doing so raced non-deterministically against the
                // cron fire path (session-18 double-consumer finding).
                if !claim_delivery(delivery_claims, event.event_id) {
                    info!(
                        event_id = %event.event_id,
                        target_role = target_role.as_str(),
                        "Skipping event delivery: already claimed by another consumer."
                    );
                    return true;
                }
                if target_role == philotic_client::OPERATOR_SURFACE_QUERY_ROLE {
                    if let Some(tx) = operator_surface_tx {
                        let _ = tx.try_send(data.clone()).ok();
                        return true;
                    }
                }
                // Muninn-cluster single-writer routing: a lobe hotel forwarded
                // a fleet-shared-vault memory write here because this hotel
                // owns the cluster PRIMARY (`MuninnConfig::shared_write_route`).
                // Applied in-process — no guest ever subscribes this role
                // (same interception pattern as the operator surface above).
                // Idempotent per {vault}:{concept}, so a redelivered envelope
                // reinforces rather than duplicates.
                if target_role == philotic_client::MEMORY_WRITE_FORWARD_ROLE {
                    match crate::memory::apply_forwarded_write(graph, data).await {
                        Ok(engram_id) => {
                            info!(
                                event_id = %event.event_id,
                                source_node = %event.source_node_id,
                                engram_id = %engram_id,
                                "memory.write_forward applied to cluster primary"
                            );
                        }
                        Err(err) => {
                            warn!(
                                event_id = %event.event_id,
                                source_node = %event.source_node_id,
                                error = %err,
                                "memory.write_forward FAILED to apply — forwarded memory write not stored on primary"
                            );
                        }
                    }
                    return true;
                }
                let target_guest_id: Option<String> =
                    serde_json::from_str::<serde_json::Value>(data)
                        .ok()
                        .and_then(|v| {
                            v.get("delivery_target_guest_id")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_string)
                        });

                let is_subscribed = {
                    let guard = inboxes.lock().await;
                    let role_subs = guard.get(target_role.as_str()).cloned().unwrap_or_default();
                    match &target_guest_id {
                        Some(g) => role_subs.iter().any(|s| s.guest_id == g.as_str()),
                        None => !role_subs.is_empty(),
                    }
                };

                // A task addressed to an AGENT id rather than a role
                // (`delegate.to_peer` sets `target_agent_id` to the peer agent,
                // e.g. "agent-bjork-01"). No guest subscribes a role by that
                // name; the agent's orchestrator incarnation subscribes `agent`
                // (and `role:<agent>:orchestrator`) and handles `peer.delegate`.
                // Live 2026-09-16 18:40 UTC (DEF-151): Beacon's delegation
                // crossed the mesh in one second and was dropped here with "no
                // subscriber for role 'agent-bjork-01'".
                if !is_subscribed && target_guest_id.is_none() {
                    if let Some((role, guest)) =
                        agent_addressed_subscriber(inboxes, target_role.as_str()).await
                    {
                        info!(
                            event_id = %event.event_id,
                            agent_id = target_role.as_str(),
                            role = %role,
                            guest_id = %guest,
                            "Cross-hotel task addressed to an agent id: delivering to its orchestrator"
                        );
                        Self::deliver_inbound_task(
                            inboxes,
                            &event.source_node_id,
                            &role,
                            Some(guest.as_str()),
                            event.event_id,
                            data.clone(),
                        )
                        .await;
                        return true;
                    }
                }

                if is_subscribed {
                    // Register the active incarnation so that model_responses for this
                    // session route back to the correct specialist philote rather than
                    // falling through to the orchestrator. Without this, a cross-hotel
                    // paracrine turn's model_response is rerouted to bjork/orchestrator
                    // because the session's active_incarnation_id was never set via mesh.
                    if let (Some(guest_id), Some(session_id)) = (
                        &target_guest_id,
                        serde_json::from_str::<serde_json::Value>(data)
                            .ok()
                            .and_then(|v| {
                                v.get("session_id")
                                    .and_then(serde_json::Value::as_str)
                                    .map(str::to_string)
                            }),
                    ) {
                        if let Err(err) =
                            Self::update_session_active_incarnation(graph, &session_id, guest_id)
                        {
                            warn!(
                                "deliver_event_envelope_or_park: session activation skipped [{}]: {}",
                                session_id, err
                            );
                        }
                    }
                    Self::deliver_inbound_task(
                        inboxes,
                        &event.source_node_id,
                        target_role,
                        target_guest_id.as_deref(),
                        event.event_id,
                        data.clone(),
                    )
                    .await;
                } else if let Some(ref agent_guest_id) = target_guest_id {
                    Self::park_and_materialize(
                        graph,
                        inboxes,
                        parked_inbound,
                        mat_req,
                        local_node_id,
                        &event.source_node_id,
                        event.event_id,
                        data.clone(),
                        ParkTarget::CrossHotelGuest { agent_guest_id },
                    )
                    .await;
                } else {
                    // Same rescue as the local EmitTask path: a governed task
                    // arriving over the mesh for a runner role this hotel seeds
                    // (egress-http-runner dormant after a deploy, or any runner
                    // dead after a hotel crash) revives the guest and parks the
                    // task instead of dropping it. This is the exit-hotel side
                    // of the black-hole: mac-jane's catalog syncs prefer
                    // vps-jane as egress exit, and every one of them died here
                    // whenever vps's runner was down — the caller only ever saw
                    // its own deadline expire.
                    let rescued = Self::rescue_unserved_role_task(
                        graph,
                        parked_inbound,
                        mat_req,
                        local_node_id,
                        target_role,
                        &event.source_node_id,
                        event.event_id,
                        data,
                    )
                    .await;
                    if rescued.is_none() {
                        warn!(
                            "Cross-hotel task {}: no subscriber for role '{}', no specific guest; task dropped.",
                            event.event_id, target_role
                        );
                    }
                }
                true
            }
            _ => false,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_handoff_to_role(
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        dispatcher_tx: &mpsc::Sender<LedgerCommand>,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        local_node_id: &str,
        current_identity: Option<&GuestIdentity>,
        session_id: String,
        role_name: String,
        handoff_bundle: HandoffBundle,
    ) -> IpcResponse {
        let Some(identity) = current_identity else {
            return IpcResponse::error(
                "handoff_to_role",
                "HANDOFF_UNREGISTERED",
                "guest must register before requesting a handoff",
            );
        };
        if !Self::is_agent_handoff_caller(graph, identity) {
            return IpcResponse::error(
                "handoff_to_role",
                "HANDOFF_FORBIDDEN",
                "only agent guests may initiate role handoff",
            );
        }

        let target_role = match Self::resolve_role_incarnation(graph, &session_id, &role_name) {
            Ok(role_record) => role_record,
            Err(err) => {
                return IpcResponse::error(
                    "handoff_to_role",
                    "HANDOFF_ROLE_UNKNOWN",
                    err.to_string(),
                );
            }
        };

        // Remote role: dispatch over mesh to the role's home hotel. A home
        // stored as the bare hotel name (records that predate DEF-124, or any
        // seed that still writes the name) must be resolved to its node_id
        // first: live 2026-09-15 14:44 UTC bjork's handoff to the
        // theoretician — home_node "mac-jane", on mac-jane-aiua-01 — was
        // dispatched "remote" to a peer that does not exist, and the
        // operator's thread went silent (DEF-132).
        if let Some(home_node) = target_role.home_node.as_deref().map(|home| {
            Self::resolve_hotel_node_id(graph, home).unwrap_or_else(|| home.to_string())
        }) {
            if home_node != local_node_id {
                let toolset_record = graph
                    .get_toolset_profile(&target_role.toolset_profile)
                    .ok()
                    .flatten();
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let task_id = Uuid::new_v4();
                let event = EventEnvelope {
                    event_id: task_id,
                    seq: 0,
                    source_node_id: local_node_id.to_string(),
                    target_node_id: Some(home_node.clone()),
                    source_agent_id: identity.guest_id.clone(),
                    target_agent_id: Some(target_role.routing_role()),
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
                            "role_record": target_role,
                            "toolset_record": toolset_record,
                        })
                        .to_string(),
                    },
                    trace: vec![],
                };
                let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(event)).await;
                info!(
                    "Dispatched remote handoff for role '{}' to home_node '{}'",
                    role_name, home_node
                );
                // Update local session active_incarnation_id so subsequent messages
                // on this hotel route cross-hotel to the remote role guest.
                if let Ok(Some(mut session_rec)) = graph.get_session(&session_id) {
                    session_rec.active_incarnation_id = Some(target_role.guest_id.clone());
                    session_rec.updated_at = unix_ts();
                    let _ = graph.upsert_session(&session_rec);
                }
                return IpcResponse::HandoffAck {
                    handoff_guest_id: target_role.guest_id,
                    became_active: true,
                };
            }
        }

        let readiness = match Self::ensure_role_materialized(
            graph,
            inboxes,
            materialization_requester,
            local_node_id,
            &target_role.agent_id,
            // Use the resolved record's canonical role_name, not the raw
            // caller-supplied `role_name` — resolve_role_incarnation above
            // may have matched it case-insensitively (e.g. "chronos" against
            // a role stored as "Chronos"), and ensure_role_materialized does
            // its own exact-match lookup that would otherwise re-fail here.
            &target_role.role_name,
        )
        .await
        {
            Ok(readiness) => readiness,
            Err(err) => {
                return IpcResponse::error(
                    "handoff_to_role",
                    "HANDOFF_MATERIALIZATION_FAILED",
                    err.to_string(),
                );
            }
        };
        if matches!(
            readiness,
            RoleReadinessState::Configured
                | RoleReadinessState::Materializing
                | RoleReadinessState::Materialized
        ) {
            return IpcResponse::HandoffPending {
                role_name,
                readiness: readiness.as_str().into(),
                retry_after_ms: Some(250),
            };
        }
        let target_guest_id = target_role.guest_id.clone();
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

        let agent_id = match graph.get_session(&session_id) {
            Ok(Some(session)) => session.primary_agent_id,
            Ok(None) => None,
            Err(err) => {
                return IpcResponse::error(
                    "handoff_to_role",
                    "HANDOFF_SESSION_LOOKUP_FAILED",
                    err.to_string(),
                );
            }
        };
        let authority_hotel = agent_id
            .as_deref()
            .and_then(|agent_id| lookup_agent_authority_hotel(graph, agent_id));
        let task_json = serde_json::json!({
            "action": "handoff_bundle",
            "agent_id": agent_id,
            "authority_hotel": authority_hotel,
            "session_id": session_id,
            "handoff_bundle": handoff_bundle,
        })
        .to_string();
        let task_json = attach_agent_graph_snapshot(&task_json, agent_id.as_deref(), local_node_id);

        match Self::deliver_live_guest_task(
            graph,
            inboxes,
            local_node_id,
            &target_role.routing_role(),
            &target_guest_id,
            task_id,
            task_json,
            Some(session_id),
        )
        .await
        {
            Ok(true) => {
                // Single-active invariant: promoting this role demotes any
                // sibling incarnation of the same agent that is still active,
                // so two roles can never both be ActiveInSession at once.
                if let Err(err) = graph
                    .promote_role_incarnation_active(&target_role.agent_id, &target_role.role_name)
                {
                    warn!(
                        "Failed to mark role [{}] active in session: {}",
                        target_role.role_name, err
                    );
                }
                IpcResponse::HandoffAck {
                    handoff_guest_id: target_guest_id,
                    became_active: true,
                }
            }
            Ok(false) => {
                let _ = graph.set_role_incarnation_readiness(
                    &target_role.agent_id,
                    &target_role.role_name,
                    RoleReadinessState::Materializing,
                );
                IpcResponse::HandoffPending {
                    role_name,
                    readiness: RoleReadinessState::Materializing.as_str().into(),
                    retry_after_ms: Some(250),
                }
            }
            Err(err) => IpcResponse::error(
                "handoff_to_role",
                "HANDOFF_DELIVERY_FAILED",
                err.to_string(),
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_handoff_back(
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        dispatcher_tx: &mpsc::Sender<LedgerCommand>,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        local_node_id: &str,
        current_identity: Option<&GuestIdentity>,
        session_id: String,
        summary: String,
        return_to: Option<String>,
    ) -> IpcResponse {
        let Some(identity) = current_identity else {
            return IpcResponse::error(
                "handoff_back",
                "HANDOFF_UNREGISTERED",
                "guest must register before handing back",
            );
        };
        if !Self::is_agent_handoff_caller(graph, identity) {
            return IpcResponse::error(
                "handoff_back",
                "HANDOFF_FORBIDDEN",
                "only agent guests may initiate role handoff",
            );
        }
        let target_role = return_to.unwrap_or_else(|| "orchestrator".into());
        let target_role_record =
            match Self::resolve_role_incarnation(graph, &session_id, &target_role) {
                Ok(role_record) => role_record,
                Err(err) => {
                    return IpcResponse::error(
                        "handoff_back",
                        "HANDOFF_ROLE_UNKNOWN",
                        err.to_string(),
                    );
                }
            };
        let readiness = match Self::ensure_role_materialized(
            graph,
            inboxes,
            materialization_requester,
            local_node_id,
            &target_role_record.agent_id,
            &target_role_record.role_name,
        )
        .await
        {
            Ok(readiness) => readiness,
            Err(err) => {
                return IpcResponse::error(
                    "handoff_back",
                    "HANDOFF_BACK_MATERIALIZATION_FAILED",
                    err.to_string(),
                );
            }
        };
        if matches!(
            readiness,
            RoleReadinessState::Configured
                | RoleReadinessState::Materializing
                | RoleReadinessState::Materialized
        ) {
            return IpcResponse::HandoffPending {
                role_name: target_role,
                readiness: readiness.as_str().into(),
                retry_after_ms: Some(250),
            };
        }
        let target_guest_id = target_role_record.guest_id.clone();
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

        let agent_id = match graph.get_session(&session_id) {
            Ok(Some(session)) => session.primary_agent_id,
            Ok(None) => None,
            Err(err) => {
                return IpcResponse::error(
                    "handoff_back",
                    "HANDOFF_SESSION_LOOKUP_FAILED",
                    err.to_string(),
                );
            }
        };
        let authority_hotel = agent_id
            .as_deref()
            .and_then(|agent_id| lookup_agent_authority_hotel(graph, agent_id));
        let task_json = serde_json::json!({
            "action": "handoff_return",
            "agent_id": agent_id,
            "authority_hotel": authority_hotel,
            "session_id": session_id,
            "summary": summary,
            "from_incarnation_id": identity.guest_id,
        })
        .to_string();
        let task_json = attach_agent_graph_snapshot(&task_json, agent_id.as_deref(), local_node_id);
        match Self::deliver_live_guest_task(
            graph,
            inboxes,
            local_node_id,
            &target_role_record.routing_role(),
            &target_guest_id,
            task_id,
            task_json,
            Some(session_id),
        )
        .await
        {
            Ok(true) => {
                // Single-active invariant (see promote_role_incarnation_active):
                // returning to a role demotes any other active incarnation.
                if let Err(err) = graph.promote_role_incarnation_active(
                    &target_role_record.agent_id,
                    &target_role_record.role_name,
                ) {
                    warn!(
                        "Failed to mark return role [{}] active in session: {}",
                        target_role_record.role_name, err
                    );
                }
                IpcResponse::HandoffBackAck {
                    return_guest_id: target_guest_id,
                    became_active: true,
                }
            }
            Ok(false) => {
                let _ = graph.set_role_incarnation_readiness(
                    &target_role_record.agent_id,
                    &target_role_record.role_name,
                    RoleReadinessState::Materializing,
                );
                IpcResponse::HandoffPending {
                    role_name: target_role_record.role_name,
                    readiness: RoleReadinessState::Materializing.as_str().into(),
                    retry_after_ms: Some(250),
                }
            }
            Err(err) => {
                IpcResponse::error("handoff_back", "HANDOFF_DELIVERY_FAILED", err.to_string())
            }
        }
    }

    pub(super) fn handle_set_role_home(
        graph: &GraphDomain,
        current_identity: Option<&GuestIdentity>,
        agent_id: String,
        role_name: String,
        calling_role: String,
        target_hotel: Option<String>,
    ) -> IpcResponse {
        let Some(identity) = current_identity else {
            return IpcResponse::error(
                "set_role_home",
                "SET_ROLE_HOME_UNREGISTERED",
                "guest must register before calling set_role_home",
            );
        };
        if !Self::is_agent_handoff_caller(graph, identity) {
            return IpcResponse::error(
                "set_role_home",
                "SET_ROLE_HOME_FORBIDDEN",
                "only agent guests may call set_role_home",
            );
        }

        // Only roles with operational admin authority may move roles.
        let calling_role_record = graph.get_role_incarnation(&agent_id, &calling_role);
        let is_admin = calling_role_record
            .ok()
            .flatten()
            .map(|r| r.has_operational_admin_authority())
            .unwrap_or(false);
        if !is_admin {
            return IpcResponse::error(
                "set_role_home",
                "SET_ROLE_HOME_FORBIDDEN",
                format!(
                    "role '{}' does not have authority to set home_node for other roles",
                    calling_role
                ),
            );
        }

        Self::perform_set_role_home(graph, agent_id, role_name, calling_role, target_hotel)
    }

    /// Core "move a role incarnation's `home_node` to `target_hotel`" logic,
    /// shared by `role.set_home`'s direct IPC dispatch (which gates caller
    /// identity/authority itself before calling this) and the Relocation
    /// Ceremony's SWITCH phase (which gates once at INTENT for the whole
    /// ceremony — see [`Self::handle_relocate_hotel`]). Does NOT check
    /// caller authority; callers must gate before invoking it.
    pub(super) fn perform_set_role_home(
        graph: &GraphDomain,
        agent_id: String,
        role_name: String,
        calling_role: String,
        target_hotel: Option<String>,
    ) -> IpcResponse {
        let mut record = match graph.get_role_incarnation(&agent_id, &role_name) {
            Ok(Some(r)) => r,
            Ok(None) => {
                return IpcResponse::error(
                    "set_role_home",
                    "SET_ROLE_HOME_UNKNOWN",
                    format!("role '{}' not found for agent '{}'", role_name, agent_id),
                );
            }
            Err(err) => {
                return IpcResponse::error(
                    "set_role_home",
                    "SET_ROLE_HOME_DB_ERROR",
                    err.to_string(),
                );
            }
        };

        // Resolve a bare hotel_name (the documented example, e.g. "vps-jane")
        // or an already-canonical node_id (e.g. "vps-jane-aiua-01") to the
        // node_id every routing comparison actually keys on (DEF-124).
        let resolved_target = match target_hotel.as_deref() {
            None => None,
            Some(hotel_ref) => match Self::resolve_hotel_node_id(graph, hotel_ref) {
                Some(node_id) => Some(node_id),
                None => {
                    return IpcResponse::error(
                        "set_role_home",
                        "SET_ROLE_HOME_UNKNOWN_HOTEL",
                        format!("no known hotel matches '{}'", hotel_ref),
                    );
                }
            },
        };

        record.home_node = resolved_target.clone();
        record.placement_updated_unix = ansible_mesh_core::graph::placement_stamp_now();
        if let Err(err) = graph.upsert_role_incarnation(&record) {
            return IpcResponse::error(
                "set_role_home",
                "SET_ROLE_HOME_PERSIST_FAILED",
                err.to_string(),
            );
        }

        info!(
            "Role '{}' (agent '{}') home_node set to {:?} by '{}'",
            role_name, agent_id, resolved_target, calling_role
        );
        IpcResponse::RoleHomeSet {
            role_name,
            home_node: resolved_target,
        }
    }

    /// Core "move a transport's active home to `target_hotel`" logic, shared
    /// by `transport.set_home`'s direct IPC dispatch (which gates caller
    /// identity/authority itself before calling this) and the Relocation
    /// Ceremony's SWITCH phase (which gates once at INTENT for the whole
    /// ceremony — see [`Self::handle_relocate_hotel`]). Does NOT check
    /// caller authority; callers must gate before invoking it.
    pub(super) fn perform_set_transport_home(
        graph: &GraphDomain,
        agent_id: String,
        transport: String,
        resource_ref: String,
        calling_role: String,
        target_hotel: String,
        standby_hotels: Vec<String>,
    ) -> IpcResponse {
        if graph.get_agent_identity(&agent_id).ok().flatten().is_none() {
            return IpcResponse::error(
                "set_transport_home",
                "SET_TRANSPORT_HOME_AGENT_UNKNOWN",
                format!("agent '{}' not found", agent_id),
            );
        }

        // Resolve a bare hotel_name (documented example, e.g. "vps-jane")
        // or an already-canonical node_id to the node_id every routing
        // comparison actually keys on (DEF-124).
        let Some(target_hotel) = Self::resolve_hotel_node_id(graph, &target_hotel) else {
            return IpcResponse::error(
                "set_transport_home",
                "SET_TRANSPORT_HOME_UNKNOWN_HOTEL",
                format!("no known hotel matches '{}'", target_hotel),
            );
        };
        let mut resolved_standby_hotels = Vec::with_capacity(standby_hotels.len());
        for hotel_ref in &standby_hotels {
            let Some(node_id) = Self::resolve_hotel_node_id(graph, hotel_ref) else {
                return IpcResponse::error(
                    "set_transport_home",
                    "SET_TRANSPORT_HOME_UNKNOWN_HOTEL",
                    format!("no known hotel matches standby '{}'", hotel_ref),
                );
            };
            resolved_standby_hotels.push(node_id);
        }
        let standby_hotels = resolved_standby_hotels;

        let home = MembraneTransportHomeRecord {
            agent_id: agent_id.clone(),
            transport: transport.clone(),
            resource_ref: resource_ref.clone(),
            active_home_hotel: target_hotel.clone(),
            standby_hotels: standby_hotels.clone(),
            managed_by_role: calling_role.clone(),
            lease_type: match transport.as_str() {
                "telegram" => "telegram_poll".to_string(),
                "discord" => "discord_gateway".to_string(),
                other => format!("{other}_transport"),
            },
            failover_policy: "manual-or-explicit-delegation".to_string(),
            status: MembraneTransportHomeStatus::Active,
            updated_unix: ansible_mesh_core::graph::placement_stamp_now(),
        };

        if let Err(err) = graph.upsert_membrane_transport_home(&home) {
            return IpcResponse::error(
                "set_transport_home",
                "SET_TRANSPORT_HOME_PERSIST_FAILED",
                err.to_string(),
            );
        }

        info!(
            "Transport home for agent '{}' transport '{}' resource '{}' set to '{}' by '{}'",
            agent_id, transport, resource_ref, target_hotel, calling_role
        );
        IpcResponse::TransportHomeSet {
            agent_id,
            transport,
            resource_ref,
            active_home_hotel: target_hotel,
            standby_hotels,
        }
    }

    /// Relocation Ceremony R3 (STANDBY phase): dispatch a `MaterializeRequest`
    /// mesh event asking `target_hotel` to pre-warm `role_name`'s process,
    /// without touching `home_node` — that stays [`Self::handle_set_role_home`]'s
    /// job alone, so STANDBY can complete well before SWITCH. Gated identically
    /// to `set_role_home`/`set_transport_home` (G9): only operational-admin
    /// roles may request cross-hotel materialization.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_materialize_request(
        graph: &GraphDomain,
        dispatcher_tx: &mpsc::Sender<LedgerCommand>,
        local_node_id: &str,
        current_identity: Option<&GuestIdentity>,
        agent_id: String,
        role_name: String,
        calling_role: String,
        target_hotel: String,
        dry_run: bool,
    ) -> IpcResponse {
        let Some(identity) = current_identity else {
            return IpcResponse::error(
                "materialize_request",
                "MATERIALIZE_REQUEST_UNREGISTERED",
                "guest must register before calling hotel.materialize_request",
            );
        };
        if !Self::is_agent_handoff_caller(graph, identity) {
            return IpcResponse::error(
                "materialize_request",
                "MATERIALIZE_REQUEST_FORBIDDEN",
                "only agent guests may call hotel.materialize_request",
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
                "materialize_request",
                "MATERIALIZE_REQUEST_FORBIDDEN",
                format!(
                    "role '{}' does not have authority to request cross-hotel materialization",
                    calling_role
                ),
            );
        }

        Self::dispatch_materialize_request_core(
            graph,
            dispatcher_tx,
            local_node_id,
            identity.guest_id.clone(),
            agent_id,
            role_name,
            calling_role,
            target_hotel,
            dry_run,
        )
        .await
    }

    /// Core STANDBY dispatch: resolve `target_hotel`, load the role/toolset
    /// records, and send the `MaterializeRequest` mesh event. Shared by
    /// [`Self::handle_materialize_request`] (which gates the caller before
    /// calling this) and the Relocation Ceremony's FEASIBILITY/STANDBY
    /// phases (gated once at ceremony INTENT — see
    /// [`Self::handle_relocate_hotel`]). Does NOT check caller authority.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn dispatch_materialize_request_core(
        graph: &GraphDomain,
        dispatcher_tx: &mpsc::Sender<LedgerCommand>,
        local_node_id: &str,
        requester_guest_id: String,
        agent_id: String,
        role_name: String,
        calling_role: String,
        target_hotel: String,
        dry_run: bool,
    ) -> IpcResponse {
        // Resolve a bare hotel_name (the documented example, e.g. "vps-jane")
        // or an already-canonical node_id to the node_id every routing
        // comparison and mesh envelope actually keys on (DEF-124).
        let Some(target_hotel) = Self::resolve_hotel_node_id(graph, &target_hotel) else {
            return IpcResponse::error(
                "materialize_request",
                "MATERIALIZE_REQUEST_UNKNOWN_HOTEL",
                format!("no known hotel matches '{}'", target_hotel),
            );
        };

        if target_hotel == local_node_id {
            return IpcResponse::error(
                "materialize_request",
                "MATERIALIZE_REQUEST_LOCAL_TARGET",
                "target_hotel is this hotel — the role is already local; nothing to pre-warm remotely",
            );
        }

        let record = match graph.get_role_incarnation(&agent_id, &role_name) {
            Ok(Some(r)) => r,
            Ok(None) => {
                return IpcResponse::error(
                    "materialize_request",
                    "MATERIALIZE_REQUEST_ROLE_UNKNOWN",
                    format!("role '{}' not found for agent '{}'", role_name, agent_id),
                );
            }
            Err(err) => {
                return IpcResponse::error(
                    "materialize_request",
                    "MATERIALIZE_REQUEST_DB_ERROR",
                    err.to_string(),
                );
            }
        };
        let toolset_record = graph
            .get_toolset_profile(&record.toolset_profile)
            .ok()
            .flatten();

        let request_id = Uuid::new_v4();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let event = EventEnvelope {
            event_id: request_id,
            seq: 0,
            source_node_id: local_node_id.to_string(),
            target_node_id: Some(target_hotel.clone()),
            source_agent_id: requester_guest_id,
            target_agent_id: None,
            kind: EventKind::MaterializeRequest,
            corr_id: request_id.to_string(),
            attempt: 0,
            created_at: ts,
            expires_at: None,
            payload: EventPayload::Inline {
                data: serde_json::json!({
                    "request_id": request_id.to_string(),
                    "role_record": record,
                    "toolset_record": toolset_record,
                    "dry_run": dry_run,
                    "requester_build_version": env!("CARGO_PKG_VERSION"),
                })
                .to_string(),
            },
            trace: vec![],
        };
        let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(event)).await;

        info!(
            "Materialize request [{}] dispatched (dry_run={}): role '{}' (agent '{}') -> hotel '{}', requested by '{}'",
            request_id, dry_run, role_name, agent_id, target_hotel, calling_role
        );

        IpcResponse::MaterializeRequested {
            materialize_requested: true,
            request_id: request_id.to_string(),
            role_name,
            target_hotel,
        }
    }

    /// Poll the outcome of a prior [`Self::handle_materialize_request`]. `None`
    /// fields mean the target hotel's `MaterializeReady` reply has not landed
    /// yet — a pending request, not an error.
    pub(super) fn handle_materialize_status(
        graph: &GraphDomain,
        request_id: String,
    ) -> IpcResponse {
        let key = format!("materialize_ready:{request_id}");
        match graph.get_config_value(&key) {
            Ok(Some(raw)) => {
                let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
                IpcResponse::MaterializeStatus {
                    materialize_status: true,
                    request_id,
                    ok: parsed.get("ok").and_then(|v| v.as_bool()),
                    readiness: parsed
                        .get("readiness")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    error: parsed
                        .get("error")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                }
            }
            Ok(None) => IpcResponse::MaterializeStatus {
                materialize_status: true,
                request_id,
                ok: None,
                readiness: None,
                error: None,
            },
            Err(err) => IpcResponse::error(
                "materialize_status",
                "MATERIALIZE_STATUS_DB_ERROR",
                err.to_string(),
            ),
        }
    }

    /// Relocation Ceremony R4 (Feasibility and placement): does `command`
    /// resolve to a real, executable file given THIS process's current
    /// `PHILOTIC_BIN_DIR`/`PATH`? Mirrors `LocalProcessMaterializer::spawn_guest`'s
    /// resolution rule (`guest_manager.rs`) exactly, as a side-effect-free
    /// pre-check rather than an actual spawn attempt — a positive result is
    /// not a guarantee (the file could still be removed or lose its execute
    /// bit before a real spawn), but a negative result is a reliable decline.
    fn resolve_binary_feasible(command: &str) -> bool {
        let path = std::path::Path::new(command);
        if path.is_absolute() {
            return path.is_file();
        }
        if let Ok(bin_dir) = std::env::var("PHILOTIC_BIN_DIR") {
            let candidate = std::path::Path::new(bin_dir.trim_end_matches('/')).join(command);
            return candidate.is_file();
        }
        // Bare command relies on PATH at spawn time (dev mode) — search it
        // the same way a shell would.
        std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(command).is_file()))
            .unwrap_or(false)
    }

    /// Relocation Ceremony R4 (Feasibility and placement): evaluate whether
    /// THIS hotel (`hotel_name`, the target) can actually host `role_record`
    /// right now. Returns decline reasons — empty means feasible. Every
    /// check runs against LOCAL truth (this hotel's own filesystem, guest
    /// table, build version): binary presence isn't gossiped at all, and
    /// controller liveness is more trustworthy read live than from a
    /// possibly-stale gossiped snapshot (that snapshot is what
    /// `best_place_to_run_view`'s ranking uses instead, for candidates that
    /// haven't been asked yet).
    ///
    /// Secret-ref presence — the proposal's third feasibility check — is a
    /// deliberate no-op here: neither `RoleIncarnationRecord` nor
    /// `ToolsetProfileRecord` carries a structured `secret_ref`. That
    /// concept lives on integration/OIDC credential bindings
    /// (`ansible_mesh_core::integration`), which belong to the higher-risk
    /// membrane/integration component classes the Ceremony proposal scopes
    /// separately (medium/high tier) — not this low-tier role move.
    pub(super) fn evaluate_role_relocation_feasibility(
        graph: &GraphDomain,
        hotel_name: &str,
        role_record: &ansible_mesh_core::graph::RoleIncarnationRecord,
        requester_build_version: Option<&str>,
    ) -> Vec<String> {
        let mut reasons = Vec::new();

        if !Self::resolve_binary_feasible("philote") {
            reasons.push(
                "role-incarnation worker binary 'philote' does not resolve on this hotel \
                 (PHILOTIC_BIN_DIR/PATH) — likely a stale or incomplete deploy"
                    .to_string(),
            );
        }

        let active_guest_roles: std::collections::BTreeSet<String> = graph
            .list_guests(hotel_name, true)
            .map(|guests| guests.into_iter().map(|g| g.role).collect())
            .unwrap_or_default();
        let tiers: Vec<String> = if role_record.turn_loop_config.fallback_tiers.is_empty() {
            ansible_mesh_core::model_routing::DEFAULT_FALLBACK_TIERS
                .iter()
                .map(|t| t.to_string())
                .collect()
        } else {
            role_record.turn_loop_config.fallback_tiers.clone()
        };
        if let Some(primary_tier) = tiers.first() {
            if !active_guest_roles.contains(primary_tier) {
                reasons.push(format!(
                    "this role's primary model controller ('{primary_tier}') has no live guest on this hotel"
                ));
            }
        }

        if let Some(requester_version) = requester_build_version {
            let local_version = env!("CARGO_PKG_VERSION");
            if !requester_version.is_empty() && requester_version != local_version {
                reasons.push(format!(
                    "build version mismatch: requester is '{requester_version}', this hotel is '{local_version}'"
                ));
            }
        }

        reasons
    }

    /// Relocation Ceremony R6: `hotel.relocate` — the philote-facing entry
    /// point. Validates the caller, creates a [`RelocationCeremonyRecord`]
    /// at phase INTENT, and spawns a background task
    /// ([`Self::run_relocation_ceremony`]) that walks FEASIBILITY → STANDBY
    /// → CONTINUITY → SWITCH → RECONCILE → CLOSE, recording every
    /// transition so an interrupted ceremony can be found and classified on
    /// the next boot (see
    /// [`crate::service::role_materialization::scan_interrupted_relocation_ceremonies`]).
    ///
    /// Gated identically to `role.set_home`/`hotel.materialize_request` for
    /// the low tier (role incarnation alone: operational admin authority).
    /// If `include_transport` is set, the ceremony's risk tier is `High`
    /// and the gate additionally requires full admin authority (`is_admin`)
    /// — see [`ansible_mesh_core::relocation_ceremony::RelocationRiskTier::High`]'s
    /// doc comment for the scope trim this represents.
    ///
    /// Fire-and-track like `MaterializeRequest`: returns
    /// [`IpcResponse::RelocationCeremonyStarted`] as soon as the ceremony
    /// record exists and its orchestration task is spawned, not when the
    /// move completes. Poll [`Self::handle_relocate_hotel_status`] with
    /// `ceremony_id` for progress.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_relocate_hotel(
        graph: &GraphDomain,
        dispatcher_tx: &mpsc::Sender<LedgerCommand>,
        local_node_id: &str,
        current_identity: Option<&GuestIdentity>,
        agent_id: String,
        role_name: String,
        calling_role: String,
        target_hotel: String,
        include_transport: bool,
        transport: Option<String>,
        transport_resource_ref: Option<String>,
        reason: String,
    ) -> IpcResponse {
        let Some(identity) = current_identity else {
            return IpcResponse::error(
                "relocate_hotel",
                "RELOCATE_HOTEL_UNREGISTERED",
                "guest must register before calling hotel.relocate",
            );
        };
        if !Self::is_agent_handoff_caller(graph, identity) {
            return IpcResponse::error(
                "relocate_hotel",
                "RELOCATE_HOTEL_FORBIDDEN",
                "only agent guests may call hotel.relocate",
            );
        }

        let calling_record = match graph.get_role_incarnation(&agent_id, &calling_role) {
            Ok(Some(r)) => r,
            _ => {
                return IpcResponse::error(
                    "relocate_hotel",
                    "RELOCATE_HOTEL_UNKNOWN_CALLER",
                    format!(
                        "calling role '{}' not found for agent '{}'",
                        calling_role, agent_id
                    ),
                );
            }
        };
        if !calling_record.has_operational_admin_authority() {
            return IpcResponse::error(
                "relocate_hotel",
                "RELOCATE_HOTEL_FORBIDDEN",
                format!(
                    "role '{}' does not have authority to relocate hotels",
                    calling_role
                ),
            );
        }
        if include_transport && !calling_record.has_full_admin_authority() {
            return IpcResponse::error(
                "relocate_hotel",
                "RELOCATE_HOTEL_FORBIDDEN_HIGH_TIER",
                format!(
                    "role '{}' has operational admin authority but moving a transport atomically \
                     is a High-tier ceremony requiring full admin authority (is_admin)",
                    calling_role
                ),
            );
        }
        if include_transport && (transport.is_none() || transport_resource_ref.is_none()) {
            return IpcResponse::error(
                "relocate_hotel",
                "RELOCATE_HOTEL_TRANSPORT_ARGS_MISSING",
                "include_transport requires both transport and transport_resource_ref",
            );
        }

        let Some(target_node_id) = Self::resolve_hotel_node_id(graph, &target_hotel) else {
            return IpcResponse::error(
                "relocate_hotel",
                "RELOCATE_HOTEL_UNKNOWN_HOTEL",
                format!("no known hotel matches '{}'", target_hotel),
            );
        };
        if target_node_id == local_node_id {
            return IpcResponse::error(
                "relocate_hotel",
                "RELOCATE_HOTEL_LOCAL_TARGET",
                "target_hotel is this hotel; nothing to relocate",
            );
        }

        if graph
            .get_role_incarnation(&agent_id, &role_name)
            .ok()
            .flatten()
            .is_none()
        {
            return IpcResponse::error(
                "relocate_hotel",
                "RELOCATE_HOTEL_UNKNOWN_ROLE",
                format!("role '{}' not found for agent '{}'", role_name, agent_id),
            );
        }

        let ceremony_id = format!("relocate-{}", Uuid::new_v4());
        let now = unix_ts();
        let ceremony = RelocationCeremonyRecord::new(
            ceremony_id.clone(),
            agent_id.clone(),
            role_name.clone(),
            local_node_id.to_string(),
            target_node_id.clone(),
            include_transport,
            transport,
            transport_resource_ref,
            calling_role.clone(),
            reason,
            now,
        );
        if let Err(err) = graph.upsert_relocation_ceremony(&ceremony) {
            return IpcResponse::error(
                "relocate_hotel",
                "RELOCATE_HOTEL_PERSIST_FAILED",
                err.to_string(),
            );
        }

        info!(
            "Relocation ceremony [{}] started: role '{}' (agent '{}') {} -> {}, include_transport={}, requested by '{}'",
            ceremony_id,
            role_name,
            agent_id,
            local_node_id,
            target_node_id,
            include_transport,
            calling_role
        );

        let graph_bg = graph.clone();
        let dispatcher_bg = dispatcher_tx.clone();
        let local_node_bg = local_node_id.to_string();
        let ceremony_id_bg = ceremony_id.clone();
        tokio::spawn(async move {
            Self::run_relocation_ceremony(
                &graph_bg,
                &dispatcher_bg,
                &local_node_bg,
                ceremony_id_bg,
            )
            .await;
        });

        IpcResponse::RelocationCeremonyStarted {
            relocation_ceremony_started: true,
            ceremony_id,
            role_name,
            target_hotel: target_node_id,
        }
    }

    /// Bounded poll of the `materialize_ready:{request_id}` config blob
    /// [`Self::dispatch_materialize_request_core`] writes into. 250ms ×
    /// `max_attempts`, mirroring the cadence
    /// `handle_remote_materialize_request` already uses target-side for the
    /// same wait shape. Returns `None` on timeout (still pending).
    async fn poll_materialize_ready(
        graph: &GraphDomain,
        request_id: &str,
        max_attempts: u32,
    ) -> Option<serde_json::Value> {
        let key = format!("materialize_ready:{request_id}");
        for _ in 0..max_attempts {
            if let Ok(Some(raw)) = graph.get_config_value(&key) {
                return Some(serde_json::from_str(&raw).unwrap_or_default());
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        None
    }

    /// Relocation Ceremony R5: CONTINUITY. Export the moving role's session
    /// checkpoints, session rows and the agent identity, ship them to the
    /// target inline over the signed execution plane, and wait for its ack.
    ///
    /// Advances the ceremony to CONTINUITY and records the outcome in the
    /// phase note. A target that did not advertise `supports_continuity` in
    /// its STANDBY reply predates R5: the move runs degraded, as before,
    /// rather than send it an event kind it cannot parse. `Err` is a reason
    /// to roll back — CONTINUITY is still pre-commitment.
    async fn transfer_continuity(
        graph: &GraphDomain,
        dispatcher_tx: &mpsc::Sender<LedgerCommand>,
        local_node_id: &str,
        ceremony: &mut RelocationCeremonyRecord,
        standby_ready: &serde_json::Value,
    ) -> Result<(), String> {
        let now = unix_ts();
        if standby_ready
            .get("supports_continuity")
            .and_then(|v| v.as_bool())
            != Some(true)
        {
            ceremony.advance(
                RelocationCeremonyPhase::Continuity,
                "degraded: the target predates continuity transfer; the role moves without its \
                 session checkpoints and resumes its conversations fresh",
                now,
            );
            let _ = graph.upsert_relocation_ceremony(ceremony);
            return Ok(());
        }

        let bundle = crate::service::continuity::export_continuity_bundle(
            graph,
            ceremony,
            IpcServer::local_hotel_name(graph, local_node_id),
            IpcServer::local_hotel_name(graph, &ceremony.target_hotel),
            now,
        )
        .map_err(|err| format!("CONTINUITY export failed: {err}"))?;
        let request_id = Uuid::new_v4();
        let data = serde_json::json!({
            "request_id": request_id.to_string(),
            "bundle": bundle,
        })
        .to_string();
        if data.len() > crate::service::continuity::CONTINUITY_MAX_BYTES {
            return Err(format!(
                "CONTINUITY bundle is {} bytes, over the {}-byte inline limit",
                data.len(),
                crate::service::continuity::CONTINUITY_MAX_BYTES
            ));
        }
        ceremony.continuity_request_id = Some(request_id.to_string());
        ceremony.advance(
            RelocationCeremonyPhase::Continuity,
            format!(
                "sending {} session checkpoint(s) and {} session row(s) ({} bytes)",
                bundle.apartments.len(),
                bundle.sessions.len(),
                data.len()
            ),
            now,
        );
        let _ = graph.upsert_relocation_ceremony(ceremony);

        let event = EventEnvelope {
            event_id: request_id,
            seq: 0,
            source_node_id: local_node_id.to_string(),
            target_node_id: Some(ceremony.target_hotel.clone()),
            source_agent_id: local_node_id.to_string(),
            target_agent_id: None,
            kind: EventKind::ContinuityImport,
            corr_id: request_id.to_string(),
            attempt: 0,
            created_at: now,
            expires_at: None,
            payload: EventPayload::Inline { data },
            trace: vec![],
        };
        let _ = dispatcher_tx.send(LedgerCommand::AppendLocal(event)).await;

        let key = format!("continuity_ack:{request_id}");
        let mut ack = None;
        for _ in 0..40 {
            if let Ok(Some(raw)) = graph.get_config_value(&key) {
                ack = Some(serde_json::from_str::<serde_json::Value>(&raw).unwrap_or_default());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
        let Some(ack) = ack else {
            return Err("CONTINUITY timed out waiting for the target's import ack".to_string());
        };
        if ack.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let error = ack
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("target refused the import without a reason");
            return Err(format!("target failed the CONTINUITY import: {error}"));
        }
        info!(
            "Relocation ceremony [{}] CONTINUITY acked by '{}': {}",
            ceremony.ceremony_id,
            ceremony.target_hotel,
            ack.get("summary").cloned().unwrap_or_default()
        );
        Ok(())
    }

    /// Roll a still-pre-commitment ceremony back (free — nothing on the
    /// origin was ever touched) and persist the terminal state.
    fn roll_back_ceremony(
        graph: &GraphDomain,
        ceremony: &mut RelocationCeremonyRecord,
        reason: impl Into<String>,
    ) {
        let reason = reason.into();
        ceremony.decline_reason = Some(reason.clone());
        let now = unix_ts();
        ceremony.advance(RelocationCeremonyPhase::RolledBack, reason.clone(), now);
        let _ = graph.upsert_relocation_ceremony(ceremony);
        warn!(
            "Relocation ceremony [{}] rolled back: {}",
            ceremony.ceremony_id, reason
        );
    }

    /// Mark a ceremony interrupted at or after SWITCH as `Failed` and flag
    /// it for operator review — origin state may be inconsistent, so this
    /// is never auto-resumed (invariant 7).
    fn fail_ceremony_needs_review(
        graph: &GraphDomain,
        ceremony: &mut RelocationCeremonyRecord,
        reason: impl Into<String>,
    ) {
        let reason = reason.into();
        ceremony.decline_reason = Some(reason.clone());
        ceremony.needs_operator_review = true;
        let now = unix_ts();
        ceremony.advance(RelocationCeremonyPhase::Failed, reason.clone(), now);
        let _ = graph.upsert_relocation_ceremony(ceremony);
        warn!(
            "Relocation ceremony [{}] FAILED and needs operator review: {}",
            ceremony.ceremony_id, reason
        );
    }

    /// Drive one ceremony through FEASIBILITY → STANDBY → CONTINUITY →
    /// SWITCH → RECONCILE → CLOSE, recording every transition. Runs as a
    /// spawned background task from [`Self::handle_relocate_hotel`].
    ///
    /// Degraded continuity (R5 not built): CONTINUITY does not transfer a
    /// session/checkpoint/dialogue-window blob — STANDBY's
    /// `MaterializeRequest` payload already carries the role/toolset
    /// records, which is all this ceremony moves in its current form.
    ///
    /// SWITCH does not reach into the philote guest's live in-process turn
    /// state to force a park (that state is private to the guest process,
    /// not something the hotel can see or mutate) — it only flips
    /// `home_node`/transport-home routing truth. Any turn already in flight
    /// on the origin process finishes naturally there since no *new* turn
    /// will route to it after SWITCH; RECONCILE then deactivates the origin
    /// guest in the graph (not a forced kill) so the existing
    /// supervisor/TTL path reclaims the process once it is actually idle.
    async fn run_relocation_ceremony(
        graph: &GraphDomain,
        dispatcher_tx: &mpsc::Sender<LedgerCommand>,
        local_node_id: &str,
        ceremony_id: String,
    ) {
        let Ok(Some(mut ceremony)) = graph.get_relocation_ceremony(&ceremony_id) else {
            warn!(
                "run_relocation_ceremony: ceremony [{}] vanished before orchestration could run",
                ceremony_id
            );
            return;
        };

        let Ok(Some(role_record)) =
            graph.get_role_incarnation(&ceremony.agent_id, &ceremony.role_name)
        else {
            Self::roll_back_ceremony(
                graph,
                &mut ceremony,
                "role record disappeared before FEASIBILITY could run",
            );
            return;
        };

        // ── FEASIBILITY ──────────────────────────────────────────────────
        let now = unix_ts();
        ceremony.advance(
            RelocationCeremonyPhase::Feasibility,
            "dispatching dry-run feasibility check",
            now,
        );
        let _ = graph.upsert_relocation_ceremony(&ceremony);

        let requester_guest_id = role_record.guest_id.clone();
        let feasibility_reply = Self::dispatch_materialize_request_core(
            graph,
            dispatcher_tx,
            local_node_id,
            requester_guest_id.clone(),
            ceremony.agent_id.clone(),
            ceremony.role_name.clone(),
            ceremony.requested_by_role.clone(),
            ceremony.target_hotel.clone(),
            true,
        )
        .await;
        let IpcResponse::MaterializeRequested { request_id, .. } = feasibility_reply else {
            Self::roll_back_ceremony(
                graph,
                &mut ceremony,
                "FEASIBILITY dispatch failed before any mesh round trip".to_string(),
            );
            return;
        };
        ceremony.materialize_request_id = Some(request_id.clone());
        let _ = graph.upsert_relocation_ceremony(&ceremony);

        match Self::poll_materialize_ready(graph, &request_id, 20).await {
            None => {
                Self::roll_back_ceremony(
                    graph,
                    &mut ceremony,
                    "FEASIBILITY check timed out waiting for the target's reply",
                );
                return;
            }
            Some(reply) if reply.get("ok").and_then(|v| v.as_bool()) != Some(true) => {
                let decline = reply
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("target declined without a reason")
                    .to_string();
                Self::roll_back_ceremony(
                    graph,
                    &mut ceremony,
                    format!("target declined FEASIBILITY: {decline}"),
                );
                return;
            }
            Some(_) => {}
        }

        // ── STANDBY ───────────────────────────────────────────────────────
        let now = unix_ts();
        ceremony.advance(
            RelocationCeremonyPhase::Standby,
            "target feasible; committing STANDBY (materialize + spawn)",
            now,
        );
        let _ = graph.upsert_relocation_ceremony(&ceremony);

        let standby_reply = Self::dispatch_materialize_request_core(
            graph,
            dispatcher_tx,
            local_node_id,
            requester_guest_id,
            ceremony.agent_id.clone(),
            ceremony.role_name.clone(),
            ceremony.requested_by_role.clone(),
            ceremony.target_hotel.clone(),
            false,
        )
        .await;
        let IpcResponse::MaterializeRequested { request_id, .. } = standby_reply else {
            Self::roll_back_ceremony(
                graph,
                &mut ceremony,
                "STANDBY dispatch failed before any mesh round trip".to_string(),
            );
            return;
        };
        ceremony.materialize_request_id = Some(request_id.clone());
        let _ = graph.upsert_relocation_ceremony(&ceremony);

        let standby_ready = match Self::poll_materialize_ready(graph, &request_id, 40).await {
            None => {
                Self::roll_back_ceremony(
                    graph,
                    &mut ceremony,
                    "STANDBY timed out waiting for the target to report ready",
                );
                return;
            }
            Some(reply) if reply.get("ok").and_then(|v| v.as_bool()) != Some(true) => {
                let decline = reply
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("target failed STANDBY without a reason")
                    .to_string();
                Self::roll_back_ceremony(
                    graph,
                    &mut ceremony,
                    format!("target failed STANDBY: {decline}"),
                );
                return;
            }
            Some(reply) => reply,
        };

        // ── CONTINUITY ───────────────────────────────────────────────────
        // The export is the last read before SWITCH, so the snapshot the
        // target resumes from is as close to the cutover as the round trip
        // allows. Anything the origin process does between this export and
        // SWITCH (a turn finishing mid-flight) is not carried — closing that
        // window needs a drain contract with the origin philote.
        if let Err(reason) = Self::transfer_continuity(
            graph,
            dispatcher_tx,
            local_node_id,
            &mut ceremony,
            &standby_ready,
        )
        .await
        {
            Self::roll_back_ceremony(graph, &mut ceremony, reason);
            return;
        }

        // ── SWITCH ───────────────────────────────────────────────────────
        let now = unix_ts();
        ceremony.advance(
            RelocationCeremonyPhase::Switch,
            "flipping home_node (and transport home, if included)",
            now,
        );
        let _ = graph.upsert_relocation_ceremony(&ceremony);

        let role_home_reply = Self::perform_set_role_home(
            graph,
            ceremony.agent_id.clone(),
            ceremony.role_name.clone(),
            ceremony.requested_by_role.clone(),
            Some(ceremony.target_hotel.clone()),
        );
        if !matches!(role_home_reply, IpcResponse::RoleHomeSet { .. }) {
            // Nothing committed yet at this exact call (perform_set_role_home
            // is a single atomic upsert) — safe to roll back.
            Self::roll_back_ceremony(
                graph,
                &mut ceremony,
                "SWITCH failed setting home_node; origin was not touched",
            );
            return;
        }

        if ceremony.include_transport {
            let (Some(transport), Some(resource_ref)) = (
                ceremony.transport.clone(),
                ceremony.transport_resource_ref.clone(),
            ) else {
                Self::fail_ceremony_needs_review(
                    graph,
                    &mut ceremony,
                    "SWITCH committed home_node but transport/resource_ref were missing for the \
                     include_transport leg — role already moved, transport did not",
                );
                return;
            };
            let transport_reply = Self::perform_set_transport_home(
                graph,
                ceremony.agent_id.clone(),
                transport,
                resource_ref,
                ceremony.requested_by_role.clone(),
                ceremony.target_hotel.clone(),
                Vec::new(),
            );
            if !matches!(transport_reply, IpcResponse::TransportHomeSet { .. }) {
                // Partial commitment: home_node already moved, transport
                // didn't. Never auto-rolled-back post-SWITCH (invariant 7) —
                // surface loudly instead.
                Self::fail_ceremony_needs_review(
                    graph,
                    &mut ceremony,
                    format!(
                        "SWITCH committed home_node but transport move failed: {transport_reply:?} \
                         — role already moved, transport did not"
                    ),
                );
                return;
            }
        }

        // ── RECONCILE ────────────────────────────────────────────────────
        let now = unix_ts();
        ceremony.advance(
            RelocationCeremonyPhase::Reconcile,
            "deactivating origin guest (dormant, not deleted — never resurrected from seed)",
            now,
        );
        let _ = graph.upsert_relocation_ceremony(&ceremony);

        if let Some(origin_hotel_name) = IpcServer::local_hotel_name(graph, local_node_id) {
            if let Err(err) =
                graph.set_guest_active(&origin_hotel_name, &role_record.guest_id, false)
            {
                warn!(
                    "Relocation ceremony [{}] RECONCILE: failed to deactivate origin guest '{}': {} \
                     (non-fatal — the move itself already committed)",
                    ceremony.ceremony_id, role_record.guest_id, err
                );
            }
        } else {
            warn!(
                "Relocation ceremony [{}] RECONCILE: could not resolve local hotel_name for '{}' \
                 — origin guest left active (non-fatal, the move itself already committed)",
                ceremony.ceremony_id, local_node_id
            );
        }

        // ── CLOSE ────────────────────────────────────────────────────────
        let now = unix_ts();
        ceremony.advance(RelocationCeremonyPhase::Close, "ceremony complete", now);
        let _ = graph.upsert_relocation_ceremony(&ceremony);

        info!(
            "Relocation ceremony [{}] CLOSED: role '{}' (agent '{}') now home at '{}'",
            ceremony.ceremony_id, ceremony.role_name, ceremony.agent_id, ceremony.target_hotel
        );
    }

    /// Poll the progress/outcome of a prior [`Self::handle_relocate_hotel`].
    pub(super) fn handle_relocate_hotel_status(
        graph: &GraphDomain,
        ceremony_id: String,
    ) -> IpcResponse {
        match graph.get_relocation_ceremony(&ceremony_id) {
            Ok(Some(ceremony)) => IpcResponse::RelocationCeremonyStatus {
                relocation_ceremony_status: true,
                ceremony_id,
                phase: ceremony.phase.as_str().to_string(),
                risk_tier: ceremony.risk_tier.as_str().to_string(),
                origin_hotel: ceremony.origin_hotel,
                target_hotel: ceremony.target_hotel,
                include_transport: ceremony.include_transport,
                decline_reason: ceremony.decline_reason,
                needs_operator_review: ceremony.needs_operator_review,
            },
            Ok(None) => IpcResponse::error(
                "relocate_hotel_status",
                "RELOCATE_HOTEL_STATUS_UNKNOWN",
                format!("no relocation ceremony found for id '{}'", ceremony_id),
            ),
            Err(err) => IpcResponse::error(
                "relocate_hotel_status",
                "RELOCATE_HOTEL_STATUS_DB_ERROR",
                err.to_string(),
            ),
        }
    }

    /// List `membrane_transport_home` records, optionally filtered to one
    /// agent and/or one transport. Read-only, no authority gate beyond
    /// registration — used by a membrane guest to discover which agents it
    /// should seat, from graph truth rather than only the static
    /// `PHILOTIC_AGENT_ROSTER` a hotel was booted with (see
    /// `membrane-telegram`'s `discover_graph_roster_entries` and
    /// `run_roster_watcher`).
    pub(super) fn handle_list_membrane_transport_homes(
        graph: &GraphDomain,
        agent_id: Option<String>,
        transport: Option<String>,
    ) -> IpcResponse {
        match graph.list_membrane_transport_homes(agent_id.as_deref()) {
            Ok(homes) => {
                let homes = match transport {
                    Some(transport) => homes
                        .into_iter()
                        .filter(|h| h.transport == transport)
                        .collect(),
                    None => homes,
                };
                IpcResponse::MembraneTransportHomeList {
                    membrane_transport_home_list: true,
                    homes,
                }
            }
            Err(err) => IpcResponse::error(
                "list_membrane_transport_homes",
                "LIST_MEMBRANE_TRANSPORT_HOMES_DB_ERROR",
                err.to_string(),
            ),
        }
    }
}

/// Relocation Ceremony R6: boot-time scan for a ceremony an unclean restart
/// interrupted mid-flight (invariant 7 — "a crashed ceremony resumes from
/// its last recorded phase or rolls back to the origin"). Called once from
/// hotel startup, after the graph is open.
///
/// A ceremony interrupted while still pre-commitment (INTENT through
/// CONTINUITY — SWITCH never ran) is rolled back for free: nothing on the
/// origin was ever touched, so there is nothing to undo. A ceremony
/// interrupted at or after SWITCH is never auto-resumed — blindly re-driving
/// SWITCH/RECONCILE after an unknown crash point risks a double-action, so
/// it is instead flagged `needs_operator_review` and left exactly where it
/// stopped for a human (or a deliberate follow-up ceremony) to resolve.
pub fn scan_interrupted_relocation_ceremonies(graph: &GraphDomain) {
    let ceremonies = match graph.list_relocation_ceremonies(None, None) {
        Ok(c) => c,
        Err(err) => {
            warn!(
                "scan_interrupted_relocation_ceremonies: failed to list ceremonies: {}",
                err
            );
            return;
        }
    };
    let now = unix_ts();
    for mut ceremony in ceremonies {
        if ceremony.phase.is_terminal() {
            continue;
        }
        let interrupted_phase = ceremony.phase;
        if interrupted_phase.is_pre_commitment() {
            ceremony.decline_reason = Some(format!(
                "interrupted by hotel restart at phase '{}', before SWITCH — no committed \
                 changes, safe rollback",
                interrupted_phase.as_str()
            ));
            ceremony.advance(
                RelocationCeremonyPhase::RolledBack,
                format!(
                    "boot-time scan: rolled back from interrupted phase '{}'",
                    interrupted_phase.as_str()
                ),
                now,
            );
            let _ = graph.upsert_relocation_ceremony(&ceremony);
            warn!(
                "Relocation ceremony [{}] rolled back on boot: interrupted at '{}' before SWITCH",
                ceremony.ceremony_id,
                interrupted_phase.as_str()
            );
        } else {
            ceremony.needs_operator_review = true;
            ceremony.updated_at = now;
            let _ = graph.upsert_relocation_ceremony(&ceremony);
            warn!(
                "Relocation ceremony [{}] interrupted by hotel restart AFTER SWITCH (phase '{}') \
                 — origin state may be inconsistent, flagged needs_operator_review",
                ceremony.ceremony_id,
                interrupted_phase.as_str()
            );
        }
    }
}

/// The live subscriber that should receive a task addressed to an agent id:
/// the agent's orchestrator incarnation on the `agent` role, else its
/// `role:<agent>:orchestrator` inbox, else any `agent` guest of that agent.
/// `None` when the name is not an agent id this hotel is serving right now.
pub(crate) async fn agent_addressed_subscriber(
    inboxes: &InboxRegistry,
    agent_id: &str,
) -> Option<(String, String)> {
    if agent_id.is_empty() || agent_id.contains(':') {
        return None;
    }
    let orchestrator_guest = format!("{agent_id}:orchestrator");
    let guest_prefix = format!("{agent_id}:");
    let guard = inboxes.lock().await;
    if let Some(subs) = guard.get("agent") {
        if let Some(sub) = subs.iter().find(|s| s.guest_id == orchestrator_guest) {
            return Some(("agent".to_string(), sub.guest_id.clone()));
        }
    }
    let incarnation_role = format!("role:{agent_id}:orchestrator");
    if let Some(sub) = guard.get(&incarnation_role).and_then(|subs| subs.first()) {
        return Some((incarnation_role, sub.guest_id.clone()));
    }
    if let Some(subs) = guard.get("agent") {
        if let Some(sub) = subs
            .iter()
            .find(|s| s.guest_id == agent_id || s.guest_id.starts_with(&guest_prefix))
        {
            return Some(("agent".to_string(), sub.guest_id.clone()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::ipc::tests::{
        MockMaterializationRequester, TestGraphAdapter, ipc_env_guard, test_socket_path,
    };
    use crate::service::ipc::{
        ParkedInboundRegistry, new_delivery_claim_registry, test_dispatcher_channel,
    };
    use ansible_mesh_core::NodeCapabilities;
    use ansible_mesh_core::cron::{CronJob, CronJobSource};
    use ansible_mesh_core::graph::{RoleIncarnationRecord, TurnLoopConfig};
    use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
    use ansible_mesh_core::storage::{GuestRecord, HotelRecord, SessionRecord};
    use philotic_client::{IpcRequest, PhiloticClient};
    use std::path::Path;
    use std::sync::atomic::Ordering;

    fn test_cron_job(target_role: &str, agent_id: &str) -> CronJob {
        CronJob {
            id: "job-1".into(),
            schedule: "0 0 7 * * * *".into(),
            target_role: target_role.into(),
            target_node_id: None,
            payload: "{}".into(),
            guaranteed: false,
            enabled: true,
            last_fired_epoch: None,
            next_fire_at: 0,
            created_at: 0,
            created_by: CronJobSource::Guest(agent_id.into()),
            silent_ok: false,
            session_target: ansible_mesh_core::cron::CronSessionTarget::Main,
        }
    }

    #[test]
    fn normalize_cron_target_role_resolves_bare_role_name_to_routing_key() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-beacon".into(),
                role_name: "orchestrator".into(),
                guest_id: "agent-beacon:orchestrator".into(),
                toolset_profile: "orchestrator".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: true,
                readiness_state: RoleReadinessState::Configured,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
            })
            .expect("seed role incarnation");

        let mut job = test_cron_job("orchestrator", "agent-beacon");
        IpcServer::normalize_cron_target_role(&graph, &mut job);

        assert_eq!(job.target_role, "role:agent-beacon:orchestrator");
    }

    #[test]
    fn set_role_home_resolves_bare_hotel_name_to_real_node_id() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        // DEF-124: hotel_name ("vps-jane", the tool's own documented example)
        // differs from the real node_id ("vps-jane-aiua-01") that every
        // cross-hotel routing comparison actually keys on.
        graph
            .upsert_hotel(&ansible_mesh_core::storage::HotelRecord {
                hotel_name: "vps-jane".into(),
                capabilities: NodeCapabilities {
                    node_id: "vps-jane-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: String::new(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed vps-jane hotel record");
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-beacon".into(),
                role_name: "orchestrator".into(),
                guest_id: "agent-beacon:orchestrator".into(),
                toolset_profile: "orchestrator".into(),
                is_admin: true,
                readiness_state: RoleReadinessState::Routable,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
            })
            .expect("seed admin orchestrator role");
        let identity = GuestIdentity {
            guest_id: "agent-beacon".into(),
            role: "agent".into(),
            supported_tools: vec![],
        };

        let resp = IpcServer::handle_set_role_home(
            &graph,
            Some(&identity),
            "agent-beacon".into(),
            "orchestrator".into(),
            "orchestrator".into(),
            Some("vps-jane".into()),
        );

        match resp {
            IpcResponse::RoleHomeSet { home_node, .. } => {
                assert_eq!(home_node.as_deref(), Some("vps-jane-aiua-01"));
            }
            other => panic!("expected IpcResponse::RoleHomeSet, got {other:?}"),
        }

        let persisted = graph
            .get_role_incarnation("agent-beacon", "orchestrator")
            .expect("read back role")
            .expect("role exists");
        assert_eq!(persisted.home_node.as_deref(), Some("vps-jane-aiua-01"));
    }

    #[test]
    fn set_role_home_rejects_target_hotel_matching_no_known_hotel() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-beacon".into(),
                role_name: "orchestrator".into(),
                guest_id: "agent-beacon:orchestrator".into(),
                toolset_profile: "orchestrator".into(),
                is_admin: true,
                readiness_state: RoleReadinessState::Routable,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
            })
            .expect("seed admin orchestrator role");
        let identity = GuestIdentity {
            guest_id: "agent-beacon".into(),
            role: "agent".into(),
            supported_tools: vec![],
        };

        let resp = IpcServer::handle_set_role_home(
            &graph,
            Some(&identity),
            "agent-beacon".into(),
            "orchestrator".into(),
            "orchestrator".into(),
            Some("nonexistent-hotel".into()),
        );

        match resp {
            IpcResponse::Standard {
                ok: false, code, ..
            } => assert_eq!(code, "SET_ROLE_HOME_UNKNOWN_HOTEL"),
            other => panic!("expected a rejecting IpcResponse::Standard, got {other:?}"),
        }
    }

    #[test]
    fn normalize_cron_target_role_leaves_unresolvable_role_untouched() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));

        // No role incarnation seeded for "agent-beacon"/"orchestrator" — normalization
        // should leave the bare string alone rather than guess.
        let mut job = test_cron_job("orchestrator", "agent-beacon");
        IpcServer::normalize_cron_target_role(&graph, &mut job);

        assert_eq!(job.target_role, "orchestrator");
    }

    #[test]
    fn normalize_cron_target_role_is_idempotent_for_already_qualified_roles() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));

        let mut job = test_cron_job("role:agent-beacon:orchestrator", "agent-beacon");
        IpcServer::normalize_cron_target_role(&graph, &mut job);

        assert_eq!(job.target_role, "role:agent-beacon:orchestrator");
    }

    #[tokio::test]
    async fn materialize_request_rejects_caller_without_operational_admin_authority() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-beacon".into(),
                role_name: "vixen".into(),
                guest_id: "agent-beacon:vixen".into(),
                toolset_profile: "vixen".into(),
                is_admin: false,
                readiness_state: RoleReadinessState::Routable,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
            })
            .expect("seed non-admin calling role");
        let (dispatcher_tx, _rx) = test_dispatcher_channel();
        let identity = GuestIdentity {
            guest_id: "agent-beacon".into(),
            role: "agent".into(),
            supported_tools: vec![],
        };

        let resp = IpcServer::handle_materialize_request(
            &graph,
            &dispatcher_tx,
            "mac-jane",
            Some(&identity),
            "agent-beacon".into(),
            "vixen".into(),
            "vixen".into(), // calling_role: the non-admin role itself, not orchestrator
            "vps-jane".into(),
            false,
        )
        .await;

        match resp {
            IpcResponse::Standard {
                ok: false, message, ..
            } => assert!(
                message.contains("does not have authority"),
                "expected authority-denial message, got: {message}"
            ),
            other => panic!("expected a rejecting IpcResponse::Standard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn materialize_request_dispatches_mesh_event_carrying_role_and_toolset_records() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        // DEF-124: seed a hotel whose hotel_name ("vps-jane", the documented
        // target_hotel example) differs from its real node_id
        // ("vps-jane-aiua-01", what routing actually keys on), so this test
        // exercises the resolution, not a self-consistent coincidence.
        graph
            .upsert_hotel(&ansible_mesh_core::storage::HotelRecord {
                hotel_name: "vps-jane".into(),
                capabilities: NodeCapabilities {
                    node_id: "vps-jane-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: String::new(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed vps-jane hotel record");
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-beacon".into(),
                role_name: "orchestrator".into(),
                guest_id: "agent-beacon:orchestrator".into(),
                toolset_profile: "orchestrator".into(),
                is_admin: true,
                readiness_state: RoleReadinessState::Routable,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: Some("mac-jane".into()),
                ..Default::default()
            })
            .expect("seed admin orchestrator role");
        let (dispatcher_tx, mut rx) = test_dispatcher_channel();
        let identity = GuestIdentity {
            guest_id: "agent-beacon".into(),
            role: "agent".into(),
            supported_tools: vec![],
        };

        let resp = IpcServer::handle_materialize_request(
            &graph,
            &dispatcher_tx,
            "mac-jane",
            Some(&identity),
            "agent-beacon".into(),
            "orchestrator".into(),
            "orchestrator".into(),
            "vps-jane".into(),
            false,
        )
        .await;

        let (request_id, role_name, target_hotel) = match resp {
            IpcResponse::MaterializeRequested {
                materialize_requested,
                request_id,
                role_name,
                target_hotel,
            } => {
                assert!(materialize_requested);
                (request_id, role_name, target_hotel)
            }
            other => panic!("expected IpcResponse::MaterializeRequested, got {other:?}"),
        };
        assert_eq!(role_name, "orchestrator");
        // Resolved to the real node_id, not echoed back as the bare
        // hotel_name that was passed in (DEF-124).
        assert_eq!(target_hotel, "vps-jane-aiua-01");

        let cmd = rx.recv().await.expect("mesh envelope dispatched");
        let LedgerCommand::AppendLocal(event) = cmd else {
            panic!("expected LedgerCommand::AppendLocal");
        };
        assert_eq!(event.kind, EventKind::MaterializeRequest);
        assert_eq!(event.target_node_id.as_deref(), Some("vps-jane-aiua-01"));
        let EventPayload::Inline { data } = &event.payload else {
            panic!("expected inline payload");
        };
        let v: serde_json::Value = serde_json::from_str(data).expect("valid json payload");
        assert_eq!(v["request_id"].as_str(), Some(request_id.as_str()));
        assert_eq!(v["role_record"]["home_node"].as_str(), Some("mac-jane"));
        assert_eq!(v["role_record"]["readiness_state"], "routable");
    }

    #[tokio::test]
    async fn materialize_request_rejects_target_hotel_matching_no_known_hotel() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-beacon".into(),
                role_name: "orchestrator".into(),
                guest_id: "agent-beacon:orchestrator".into(),
                toolset_profile: "orchestrator".into(),
                is_admin: true,
                readiness_state: RoleReadinessState::Routable,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
            })
            .expect("seed admin orchestrator role");
        let (dispatcher_tx, _rx) = test_dispatcher_channel();
        let identity = GuestIdentity {
            guest_id: "agent-beacon".into(),
            role: "agent".into(),
            supported_tools: vec![],
        };

        // No hotel named or node_id'd "nonexistent-hotel" was ever seeded.
        let resp = IpcServer::handle_materialize_request(
            &graph,
            &dispatcher_tx,
            "mac-jane",
            Some(&identity),
            "agent-beacon".into(),
            "orchestrator".into(),
            "orchestrator".into(),
            "nonexistent-hotel".into(),
            false,
        )
        .await;

        match resp {
            IpcResponse::Standard {
                ok: false, code, ..
            } => assert_eq!(code, "MATERIALIZE_REQUEST_UNKNOWN_HOTEL"),
            other => panic!("expected a rejecting IpcResponse::Standard, got {other:?}"),
        }
    }

    #[test]
    fn materialize_status_reports_pending_when_no_reply_has_landed() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));

        let resp = IpcServer::handle_materialize_status(&graph, "no-such-request".into());

        match resp {
            IpcResponse::MaterializeStatus {
                materialize_status,
                request_id,
                ok,
                readiness,
                error,
            } => {
                assert!(materialize_status);
                assert_eq!(request_id, "no-such-request");
                assert_eq!(ok, None);
                assert_eq!(readiness, None);
                assert_eq!(error, None);
            }
            other => panic!("expected IpcResponse::MaterializeStatus, got {other:?}"),
        }
    }

    #[test]
    fn materialize_status_surfaces_a_landed_ready_reply() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        graph
            .set_config_value(
                "materialize_ready:req-123",
                &serde_json::json!({
                    "request_id": "req-123",
                    "guest_id": "agent-beacon:orchestrator",
                    "ok": true,
                    "readiness": "routable",
                    "error": null,
                })
                .to_string(),
            )
            .expect("seed materialize_ready reply");

        let resp = IpcServer::handle_materialize_status(&graph, "req-123".into());

        match resp {
            IpcResponse::MaterializeStatus {
                ok,
                readiness,
                error,
                ..
            } => {
                assert_eq!(ok, Some(true));
                assert_eq!(readiness.as_deref(), Some("routable"));
                assert_eq!(error, None);
            }
            other => panic!("expected IpcResponse::MaterializeStatus, got {other:?}"),
        }
    }

    fn seed_relocatable_orchestrator(graph: &GraphDomain) {
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "vps-jane".into(),
                capabilities: NodeCapabilities {
                    node_id: "vps-jane-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: String::new(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed vps-jane hotel record");
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-beacon".into(),
                role_name: "orchestrator".into(),
                guest_id: "agent-beacon:orchestrator".into(),
                toolset_profile: "orchestrator".into(),
                is_admin: true,
                readiness_state: RoleReadinessState::Routable,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: Some("mac-jane".into()),
                ..Default::default()
            })
            .expect("seed admin orchestrator role");
        graph
            .upsert_hotel(&HotelRecord {
                hotel_name: "mac-jane".into(),
                capabilities: NodeCapabilities {
                    node_id: "mac-jane-aiua-01".into(),
                    roles: vec![],
                    models: vec![],
                    tools: vec![],
                    constraints: Default::default(),
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: String::new(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed mac-jane hotel record");
        graph
            .upsert_guest(&GuestRecord {
                hotel_name: "mac-jane".into(),
                guest_id: "agent-beacon:orchestrator".into(),
                role: "agent".into(),
                config_json: "{}".into(),
                is_active: true,
                active_pid: None,
                last_active_at: None,
            })
            .expect("seed orchestrator guest record on mac-jane");
    }

    #[tokio::test]
    async fn relocate_hotel_rejects_caller_without_operational_admin_authority() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-beacon".into(),
                role_name: "vixen".into(),
                guest_id: "agent-beacon:vixen".into(),
                toolset_profile: "vixen".into(),
                is_admin: false,
                readiness_state: RoleReadinessState::Routable,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
            })
            .expect("seed non-admin calling role");
        let (dispatcher_tx, _rx) = test_dispatcher_channel();
        let identity = GuestIdentity {
            guest_id: "agent-beacon".into(),
            role: "agent".into(),
            supported_tools: vec![],
        };

        let resp = IpcServer::handle_relocate_hotel(
            &graph,
            &dispatcher_tx,
            "mac-jane-aiua-01",
            Some(&identity),
            "agent-beacon".into(),
            "vixen".into(),
            "vixen".into(),
            "vps-jane".into(),
            false,
            None,
            None,
            "test move".into(),
        )
        .await;

        match resp {
            IpcResponse::Standard {
                ok: false, message, ..
            } => assert!(
                message.contains("does not have authority"),
                "expected authority-denial message, got: {message}"
            ),
            other => panic!("expected a rejecting IpcResponse::Standard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn relocate_hotel_rejects_transport_move_without_full_admin_authority() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-beacon".into(),
                role_name: "orchestrator".into(),
                guest_id: "agent-beacon:orchestrator".into(),
                toolset_profile: "orchestrator".into(),
                // operational admin authority via role_name=="orchestrator",
                // but NOT is_admin — insufficient for the High tier.
                is_admin: false,
                readiness_state: RoleReadinessState::Routable,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: Some("mac-jane".into()),
                ..Default::default()
            })
            .expect("seed non-full-admin orchestrator role");
        let (dispatcher_tx, _rx) = test_dispatcher_channel();
        let identity = GuestIdentity {
            guest_id: "agent-beacon".into(),
            role: "agent".into(),
            supported_tools: vec![],
        };

        let resp = IpcServer::handle_relocate_hotel(
            &graph,
            &dispatcher_tx,
            "mac-jane-aiua-01",
            Some(&identity),
            "agent-beacon".into(),
            "orchestrator".into(),
            "orchestrator".into(),
            "vps-jane".into(),
            true,
            Some("telegram".into()),
            Some("bjork-bot".into()),
            "test move with transport".into(),
        )
        .await;

        match resp {
            IpcResponse::Standard {
                ok: false, message, ..
            } => assert!(
                message.contains("High-tier"),
                "expected High-tier-denial message, got: {message}"
            ),
            other => panic!("expected a rejecting IpcResponse::Standard, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn relocate_hotel_full_ceremony_reaches_close_and_flips_home_node() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        seed_relocatable_orchestrator(&graph);
        let (dispatcher_tx, mut rx) = test_dispatcher_channel();
        let identity = GuestIdentity {
            guest_id: "agent-beacon".into(),
            role: "agent".into(),
            supported_tools: vec![],
        };

        let resp = IpcServer::handle_relocate_hotel(
            &graph,
            &dispatcher_tx,
            "mac-jane-aiua-01",
            Some(&identity),
            "agent-beacon".into(),
            "orchestrator".into(),
            "orchestrator".into(),
            "vps-jane".into(),
            false,
            None,
            None,
            "test move".into(),
        )
        .await;
        let ceremony_id = match resp {
            IpcResponse::RelocationCeremonyStarted {
                relocation_ceremony_started,
                ceremony_id,
                ..
            } => {
                assert!(relocation_ceremony_started);
                ceremony_id
            }
            other => panic!("expected IpcResponse::RelocationCeremonyStarted, got {other:?}"),
        };

        // FEASIBILITY's dry-run MaterializeRequest.
        let cmd = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("feasibility dispatch within timeout")
            .expect("feasibility mesh envelope dispatched");
        let LedgerCommand::AppendLocal(event) = cmd else {
            panic!("expected LedgerCommand::AppendLocal");
        };
        let EventPayload::Inline { data } = &event.payload else {
            panic!("expected inline payload");
        };
        let v: serde_json::Value = serde_json::from_str(data).expect("valid json payload");
        assert_eq!(v["dry_run"], serde_json::Value::Bool(true));
        let feasibility_request_id = v["request_id"].as_str().expect("request_id").to_string();
        graph
            .set_config_value(
                &format!("materialize_ready:{feasibility_request_id}"),
                &serde_json::json!({"ok": true, "readiness": "feasible"}).to_string(),
            )
            .expect("seed feasibility reply");

        // STANDBY's real MaterializeRequest.
        let cmd = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("standby dispatch within timeout")
            .expect("standby mesh envelope dispatched");
        let LedgerCommand::AppendLocal(event) = cmd else {
            panic!("expected LedgerCommand::AppendLocal");
        };
        let EventPayload::Inline { data } = &event.payload else {
            panic!("expected inline payload");
        };
        let v: serde_json::Value = serde_json::from_str(data).expect("valid json payload");
        assert_eq!(v["dry_run"], serde_json::Value::Bool(false));
        let standby_request_id = v["request_id"].as_str().expect("request_id").to_string();
        graph
            .set_config_value(
                &format!("materialize_ready:{standby_request_id}"),
                &serde_json::json!({"ok": true, "readiness": "routable"}).to_string(),
            )
            .expect("seed standby reply");

        // Poll until the ceremony reaches a terminal phase.
        let mut final_ceremony = None;
        for _ in 0..40 {
            let ceremony = graph
                .get_relocation_ceremony(&ceremony_id)
                .expect("query ceremony")
                .expect("ceremony exists");
            if ceremony.phase.is_terminal() {
                final_ceremony = Some(ceremony);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let ceremony = final_ceremony.expect("ceremony reached a terminal phase within bound");
        assert_eq!(ceremony.phase, RelocationCeremonyPhase::Close);
        assert!(!ceremony.needs_operator_review);

        let moved_role = graph
            .get_role_incarnation("agent-beacon", "orchestrator")
            .expect("query role")
            .expect("role exists");
        assert_eq!(moved_role.home_node.as_deref(), Some("vps-jane-aiua-01"));

        let origin_guest = graph
            .list_guests("mac-jane", false)
            .expect("list mac-jane guests")
            .into_iter()
            .find(|g| g.guest_id == "agent-beacon:orchestrator")
            .expect("origin guest record still present (dormant, not deleted)");
        assert!(
            !origin_guest.is_active,
            "RECONCILE should have deactivated the origin guest"
        );
    }

    /// Start a ceremony for Beacon's orchestrator, answer FEASIBILITY, and
    /// answer STANDBY with `standby_reply`. Returns the ceremony id.
    async fn drive_ceremony_through_standby(
        graph: &GraphDomain,
        dispatcher_tx: &mpsc::Sender<LedgerCommand>,
        rx: &mut mpsc::UnboundedReceiver<LedgerCommand>,
        standby_reply: serde_json::Value,
    ) -> String {
        let identity = GuestIdentity {
            guest_id: "agent-beacon".into(),
            role: "agent".into(),
            supported_tools: vec![],
        };
        let resp = IpcServer::handle_relocate_hotel(
            graph,
            dispatcher_tx,
            "mac-jane-aiua-01",
            Some(&identity),
            "agent-beacon".into(),
            "orchestrator".into(),
            "orchestrator".into(),
            "vps-jane".into(),
            false,
            None,
            None,
            "test move".into(),
        )
        .await;
        let IpcResponse::RelocationCeremonyStarted { ceremony_id, .. } = resp else {
            panic!("expected IpcResponse::RelocationCeremonyStarted, got {resp:?}");
        };
        for reply in [
            serde_json::json!({"ok": true, "readiness": "feasible"}),
            standby_reply,
        ] {
            let request_id = next_inline_payload(rx).await["request_id"]
                .as_str()
                .expect("request_id")
                .to_string();
            graph
                .set_config_value(
                    &format!("materialize_ready:{request_id}"),
                    &reply.to_string(),
                )
                .expect("seed materialize reply");
        }
        ceremony_id
    }

    async fn next_inline_payload(
        rx: &mut mpsc::UnboundedReceiver<LedgerCommand>,
    ) -> serde_json::Value {
        let cmd = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .expect("dispatch within timeout")
            .expect("mesh envelope dispatched");
        let LedgerCommand::AppendLocal(event) = cmd else {
            panic!("expected LedgerCommand::AppendLocal");
        };
        let EventPayload::Inline { data } = &event.payload else {
            panic!("expected inline payload");
        };
        let mut payload: serde_json::Value = serde_json::from_str(data).expect("valid json");
        payload["__kind"] = serde_json::to_value(&event.kind).expect("kind serializes");
        payload["__target"] = serde_json::json!(event.target_node_id);
        payload
    }

    async fn wait_for_terminal_ceremony(
        graph: &GraphDomain,
        ceremony_id: &str,
    ) -> RelocationCeremonyRecord {
        for _ in 0..80 {
            let ceremony = graph
                .get_relocation_ceremony(ceremony_id)
                .expect("query ceremony")
                .expect("ceremony exists");
            if ceremony.phase.is_terminal() {
                return ceremony;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("ceremony did not reach a terminal phase within bound");
    }

    fn seed_beacon_session_checkpoint(graph: &GraphDomain) {
        graph
            .upsert_session(&ansible_mesh_core::storage::SessionRecord {
                session_id: "telegram:7:agent-beacon".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-beacon".into()),
                active_incarnation_id: Some("agent-beacon:orchestrator".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("7".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({}),
                created_at: 1,
                updated_at: 2,
            })
            .expect("seed session");
        graph
            .sync_apartment(
                "agent-beacon",
                "short_session:telegram:7:agent-beacon",
                &serde_json::json!({
                    "session_id": "telegram:7:agent-beacon",
                    "carryover_plan": {"goal": "garden the LifeGraph"},
                }),
            )
            .expect("seed checkpoint");
    }

    /// R5: a target that supports continuity gets the role's checkpoints
    /// before SWITCH, and the move only closes once it acks.
    #[tokio::test]
    async fn relocate_hotel_carries_session_checkpoints_before_switch() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        seed_relocatable_orchestrator(&graph);
        seed_beacon_session_checkpoint(&graph);
        let (dispatcher_tx, mut rx) = test_dispatcher_channel();

        let ceremony_id = drive_ceremony_through_standby(
            &graph,
            &dispatcher_tx,
            &mut rx,
            serde_json::json!({"ok": true, "readiness": "routable", "supports_continuity": true}),
        )
        .await;

        let import = next_inline_payload(&mut rx).await;
        assert_eq!(import["__kind"], "CONTINUITY_IMPORT");
        assert_eq!(import["__target"], "vps-jane-aiua-01");
        assert_eq!(
            import["bundle"]["apartments"][0]["content"]["carryover_plan"]["goal"],
            "garden the LifeGraph"
        );
        assert_eq!(
            import["bundle"]["sessions"][0]["session_id"],
            "telegram:7:agent-beacon"
        );
        // SWITCH must wait for the ack: the role has not moved yet.
        let role = graph
            .get_role_incarnation("agent-beacon", "orchestrator")
            .expect("query role")
            .expect("role exists");
        assert_eq!(role.home_node.as_deref(), Some("mac-jane"));

        let request_id = import["request_id"].as_str().expect("request_id");
        graph
            .set_config_value(
                &format!("continuity_ack:{request_id}"),
                &serde_json::json!({"request_id": request_id, "ok": true}).to_string(),
            )
            .expect("seed continuity ack");

        let ceremony = wait_for_terminal_ceremony(&graph, &ceremony_id).await;
        assert_eq!(ceremony.phase, RelocationCeremonyPhase::Close);
        assert_eq!(ceremony.continuity_request_id.as_deref(), Some(request_id));
        let role = graph
            .get_role_incarnation("agent-beacon", "orchestrator")
            .expect("query role")
            .expect("role exists");
        assert_eq!(role.home_node.as_deref(), Some("vps-jane-aiua-01"));
    }

    /// R5: a refused import is still pre-commitment — the ceremony rolls
    /// back and the role stays home.
    #[tokio::test]
    async fn relocate_hotel_rolls_back_when_target_refuses_continuity() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        seed_relocatable_orchestrator(&graph);
        seed_beacon_session_checkpoint(&graph);
        let (dispatcher_tx, mut rx) = test_dispatcher_channel();

        let ceremony_id = drive_ceremony_through_standby(
            &graph,
            &dispatcher_tx,
            &mut rx,
            serde_json::json!({"ok": true, "readiness": "routable", "supports_continuity": true}),
        )
        .await;
        let import = next_inline_payload(&mut rx).await;
        let request_id = import["request_id"].as_str().expect("request_id");
        graph
            .set_config_value(
                &format!("continuity_ack:{request_id}"),
                &serde_json::json!({"request_id": request_id, "ok": false, "error": "disk full"})
                    .to_string(),
            )
            .expect("seed refusing ack");

        let ceremony = wait_for_terminal_ceremony(&graph, &ceremony_id).await;
        assert_eq!(ceremony.phase, RelocationCeremonyPhase::RolledBack);
        assert!(
            ceremony
                .decline_reason
                .as_deref()
                .is_some_and(|r| r.contains("disk full")),
            "decline reason should carry the target's error: {:?}",
            ceremony.decline_reason
        );
        let role = graph
            .get_role_incarnation("agent-beacon", "orchestrator")
            .expect("query role")
            .expect("role exists");
        assert_eq!(role.home_node.as_deref(), Some("mac-jane"));
    }

    #[tokio::test]
    async fn relocate_hotel_rolls_back_for_free_when_target_declines_feasibility() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        seed_relocatable_orchestrator(&graph);
        let (dispatcher_tx, mut rx) = test_dispatcher_channel();
        let identity = GuestIdentity {
            guest_id: "agent-beacon".into(),
            role: "agent".into(),
            supported_tools: vec![],
        };

        let resp = IpcServer::handle_relocate_hotel(
            &graph,
            &dispatcher_tx,
            "mac-jane-aiua-01",
            Some(&identity),
            "agent-beacon".into(),
            "orchestrator".into(),
            "orchestrator".into(),
            "vps-jane".into(),
            false,
            None,
            None,
            "test move".into(),
        )
        .await;
        let ceremony_id = match resp {
            IpcResponse::RelocationCeremonyStarted { ceremony_id, .. } => ceremony_id,
            other => panic!("expected IpcResponse::RelocationCeremonyStarted, got {other:?}"),
        };

        let cmd = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("feasibility dispatch within timeout")
            .expect("feasibility mesh envelope dispatched");
        let LedgerCommand::AppendLocal(event) = cmd else {
            panic!("expected LedgerCommand::AppendLocal");
        };
        let EventPayload::Inline { data } = &event.payload else {
            panic!("expected inline payload");
        };
        let v: serde_json::Value = serde_json::from_str(data).expect("valid json payload");
        let feasibility_request_id = v["request_id"].as_str().expect("request_id").to_string();
        graph
            .set_config_value(
                &format!("materialize_ready:{feasibility_request_id}"),
                &serde_json::json!({"ok": false, "error": "no live controller"}).to_string(),
            )
            .expect("seed feasibility decline");

        let mut final_ceremony = None;
        for _ in 0..40 {
            let ceremony = graph
                .get_relocation_ceremony(&ceremony_id)
                .expect("query ceremony")
                .expect("ceremony exists");
            if ceremony.phase.is_terminal() {
                final_ceremony = Some(ceremony);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        let ceremony = final_ceremony.expect("ceremony reached a terminal phase within bound");
        assert_eq!(ceremony.phase, RelocationCeremonyPhase::RolledBack);
        assert!(
            ceremony
                .decline_reason
                .as_deref()
                .unwrap_or_default()
                .contains("no live controller"),
            "expected the target's decline reason to surface, got: {:?}",
            ceremony.decline_reason
        );

        // The role was never touched — free rollback.
        let role = graph
            .get_role_incarnation("agent-beacon", "orchestrator")
            .expect("query role")
            .expect("role exists");
        assert_eq!(role.home_node.as_deref(), Some("mac-jane"));

        // No STANDBY dispatch should have followed the decline.
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv())
                .await
                .is_err(),
            "expected no further mesh dispatch after a FEASIBILITY decline"
        );
    }

    #[test]
    fn boot_scan_rolls_back_a_ceremony_interrupted_before_switch() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        let ceremony = RelocationCeremonyRecord::new(
            "cer-interrupted-pre".into(),
            "agent-beacon".into(),
            "orchestrator".into(),
            "mac-jane-aiua-01".into(),
            "vps-jane-aiua-01".into(),
            false,
            None,
            None,
            "orchestrator".into(),
            "test".into(),
            1000,
        );
        graph
            .upsert_relocation_ceremony(&ceremony)
            .expect("seed ceremony at INTENT");

        scan_interrupted_relocation_ceremonies(&graph);

        let after = graph
            .get_relocation_ceremony("cer-interrupted-pre")
            .expect("query ceremony")
            .expect("ceremony exists");
        assert_eq!(after.phase, RelocationCeremonyPhase::RolledBack);
        assert!(!after.needs_operator_review);
    }

    #[test]
    fn boot_scan_flags_a_ceremony_interrupted_after_switch_for_operator_review() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        let mut ceremony = RelocationCeremonyRecord::new(
            "cer-interrupted-post".into(),
            "agent-beacon".into(),
            "orchestrator".into(),
            "mac-jane-aiua-01".into(),
            "vps-jane-aiua-01".into(),
            false,
            None,
            None,
            "orchestrator".into(),
            "test".into(),
            1000,
        );
        ceremony.advance(RelocationCeremonyPhase::Switch, "home_node flipped", 1010);
        graph
            .upsert_relocation_ceremony(&ceremony)
            .expect("seed ceremony interrupted at SWITCH");

        scan_interrupted_relocation_ceremonies(&graph);

        let after = graph
            .get_relocation_ceremony("cer-interrupted-post")
            .expect("query ceremony")
            .expect("ceremony exists");
        // Left exactly where it stopped — never auto-resumed past SWITCH.
        assert_eq!(after.phase, RelocationCeremonyPhase::Switch);
        assert!(after.needs_operator_review);
    }

    #[test]
    fn boot_scan_ignores_terminal_ceremonies() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        let mut ceremony = RelocationCeremonyRecord::new(
            "cer-already-closed".into(),
            "agent-beacon".into(),
            "orchestrator".into(),
            "mac-jane-aiua-01".into(),
            "vps-jane-aiua-01".into(),
            false,
            None,
            None,
            "orchestrator".into(),
            "test".into(),
            1000,
        );
        ceremony.advance(RelocationCeremonyPhase::Close, "done", 1010);
        graph
            .upsert_relocation_ceremony(&ceremony)
            .expect("seed closed ceremony");

        scan_interrupted_relocation_ceremonies(&graph);

        let after = graph
            .get_relocation_ceremony("cer-already-closed")
            .expect("query ceremony")
            .expect("ceremony exists");
        assert_eq!(after.phase, RelocationCeremonyPhase::Close);
        assert!(!after.needs_operator_review);
        assert_eq!(after.updated_at, 1010, "untouched by the scan");
    }

    #[test]
    fn list_membrane_transport_homes_filters_by_agent_and_transport() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        graph
            .upsert_membrane_transport_home(&MembraneTransportHomeRecord {
                agent_id: "agent-beacon".into(),
                transport: "telegram".into(),
                resource_ref: "telegram_bot_token_beacon".into(),
                active_home_hotel: "vps-jane-aiua-01".into(),
                standby_hotels: vec!["mac-jane-aiua-01".into()],
                managed_by_role: "orchestrator".into(),
                lease_type: "telegram_poll".into(),
                failover_policy: "manual-or-explicit-delegation".into(),
                status: MembraneTransportHomeStatus::Active,
                updated_unix: 0,
            })
            .expect("seed beacon telegram home");
        graph
            .upsert_membrane_transport_home(&MembraneTransportHomeRecord {
                agent_id: "agent-coach".into(),
                transport: "discord".into(),
                resource_ref: "discord_bot_token_coach".into(),
                active_home_hotel: "vps-jane-aiua-01".into(),
                standby_hotels: vec![],
                managed_by_role: "orchestrator".into(),
                lease_type: "discord_gateway".into(),
                failover_policy: "manual-or-explicit-delegation".into(),
                status: MembraneTransportHomeStatus::Active,
                updated_unix: 0,
            })
            .expect("seed coach discord home");

        // No filters: both records.
        match IpcServer::handle_list_membrane_transport_homes(&graph, None, None) {
            IpcResponse::MembraneTransportHomeList { homes, .. } => {
                assert_eq!(homes.len(), 2);
            }
            other => panic!("expected IpcResponse::MembraneTransportHomeList, got {other:?}"),
        }

        // Filtered to telegram: only Beacon's.
        match IpcServer::handle_list_membrane_transport_homes(&graph, None, Some("telegram".into()))
        {
            IpcResponse::MembraneTransportHomeList { homes, .. } => {
                assert_eq!(homes.len(), 1);
                assert_eq!(homes[0].agent_id, "agent-beacon");
                assert_eq!(
                    homes[0].standby_hotels,
                    vec!["mac-jane-aiua-01".to_string()]
                );
            }
            other => panic!("expected IpcResponse::MembraneTransportHomeList, got {other:?}"),
        }

        // Filtered to an agent with no records: empty, not an error.
        match IpcServer::handle_list_membrane_transport_homes(
            &graph,
            Some("agent-nobody".into()),
            None,
        ) {
            IpcResponse::MembraneTransportHomeList { homes, .. } => {
                assert!(homes.is_empty());
            }
            other => panic!("expected IpcResponse::MembraneTransportHomeList, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn deliver_event_envelope_or_park_ignores_events_addressed_to_another_node() {
        // Regression: a mesh inbox batch can carry events explicitly addressed to a
        // different node (e.g. gossiped/relayed). Previously this hotel would try to
        // park-and-materialize the guest locally, which can never succeed for a remote
        // hotel's infrastructure guest (e.g. life-graph-runner) since it has no
        // role_incarnation record — the task parked forever until the turn watchdog
        // evicted it ~90s later. The function must skip entirely for foreign-targeted events.
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));
        let graph = Arc::new(GraphDomain::new(Arc::new(TestGraphAdapter)));
        let parked_inbound: Arc<Mutex<HashMap<String, Vec<ParkedInboundTask>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let mat_req = MockMaterializationRequester::default();

        let event = EventEnvelope {
            event_id: Uuid::new_v4(),
            seq: 1,
            source_node_id: "vps-jane-aiua-01".into(),
            target_node_id: Some("vps-jane-aiua-01".into()),
            source_agent_id: "unknown".into(),
            target_agent_id: Some("life-graph-runner".into()),
            kind: EventKind::TaskInvoke,
            corr_id: "test".into(),
            attempt: 0,
            created_at: 0,
            expires_at: None,
            payload: EventPayload::Inline {
                data: serde_json::json!({
                    "delivery_target_guest_id": "vps-jane:life-graph-runner",
                })
                .to_string(),
            },
            trace: vec![],
        };

        let delivered = IpcServer::deliver_event_envelope_or_park(
            &inboxes,
            &event,
            None,
            &graph,
            "mbp-jane-aiua-01",
            &parked_inbound,
            Some(&mat_req),
            &new_delivery_claim_registry(),
        )
        .await;

        assert!(
            !delivered,
            "event addressed to a different node must not be handled here"
        );
        assert_eq!(
            mat_req.calls.load(Ordering::SeqCst),
            0,
            "must not attempt materialization for a foreign-targeted event"
        );
        assert!(
            parked_inbound.lock().await.is_empty(),
            "must not park a task that belongs to a different node"
        );
    }

    /// DEF-151, live 2026-09-16 18:40 UTC: a `delegate.to_peer` envelope
    /// addressed to "agent-bjork-01" was dropped on arrival.
    #[tokio::test]
    async fn peer_delegation_addressed_to_an_agent_id_reaches_its_orchestrator() {
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));
        let graph = Arc::new(GraphDomain::new(Arc::new(TestGraphAdapter)));
        let parked_inbound: Arc<Mutex<HashMap<String, Vec<ParkedInboundTask>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let mat_req = MockMaterializationRequester::default();

        let (theoretician_tx, mut theoretician_rx) = mpsc::unbounded_channel::<IpcResponse>();
        let (orchestrator_tx, mut orchestrator_rx) = mpsc::unbounded_channel::<IpcResponse>();
        let mut roles = Vec::new();
        IpcServer::add_subscription(
            &inboxes,
            "agent",
            Uuid::new_v4(),
            "agent-bjork-01:theoretician",
            &[],
            &crate::service::ipc::CountedSender::detached(&theoretician_tx),
            &mut roles,
        )
        .await;
        IpcServer::add_subscription(
            &inboxes,
            "agent",
            Uuid::new_v4(),
            "agent-bjork-01:orchestrator",
            &[],
            &crate::service::ipc::CountedSender::detached(&orchestrator_tx),
            &mut roles,
        )
        .await;

        let event = EventEnvelope {
            event_id: Uuid::new_v4(),
            seq: 1,
            source_node_id: "vps-jane-aiua-01".into(),
            target_node_id: Some("mac-jane-aiua-01".into()),
            source_agent_id: "agent-beacon:orchestrator".into(),
            target_agent_id: Some("agent-bjork-01".into()),
            kind: EventKind::TaskInvoke,
            corr_id: "delegation".into(),
            attempt: 0,
            created_at: 0,
            expires_at: None,
            payload: EventPayload::Inline {
                data: serde_json::json!({
                    "action": "peer.delegate",
                    "agent_id": "agent-bjork-01",
                    "session_id": "7898847424:peer:agent-bjork-01",
                    "chat_id": "7898847424",
                    "content": "Handoff from peer agent-beacon:orchestrator: organ practice tonight",
                })
                .to_string(),
            },
            trace: vec![],
        };
        let handled = IpcServer::deliver_event_envelope_or_park(
            &inboxes,
            &event,
            None,
            &graph,
            "mac-jane-aiua-01",
            &parked_inbound,
            Some(&mat_req),
            &new_delivery_claim_registry(),
        )
        .await;
        assert!(handled);
        assert!(
            matches!(
                orchestrator_rx.try_recv(),
                Ok(IpcResponse::InboundTask { .. })
            ),
            "the agent's orchestrator must receive the peer delegation"
        );
        assert!(
            theoretician_rx.try_recv().is_err(),
            "a sibling incarnation must not receive it"
        );
        assert_eq!(mat_req.calls.load(Ordering::SeqCst), 0);

        // Role names and unknown agents are not rerouted.
        assert!(
            agent_addressed_subscriber(&inboxes, "role:agent-bjork-01:orchestrator")
                .await
                .is_none()
        );
        assert!(
            agent_addressed_subscriber(&inboxes, "agent-nobody")
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn deliver_event_envelope_or_park_delivers_each_event_id_exactly_once() {
        // Single-delivery ownership: the same envelope can be observed more than
        // once by the mesh/ledger consumer (retransmitted batch before the ACK
        // lands, relayed echo). Only the first observation may deliver; every
        // later one must be a structural no-op via the shared claim set.
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));
        let graph = Arc::new(GraphDomain::new(Arc::new(TestGraphAdapter)));
        let parked_inbound: Arc<Mutex<HashMap<String, Vec<ParkedInboundTask>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let mat_req = MockMaterializationRequester::default();
        let claims = new_delivery_claim_registry();

        let (subscriber_tx, mut subscriber_rx) = mpsc::unbounded_channel::<IpcResponse>();
        let mut subscribed_roles = Vec::new();
        IpcServer::add_subscription(
            &inboxes,
            "role:agent-test:orchestrator",
            Uuid::new_v4(),
            "agent-test:orchestrator",
            &[],
            &crate::service::ipc::CountedSender::detached(&subscriber_tx),
            &mut subscribed_roles,
        )
        .await;

        let event = EventEnvelope {
            event_id: Uuid::new_v4(),
            seq: 1,
            source_node_id: "mbp-jane-aiua-01".into(),
            target_node_id: Some("mbp-jane-aiua-01".into()),
            source_agent_id: "cron-ticker".into(),
            target_agent_id: Some("role:agent-test:orchestrator".into()),
            kind: EventKind::TaskInvoke,
            corr_id: "cron:job-1".into(),
            attempt: 0,
            created_at: 0,
            expires_at: None,
            payload: EventPayload::Inline {
                data: serde_json::json!({ "cron_job_id": "job-1" }).to_string(),
            },
            trace: vec![],
        };

        for attempt in 0..2 {
            let handled = IpcServer::deliver_event_envelope_or_park(
                &inboxes,
                &event,
                None,
                &graph,
                "mbp-jane-aiua-01",
                &parked_inbound,
                Some(&mat_req),
                &claims,
            )
            .await;
            assert!(
                handled,
                "attempt {attempt} should report the event as handled"
            );
        }

        assert!(
            matches!(
                subscriber_rx.try_recv(),
                Ok(IpcResponse::InboundTask { .. })
            ),
            "first observation must deliver the task to the live subscriber"
        );
        assert!(
            subscriber_rx.try_recv().is_err(),
            "replayed envelope with the same event_id must not be delivered a second time"
        );
        assert!(
            parked_inbound.lock().await.is_empty(),
            "claimed replay must not park a duplicate copy"
        );
    }

    /// Shared fixture for the two `park_and_materialize` arm tests: a local hotel plus a
    /// role incarnation `agent-test:orchestrator`. Identical inputs — only the
    /// [`ParkTarget`] arm differs — so the tests pin down exactly the semantic split that
    /// PR #80 got wrong when the two twin helpers were separate functions.
    fn park_test_graph() -> GraphDomain {
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: "/tmp/park-materialize-test.sock".into(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-test".into(),
                role_name: "orchestrator".into(),
                guest_id: "agent-test:orchestrator".into(),
                toolset_profile: "orchestrator".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: true,
                readiness_state: RoleReadinessState::Configured,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
            })
            .expect("seed role incarnation");
        graph
    }

    // PR #80 regression, local arm: a task for a *local* role incarnation must be parked
    // under the incarnation's own guest_id and materialized via ensure_role_materialized
    // targeting that same guest_id — NOT the cross-hotel `{hotel}:philote-{role}` scheme
    // (reusing the cross-hotel helper here once spawned a wrong-named guest that
    // dead-ended, because nothing ever registers under that name for a local role).
    #[tokio::test]
    async fn park_and_materialize_local_role_incarnation_targets_role_guest_id() {
        let graph = park_test_graph();
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));
        let parked_inbound: ParkedInboundRegistry = Arc::new(Mutex::new(HashMap::new()));
        let mat_req = MockMaterializationRequester::default();
        let role_record = graph
            .list_role_incarnations_by_guest_id("agent-test:orchestrator")
            .expect("list role incarnations")
            .into_iter()
            .next()
            .expect("seeded role incarnation");
        let task_id = Uuid::new_v4();

        IpcServer::park_and_materialize(
            &graph,
            &inboxes,
            &parked_inbound,
            Some(&mat_req),
            "local-aiua-01",
            "local-aiua-01",
            task_id,
            "{}".into(),
            ParkTarget::LocalRoleIncarnation {
                role_record: &role_record,
            },
        )
        .await;

        assert_eq!(
            parked_inbound
                .lock()
                .await
                .get("agent-test:orchestrator")
                .map(Vec::len),
            Some(1),
            "task must be parked under the role incarnation's own guest_id"
        );
        assert_eq!(mat_req.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            mat_req
                .last_guest_id
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .as_deref(),
            Some("agent-test:orchestrator"),
            "materialization must target the role incarnation's own guest_id, not a \
             cross-hotel philote-{{role}} placeholder"
        );
        let guest = graph
            .get_guest("local-hotel", "agent-test:orchestrator")
            .expect("get_guest should not error")
            .expect("materialization should have upserted the local role guest record");
        assert!(
            guest.is_active,
            "materialization must flip the dormant role guest active"
        );
    }

    // PR #80 regression, cross-hotel arm: a cross-hotel TaskInvoke addressed to an
    // agent-centric guest_id must be parked under that guest_id but materialized via the
    // dedicated-process `{hotel}:philote-{role}` naming scheme (seeding its hotel guest
    // record) — the exact opposite target choice from the local arm above.
    #[tokio::test]
    async fn park_and_materialize_cross_hotel_guest_targets_philote_naming_scheme() {
        let graph = park_test_graph();
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));
        let parked_inbound: ParkedInboundRegistry = Arc::new(Mutex::new(HashMap::new()));
        let mat_req = MockMaterializationRequester::default();
        let task_id = Uuid::new_v4();

        IpcServer::park_and_materialize(
            &graph,
            &inboxes,
            &parked_inbound,
            Some(&mat_req),
            "local-aiua-01",
            "remote-aiua-01",
            task_id,
            "{}".into(),
            ParkTarget::CrossHotelGuest {
                agent_guest_id: "agent-test:orchestrator",
            },
        )
        .await;

        assert_eq!(
            parked_inbound
                .lock()
                .await
                .get("agent-test:orchestrator")
                .map(Vec::len),
            Some(1),
            "task must be parked under the agent-centric guest_id it was addressed to"
        );
        assert_eq!(mat_req.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            mat_req
                .last_guest_id
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .as_deref(),
            Some("local-hotel:philote-orchestrator"),
            "cross-hotel materialization must target the dedicated-process \
             {{hotel}}:philote-{{role}} guest, not the agent-centric guest_id"
        );
        let guest = graph
            .get_guest("local-hotel", "local-hotel:philote-orchestrator")
            .expect("get_guest should not error")
            .expect("cross-hotel arm must seed the philote hotel guest record");
        assert!(guest.is_active);
        assert_eq!(guest.role, "orchestrator");

        let config: serde_json::Value =
            serde_json::from_str(&guest.config_json).expect("config_json must be valid JSON");
        let env = config["env"].clone();
        assert_eq!(
            env["PHILOTIC_ROLE_INBOX"], "role:agent-test:orchestrator",
            "spawned philote must self-report the canonical routing role, or it can never \
             pass Self::is_agent_handoff_caller and hand back out of the role"
        );
        assert_eq!(env["PHILOTIC_GUEST_ID"], "agent-test:orchestrator");
        assert_eq!(env["PHILOTIC_HOTEL_NAME"], "local-hotel");
    }

    #[tokio::test]
    async fn resolve_agent_route_keeps_transport_continuity_marker_under_newer_conflicting_active_incarnation()
     {
        let _env_guard = ipc_env_guard();
        let now = unix_ts();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: "/tmp/unused.sock".into(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        graph
            .seed_guests(
                "local-hotel",
                &[
                    GuestRecord {
                        hotel_name: "local-hotel".into(),
                        guest_id: "agent-jane:orchestrator".into(),
                        role: "agent".into(),
                        config_json: "{}".into(),
                        is_active: true,
                        active_pid: None,
                        last_active_at: None,
                    },
                    GuestRecord {
                        hotel_name: "local-hotel".into(),
                        guest_id: "agent-jane:developer".into(),
                        role: "agent".into(),
                        config_json: "{}".into(),
                        is_active: true,
                        active_pid: None,
                        last_active_at: None,
                    },
                ],
            )
            .expect("seed local guests");
        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-transport-marker-survives".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: Some("agent-jane:orchestrator".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({
                    "agent_runtime_provenance": {
                        "authority_hotel": "remote-hotel",
                        "delivery_hotel": "local-hotel",
                        "delivery_target_guest_id": "agent-jane:developer",
                        "marker_kind": "transport_continuity",
                        "marker_source": "operator_chat",
                        "updated_at": now.saturating_sub(1)
                    }
                }),
                created_at: now.saturating_sub(30),
                updated_at: now,
            })
            .expect("session should seed");
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));
        let route = IpcServer::resolve_agent_route(
            &graph,
            &inboxes,
            "local-aiua-01",
            "agent",
            None,
            &serde_json::json!({
                "session_id": "sess-transport-marker-survives",
                "source": "telegram",
                "chat_id": "123",
                "content": "route with durable transport continuity"
            })
            .to_string(),
        )
        .await;

        assert_eq!(
            route,
            AgentRouteResolution::Park {
                guest_id: "agent-jane:developer".into()
            }
        );
    }

    #[tokio::test]
    async fn resolve_agent_route_does_not_park_for_weak_receptor_marker_without_live_guest() {
        let _env_guard = ipc_env_guard();
        let now = unix_ts();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: "/tmp/unused.sock".into(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-jane-01".into(),
                role_name: "orchestrator".into(),
                guest_id: "agent-jane:orchestrator".into(),
                toolset_profile: "orchestrator".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                readiness_state: RoleReadinessState::Configured,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
            })
            .expect("seed orchestrator role");
        graph
            .seed_guests(
                "local-hotel",
                &[
                    GuestRecord {
                        hotel_name: "local-hotel".into(),
                        guest_id: "agent-jane:orchestrator".into(),
                        role: "agent".into(),
                        config_json: "{}".into(),
                        is_active: true,
                        active_pid: None,
                        last_active_at: None,
                    },
                    GuestRecord {
                        hotel_name: "local-hotel".into(),
                        guest_id: "agent-jane:developer".into(),
                        role: "agent".into(),
                        config_json: "{}".into(),
                        is_active: true,
                        active_pid: None,
                        last_active_at: None,
                    },
                ],
            )
            .expect("seed local guests");
        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-weak-receptor-no-park".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: None,
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({
                    "agent_runtime_provenance": {
                        "authority_hotel": "remote-hotel",
                        "delivery_hotel": "local-hotel",
                        "delivery_target_guest_id": "agent-jane:developer",
                        "marker_kind": "receptor_ingress",
                        "marker_source": "telegram",
                        "marker_strength": "weak",
                        "updated_at": now.saturating_sub(1)
                    }
                }),
                created_at: now.saturating_sub(30),
                updated_at: now,
            })
            .expect("session should seed");
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));
        let route = IpcServer::resolve_agent_route(
            &graph,
            &inboxes,
            "local-aiua-01",
            "agent",
            None,
            &serde_json::json!({
                "session_id": "sess-weak-receptor-no-park",
                "source": "telegram",
                "chat_id": "123",
                "content": "weak receptor should not trigger developer parking"
            })
            .to_string(),
        )
        .await;

        assert_eq!(
            route,
            AgentRouteResolution::Park {
                guest_id: "agent-jane:orchestrator".into()
            }
        );
    }

    #[tokio::test]
    async fn resolve_agent_route_can_park_for_strong_custom_marker_without_live_guest() {
        let _env_guard = ipc_env_guard();
        let now = unix_ts();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: "/tmp/unused.sock".into(),
                active_pid: None,
                mesh_host: None,
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
                session_id: "sess-strong-marker-park".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-jane-01".into()),
                active_incarnation_id: None,
                channel_kind: Some("operator".into()),
                channel_session_key: Some("chat-1".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({
                    "agent_runtime_provenance": {
                        "authority_hotel": "remote-hotel",
                        "delivery_hotel": "local-hotel",
                        "delivery_target_guest_id": "agent-jane:developer",
                        "marker_kind": "routing_enzyme",
                        "marker_source": "routing_refinement",
                        "marker_strength": "strong",
                        "updated_at": now.saturating_sub(1)
                    }
                }),
                created_at: now.saturating_sub(30),
                updated_at: now,
            })
            .expect("session should seed");
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));
        let route = IpcServer::resolve_agent_route(
            &graph,
            &inboxes,
            "local-aiua-01",
            "agent",
            None,
            &serde_json::json!({
                "session_id": "sess-strong-marker-park",
                "source": "operator_chat",
                "chat_id": "chat-1",
                "content": "strong custom marker should preserve developer parking"
            })
            .to_string(),
        )
        .await;

        assert_eq!(
            route,
            AgentRouteResolution::Park {
                guest_id: "agent-jane:developer".into()
            }
        );
    }

    // Guard regression (2026-07-06 parked-tool-result incident): a placement-provenance
    // hint naming a non-agent infrastructure guest (here the life-graph-runner, whose
    // guest record role is "life-graph-runner") must be rejected — never parked for.
    // Before the fix this exact setup parked the agent's tool RESULT for the runner
    // itself and the turn died at the watchdog. With the poisoned hint rejected, routing
    // falls back to parking for the primary agent's configured orchestrator incarnation.
    #[tokio::test]
    async fn resolve_agent_route_rejects_poisoned_non_agent_provenance_hint() {
        let _env_guard = ipc_env_guard();
        let now = unix_ts();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: "/tmp/unused.sock".into(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-beacon".into(),
                role_name: "orchestrator".into(),
                guest_id: "agent-beacon:orchestrator".into(),
                toolset_profile: "orchestrator".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                readiness_state: RoleReadinessState::Configured,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
            })
            .expect("seed orchestrator role");
        graph
            .seed_guests(
                "local-hotel",
                &[
                    // The tool runner: configured locally, so the pre-fix code path
                    // would happily Park for it under a transport_continuity marker.
                    GuestRecord {
                        hotel_name: "local-hotel".into(),
                        guest_id: "vps-jane:life-graph-runner".into(),
                        role: "life-graph-runner".into(),
                        config_json: "{}".into(),
                        is_active: true,
                        active_pid: None,
                        last_active_at: None,
                    },
                    GuestRecord {
                        hotel_name: "local-hotel".into(),
                        guest_id: "agent-beacon:orchestrator".into(),
                        role: "agent".into(),
                        config_json: "{}".into(),
                        is_active: true,
                        active_pid: None,
                        last_active_at: None,
                    },
                ],
            )
            .expect("seed local guests");
        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-poisoned-hint".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-beacon".into()),
                active_incarnation_id: None,
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({
                    "agent_runtime_provenance": {
                        "delivery_hotel": "local-hotel",
                        "delivery_target_guest_id": "vps-jane:life-graph-runner",
                        "delivery_target_role": "life-graph-runner",
                        "marker_kind": "transport_continuity",
                        "marker_source": "operator_chat",
                        "updated_at": now.saturating_sub(1)
                    }
                }),
                created_at: now.saturating_sub(30),
                updated_at: now,
            })
            .expect("session should seed");
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));
        let route = IpcServer::resolve_agent_route(
            &graph,
            &inboxes,
            "local-aiua-01",
            "agent",
            None,
            &serde_json::json!({
                "session_id": "sess-poisoned-hint",
                "action": "tool_result",
                "content": "life.observe result returning to the agent"
            })
            .to_string(),
        )
        .await;

        assert_ne!(
            route,
            AgentRouteResolution::Park {
                guest_id: "vps-jane:life-graph-runner".into()
            },
            "agent-role task must never be parked for a tool-runner guest"
        );
        assert_eq!(
            route,
            AgentRouteResolution::Park {
                guest_id: "agent-beacon:orchestrator".into()
            },
            "poisoned hint rejected; routing must fall back to the agent's orchestrator"
        );
    }

    // Enabler regression (2026-07-06 parked-tool-result incident): philote registers
    // under its bare agent id ("agent-beacon") while the session's
    // active_incarnation_id stores "agent-beacon:orchestrator". The registry lookup
    // must normalize to the live base-agent registration and deliver there directly —
    // before the fix the miss handed routing to the (poisoned) provenance-hint park
    // path.
    #[tokio::test]
    async fn resolve_agent_route_delivers_to_live_base_agent_for_unregistered_incarnation() {
        let _env_guard = ipc_env_guard();
        let now = unix_ts();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: "/tmp/unused.sock".into(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        graph
            .seed_guests(
                "local-hotel",
                &[GuestRecord {
                    hotel_name: "local-hotel".into(),
                    guest_id: "vps-jane:life-graph-runner".into(),
                    role: "life-graph-runner".into(),
                    config_json: "{}".into(),
                    is_active: true,
                    active_pid: None,
                    last_active_at: None,
                }],
            )
            .expect("seed runner guest");
        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-incarnation-mismatch".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-beacon".into()),
                active_incarnation_id: Some("agent-beacon:orchestrator".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("123".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({
                    "agent_runtime_provenance": {
                        "delivery_hotel": "local-hotel",
                        "delivery_target_guest_id": "vps-jane:life-graph-runner",
                        "delivery_target_role": "life-graph-runner",
                        "marker_kind": "transport_continuity",
                        "marker_source": "operator_chat",
                        "updated_at": now.saturating_sub(1)
                    }
                }),
                created_at: now.saturating_sub(30),
                updated_at: now,
            })
            .expect("session should seed");

        // Live registry: the philote registered under its BARE agent id, not the
        // incarnation id stored on the session — the standing mismatch from the
        // incident.
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));
        let (tx, _rx) = mpsc::unbounded_channel::<IpcResponse>();
        let mut subscribed_roles = Vec::new();
        IpcServer::add_subscription(
            &inboxes,
            "agent",
            Uuid::new_v4(),
            "agent-beacon",
            &[],
            &crate::service::ipc::CountedSender::detached(&tx),
            &mut subscribed_roles,
        )
        .await;

        let route = IpcServer::resolve_agent_route(
            &graph,
            &inboxes,
            "local-aiua-01",
            "agent",
            None,
            &serde_json::json!({
                "session_id": "sess-incarnation-mismatch",
                "action": "tool_result",
                "content": "life.observe result returning to the agent"
            })
            .to_string(),
        )
        .await;

        assert_eq!(
            route,
            AgentRouteResolution::Deliver(Some("agent-beacon".into())),
            "unregistered incarnation must normalize to its live base-agent registration"
        );
    }

    /// A paracrine_response addressed explicitly to an incarnation guest id
    /// ("{agent_id}:{role_name}") that is NOT subscribed under that exact id must
    /// normalize to the live base agent instead of being delivered-then-dropped.
    #[tokio::test]
    async fn resolve_agent_route_explicit_incarnation_delivers_to_live_base() {
        let _env_guard = ipc_env_guard();
        let now = unix_ts();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: "/tmp/unused.sock".into(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-para-reply".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-aria".into()),
                active_incarnation_id: Some("agent-aria:orchestrator".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("555".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({}),
                created_at: now.saturating_sub(30),
                updated_at: now,
            })
            .expect("session should seed");

        // Base philote is live under its bare agent id; the incarnation id is NOT subscribed.
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));
        let (tx, _rx) = mpsc::unbounded_channel::<IpcResponse>();
        let mut subscribed_roles = Vec::new();
        IpcServer::add_subscription(
            &inboxes,
            "agent",
            Uuid::new_v4(),
            "agent-aria",
            &[],
            &crate::service::ipc::CountedSender::detached(&tx),
            &mut subscribed_roles,
        )
        .await;

        let route = IpcServer::resolve_agent_route(
            &graph,
            &inboxes,
            "local-aiua-01",
            "agent",
            Some("agent-aria:orchestrator".into()),
            &serde_json::json!({
                "session_id": "sess-para-reply",
                "action": "paracrine_response",
                "content": "specialist reply"
            })
            .to_string(),
        )
        .await;

        assert_eq!(
            route,
            AgentRouteResolution::Deliver(Some("agent-aria".into())),
            "paracrine_response to an unsubscribed incarnation must normalize to the live base agent, not drop"
        );
    }

    /// When neither the incarnation NOR its base agent is live, but the incarnation
    /// is configured on this hotel, the reply must park + trigger materialization
    /// instead of being dropped ledger-only.
    #[tokio::test]
    async fn resolve_agent_route_explicit_incarnation_parks_when_nothing_live() {
        let _env_guard = ipc_env_guard();
        let now = unix_ts();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: "/tmp/unused.sock".into(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        graph
            .seed_guests(
                "local-hotel",
                &[GuestRecord {
                    hotel_name: "local-hotel".into(),
                    guest_id: "agent-aria:orchestrator".into(),
                    role: "orchestrator".into(),
                    config_json: "{}".into(),
                    is_active: true,
                    active_pid: None,
                    last_active_at: None,
                }],
            )
            .expect("seed incarnation guest");
        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-para-reply".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-aria".into()),
                active_incarnation_id: Some("agent-aria:orchestrator".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("555".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({}),
                created_at: now.saturating_sub(30),
                updated_at: now,
            })
            .expect("session should seed");

        // Nothing subscribed: neither the incarnation nor its base agent is live.
        let inboxes: InboxRegistry = Arc::new(Mutex::new(HashMap::new()));

        let route = IpcServer::resolve_agent_route(
            &graph,
            &inboxes,
            "local-aiua-01",
            "agent",
            Some("agent-aria:orchestrator".into()),
            &serde_json::json!({
                "session_id": "sess-para-reply",
                "action": "paracrine_response",
                "content": "specialist reply"
            })
            .to_string(),
        )
        .await;

        assert_eq!(
            route,
            AgentRouteResolution::Park {
                guest_id: "agent-aria:orchestrator".into()
            },
            "an offline but locally-configured incarnation must park + materialize, not drop"
        );
    }

    #[tokio::test]
    async fn handoff_to_live_role_switches_active_incarnation_and_delivers_bundle() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = test_dispatcher_channel();
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
                readiness_state: RoleReadinessState::Configured,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
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
        developer
            .send_request(IpcRequest::SubscribeInbox {
                role: "role:agent-jane-01:developer".into(),
            })
            .await
            .expect("developer role inbox subscribe");

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

    /// DEF-132: the theoretician's home was stored as the bare hotel name
    /// ("mac-jane") while every routing comparison keys on the node_id
    /// ("mac-jane-aiua-01"); the handoff was dispatched "remote" to a peer
    /// that does not exist and the bundle never reached the local role.
    #[tokio::test]
    async fn handoff_to_role_homed_by_local_hotel_name_is_delivered_locally() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = test_dispatcher_channel();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-handoff-home-name".into(),
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
                role_name: "theoretician".into(),
                guest_id: "agent-jane:theoretician".into(),
                toolset_profile: "codex".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                readiness_state: RoleReadinessState::Configured,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
                // The bare hotel NAME of this very hotel — not its node_id.
                home_node: Some("local-hotel".into()),
                ..Default::default()
            })
            .expect("theoretician role should seed");
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
        let mut theoretician = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-jane:theoretician".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("theoretician connect");
        theoretician
            .send_request(IpcRequest::SubscribeInbox {
                role: "role:agent-jane-01:theoretician".into(),
            })
            .await
            .expect("theoretician role inbox subscribe");

        let response = orchestrator
            .send_request(IpcRequest::HandoffToRole {
                session_id: "sess-handoff-home-name".into(),
                role_name: "theoretician".into(),
                handoff_bundle: HandoffBundle {
                    goal: "map the nocturne's sections".into(),
                    context_excerpt: "Chopin posthumous nocturne".into(),
                    session_id: "sess-handoff-home-name".into(),
                    initiating_turn_id: "turn-1".into(),
                    return_to: Some("orchestrator".into()),
                    handoff_reason: Some("manual_role_switch".into()),
                    active_goal: Some("map the nocturne's sections".into()),
                    active_constraints: Vec::new(),
                    relevant_session_facts: Vec::new(),
                    working_summary: None,
                    from_role: Some("orchestrator".into()),
                    to_role: Some("theoretician".into()),
                    suggested_memory_refs: Vec::new(),
                    expected_return_mode: Some("required".into()),
                    cleanup_actions: Vec::new(),
                },
            })
            .await
            .expect("handoff request");
        match response {
            IpcResponse::HandoffAck {
                handoff_guest_id, ..
            } => {
                assert_eq!(handoff_guest_id, "agent-jane:theoretician");
            }
            other => panic!("unexpected handoff response: {other:?}"),
        }

        // The bundle must land in the LOCAL role inbox — a remote dispatch to
        // a peer named "local-hotel" never delivers anywhere.
        let delivered = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            theoretician.recv_task(),
        )
        .await
        .expect("theoretician should receive the handoff bundle locally")
        .expect("theoretician recv should succeed");
        match delivered {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("handoff payload should decode");
                assert_eq!(payload["action"], "handoff_bundle");
                assert_eq!(
                    payload["handoff_bundle"]["goal"],
                    "map the nocturne's sections"
                );
            }
            other => panic!("unexpected theoretician inbound response: {other:?}"),
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

    /// DEF-134: a mesh-config-seeded role guest (PHILOTIC_ROLE_NAME only, no
    /// PHILOTIC_ROLE_INBOX) built before the philote-side default registered
    /// under the bare role name. It is still the incarnation the record
    /// describes and must be allowed to hand back; a tool runner is not.
    #[test]
    fn bare_role_name_identity_is_an_agent_handoff_caller() {
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-bjork-01".into(),
                role_name: "theoretician".into(),
                guest_id: "agent-bjork-01:theoretician".into(),
                toolset_profile: "theoretician".into(),
                readiness_state: RoleReadinessState::ActiveInSession,
                ..Default::default()
            })
            .expect("seed role incarnation");
        let bare = GuestIdentity {
            guest_id: "agent-bjork-01:theoretician".into(),
            role: "theoretician".into(),
            supported_tools: Vec::new(),
        };
        assert!(IpcServer::is_agent_handoff_caller(&graph, &bare));
        let routing = GuestIdentity {
            guest_id: "agent-bjork-01:theoretician".into(),
            role: "role:agent-bjork-01:theoretician".into(),
            supported_tools: Vec::new(),
        };
        assert!(IpcServer::is_agent_handoff_caller(&graph, &routing));
        let tool = GuestIdentity {
            guest_id: "agent-bjork-01:theoretician".into(),
            role: "tool".into(),
            supported_tools: Vec::new(),
        };
        assert!(!IpcServer::is_agent_handoff_caller(&graph, &tool));
    }

    #[tokio::test]
    async fn role_incarnation_can_initiate_manual_handoff_to_role() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = test_dispatcher_channel();
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-role-incarnation-handoff".into(),
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
                role_name: "orchestrator".into(),
                guest_id: "agent-jane:orchestrator".into(),
                toolset_profile: "orchestrator".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                readiness_state: RoleReadinessState::ActiveInSession,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
            })
            .expect("orchestrator role should seed");
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-jane-01".into(),
                role_name: "developer".into(),
                guest_id: "agent-jane:developer".into(),
                toolset_profile: "codex".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                readiness_state: RoleReadinessState::Configured,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
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
            role: "role:agent-jane-01:orchestrator".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("orchestrator connect");
        let mut developer = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-jane:developer".into(),
            role: "role:agent-jane-01:developer".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("developer connect");
        developer
            .send_request(IpcRequest::SubscribeInbox {
                role: "agent".into(),
            })
            .await
            .expect("developer agent inbox subscribe");

        let response = orchestrator
            .send_request(IpcRequest::HandoffToRole {
                session_id: "sess-role-incarnation-handoff".into(),
                role_name: "developer".into(),
                handoff_bundle: HandoffBundle {
                    goal: "switch role".into(),
                    context_excerpt: "manual slash command".into(),
                    session_id: "sess-role-incarnation-handoff".into(),
                    initiating_turn_id: "turn-1".into(),
                    return_to: Some("orchestrator".into()),
                    handoff_reason: Some("manual_role_switch".into()),
                    active_goal: None,
                    active_constraints: vec!["same_identity_role_handoff".into()],
                    relevant_session_facts: Vec::new(),
                    working_summary: None,
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
            .get_session("sess-role-incarnation-handoff")
            .expect("session lookup")
            .expect("session exists");
        assert_eq!(
            session.active_incarnation_id.as_deref(),
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
    async fn handoff_to_role_resolves_role_name_case_insensitively() {
        // Role names are stored with whatever casing they were configured with
        // (e.g. "Chronos"), but an operator typing `/role chronos` from memory,
        // or a model emitting a lowercased argument, must still resolve —
        // live-observed: HANDOFF_ROLE_UNKNOWN for `/role chronos` against a
        // role stored as "Chronos".
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = test_dispatcher_channel();
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));
        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-role-case-insensitive".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-beacon".into()),
                active_incarnation_id: Some("agent-beacon".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("456".into()),
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
                agent_id: "agent-beacon".into(),
                role_name: "Chronos".into(),
                guest_id: "agent-beacon:Chronos".into(),
                toolset_profile: "scheduler".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                readiness_state: RoleReadinessState::Configured,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
            })
            .expect("Chronos role should seed");
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

        let mut base_agent = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-beacon".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("base agent connect");
        let mut chronos = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-beacon:Chronos".into(),
            role: "role:agent-beacon:Chronos".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("chronos connect");
        chronos
            .send_request(IpcRequest::SubscribeInbox {
                role: "agent".into(),
            })
            .await
            .expect("chronos agent inbox subscribe");

        let response = base_agent
            .send_request(IpcRequest::HandoffToRole {
                session_id: "sess-role-case-insensitive".into(),
                role_name: "chronos".into(),
                handoff_bundle: HandoffBundle {
                    goal: "switch role".into(),
                    context_excerpt: "manual slash command".into(),
                    session_id: "sess-role-case-insensitive".into(),
                    initiating_turn_id: "turn-1".into(),
                    return_to: Some("orchestrator".into()),
                    handoff_reason: Some("manual_role_switch".into()),
                    active_goal: None,
                    active_constraints: Vec::new(),
                    relevant_session_facts: Vec::new(),
                    working_summary: None,
                    from_role: Some("orchestrator".into()),
                    to_role: Some("chronos".into()),
                    suggested_memory_refs: Vec::new(),
                    expected_return_mode: Some("required".into()),
                    cleanup_actions: Vec::new(),
                },
            })
            .await
            .expect("handoff request");

        match response {
            IpcResponse::HandoffAck {
                handoff_guest_id,
                became_active,
            } => {
                assert_eq!(handoff_guest_id, "agent-beacon:Chronos");
                assert!(became_active);
            }
            other => panic!("lowercase role name must still resolve, got: {other:?}"),
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
    async fn handoff_to_missing_role_returns_pending_until_role_inbox_is_routable() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = test_dispatcher_channel();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
                mesh_host: None,
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
                readiness_state: RoleReadinessState::Configured,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
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
            IpcResponse::HandoffPending {
                role_name,
                readiness,
                ..
            } => {
                assert_eq!(role_name, "developer");
                assert!(
                    matches!(readiness.as_str(), "materializing" | "materialized"),
                    "unexpected readiness: {readiness}"
                );
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
        developer
            .send_request(IpcRequest::SubscribeInbox {
                role: "role:agent-jane-01:developer".into(),
            })
            .await
            .expect("developer role inbox subscribe");

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
            .expect("handoff retry");

        match response {
            IpcResponse::HandoffAck {
                handoff_guest_id,
                became_active,
            } => {
                assert_eq!(handoff_guest_id, "agent-jane:developer");
                assert!(became_active);
            }
            other => panic!("unexpected retry handoff response: {other:?}"),
        }

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
    async fn configure_role_persists_config_successfully() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _) = test_dispatcher_channel();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
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
                fallback_tiers: None,
                model_bindings: None,
                content_policy: None,
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

    /// Regression test for the config-eating bug: a brand-new role configured
    /// with `fallback_tiers: None` must get `DEFAULT_FALLBACK_TIERS`, not an
    /// empty ladder.
    #[tokio::test]
    async fn configure_role_new_role_defaults_fallback_tiers() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _) = test_dispatcher_channel();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

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
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                inactive_ttl_seconds: None,
                iteration_cap: None,
                approval_policy: None,
                model_profile: None,
                context_window_policy: None,
                fallback_tiers: None,
                model_bindings: None,
                content_policy: None,
            })
            .await
            .expect("configure request");
        match resp {
            IpcResponse::ConfigureRoleOk { role_name } => assert_eq!(role_name, "developer"),
            other => panic!("expected ConfigureRoleOk, got {:?}", other),
        }

        let role = graph
            .get_role_incarnation("agent-jane-01", "developer")
            .expect("role lookup")
            .expect("role exists");
        let expected: Vec<String> = ansible_mesh_core::model_routing::DEFAULT_FALLBACK_TIERS
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(role.turn_loop_config.fallback_tiers, expected);

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    /// Regression test for the config-eating bug: `Some(tiers)` explicitly
    /// sets the ladder.
    #[tokio::test]
    async fn configure_role_sets_fallback_tiers_when_some() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _) = test_dispatcher_channel();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

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

        let custom_tiers = vec![
            "model".to_string(),
            "model.openrouter".to_string(),
            "model.custom".to_string(),
        ];
        let resp = orchestrator
            .send_request(IpcRequest::ConfigureRole {
                agent_id: "agent-jane-01".into(),
                role_name: "developer".into(),
                guest_id: "agent-jane-01:developer".into(),
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
                fallback_tiers: Some(custom_tiers.clone()),
                model_bindings: None,
                content_policy: None,
            })
            .await
            .expect("configure request");
        match resp {
            IpcResponse::ConfigureRoleOk { role_name } => assert_eq!(role_name, "developer"),
            other => panic!("expected ConfigureRoleOk, got {:?}", other),
        }

        let role = graph
            .get_role_incarnation("agent-jane-01", "developer")
            .expect("role lookup")
            .expect("role exists");
        assert_eq!(role.turn_loop_config.fallback_tiers, custom_tiers);

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    /// The actual regression: a second ConfigureRole call with `fallback_tiers:
    /// None` must PRESERVE the custom ladder set by an earlier call, not wipe
    /// it to empty (the bug: mac-jane's orchestrator ladder lost its
    /// model.openrouter tier on every reconfigure).
    #[tokio::test]
    async fn configure_role_preserves_existing_fallback_tiers_when_none() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _) = test_dispatcher_channel();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

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

        let custom_tiers = vec!["model".to_string(), "model.openrouter".to_string()];

        // First call: set a custom ladder.
        let resp1 = orchestrator
            .send_request(IpcRequest::ConfigureRole {
                agent_id: "agent-jane-01".into(),
                role_name: "developer".into(),
                guest_id: "agent-jane-01:developer".into(),
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
                fallback_tiers: Some(custom_tiers.clone()),
                model_bindings: None,
                content_policy: None,
            })
            .await
            .expect("first configure request");
        assert!(matches!(resp1, IpcResponse::ConfigureRoleOk { .. }));

        // Second call: an unrelated reconfigure (e.g. changing iteration_cap)
        // that does NOT touch fallback_tiers — must preserve the ladder.
        let resp2 = orchestrator
            .send_request(IpcRequest::ConfigureRole {
                agent_id: "agent-jane-01".into(),
                role_name: "developer".into(),
                guest_id: "agent-jane-01:developer".into(),
                calling_role: "orchestrator".into(),
                toolset_profile: "developer".into(),
                role_identity_addendum: Some("updated addendum".into()),
                role_manifest: None,
                is_admin: false,
                inactive_ttl_seconds: None,
                iteration_cap: Some(25),
                approval_policy: None,
                model_profile: None,
                context_window_policy: None,
                fallback_tiers: None,
                model_bindings: None,
                content_policy: None,
            })
            .await
            .expect("second configure request");
        assert!(matches!(resp2, IpcResponse::ConfigureRoleOk { .. }));

        let role = graph
            .get_role_incarnation("agent-jane-01", "developer")
            .expect("role lookup")
            .expect("role exists");
        assert_eq!(
            role.turn_loop_config.fallback_tiers, custom_tiers,
            "fallback_tiers must survive a reconfigure that passes None"
        );
        assert_eq!(role.turn_loop_config.iteration_cap, Some(25));

        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    /// A non-orchestrator incarnation may retune ITS OWN model routing (the
    /// operator's `/model` swap from inside a register like vixen), but only
    /// that: a model-selection-only change to its own EXISTING record. Any
    /// privileged field, a different role's record, or a nonexistent record
    /// stays orchestrator-gated.
    #[tokio::test]
    async fn configure_role_allows_self_model_retune_from_own_register() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _) = test_dispatcher_channel();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        // Pre-existing vixen record with a known toolset — the retune must
        // preserve it and only touch the model routing.
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-jane-01".into(),
                role_name: "vixen".into(),
                guest_id: "agent-jane-01:vixen".into(),
                toolset_profile: "orchestrator".into(),
                role_identity_addendum: Some("register addendum".into()),
                role_manifest: None,
                is_admin: false,
                readiness_state: RoleReadinessState::Configured,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig {
                    fallback_tiers: vec!["model.openrouter".into()],
                    ..Default::default()
                },
                home_node: None,
                ..Default::default()
            })
            .expect("seed vixen role");
        // A model-bindings change on an existing role is a breaking change and
        // restarts the role worker — give the test server a mock materializer.
        let requester = Arc::new(MockMaterializationRequester::default());
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        )
        .with_materialization_requester(requester);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut vixen = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-jane-01:vixen".into(),
            role: "role:agent-jane-01:vixen".into(),
            supported_tools: vec![],
        })
        .await
        .expect("vixen connect");

        let retune = |model_bindings: Option<std::collections::BTreeMap<String, String>>,
                      role_name: &str,
                      addendum: Option<String>| {
            IpcRequest::ConfigureRole {
                agent_id: "agent-jane-01".into(),
                role_name: role_name.into(),
                guest_id: format!("agent-jane-01:{role_name}"),
                calling_role: "vixen".into(),
                toolset_profile: "sneaky-elevated-toolset".into(),
                role_identity_addendum: addendum,
                role_manifest: None,
                is_admin: false,
                inactive_ttl_seconds: None,
                iteration_cap: None,
                approval_policy: None,
                model_profile: None,
                context_window_policy: None,
                fallback_tiers: Some(vec!["model.openrouter".into()]),
                model_bindings,
                content_policy: None,
            }
        };

        // Self model retune: allowed, and the caller-supplied toolset is ignored.
        let bindings = std::collections::BTreeMap::from([(
            "model.openrouter".to_string(),
            "sao10k/l3.1-euryale-70b".to_string(),
        )]);
        let resp = vixen
            .send_request(retune(Some(bindings.clone()), "vixen", None))
            .await
            .expect("self retune request");
        match resp {
            IpcResponse::ConfigureRoleOk { role_name } => assert_eq!(role_name, "vixen"),
            other => panic!("expected ConfigureRoleOk for self model retune, got {other:?}"),
        }
        let role = graph
            .get_role_incarnation("agent-jane-01", "vixen")
            .expect("role lookup")
            .expect("role exists");
        assert_eq!(role.turn_loop_config.model_bindings, bindings);
        assert_eq!(
            role.toolset_profile, "orchestrator",
            "self retune must preserve the existing toolset, not adopt the caller's"
        );
        assert_eq!(
            role.role_identity_addendum.as_deref(),
            Some("register addendum"),
            "self retune must not touch the addendum"
        );

        // A privileged field (addendum) from the register: still forbidden.
        let resp = vixen
            .send_request(retune(None, "vixen", Some("rewrite myself".into())))
            .await
            .expect("addendum request");
        assert!(
            matches!(resp, IpcResponse::Standard { ok: false, ref code, .. } if code == "CONFIGURE_FORBIDDEN"),
            "non-model change from a register must stay forbidden, got {resp:?}"
        );

        // Another role's record: still forbidden (would also be a CREATE here).
        let resp = vixen
            .send_request(retune(Some(bindings), "developer", None))
            .await
            .expect("cross-role request");
        assert!(
            matches!(resp, IpcResponse::Standard { ok: false, ref code, .. } if code == "CONFIGURE_FORBIDDEN"),
            "a register may only retune its own role, got {resp:?}"
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

    /// End-to-end config passthrough for the per-agent content policy feature:
    /// `role.configure` (via the `ConfigureRole` IPC) sets `content_policy`,
    /// it lands on the persisted `RoleIncarnationRecord`, an unrelated
    /// reconfigure with `content_policy: None` preserves it (same
    /// preserve-on-None contract as `fallback_tiers`), and an invalid value
    /// is rejected rather than silently stored.
    #[tokio::test]
    async fn configure_role_sets_and_preserves_content_policy() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _) = test_dispatcher_channel();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

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

        // A brand-new role with content_policy omitted defaults to "standard"
        // — nothing changes for agents that never touch this feature.
        let resp0 = orchestrator
            .send_request(IpcRequest::ConfigureRole {
                agent_id: "agent-jane-01".into(),
                role_name: "jane".into(),
                guest_id: "agent-jane-01:jane".into(),
                calling_role: "orchestrator".into(),
                toolset_profile: "companion".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                inactive_ttl_seconds: None,
                iteration_cap: None,
                approval_policy: None,
                model_profile: None,
                context_window_policy: None,
                fallback_tiers: None,
                model_bindings: None,
                content_policy: None,
            })
            .await
            .expect("create role request");
        assert!(matches!(resp0, IpcResponse::ConfigureRoleOk { .. }));
        let role = graph
            .get_role_incarnation("agent-jane-01", "jane")
            .expect("role lookup")
            .expect("role exists");
        assert_eq!(role.content_policy, "standard");

        // Explicitly set content_policy = "unrestricted".
        let resp1 = orchestrator
            .send_request(IpcRequest::ConfigureRole {
                agent_id: "agent-jane-01".into(),
                role_name: "jane".into(),
                guest_id: "agent-jane-01:jane".into(),
                calling_role: "orchestrator".into(),
                toolset_profile: "companion".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                inactive_ttl_seconds: None,
                iteration_cap: None,
                approval_policy: None,
                model_profile: None,
                context_window_policy: None,
                fallback_tiers: None,
                model_bindings: None,
                content_policy: Some("unrestricted".into()),
            })
            .await
            .expect("set content_policy request");
        assert!(matches!(resp1, IpcResponse::ConfigureRoleOk { .. }));
        let role = graph
            .get_role_incarnation("agent-jane-01", "jane")
            .expect("role lookup")
            .expect("role exists");
        assert_eq!(role.content_policy, "unrestricted");

        // An unrelated reconfigure with content_policy: None must PRESERVE
        // "unrestricted" — must not silently reset to "standard".
        let resp2 = orchestrator
            .send_request(IpcRequest::ConfigureRole {
                agent_id: "agent-jane-01".into(),
                role_name: "jane".into(),
                guest_id: "agent-jane-01:jane".into(),
                calling_role: "orchestrator".into(),
                toolset_profile: "companion".into(),
                role_identity_addendum: Some("updated addendum".into()),
                role_manifest: None,
                is_admin: false,
                inactive_ttl_seconds: None,
                iteration_cap: Some(30),
                approval_policy: None,
                model_profile: None,
                context_window_policy: None,
                fallback_tiers: None,
                model_bindings: None,
                content_policy: None,
            })
            .await
            .expect("unrelated reconfigure request");
        assert!(matches!(resp2, IpcResponse::ConfigureRoleOk { .. }));
        let role = graph
            .get_role_incarnation("agent-jane-01", "jane")
            .expect("role lookup")
            .expect("role exists");
        assert_eq!(
            role.content_policy, "unrestricted",
            "content_policy must survive a reconfigure that passes None"
        );
        assert_eq!(role.turn_loop_config.iteration_cap, Some(30));

        // An invalid value is rejected, not silently stored.
        let resp3 = orchestrator
            .send_request(IpcRequest::ConfigureRole {
                agent_id: "agent-jane-01".into(),
                role_name: "jane".into(),
                guest_id: "agent-jane-01:jane".into(),
                calling_role: "orchestrator".into(),
                toolset_profile: "companion".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                inactive_ttl_seconds: None,
                iteration_cap: None,
                approval_policy: None,
                model_profile: None,
                context_window_policy: None,
                fallback_tiers: None,
                model_bindings: None,
                content_policy: Some("permissive".into()),
            })
            .await
            .expect("invalid content_policy request");
        match resp3 {
            IpcResponse::Standard { ok: false, .. } | IpcResponse::Error(_) => {}
            other => panic!("expected an error response for invalid content_policy, got {other:?}"),
        }
        let role = graph
            .get_role_incarnation("agent-jane-01", "jane")
            .expect("role lookup")
            .expect("role exists");
        assert_eq!(
            role.content_policy, "unrestricted",
            "a rejected update must not have mutated the stored policy"
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

    /// Shape validation: `Some(vec![])` and tiers containing empty/whitespace
    /// strings must be rejected rather than silently accepted as a wipe.
    #[tokio::test]
    async fn configure_role_rejects_invalid_fallback_tiers_shape() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _) = test_dispatcher_channel();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );

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

        for bad_tiers in [Vec::<String>::new(), vec!["  ".to_string()]] {
            let resp = orchestrator
                .send_request(IpcRequest::ConfigureRole {
                    agent_id: "agent-jane-01".into(),
                    role_name: "developer".into(),
                    guest_id: "agent-jane-01:developer".into(),
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
                    fallback_tiers: Some(bad_tiers),
                    model_bindings: None,
                    content_policy: None,
                })
                .await
                .expect("configure request");
            match resp {
                IpcResponse::Standard { ok, code, .. } => {
                    assert!(!ok);
                    assert_eq!(code, "CONFIGURE_INVALID_FALLBACK_TIERS");
                }
                other => panic!("expected rejection, got {:?}", other),
            }
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
    async fn execute_role_create_workflow_persists_config_successfully() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _) = test_dispatcher_channel();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        let requester = Arc::new(MockMaterializationRequester::default());
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        )
        .with_materialization_requester(requester);

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
            .send_request(IpcRequest::ExecuteWorkflow {
                workflow_name: "role.create_or_update".into(),
                agent_id: "agent-jane-01".into(),
                calling_role: "orchestrator".into(),
                arguments: serde_json::json!({
                    "role_name": "developer",
                    "toolset_profile": "developer",
                    "role_identity_addendum": "Addendum",
                    "inactive_ttl_seconds": 60,
                    "iteration_cap": 10,
                    "approval_policy": "auto",
                    "model_profile": "fast",
                    "context_window_policy": "standard",
                    "reasoning": {
                        "purpose": "Focused implementation role.",
                        "toolset_rationale": "Use developer posture.",
                        "handoff_posture_and_limits": "Return when done."
                    }
                }),
            })
            .await
            .expect("workflow request");

        match resp {
            IpcResponse::WorkflowExecutionOk {
                workflow_name,
                result,
            } => {
                assert_eq!(workflow_name, "role.create_or_update");
                assert_eq!(result["role_name"].as_str(), Some("developer"));
            }
            other => panic!("expected WorkflowExecutionOk, got {:?}", other),
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

    /// The `role.create_or_update` workflow surface must plumb an explicit
    /// `fallback_tiers` argument array through to the persisted record, same
    /// as the direct ConfigureRole IPC path.
    #[tokio::test]
    async fn execute_role_create_workflow_sets_fallback_tiers() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _) = test_dispatcher_channel();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        let requester = Arc::new(MockMaterializationRequester::default());
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        )
        .with_materialization_requester(requester);

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
            .send_request(IpcRequest::ExecuteWorkflow {
                workflow_name: "role.create_or_update".into(),
                agent_id: "agent-jane-01".into(),
                calling_role: "orchestrator".into(),
                arguments: serde_json::json!({
                    "role_name": "developer",
                    "toolset_profile": "developer",
                    "fallback_tiers": ["model", "model.openrouter"],
                    "reasoning": {
                        "purpose": "Focused implementation role.",
                        "toolset_rationale": "Use developer posture.",
                        "handoff_posture_and_limits": "Return when done."
                    }
                }),
            })
            .await
            .expect("workflow request");
        assert!(matches!(resp, IpcResponse::WorkflowExecutionOk { .. }));

        let role = graph
            .get_role_incarnation("agent-jane-01", "developer")
            .expect("role lookup")
            .expect("role exists");
        assert_eq!(
            role.turn_loop_config.fallback_tiers,
            vec!["model".to_string(), "model.openrouter".to_string()]
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
    async fn configure_role_eagerly_materializes_new_role_worker() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _) = test_dispatcher_channel();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        let requester = Arc::new(MockMaterializationRequester::default());
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        )
        .with_materialization_requester(requester.clone());

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
                guest_id: "agent-jane:developer".into(),
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
                fallback_tiers: None,
                model_bindings: None,
                content_policy: None,
            })
            .await
            .expect("configure request");

        match resp {
            IpcResponse::ConfigureRoleOk { role_name } => assert_eq!(role_name, "developer"),
            other => panic!("expected ConfigureRoleOk, got {:?}", other),
        }

        assert_eq!(requester.calls.load(Ordering::SeqCst), 2);
        let role = graph
            .get_role_incarnation("agent-jane-01", "developer")
            .expect("role lookup")
            .expect("role exists");
        assert_eq!(role.guest_id, "agent-jane:developer");
        assert!(matches!(
            role.readiness_state,
            RoleReadinessState::Materializing
                | RoleReadinessState::Materialized
                | RoleReadinessState::Routable
        ));

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
        let (dispatcher_tx, _) = test_dispatcher_channel();
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
                fallback_tiers: None,
                model_bindings: None,
                content_policy: None,
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

    #[tokio::test]
    async fn configure_role_allows_model_selection_self_service_from_non_orchestrator() {
        // The operator's /model preset swap runs as the session's ACTIVE role
        // (philote sends calling_role = <active role>), so a session in e.g.
        // vixen posture retunes vixen's own record. A model-selection-only
        // change to the caller's own role must pass without orchestrator
        // posture; anything broader stays orchestrator-gated.
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _) = test_dispatcher_channel();
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        // Self-service may only retune an EXISTING record (never create one),
        // so seed the vixen role the caller will retune. The bindings change
        // is a breaking change that restarts the role worker, hence the mock
        // materializer.
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-jane-01".into(),
                role_name: "vixen".into(),
                guest_id: "agent-jane-01:vixen".into(),
                toolset_profile: "orchestrator".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                readiness_state: RoleReadinessState::Configured,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
            })
            .expect("seed vixen role");
        let requester = Arc::new(MockMaterializationRequester::default());
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        )
        .with_materialization_requester(requester);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut vixen = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-jane-01:vixen".into(),
            role: "vixen".into(),
            supported_tools: vec![],
        })
        .await
        .expect("vixen connect");

        let model_selection_request =
            |role_name: &str, role_manifest: Option<String>| -> IpcRequest {
                IpcRequest::ConfigureRole {
                    agent_id: "agent-jane-01".into(),
                    role_name: role_name.into(),
                    guest_id: format!("agent-jane-01:{role_name}"),
                    calling_role: "vixen".into(),
                    toolset_profile: "default".into(),
                    role_identity_addendum: None,
                    role_manifest,
                    is_admin: false,
                    inactive_ttl_seconds: None,
                    iteration_cap: None,
                    approval_policy: None,
                    model_profile: None,
                    context_window_policy: None,
                    fallback_tiers: Some(vec!["model.openrouter".into(), "model".into()]),
                    model_bindings: Some(
                        [("model.openrouter".to_string(), "z-ai/glm-5.2".to_string())]
                            .into_iter()
                            .collect(),
                    ),
                    content_policy: None,
                }
            };

        // Own role, model-selection-only → allowed.
        let resp = vixen
            .send_request(model_selection_request("vixen", None))
            .await
            .expect("self-service configure request");
        match resp {
            IpcResponse::ConfigureRoleOk { .. } => {}
            IpcResponse::Standard {
                ok, code, message, ..
            } => {
                panic!("expected ConfigureRoleOk, got ok={ok} code={code:?} msg={message:?}")
            }
            other => panic!("expected ConfigureRoleOk, got {:?}", other),
        }

        // A DIFFERENT role's record, even model-selection-only → forbidden.
        let resp = vixen
            .send_request(model_selection_request("researcher", None))
            .await
            .expect("cross-role configure request");
        match resp {
            IpcResponse::Standard { ok, code, .. } => {
                assert!(!ok);
                assert_eq!(code, "CONFIGURE_FORBIDDEN");
            }
            other => panic!("expected CONFIGURE_FORBIDDEN, got {:?}", other),
        }

        // Own role but with a privileged field (manifest) → forbidden: the
        // self-service exemption is model-selection-only by construction.
        let resp = vixen
            .send_request(model_selection_request(
                "vixen",
                Some("rewritten manifest".into()),
            ))
            .await
            .expect("privileged-field configure request");
        match resp {
            IpcResponse::Standard { ok, code, .. } => {
                assert!(!ok);
                assert_eq!(code, "CONFIGURE_FORBIDDEN");
            }
            other => panic!("expected CONFIGURE_FORBIDDEN, got {:?}", other),
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
    async fn handoff_back_delivers_return_task_to_orchestrator_inbox() {
        // Full round-trip: developer sends HandoffBack → aiua resolves the orchestrator
        // role from the session's primary_agent_id, delivers "handoff_return" to its
        // inbox, returns HandoffBackAck.
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = test_dispatcher_channel();
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));

        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-handoff-back".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-beacon-01".into()),
                active_incarnation_id: Some("agent-beacon-01:developer".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("999".into()),
                status: "active".into(),
                lease_owner_component_id: None,
                lease_expires_at: None,
                summary_json: serde_json::json!({}),
                created_at: 1,
                updated_at: 2,
            })
            .expect("session should seed");
        // Orchestrator role record — this is what resolve_role_incarnation looks up.
        graph
            .upsert_role_incarnation(&RoleIncarnationRecord {
                agent_id: "agent-beacon-01".into(),
                role_name: "orchestrator".into(),
                guest_id: "agent-beacon-01:orchestrator".into(),
                toolset_profile: "orchestrator".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                readiness_state: RoleReadinessState::Configured,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
            })
            .expect("orchestrator role should seed");

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

        // Orchestrator subscribes to its inbox before the handoff-back is sent.
        let mut orchestrator = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-beacon-01:orchestrator".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("orchestrator connect");
        orchestrator
            .send_request(IpcRequest::SubscribeInbox {
                role: "role:agent-beacon-01:orchestrator".into(),
            })
            .await
            .expect("orchestrator role inbox subscribe");

        // Developer role sends HandoffBack — triggers return to orchestrator.
        let mut developer = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-beacon-01:developer".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("developer connect");

        let response = developer
            .send_request(IpcRequest::HandoffBack {
                session_id: "sess-handoff-back".into(),
                summary: "task complete, returning to orchestrator".into(),
                return_to: Some("orchestrator".into()),
            })
            .await
            .expect("handoff back request");

        match response {
            IpcResponse::HandoffBackAck {
                return_guest_id,
                became_active,
            } => {
                assert_eq!(return_guest_id, "agent-beacon-01:orchestrator");
                assert!(
                    became_active,
                    "orchestrator is live so became_active should be true"
                );
            }
            other => panic!("unexpected handoff back response: {other:?}"),
        }

        let session = graph
            .get_session("sess-handoff-back")
            .expect("session lookup")
            .expect("session exists");
        assert_eq!(
            session.active_incarnation_id.as_deref(),
            Some("agent-beacon-01:orchestrator")
        );

        // Orchestrator inbox must receive the "handoff_return" task.
        let delivered = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            orchestrator.recv_task(),
        )
        .await
        .expect("orchestrator should receive handoff_return within 1s")
        .expect("orchestrator recv should succeed");

        match delivered {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("handoff_return payload should decode");
                assert_eq!(payload["action"], "handoff_return");
                assert_eq!(payload["session_id"], "sess-handoff-back");
                assert_eq!(
                    payload["summary"],
                    "task complete, returning to orchestrator"
                );
                assert_eq!(payload["from_incarnation_id"], "agent-beacon-01:developer");
            }
            other => panic!("unexpected orchestrator inbound task: {other:?}"),
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
    async fn handoff_back_defaults_return_to_orchestrator_when_return_to_is_none() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = test_dispatcher_channel();
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite");
        let graph = Arc::new(GraphDomain::new(Arc::new(graph_store.adapter())));

        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-handoff-back-default".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-beacon-01".into()),
                active_incarnation_id: Some("agent-beacon-01:developer".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("888".into()),
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
                agent_id: "agent-beacon-01".into(),
                role_name: "orchestrator".into(),
                guest_id: "agent-beacon-01:orchestrator".into(),
                toolset_profile: "orchestrator".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                readiness_state: RoleReadinessState::Configured,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
            })
            .expect("orchestrator role should seed");

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
            guest_id: "agent-beacon-01:orchestrator".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("orchestrator connect");
        orchestrator
            .send_request(IpcRequest::SubscribeInbox {
                role: "role:agent-beacon-01:orchestrator".into(),
            })
            .await
            .expect("subscribe");

        let mut developer = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-beacon-01:developer".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("developer connect");

        // No explicit return_to — handler defaults to "orchestrator".
        let response = developer
            .send_request(IpcRequest::HandoffBack {
                session_id: "sess-handoff-back-default".into(),
                summary: "done".into(),
                return_to: None,
            })
            .await
            .expect("handoff back with default return_to");

        assert!(
            matches!(response, IpcResponse::HandoffBackAck { ref return_guest_id, .. } if return_guest_id == "agent-beacon-01:orchestrator"),
            "default return_to must route to orchestrator, got: {response:?}"
        );
        let session = graph
            .get_session("sess-handoff-back-default")
            .expect("session lookup")
            .expect("session exists");
        assert_eq!(
            session.active_incarnation_id.as_deref(),
            Some("agent-beacon-01:orchestrator")
        );

        // Orchestrator should still receive the task.
        let delivered = tokio::time::timeout(
            tokio::time::Duration::from_secs(1),
            orchestrator.recv_task(),
        )
        .await
        .expect("orchestrator should receive handoff_return within 1s")
        .expect("recv ok");
        assert!(
            matches!(delivered, IpcResponse::InboundTask { ref task_json, .. } if task_json.contains("handoff_return")),
            "delivered task must contain handoff_return action"
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
    async fn handoff_back_materializes_configured_orchestrator_before_return() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = test_dispatcher_channel();
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite");
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
                    build_version: String::new(),
                },
                mesh_port: 9000,
                blob_port: 9001,
                execution_port: 9002,
                ipc_socket_path: socket_path.clone(),
                active_pid: None,
                mesh_host: None,
            })
            .expect("seed local hotel");
        graph
            .upsert_session(&SessionRecord {
                session_id: "sess-handoff-back-materialize".into(),
                session_kind: "conversation".into(),
                primary_agent_id: Some("agent-beacon-01".into()),
                active_incarnation_id: Some("agent-beacon-01:developer".into()),
                channel_kind: Some("telegram".into()),
                channel_session_key: Some("888".into()),
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
                agent_id: "agent-beacon-01".into(),
                role_name: "orchestrator".into(),
                guest_id: "agent-beacon-01:orchestrator".into(),
                toolset_profile: "orchestrator".into(),
                role_identity_addendum: None,
                role_manifest: None,
                is_admin: false,
                readiness_state: RoleReadinessState::Configured,
                inactive_ttl_seconds: None,
                turn_loop_config: TurnLoopConfig::default(),
                home_node: None,
                ..Default::default()
            })
            .expect("orchestrator role should seed");

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

        let mut developer = PhiloticClient::connect(GuestIdentity {
            guest_id: "agent-beacon-01:developer".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        })
        .await
        .expect("developer connect");

        let response = developer
            .send_request(IpcRequest::HandoffBack {
                session_id: "sess-handoff-back-materialize".into(),
                summary: "task complete, returning to orchestrator".into(),
                return_to: None,
            })
            .await
            .expect("handoff back request");

        match response {
            IpcResponse::HandoffPending { role_name, .. } => {
                assert_eq!(role_name, "orchestrator");
            }
            other => panic!("expected pending handoff back, got: {other:?}"),
        }
        assert_eq!(requester.calls.load(Ordering::SeqCst), 1);
        let session = graph
            .get_session("sess-handoff-back-materialize")
            .expect("session lookup")
            .expect("session exists");
        assert_eq!(
            session.active_incarnation_id.as_deref(),
            Some("agent-beacon-01:developer")
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

    #[test]
    fn resolve_binary_feasible_checks_absolute_paths_directly() {
        let dir = std::env::temp_dir().join(format!("philotic-r4-abs-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let real_file = dir.join("real-binary");
        std::fs::write(&real_file, b"#!/bin/sh\n").expect("write dummy binary");

        assert!(IpcServer::resolve_binary_feasible(
            real_file.to_str().expect("utf8 path")
        ));
        assert!(!IpcServer::resolve_binary_feasible(
            dir.join("no-such-binary").to_str().expect("utf8 path")
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_binary_feasible_checks_philotic_bin_dir() {
        let _env_guard = ipc_env_guard();
        let previous = std::env::var_os("PHILOTIC_BIN_DIR");
        let dir = std::env::temp_dir().join(format!("philotic-r4-bindir-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(dir.join("philote"), b"#!/bin/sh\n").expect("write dummy binary");
        unsafe {
            std::env::set_var("PHILOTIC_BIN_DIR", &dir);
        }

        assert!(IpcServer::resolve_binary_feasible("philote"));
        assert!(!IpcServer::resolve_binary_feasible("no-such-guest-binary"));

        unsafe {
            match &previous {
                Some(v) => std::env::set_var("PHILOTIC_BIN_DIR", v),
                None => std::env::remove_var("PHILOTIC_BIN_DIR"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_binary_feasible_falls_back_to_path() {
        let _env_guard = ipc_env_guard();
        let previous = std::env::var_os("PHILOTIC_BIN_DIR");
        unsafe {
            std::env::remove_var("PHILOTIC_BIN_DIR");
        }

        // `ls` is present on PATH in every dev/CI environment this test runs in.
        assert!(IpcServer::resolve_binary_feasible("ls"));
        assert!(!IpcServer::resolve_binary_feasible(
            "definitely-not-a-real-guest-binary-name"
        ));

        unsafe {
            if let Some(v) = &previous {
                std::env::set_var("PHILOTIC_BIN_DIR", v);
            }
        }
    }

    fn feasibility_test_role(fallback_tiers: Vec<String>) -> RoleIncarnationRecord {
        RoleIncarnationRecord {
            agent_id: "agent-beacon".into(),
            role_name: "orchestrator".into(),
            guest_id: "agent-beacon:orchestrator".into(),
            toolset_profile: "orchestrator".into(),
            is_admin: true,
            readiness_state: RoleReadinessState::Configured,
            turn_loop_config: TurnLoopConfig {
                fallback_tiers,
                ..TurnLoopConfig::default()
            },
            home_node: None,
            ..Default::default()
        }
    }

    #[test]
    fn evaluate_role_relocation_feasibility_reports_missing_controller_and_version_mismatch() {
        let _env_guard = ipc_env_guard();
        let previous = std::env::var_os("PHILOTIC_BIN_DIR");
        let dir = std::env::temp_dir().join(format!("philotic-r4-eval-a-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(dir.join("philote"), b"#!/bin/sh\n").expect("write dummy binary");
        unsafe {
            std::env::set_var("PHILOTIC_BIN_DIR", &dir);
        }

        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        // No controller guests seeded on this hotel at all.
        let role = feasibility_test_role(vec!["model.custom-controller".into()]);

        let reasons = IpcServer::evaluate_role_relocation_feasibility(
            &graph,
            "test-hotel",
            &role,
            Some("0.0.1-not-the-real-version"),
        );

        assert!(
            reasons
                .iter()
                .any(|r| r.contains("model.custom-controller")),
            "expected a missing-controller reason, got: {reasons:?}"
        );
        assert!(
            reasons.iter().any(|r| r.contains("build version mismatch")),
            "expected a version-mismatch reason, got: {reasons:?}"
        );

        unsafe {
            match &previous {
                Some(v) => std::env::set_var("PHILOTIC_BIN_DIR", v),
                None => std::env::remove_var("PHILOTIC_BIN_DIR"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn evaluate_role_relocation_feasibility_passes_when_everything_checks_out() {
        let _env_guard = ipc_env_guard();
        let previous = std::env::var_os("PHILOTIC_BIN_DIR");
        let dir = std::env::temp_dir().join(format!("philotic-r4-eval-b-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(dir.join("philote"), b"#!/bin/sh\n").expect("write dummy binary");
        unsafe {
            std::env::set_var("PHILOTIC_BIN_DIR", &dir);
        }

        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph = GraphDomain::new(Arc::new(graph_store.adapter()));
        graph
            .seed_guests(
                "test-hotel",
                &[GuestRecord {
                    hotel_name: "test-hotel".into(),
                    guest_id: "test-hotel:model-custom-controller".into(),
                    role: "model.custom-controller".into(),
                    config_json: "{}".into(),
                    is_active: true,
                    active_pid: None,
                    last_active_at: None,
                }],
            )
            .expect("seed controller guest");
        let role = feasibility_test_role(vec!["model.custom-controller".into()]);

        let reasons = IpcServer::evaluate_role_relocation_feasibility(
            &graph,
            "test-hotel",
            &role,
            Some(env!("CARGO_PKG_VERSION")),
        );

        assert!(
            reasons.is_empty(),
            "expected no decline reasons, got: {reasons:?}"
        );

        unsafe {
            match &previous {
                Some(v) => std::env::set_var("PHILOTIC_BIN_DIR", v),
                None => std::env::remove_var("PHILOTIC_BIN_DIR"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
