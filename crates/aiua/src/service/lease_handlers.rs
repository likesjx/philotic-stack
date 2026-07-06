//! Per-transport runtime lease handlers: Telegram poll, desktop-membrane,
//! MCP-membrane, Discord-gateway, and subagent leases.
//!
//! Builds on the generic lease primitives in [`crate::service::lease`]
//! (`RuntimeLeaseRegistry`, `LeaseProvider`, `LeaseObserver`). The IPC
//! dispatch match arms remain in `ipc.rs` and delegate here via `Self::`.
//!
//! Extracted verbatim from `ipc.rs` — no behavior change.

use super::ipc::{InboxRegistry, IpcServer, SubagentHookRecord, SubagentHookRegistry, unix_ts};
use crate::service::guest_manager::GuestMaterializationRequester;
use crate::service::lease::{
    LeaseAcquireOutcome, LeaseObserver, LeaseObserverEvent, LeaseObserverEventKind, LeaseProvider,
    LeaseRenewOutcome, RuntimeLeaseRegistry,
};
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::graph::MembraneTransportHomeStatus;
use philotic_client::{GuestIdentity, IpcResponse, LeaseEnvelope, LeaseStatus, SubagentDelegation};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;

#[cfg(not(test))]
const TELEGRAM_POLL_LEASE_TTL_SECS: u64 = 90;
#[cfg(test)]
const TELEGRAM_POLL_LEASE_TTL_SECS: u64 = 1;

#[cfg(not(test))]
const DESKTOP_MEMBRANE_LEASE_TTL_SECS: u64 = 90;
#[cfg(test)]
const DESKTOP_MEMBRANE_LEASE_TTL_SECS: u64 = 1;

#[cfg(not(test))]
const DISCORD_GATEWAY_LEASE_TTL_SECS: u64 = 90;
#[cfg(test)]
const DISCORD_GATEWAY_LEASE_TTL_SECS: u64 = 1;

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

pub(super) struct LoggingSubagentLeaseObserver;

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

struct LoggingMcpMembraneLeaseObserver;

impl LeaseObserver for LoggingMcpMembraneLeaseObserver {
    fn on_event(&mut self, event: &LeaseObserverEvent) {
        match event.kind {
            LeaseObserverEventKind::Granted => info!(
                "MCP membrane lease [{}] granted to guest [{}] epoch {}.",
                event.lease.lease_scope, event.lease.owner_guest_id, event.lease.lease_epoch
            ),
            LeaseObserverEventKind::Released => info!(
                "MCP membrane lease [{}] released by guest [{}].",
                event.lease.lease_scope, event.lease.owner_guest_id
            ),
            LeaseObserverEventKind::Renewed => info!(
                "MCP membrane lease [{}] renewed by guest [{}] epoch {}.",
                event.lease.lease_scope, event.lease.owner_guest_id, event.lease.lease_epoch
            ),
            LeaseObserverEventKind::Expired => info!(
                "Dropping expired MCP membrane lease [{}] for guest [{}].",
                event.lease.lease_scope, event.lease.owner_guest_id
            ),
            LeaseObserverEventKind::StaleOwnerDropped => info!(
                "Dropping stale MCP membrane lease [{}] for guest [{}].",
                event.lease.lease_scope, event.lease.owner_guest_id
            ),
            LeaseObserverEventKind::Revoked => info!(
                "MCP membrane lease [{}] revoked for guest [{}].",
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
            // Remove ALL entries for this kind — not just the exact-ID match — so
            // stale bindings from a renamed or replaced membrane are evicted on
            // every successful lease acquisition.
            arr.retain(|b| b.get("kind").and_then(|k| k.as_str()) != Some(kind));
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

    /// Returns:
    /// - `Ok(Some(true))`  — transport home registered and matches this hotel
    /// - `Ok(Some(false))` — transport home registered but points elsewhere → MISMATCH
    /// - `Ok(None)`        — no transport home registered; caller should apply authority check
    /// - `Err(e)`          — DB lookup error
    fn hotel_may_poll_transport_home(
        graph: &GraphDomain,
        agent_id: &str,
        transport: &str,
        resource_ref: &str,
        local_hotel_name: &str,
    ) -> Result<Option<bool>, String> {
        match graph.resolve_membrane_transport_home(agent_id, transport, resource_ref) {
            Ok(Some(home)) => Ok(Some(
                home.status == MembraneTransportHomeStatus::Active
                    && home.active_home_hotel == local_hotel_name,
            )),
            Ok(None) => Ok(None),
            Err(err) => Err(err.to_string()),
        }
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

    pub(super) async fn remove_telegram_poll_leases(
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

    pub(super) async fn remove_desktop_membrane_leases(
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

    fn mcp_membrane_lease(
        lease_key: &str,
        authority_hotel: &str,
        local_node_id: &str,
        owner_guest_id: &str,
        port: u16,
    ) -> LeaseEnvelope {
        LeaseEnvelope {
            lease_type: "mcp_membrane".into(),
            lease_scope: lease_key.to_string(),
            authority_hotel: authority_hotel.to_string(),
            authority_component: Some("aiua".into()),
            owner_guest_id: owner_guest_id.to_string(),
            owner_hotel: Some(authority_hotel.to_string()),
            owner_component_type: Some("membrane.mcp".into()),
            lease_epoch: 0,
            lease_expires_at: 0,
            last_heartbeat_at: 0,
            status: LeaseStatus::Active,
            delegated_from: None,
            metadata: serde_json::json!({
                "authority_node_id": local_node_id,
                "port": port,
                "surface": "mcp",
            }),
        }
    }

    pub(super) async fn remove_mcp_membrane_leases(
        mcp_membrane_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        conn_id: Uuid,
    ) {
        let mut guard = mcp_membrane_leases.lock().await;
        let scopes: Vec<String> = guard
            .active_leases_for_connection(conn_id)
            .into_iter()
            .map(|lease| lease.lease_scope)
            .collect();
        let mut observer = LoggingMcpMembraneLeaseObserver;
        for scope in scopes {
            let _ = guard.release(&scope, conn_id, &mut observer);
        }
    }

    pub(super) async fn remove_discord_gateway_leases(
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

    pub(super) fn subagent_lease_scope(subagent_guest_id: &str) -> String {
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
    pub(super) async fn handle_spawn_subagent(
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

    // ── IPC lease request handlers (delegated from `process_request`) ──────

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_acquire_telegram_poll_lease(
        graph: &GraphDomain,
        local_node_id: &str,
        telegram_poll_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        conn_id: Uuid,
        current_identity: Option<&GuestIdentity>,
        lease_key: String,
        agent_id: String,
        resource_ref: Option<String>,
    ) -> IpcResponse {
        let Some(identity) = current_identity else {
            return IpcResponse::error(
                "telegram_poll_lease",
                "LEASE_UNREGISTERED",
                "guest must register before acquiring a Telegram poll lease",
            );
        };
        let Some(agent_identity) = graph.get_agent_identity(&agent_id).ok().flatten() else {
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
        let transport_resource_ref = resource_ref.as_deref().unwrap_or(&lease_key);
        match Self::hotel_may_poll_transport_home(
            graph,
            &agent_id,
            "telegram",
            transport_resource_ref,
            &local_hotel_name,
        ) {
            Ok(Some(true)) => {}
            Ok(Some(false)) => {
                return IpcResponse::error(
                    "telegram_poll_lease",
                    "LEASE_TRANSPORT_HOME_MISMATCH",
                    format!(
                        "hotel [{}] is not the active telegram transport home for agent [{}] resource [{}]",
                        local_hotel_name, agent_id, transport_resource_ref
                    ),
                );
            }
            Ok(None) => {
                if !Self::hotel_may_poll_for_agent(&agent_identity, &local_hotel_name) {
                    return IpcResponse::error(
                        "telegram_poll_lease",
                        "LEASE_FOREIGN_AUTHORITY",
                        format!(
                            "agent [{}] is owned by hotel [{}] and hotel [{}] is not authorized",
                            agent_id, agent_identity.authority_hotel, local_hotel_name
                        ),
                    );
                }
            }
            Err(err) => {
                return IpcResponse::error(
                    "telegram_poll_lease",
                    "LEASE_TRANSPORT_HOME_LOOKUP_FAILED",
                    err,
                );
            }
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
                Self::inject_membrane_binding(graph, &agent_id, "telegram", &identity.guest_id);
                IpcResponse::TelegramPollLease {
                    granted: true,
                    lease: Some(lease),
                }
            }
            LeaseAcquireOutcome::Denied(lease) => {
                info!(
                    "Telegram poll lease [{}] denied for guest [{}]; held by [{}] epoch {}.",
                    lease.lease_scope, identity.guest_id, lease.owner_guest_id, lease.lease_epoch
                );
                IpcResponse::TelegramPollLease {
                    granted: false,
                    lease: Some(lease),
                }
            }
        }
    }

    pub(super) async fn handle_get_telegram_poll_lease_owner(
        graph: &GraphDomain,
        local_node_id: &str,
        telegram_poll_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        lease_key: String,
    ) -> IpcResponse {
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

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_renew_telegram_poll_lease(
        graph: &GraphDomain,
        local_node_id: &str,
        telegram_poll_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        conn_id: Uuid,
        current_identity: Option<&GuestIdentity>,
        lease_key: String,
        agent_id: String,
        resource_ref: Option<String>,
        lease_epoch: u64,
    ) -> IpcResponse {
        let Some(identity) = current_identity else {
            return IpcResponse::error(
                "telegram_poll_lease",
                "LEASE_UNREGISTERED",
                "guest must register before renewing a Telegram poll lease",
            );
        };
        let Some(agent_identity) = graph.get_agent_identity(&agent_id).ok().flatten() else {
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
        let transport_resource_ref = resource_ref.as_deref().unwrap_or(&lease_key);
        match Self::hotel_may_poll_transport_home(
            graph,
            &agent_id,
            "telegram",
            transport_resource_ref,
            &local_hotel_name,
        ) {
            Ok(Some(true)) => {}
            Ok(Some(false)) => {
                return IpcResponse::error(
                    "telegram_poll_lease",
                    "LEASE_TRANSPORT_HOME_MISMATCH",
                    format!(
                        "hotel [{}] is not the active telegram transport home for agent [{}] resource [{}]",
                        local_hotel_name, agent_id, transport_resource_ref
                    ),
                );
            }
            Ok(None) => {
                if !Self::hotel_may_poll_for_agent(&agent_identity, &local_hotel_name) {
                    return IpcResponse::error(
                        "telegram_poll_lease",
                        "LEASE_FOREIGN_AUTHORITY",
                        format!(
                            "agent [{}] is owned by hotel [{}] and hotel [{}] is not authorized",
                            agent_id, agent_identity.authority_hotel, local_hotel_name
                        ),
                    );
                }
            }
            Err(err) => {
                return IpcResponse::error(
                    "telegram_poll_lease",
                    "LEASE_TRANSPORT_HOME_LOOKUP_FAILED",
                    err,
                );
            }
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

    pub(super) async fn handle_release_telegram_poll_lease(
        telegram_poll_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        conn_id: Uuid,
        current_identity: Option<&GuestIdentity>,
        lease_key: String,
    ) -> IpcResponse {
        let Some(identity) = current_identity else {
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

    pub(super) async fn handle_acquire_desktop_membrane_lease(
        graph: &GraphDomain,
        local_node_id: &str,
        desktop_membrane_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        conn_id: Uuid,
        current_identity: Option<&GuestIdentity>,
        lease_key: String,
        port: u16,
    ) -> IpcResponse {
        let Some(identity) = current_identity else {
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
                    lease.lease_scope, identity.guest_id, lease.owner_guest_id, lease.lease_epoch
                );
                IpcResponse::DesktopMembraneLease {
                    desktop_granted: false,
                    desktop_lease: Some(lease),
                }
            }
        }
    }

    pub(super) async fn handle_get_desktop_membrane_lease_owner(
        desktop_membrane_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        lease_key: String,
    ) -> IpcResponse {
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

    pub(super) async fn handle_renew_desktop_membrane_lease(
        desktop_membrane_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        conn_id: Uuid,
        current_identity: Option<&GuestIdentity>,
        lease_key: String,
        lease_epoch: u64,
    ) -> IpcResponse {
        let Some(identity) = current_identity else {
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

    pub(super) async fn handle_release_desktop_membrane_lease(
        desktop_membrane_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        conn_id: Uuid,
        current_identity: Option<&GuestIdentity>,
        lease_key: String,
    ) -> IpcResponse {
        let Some(identity) = current_identity else {
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

    pub(super) async fn handle_acquire_discord_gateway_lease(
        graph: &GraphDomain,
        local_node_id: &str,
        discord_gateway_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        conn_id: Uuid,
        current_identity: Option<&GuestIdentity>,
        lease_key: String,
        agent_id: String,
    ) -> IpcResponse {
        let Some(identity) = current_identity else {
            return IpcResponse::error(
                "discord_gateway_lease",
                "LEASE_UNREGISTERED",
                "guest must register before acquiring a Discord gateway lease",
            );
        };
        let Some(agent_identity) = graph.get_agent_identity(&agent_id).ok().flatten() else {
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
                Self::inject_membrane_binding(graph, &agent_id, "discord_text", &identity.guest_id);
                IpcResponse::DiscordGatewayLease {
                    granted: true,
                    lease: Some(lease),
                }
            }
            LeaseAcquireOutcome::Denied(lease) => {
                info!(
                    "Discord gateway lease [{}] denied for guest [{}]; held by [{}] epoch {}.",
                    lease.lease_scope, identity.guest_id, lease.owner_guest_id, lease.lease_epoch
                );
                IpcResponse::DiscordGatewayLease {
                    granted: false,
                    lease: Some(lease),
                }
            }
        }
    }

    pub(super) async fn handle_get_discord_gateway_lease_owner(
        graph: &GraphDomain,
        local_node_id: &str,
        discord_gateway_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        lease_key: String,
    ) -> IpcResponse {
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

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_renew_discord_gateway_lease(
        graph: &GraphDomain,
        local_node_id: &str,
        discord_gateway_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        conn_id: Uuid,
        current_identity: Option<&GuestIdentity>,
        lease_key: String,
        agent_id: String,
        lease_epoch: u64,
    ) -> IpcResponse {
        let Some(identity) = current_identity else {
            return IpcResponse::error(
                "discord_gateway_lease",
                "LEASE_UNREGISTERED",
                "guest must register before renewing a Discord gateway lease",
            );
        };
        let Some(agent_identity) = graph.get_agent_identity(&agent_id).ok().flatten() else {
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

    pub(super) async fn handle_release_discord_gateway_lease(
        discord_gateway_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        conn_id: Uuid,
        current_identity: Option<&GuestIdentity>,
        lease_key: String,
    ) -> IpcResponse {
        let Some(identity) = current_identity else {
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

    pub(super) async fn handle_acquire_mcp_membrane_lease(
        graph: &GraphDomain,
        local_node_id: &str,
        mcp_membrane_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        conn_id: Uuid,
        current_identity: Option<&GuestIdentity>,
        lease_key: String,
        port: u16,
    ) -> IpcResponse {
        let Some(identity) = current_identity else {
            return IpcResponse::error(
                "mcp_membrane_lease",
                "LEASE_UNREGISTERED",
                "guest must register before acquiring an MCP membrane lease",
            );
        };
        let Some(local_hotel_name) = Self::local_hotel_name(graph, local_node_id) else {
            return IpcResponse::error(
                "mcp_membrane_lease",
                "LEASE_AUTHORITY_UNKNOWN",
                format!(
                    "current hotel authority could not be resolved for node [{}]",
                    local_node_id
                ),
            );
        };

        let candidate = Self::mcp_membrane_lease(
            &lease_key,
            &local_hotel_name,
            local_node_id,
            &identity.guest_id,
            port,
        );
        let mut guard = mcp_membrane_leases.lock().await;
        let mut observer = LoggingMcpMembraneLeaseObserver;
        match guard.acquire(
            conn_id,
            candidate,
            DESKTOP_MEMBRANE_LEASE_TTL_SECS,
            unix_ts(),
            &mut observer,
        ) {
            LeaseAcquireOutcome::Granted(lease) => IpcResponse::McpMembraneLease {
                mcp_granted: true,
                mcp_lease: Some(lease),
            },
            LeaseAcquireOutcome::Denied(lease) => {
                info!(
                    "MCP membrane lease [{}] denied for guest [{}]; held by [{}] epoch {}.",
                    lease.lease_scope, identity.guest_id, lease.owner_guest_id, lease.lease_epoch
                );
                IpcResponse::McpMembraneLease {
                    mcp_granted: false,
                    mcp_lease: Some(lease),
                }
            }
        }
    }

    pub(super) async fn handle_renew_mcp_membrane_lease(
        mcp_membrane_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        conn_id: Uuid,
        lease_key: String,
        lease_epoch: u64,
    ) -> IpcResponse {
        let mut guard = mcp_membrane_leases.lock().await;
        let mut observer = LoggingMcpMembraneLeaseObserver;
        match guard.renew(
            &lease_key,
            conn_id,
            lease_epoch,
            DESKTOP_MEMBRANE_LEASE_TTL_SECS,
            unix_ts(),
            &mut observer,
        ) {
            LeaseRenewOutcome::Renewed(lease) => IpcResponse::McpMembraneLease {
                mcp_granted: true,
                mcp_lease: Some(lease),
            },
            LeaseRenewOutcome::Lost(lease) => IpcResponse::McpMembraneLease {
                mcp_granted: false,
                mcp_lease: lease,
            },
        }
    }

    pub(super) async fn handle_release_mcp_membrane_lease(
        mcp_membrane_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        conn_id: Uuid,
        lease_key: String,
    ) -> IpcResponse {
        let mut guard = mcp_membrane_leases.lock().await;
        let mut observer = LoggingMcpMembraneLeaseObserver;
        let _ = guard.release(&lease_key, conn_id, &mut observer);
        IpcResponse::success("release_mcp_membrane_lease", None)
    }

    pub(super) async fn handle_renew_subagent_lease(
        subagent_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        subagent_hooks: &SubagentHookRegistry,
        conn_id: Uuid,
        current_identity: Option<&GuestIdentity>,
        subagent_guest_id: String,
        lease_epoch: u64,
    ) -> IpcResponse {
        let Some(identity) = current_identity else {
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

    pub(super) async fn handle_release_subagent(
        graph: &GraphDomain,
        local_node_id: &str,
        subagent_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        subagent_hooks: &SubagentHookRegistry,
        conn_id: Uuid,
        subagent_guest_id: String,
    ) -> IpcResponse {
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

    pub(super) async fn handle_accept_subagent_lease(
        subagent_leases: &Arc<Mutex<RuntimeLeaseRegistry>>,
        subagent_guest_id: String,
    ) -> IpcResponse {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::ipc::test_dispatcher_channel;
    use crate::service::ipc::tests::{
        expect_config_data, ipc_env_guard, make_hotel_graph, test_socket_path,
    };
    use ansible_mesh_core::NodeCapabilities;
    use ansible_mesh_core::graph::MembraneTransportHomeRecord;
    use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
    use ansible_mesh_core::storage::{AgentIdentityRecord, GuestRecord, HotelRecord};
    use philotic_client::{
        IpcRequest, PhiloticClient, SubagentCompletionContract, SubagentContextPacket,
    };
    use std::path::Path;

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

    #[tokio::test]
    async fn spawn_subagent_acquires_lease_and_returns_ok() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _dispatcher_rx) = test_dispatcher_channel();
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
    async fn telegram_poll_lease_is_single_owner_and_released_on_disconnect() {
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
                resource_ref: None,
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
                resource_ref: None,
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
                resource_ref: None,
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
                resource_ref: None,
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
                resource_ref: None,
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
    async fn telegram_poll_lease_denies_non_home_transport_hotel() {
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
            .upsert_agent_identity(&AgentIdentityRecord {
                agent_id: "agent-beacon".into(),
                persona_name: "Beacon".into(),
                authority_hotel: "local-hotel".into(),
                bundle_json: serde_json::json!({}),
            })
            .expect("seed agent identity");
        graph
            .upsert_membrane_transport_home(&MembraneTransportHomeRecord {
                agent_id: "agent-beacon".into(),
                transport: "telegram".into(),
                resource_ref: "telegram_bot_token_beacon".into(),
                active_home_hotel: "vps-jane".into(),
                standby_hotels: vec!["local-hotel".into()],
                managed_by_role: "orchestrator".into(),
                lease_type: "telegram_poll".into(),
                failover_policy: "manual-or-explicit-delegation".into(),
                status: MembraneTransportHomeStatus::Active,
            })
            .expect("seed transport home");
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
                lease_key: "telegram:telegram_bot_token_beacon:deadbeefcafebabe".into(),
                agent_id: "agent-beacon".into(),
                resource_ref: Some("telegram_bot_token_beacon".into()),
            })
            .await
            .expect("transport home mismatch request");

        match response {
            IpcResponse::Standard {
                ok, code, message, ..
            } => {
                assert!(!ok);
                assert_eq!(code, "LEASE_TRANSPORT_HOME_MISMATCH");
                assert!(message.contains("local-hotel"));
                assert!(message.contains("telegram_bot_token_beacon"));
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
    async fn telegram_poll_lease_can_be_renewed_by_owner() {
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
                resource_ref: None,
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
                resource_ref: None,
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
                resource_ref: None,
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
                resource_ref: None,
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
                resource_ref: None,
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
                resource_ref: None,
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
                    config_json: serde_json::json!({ "command": "membrane-telegram" }).to_string(),
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
                resource_ref: None,
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

    fn expect_discord_gateway_lease(response: IpcResponse) -> (bool, Option<LeaseEnvelope>) {
        match response {
            IpcResponse::DiscordGatewayLease { granted, lease } => (granted, lease),
            // Untagged serde ambiguity: TelegramPollLease matches the same shape —
            // accept it and verify the inner lease_type is correct.
            IpcResponse::TelegramPollLease { granted, lease } => {
                if let Some(ref l) = lease {
                    assert_eq!(
                        l.lease_type, "discord_gateway",
                        "received TelegramPollLease but inner lease_type was '{}', not 'discord_gateway'",
                        l.lease_type
                    );
                }
                (granted, lease)
            }
            other => panic!("unexpected discord gateway lease response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn telegram_lease_injects_membrane_binding() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _rx) = test_dispatcher_channel();
        let graph = make_hotel_graph(&socket_path, "agent-jane-01");
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );
        let server_task = tokio::spawn(async move { server.run().await.expect("server run") });
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut membrane = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-telegram-01".into(),
            role: "membrane".into(),
            supported_tools: vec![],
        })
        .await
        .expect("connect membrane");

        // Acquire lease — must be granted.
        let lease_resp = membrane
            .send_request(IpcRequest::AcquireTelegramPollLease {
                lease_key: "telegram:bot_token:deadbeef".into(),
                agent_id: "agent-jane-01".into(),
                resource_ref: None,
            })
            .await
            .expect("lease request");
        let (granted, _) = expect_telegram_poll_lease(lease_resp);
        assert!(granted, "first acquire must be granted");

        // Fetch the agent bundle and assert the binding was injected.
        let bundle_resp = membrane
            .send_request(IpcRequest::GetConfig {
                key: "__agent_bundle__:agent-jane-01".into(),
            })
            .await
            .expect("get config");
        let bundle = expect_config_data(bundle_resp).expect("bundle must be present");
        let bindings = bundle
            .get("reflex_context")
            .and_then(|rc| rc.get("membrane_bindings"))
            .and_then(|b| b.as_array())
            .expect("reflex_context.membrane_bindings must be present");

        assert_eq!(
            bindings.len(),
            1,
            "exactly one binding after one lease grant"
        );
        assert_eq!(
            bindings[0].get("kind").and_then(|k| k.as_str()),
            Some("telegram"),
            "binding kind must be 'telegram'"
        );

        // Re-acquire (idempotent) — binding count must stay at 1.
        let lease_resp2 = membrane
            .send_request(IpcRequest::AcquireTelegramPollLease {
                lease_key: "telegram:bot_token:deadbeef".into(),
                agent_id: "agent-jane-01".into(),
                resource_ref: None,
            })
            .await
            .expect("re-acquire lease");
        let (granted2, _) = expect_telegram_poll_lease(lease_resp2);
        assert!(granted2, "re-acquire by same owner must succeed");

        let bundle_resp2 = membrane
            .send_request(IpcRequest::GetConfig {
                key: "__agent_bundle__:agent-jane-01".into(),
            })
            .await
            .expect("get config after re-acquire");
        let bundle2 = expect_config_data(bundle_resp2).expect("bundle");
        let bindings2 = bundle2
            .get("reflex_context")
            .and_then(|rc| rc.get("membrane_bindings"))
            .and_then(|b| b.as_array())
            .expect("bindings after re-acquire");
        assert_eq!(
            bindings2.len(),
            1,
            "idempotent re-acquire must not duplicate binding (got: {bindings2:?})"
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
    async fn discord_lease_injects_membrane_binding() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let (dispatcher_tx, _rx) = test_dispatcher_channel();
        let graph = make_hotel_graph(&socket_path, "agent-jane-01");
        let server = IpcServer::new(
            socket_path.clone(),
            "local-aiua-01",
            dispatcher_tx,
            graph.clone(),
        );
        let server_task = tokio::spawn(async move { server.run().await.expect("server run") });
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let mut membrane = PhiloticClient::connect(GuestIdentity {
            guest_id: "membrane-discord-01".into(),
            role: "membrane".into(),
            supported_tools: vec![],
        })
        .await
        .expect("connect membrane");

        // Acquire Discord gateway lease.
        let lease_resp = membrane
            .send_request(IpcRequest::AcquireDiscordGatewayLease {
                lease_key: "discord:bot_token:cafebabe".into(),
                agent_id: "agent-jane-01".into(),
            })
            .await
            .expect("discord lease request");
        let (granted, _) = expect_discord_gateway_lease(lease_resp);
        assert!(granted, "first discord acquire must be granted");

        // Fetch bundle and assert discord_text binding.
        let bundle_resp = membrane
            .send_request(IpcRequest::GetConfig {
                key: "__agent_bundle__:agent-jane-01".into(),
            })
            .await
            .expect("get config");
        let bundle = expect_config_data(bundle_resp).expect("bundle must be present");
        let bindings = bundle
            .get("reflex_context")
            .and_then(|rc| rc.get("membrane_bindings"))
            .and_then(|b| b.as_array())
            .expect("reflex_context.membrane_bindings must be present");

        assert_eq!(
            bindings.len(),
            1,
            "exactly one binding after discord lease grant"
        );
        assert_eq!(
            bindings[0].get("kind").and_then(|k| k.as_str()),
            Some("discord_text"),
            "binding kind must be 'discord_text'"
        );

        // Re-acquire — idempotent, no duplicate.
        let lease_resp2 = membrane
            .send_request(IpcRequest::AcquireDiscordGatewayLease {
                lease_key: "discord:bot_token:cafebabe".into(),
                agent_id: "agent-jane-01".into(),
            })
            .await
            .expect("re-acquire discord lease");
        let (granted2, _) = expect_discord_gateway_lease(lease_resp2);
        assert!(granted2);

        let bundle_resp2 = membrane
            .send_request(IpcRequest::GetConfig {
                key: "__agent_bundle__:agent-jane-01".into(),
            })
            .await
            .expect("get config after re-acquire");
        let bundle2 = expect_config_data(bundle_resp2).expect("bundle");
        let bindings2 = bundle2
            .get("reflex_context")
            .and_then(|rc| rc.get("membrane_bindings"))
            .and_then(|b| b.as_array())
            .expect("bindings after re-acquire");
        assert_eq!(
            bindings2.len(),
            1,
            "idempotent re-acquire must not duplicate discord binding (got: {bindings2:?})"
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
