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
use ansible_mesh_core::graph::RoleReadinessState;
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

        // Remote role: dispatch over mesh to the role's home hotel.
        if let Some(ref home_node) = target_role.home_node.clone() {
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
            &role_name,
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
        if identity.role != "agent" {
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

        record.home_node = target_hotel.clone();
        if let Err(err) = graph.upsert_role_incarnation(&record) {
            return IpcResponse::error(
                "set_role_home",
                "SET_ROLE_HOME_PERSIST_FAILED",
                err.to_string(),
            );
        }

        info!(
            "Role '{}' (agent '{}') home_node set to {:?} by '{}'",
            role_name, agent_id, target_hotel, calling_role
        );
        IpcResponse::RoleHomeSet {
            role_name,
            home_node: target_hotel,
        }
    }
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
}
