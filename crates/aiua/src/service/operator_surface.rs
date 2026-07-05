//! Operator-target surface: IPC handlers for the `operator.targets.*`
//! query/mutation surface, extracted verbatim from `ipc.rs`.
//!
//! These handlers reuse the desktop-membrane view builders that remain in
//! `ipc.rs` (`desktop_membrane_target_views`, `desktop_membrane_target_status_view`,
//! `desktop_membrane_target_guest_inventory_view`) because those builders are
//! shared with the desktop-membrane IPC surface.

use super::ipc::{IpcServer, OPERATOR_SURFACE_QUERY_TIMEOUT_SECS};
use crate::service::guest_manager::GuestMaterializationRequester;
use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::registry::NodeRegistry;
use ansible_mesh_core::storage::ComponentManifest;
use philotic_client::{
    ComponentInventoryEntryView, GuestIdentity, IpcRequest, IpcResponse,
    OPERATOR_REMOTE_CONFIG_KEYS, OPERATOR_REMOTE_MUTABLE_CONFIG_KEYS,
    OPERATOR_SURFACE_QUERY_HANDOFF_KIND, OPERATOR_SURFACE_QUERY_REPLY_ROLE,
    OPERATOR_SURFACE_QUERY_ROLE, OperatorSurfaceQueryHandoff, OperatorTargetAgentInventoryView,
    OperatorTargetComponentInventoryView, OperatorTargetComponentMutationAckView,
    OperatorTargetConfigMutationAckView, OperatorTargetConfigView, OperatorTargetPlacementView,
    OperatorTargetRoleHomeAckView, OperatorTargetSecretEntryView,
    OperatorTargetSecretInventoryView, OperatorTargetSecretMutationAckView, PhiloticClient,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

impl IpcServer {
    /// Dispatch a single operator-target IPC request. Called from
    /// `IpcServer::process_request` for every `IpcRequest` variant belonging
    /// to the operator-target surface.
    pub(super) async fn handle_operator_target_request(
        request: IpcRequest,
        registry: &Arc<RwLock<NodeRegistry>>,
        graph: &GraphDomain,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        local_node_id: &str,
    ) -> IpcResponse {
        match request {
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
            IpcRequest::QueryOperatorTargetComponents { target_node_id } => {
                match Self::operator_target_component_inventory_view(
                    registry,
                    graph,
                    local_node_id,
                    &target_node_id,
                )
                .await
                {
                    Ok(operator_target_components) => IpcResponse::OperatorTargetComponentsView {
                        operator_target_components,
                    },
                    Err(err) => IpcResponse::error(
                        "operator_target_components",
                        "OPERATOR_TARGET_COMPONENTS_ERROR",
                        err.to_string(),
                    ),
                }
            }
            IpcRequest::QueryOperatorTargetConfig { target_node_id } => {
                match Self::operator_target_config_view(
                    registry,
                    graph,
                    local_node_id,
                    &target_node_id,
                )
                .await
                {
                    Ok(operator_target_config) => IpcResponse::OperatorTargetConfigView {
                        operator_target_config,
                    },
                    Err(err) => IpcResponse::error(
                        "operator_target_config",
                        "OPERATOR_TARGET_CONFIG_ERROR",
                        err.to_string(),
                    ),
                }
            }
            IpcRequest::QueryOperatorTargetSecrets { target_node_id } => {
                match Self::operator_target_secret_view(
                    registry,
                    graph,
                    local_node_id,
                    &target_node_id,
                )
                .await
                {
                    Ok(operator_target_secrets) => IpcResponse::OperatorTargetSecretsView {
                        operator_target_secrets,
                    },
                    Err(err) => IpcResponse::error(
                        "operator_target_secrets",
                        "OPERATOR_TARGET_SECRETS_ERROR",
                        err.to_string(),
                    ),
                }
            }
            IpcRequest::QueryOperatorTargetPlacement {
                target_node_id,
                agent_id,
                role_name,
                tool_name,
                required_markers,
                prefer_locality,
            } => match Self::operator_target_placement_view(
                registry,
                graph,
                local_node_id,
                &target_node_id,
                agent_id.as_deref(),
                role_name.as_deref(),
                tool_name.as_deref(),
                &required_markers,
                prefer_locality,
            )
            .await
            {
                Ok(operator_target_placement) => IpcResponse::OperatorTargetPlacementView {
                    operator_target_placement,
                },
                Err(err) => IpcResponse::error(
                    "operator_target_placement",
                    "OPERATOR_TARGET_PLACEMENT_ERROR",
                    err.to_string(),
                ),
            },
            IpcRequest::RegisterOperatorTargetComponent {
                target_node_id,
                manifest,
            } => match Self::mutate_operator_target_component(
                registry,
                graph,
                materialization_requester,
                local_node_id,
                &target_node_id,
                "register",
                Some(manifest),
                None,
                None,
            )
            .await
            {
                Ok(response) => response,
                Err(err) => IpcResponse::error(
                    "operator_target_component_register",
                    "OPERATOR_TARGET_COMPONENT_REGISTER_ERROR",
                    err.to_string(),
                ),
            },
            IpcRequest::SetOperatorTargetComponentActive {
                target_node_id,
                guest_id,
                active,
            } => match Self::mutate_operator_target_component(
                registry,
                graph,
                materialization_requester,
                local_node_id,
                &target_node_id,
                "set_active",
                None,
                Some(&guest_id),
                Some(active),
            )
            .await
            {
                Ok(response) => response,
                Err(err) => IpcResponse::error(
                    "operator_target_component_set_active",
                    "OPERATOR_TARGET_COMPONENT_SET_ACTIVE_ERROR",
                    err.to_string(),
                ),
            },
            IpcRequest::RestartOperatorTargetComponent {
                target_node_id,
                guest_id,
            } => match Self::mutate_operator_target_component(
                registry,
                graph,
                materialization_requester,
                local_node_id,
                &target_node_id,
                "restart",
                None,
                Some(&guest_id),
                None,
            )
            .await
            {
                Ok(response) => response,
                Err(err) => IpcResponse::error(
                    "operator_target_component_restart",
                    "OPERATOR_TARGET_COMPONENT_RESTART_ERROR",
                    err.to_string(),
                ),
            },
            IpcRequest::RemoveOperatorTargetComponent {
                target_node_id,
                guest_id,
            } => match Self::mutate_operator_target_component(
                registry,
                graph,
                materialization_requester,
                local_node_id,
                &target_node_id,
                "remove",
                None,
                Some(&guest_id),
                None,
            )
            .await
            {
                Ok(response) => response,
                Err(err) => IpcResponse::error(
                    "operator_target_component_remove",
                    "OPERATOR_TARGET_COMPONENT_REMOVE_ERROR",
                    err.to_string(),
                ),
            },
            IpcRequest::SetOperatorTargetConfig {
                target_node_id,
                key,
                value_json,
            } => match Self::mutate_operator_target_config(
                registry,
                graph,
                local_node_id,
                &target_node_id,
                &key,
                &value_json,
            )
            .await
            {
                Ok(response) => response,
                Err(err) => IpcResponse::error(
                    "operator_target_config_set",
                    "OPERATOR_TARGET_CONFIG_SET_ERROR",
                    err.to_string(),
                ),
            },
            IpcRequest::RotateOperatorTargetSecret {
                target_node_id,
                secret_ref,
                plaintext,
            } => match Self::mutate_operator_target_secret(
                registry,
                graph,
                local_node_id,
                "rotate",
                &target_node_id,
                Some(&secret_ref),
                None,
                &plaintext,
                &[],
            )
            .await
            {
                Ok(response) => response,
                Err(err) => IpcResponse::error(
                    "operator_target_secret_rotate",
                    "OPERATOR_TARGET_SECRET_ROTATE_ERROR",
                    err.to_string(),
                ),
            },
            IpcRequest::AddOperatorTargetVaultEntry {
                target_node_id,
                vault_name,
                plaintext,
                allowed_roles,
            } => match Self::mutate_operator_target_secret(
                registry,
                graph,
                local_node_id,
                "add",
                &target_node_id,
                None,
                Some(&vault_name),
                &plaintext,
                &allowed_roles,
            )
            .await
            {
                Ok(response) => response,
                Err(err) => IpcResponse::error(
                    "operator_target_secret_add",
                    "OPERATOR_TARGET_SECRET_ADD_ERROR",
                    err.to_string(),
                ),
            },
            IpcRequest::SetOperatorTargetRoleHome {
                target_node_id,
                agent_id,
                role_name,
                calling_role,
                target_hotel,
            } => match Self::mutate_operator_target_role_home(
                registry,
                graph,
                local_node_id,
                &target_node_id,
                &agent_id,
                &role_name,
                &calling_role,
                target_hotel,
            )
            .await
            {
                Ok(response) => response,
                Err(err) => IpcResponse::error(
                    "operator_target_role_home",
                    "OPERATOR_TARGET_ROLE_HOME_ERROR",
                    err.to_string(),
                ),
            },
            other => IpcResponse::error(
                "operator_targets",
                "OPERATOR_TARGET_REQUEST_UNSUPPORTED",
                format!("unexpected request routed to operator target surface: {other:?}"),
            ),
        }
    }

    pub(super) async fn operator_target_component_inventory_view(
        registry: &Arc<RwLock<NodeRegistry>>,
        graph: &GraphDomain,
        local_node_id: &str,
        target_node_id: &str,
    ) -> anyhow::Result<OperatorTargetComponentInventoryView> {
        let source_hotel = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;

        if target_node_id == local_node_id {
            return Ok(OperatorTargetComponentInventoryView {
                target_node_id: target_node_id.to_string(),
                target_hotel: source_hotel.clone(),
                source_hotel,
                observation_kind: "local-canonical".into(),
                available: true,
                pending_remote_query_state: "none".into(),
                components: Self::component_inventory_entries(graph, local_node_id)?,
                note: Some("derived from the local hotel's canonical component inventory".into()),
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

        match Self::query_remote_operator_target_components(
            graph,
            local_node_id,
            target_node_id,
            &target_hotel,
        )
        .await
        {
            Ok(view) => Ok(view),
            Err(err) => Ok(OperatorTargetComponentInventoryView {
                target_node_id: target_node_id.to_string(),
                target_hotel,
                source_hotel,
                observation_kind: "remote-query-failed".into(),
                available: false,
                pending_remote_query_state: "error".into(),
                components: Vec::new(),
                note: Some(format!(
                    "remote target component inventory requires a target-hotel operator query: {}",
                    err
                )),
            }),
        }
    }

    fn operator_target_config_entries(
        graph: &GraphDomain,
    ) -> anyhow::Result<BTreeMap<String, serde_json::Value>> {
        let mut config = BTreeMap::new();
        for key in OPERATOR_REMOTE_CONFIG_KEYS {
            let value = match graph.get_config_value(key) {
                Ok(Some(raw)) => {
                    serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::Value::String(raw))
                }
                Ok(None) => serde_json::Value::Null,
                Err(err) => {
                    anyhow::bail!("failed to load config key [{key}] from GraphStorage: {err}")
                }
            };
            config.insert((*key).to_string(), value);
        }
        Ok(config)
    }

    fn operator_target_secret_inventory(
        graph: &GraphDomain,
    ) -> anyhow::Result<OperatorTargetSecretInventoryView> {
        let vault_registry: Vec<serde_json::Value> = graph
            .get_config_value("vault_registry")?
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        let vault_entries = vault_registry
            .into_iter()
            .filter_map(|entry| {
                let name = entry
                    .get("vault_name")
                    .or_else(|| entry.get("name"))
                    .and_then(|v| v.as_str())?
                    .to_string();
                let secret_ref = entry
                    .get("secret_ref")
                    .and_then(|v| v.as_str())?
                    .to_string();
                Some(OperatorTargetSecretEntryView {
                    kind: "vault_token".into(),
                    name,
                    secret_ref: Some(secret_ref),
                    key: None,
                    configured: Some(true),
                })
            })
            .collect();

        let named_refs = [
            ("gemini_oauth_access_token", "gemini_oauth_access_token_ref"),
            (
                "gemini_oauth_refresh_token",
                "gemini_oauth_refresh_token_ref",
            ),
            ("telegram_bot_token", "telegram_bot_token"),
        ];
        let config_refs = named_refs
            .into_iter()
            .map(|(name, key)| OperatorTargetSecretEntryView {
                kind: "config_ref".into(),
                name: name.into(),
                secret_ref: None,
                key: Some(key.into()),
                configured: Some(graph.get_config_value(key).ok().flatten().is_some()),
            })
            .collect();

        Ok(OperatorTargetSecretInventoryView {
            target_node_id: String::new(),
            target_hotel: String::new(),
            source_hotel: String::new(),
            observation_kind: String::new(),
            available: true,
            pending_remote_query_state: "none".into(),
            vault_entries,
            config_refs,
            note: None,
        })
    }

    async fn operator_target_config_view(
        registry: &Arc<RwLock<NodeRegistry>>,
        graph: &GraphDomain,
        local_node_id: &str,
        target_node_id: &str,
    ) -> anyhow::Result<OperatorTargetConfigView> {
        let source_hotel = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;

        if target_node_id == local_node_id {
            return Ok(OperatorTargetConfigView {
                target_node_id: target_node_id.to_string(),
                target_hotel: source_hotel.clone(),
                source_hotel,
                observation_kind: "local-canonical".into(),
                available: true,
                pending_remote_query_state: "none".into(),
                config: Self::operator_target_config_entries(graph)?,
                note: Some("derived from the local hotel's bounded operator config surface".into()),
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

        match Self::query_remote_operator_target_config(
            graph,
            local_node_id,
            target_node_id,
            &target_hotel,
        )
        .await
        {
            Ok(view) => Ok(view),
            Err(err) => Ok(OperatorTargetConfigView {
                target_node_id: target_node_id.to_string(),
                target_hotel,
                source_hotel,
                observation_kind: "remote-query-failed".into(),
                available: false,
                pending_remote_query_state: "error".into(),
                config: BTreeMap::new(),
                note: Some(format!(
                    "remote target config inventory requires a target-hotel operator query: {}",
                    err
                )),
            }),
        }
    }

    async fn operator_target_secret_view(
        registry: &Arc<RwLock<NodeRegistry>>,
        graph: &GraphDomain,
        local_node_id: &str,
        target_node_id: &str,
    ) -> anyhow::Result<OperatorTargetSecretInventoryView> {
        let source_hotel = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;

        if target_node_id == local_node_id {
            let mut inventory = Self::operator_target_secret_inventory(graph)?;
            inventory.target_node_id = target_node_id.to_string();
            inventory.target_hotel = source_hotel.clone();
            inventory.source_hotel = source_hotel;
            inventory.observation_kind = "local-canonical".into();
            inventory.note =
                Some("derived from the local hotel's bounded secret-ref inventory".into());
            return Ok(inventory);
        }

        let guard = registry.read().await;
        let status = guard.get_node(target_node_id).ok_or_else(|| {
            anyhow::anyhow!(
                "mesh target [{target_node_id}] is not currently active in the registry"
            )
        })?;
        let target_hotel = Self::target_hotel_name(graph, status, &source_hotel);
        drop(guard);

        match Self::query_remote_operator_target_secrets(
            graph,
            local_node_id,
            target_node_id,
            &target_hotel,
        )
        .await
        {
            Ok(view) => Ok(view),
            Err(err) => Ok(OperatorTargetSecretInventoryView {
                target_node_id: target_node_id.to_string(),
                target_hotel,
                source_hotel,
                observation_kind: "remote-query-failed".into(),
                available: false,
                pending_remote_query_state: "error".into(),
                vault_entries: Vec::new(),
                config_refs: Vec::new(),
                note: Some(format!(
                    "remote target secret inventory requires a target-hotel operator query: {}",
                    err
                )),
            }),
        }
    }

    async fn operator_target_placement_view(
        registry: &Arc<RwLock<NodeRegistry>>,
        graph: &GraphDomain,
        local_node_id: &str,
        target_node_id: &str,
        agent_id: Option<&str>,
        role_name: Option<&str>,
        tool_name: Option<&str>,
        required_markers: &[String],
        prefer_locality: bool,
    ) -> anyhow::Result<OperatorTargetPlacementView> {
        let source_hotel = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;

        if target_node_id == local_node_id {
            return Ok(OperatorTargetPlacementView {
                target_node_id: target_node_id.to_string(),
                target_hotel: source_hotel.clone(),
                source_hotel,
                observation_kind: "local-canonical".into(),
                available: true,
                pending_remote_query_state: "none".into(),
                placement: Self::best_place_to_run_view(
                    registry,
                    graph,
                    local_node_id,
                    agent_id,
                    role_name,
                    tool_name,
                    required_markers,
                    prefer_locality,
                )
                .await?,
                note: Some("derived from the local hotel's canonical placement surface".into()),
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

        match Self::query_remote_operator_target_placement(
            graph,
            local_node_id,
            target_node_id,
            &target_hotel,
            agent_id,
            role_name,
            tool_name,
            required_markers,
            prefer_locality,
        )
        .await
        {
            Ok(view) => Ok(view),
            Err(err) => Ok(OperatorTargetPlacementView {
                target_node_id: target_node_id.to_string(),
                target_hotel,
                source_hotel,
                observation_kind: "remote-query-failed".into(),
                available: false,
                pending_remote_query_state: "error".into(),
                placement: serde_json::json!({}),
                note: Some(format!(
                    "remote target placement query requires a target-hotel operator query: {}",
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
        let reply = tokio::time::timeout(
            std::time::Duration::from_secs(OPERATOR_SURFACE_QUERY_TIMEOUT_SECS),
            client.recv_task(),
        )
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

    async fn query_remote_operator_target_config(
        graph: &GraphDomain,
        local_node_id: &str,
        target_node_id: &str,
        target_hotel: &str,
    ) -> anyhow::Result<OperatorTargetConfigView> {
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
            surface: "operator.targets.config".into(),
            request_id: Uuid::new_v4().to_string(),
            source_hotel: source_hotel.clone(),
            target_hotel: target_hotel.to_string(),
            target_node_id: target_node_id.to_string(),
            caller_kind: "operator_surface_adapter".into(),
            caller_id: local_node_id.to_string(),
            visibility_scope: "operator".into(),
            grant_scope: "default".into(),
            intent: "query target bounded config inventory".into(),
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
            other => anyhow::bail!("unexpected remote config query emit response: {other:?}"),
        }
        let reply = tokio::time::timeout(
            std::time::Duration::from_secs(OPERATOR_SURFACE_QUERY_TIMEOUT_SECS),
            client.recv_task(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for remote target config reply"))??;
        let IpcResponse::InboundTask { task_json, .. } = reply else {
            anyhow::bail!("unexpected remote target config reply envelope: {reply:?}");
        };
        let view: OperatorTargetConfigView = serde_json::from_str(&task_json)?;
        if view.target_node_id != target_node_id {
            anyhow::bail!(
                "remote target config reply target mismatch: expected [{}], got [{}]",
                target_node_id,
                view.target_node_id
            );
        }
        if view.target_hotel != target_hotel {
            anyhow::bail!(
                "remote target config reply hotel mismatch: expected [{}], got [{}]",
                target_hotel,
                view.target_hotel
            );
        }
        Ok(view)
    }

    async fn query_remote_operator_target_secrets(
        graph: &GraphDomain,
        local_node_id: &str,
        target_node_id: &str,
        target_hotel: &str,
    ) -> anyhow::Result<OperatorTargetSecretInventoryView> {
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
            surface: "operator.targets.secrets".into(),
            request_id: Uuid::new_v4().to_string(),
            source_hotel: source_hotel.clone(),
            target_hotel: target_hotel.to_string(),
            target_node_id: target_node_id.to_string(),
            caller_kind: "operator_surface_adapter".into(),
            caller_id: local_node_id.to_string(),
            visibility_scope: "operator".into(),
            grant_scope: "default".into(),
            intent: "query target secret-ref inventory".into(),
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
            other => anyhow::bail!("unexpected remote secret query emit response: {other:?}"),
        }
        let reply = tokio::time::timeout(
            std::time::Duration::from_secs(OPERATOR_SURFACE_QUERY_TIMEOUT_SECS),
            client.recv_task(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for remote target secret reply"))??;
        let IpcResponse::InboundTask { task_json, .. } = reply else {
            anyhow::bail!("unexpected remote target secret reply envelope: {reply:?}");
        };
        let view: OperatorTargetSecretInventoryView = serde_json::from_str(&task_json)?;
        if view.target_node_id != target_node_id {
            anyhow::bail!(
                "remote target secret reply target mismatch: expected [{}], got [{}]",
                target_node_id,
                view.target_node_id
            );
        }
        if view.target_hotel != target_hotel {
            anyhow::bail!(
                "remote target secret reply hotel mismatch: expected [{}], got [{}]",
                target_hotel,
                view.target_hotel
            );
        }
        Ok(view)
    }

    async fn query_remote_operator_target_placement(
        graph: &GraphDomain,
        local_node_id: &str,
        target_node_id: &str,
        target_hotel: &str,
        agent_id: Option<&str>,
        role_name: Option<&str>,
        tool_name: Option<&str>,
        required_markers: &[String],
        prefer_locality: bool,
    ) -> anyhow::Result<OperatorTargetPlacementView> {
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
            surface: "operator.targets.placement".into(),
            request_id: Uuid::new_v4().to_string(),
            source_hotel: source_hotel.clone(),
            target_hotel: target_hotel.to_string(),
            target_node_id: target_node_id.to_string(),
            caller_kind: "operator_surface_adapter".into(),
            caller_id: local_node_id.to_string(),
            visibility_scope: "operator".into(),
            grant_scope: "default".into(),
            intent: "query target placement recommendation".into(),
            payload: serde_json::json!({
                "target_node_id": target_node_id,
                "agent_id": agent_id,
                "role_name": role_name,
                "tool_name": tool_name,
                "required_markers": required_markers,
                "prefer_locality": prefer_locality,
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
            other => anyhow::bail!("unexpected remote placement query emit response: {other:?}"),
        }
        let reply = tokio::time::timeout(
            std::time::Duration::from_secs(OPERATOR_SURFACE_QUERY_TIMEOUT_SECS),
            client.recv_task(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for remote target placement reply"))??;
        let IpcResponse::InboundTask { task_json, .. } = reply else {
            anyhow::bail!("unexpected remote target placement reply envelope: {reply:?}");
        };
        let view: OperatorTargetPlacementView = serde_json::from_str(&task_json)?;
        if view.target_node_id != target_node_id {
            anyhow::bail!(
                "remote target placement reply target mismatch: expected [{}], got [{}]",
                target_node_id,
                view.target_node_id
            );
        }
        if view.target_hotel != target_hotel {
            anyhow::bail!(
                "remote target placement reply hotel mismatch: expected [{}], got [{}]",
                target_hotel,
                view.target_hotel
            );
        }
        Ok(view)
    }

    async fn query_remote_operator_target_components(
        graph: &GraphDomain,
        local_node_id: &str,
        target_node_id: &str,
        target_hotel: &str,
    ) -> anyhow::Result<OperatorTargetComponentInventoryView> {
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
            surface: "operator.targets.components".into(),
            request_id: Uuid::new_v4().to_string(),
            source_hotel: source_hotel.clone(),
            target_hotel: target_hotel.to_string(),
            target_node_id: target_node_id.to_string(),
            caller_kind: "operator_surface_adapter".into(),
            caller_id: local_node_id.to_string(),
            visibility_scope: "operator".into(),
            grant_scope: "default".into(),
            intent: "query target component inventory".into(),
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
            other => anyhow::bail!("unexpected remote component query emit response: {other:?}"),
        }
        let reply = tokio::time::timeout(
            std::time::Duration::from_secs(OPERATOR_SURFACE_QUERY_TIMEOUT_SECS),
            client.recv_task(),
        )
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for remote target component reply"))??;
        let IpcResponse::InboundTask { task_json, .. } = reply else {
            anyhow::bail!("unexpected remote target component reply envelope: {reply:?}");
        };
        let view: OperatorTargetComponentInventoryView = serde_json::from_str(&task_json)?;
        if view.target_node_id != target_node_id {
            anyhow::bail!(
                "remote target components reply target mismatch: expected [{}], got [{}]",
                target_node_id,
                view.target_node_id
            );
        }
        if view.target_hotel != target_hotel {
            anyhow::bail!(
                "remote target components reply hotel mismatch: expected [{}], got [{}]",
                target_hotel,
                view.target_hotel
            );
        }
        Ok(view)
    }

    async fn mutate_operator_target_component(
        registry: &Arc<RwLock<NodeRegistry>>,
        graph: &GraphDomain,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        local_node_id: &str,
        target_node_id: &str,
        operation: &str,
        manifest: Option<ComponentManifest>,
        guest_id: Option<&str>,
        active: Option<bool>,
    ) -> anyhow::Result<IpcResponse> {
        if target_node_id == local_node_id {
            return match operation {
                "register" => {
                    let manifest =
                        manifest.ok_or_else(|| anyhow::anyhow!("component manifest missing"))?;
                    match Self::handle_register_component(
                        graph,
                        materialization_requester,
                        manifest.clone(),
                    )
                    .await
                    {
                        IpcResponse::ComponentRegistered { .. } => {
                            let component = Self::component_inventory_entries(
                                graph,
                                local_node_id,
                            )?
                            .into_iter()
                            .find(|component| component.guest_id == manifest.guest_id)
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "registered component [{}] missing from refreshed inventory",
                                    manifest.guest_id
                                )
                            })?;
                            Ok(IpcResponse::ComponentInventory {
                                components: vec![
                                    serde_json::to_value(component)
                                        .unwrap_or(serde_json::Value::Null),
                                ],
                            })
                        }
                        other => Ok(other),
                    }
                }
                "set_active" => {
                    let guest_id = guest_id
                        .ok_or_else(|| anyhow::anyhow!("guest id missing"))?
                        .to_string();
                    let active = active.ok_or_else(|| anyhow::anyhow!("active state missing"))?;
                    match Self::handle_set_component_active(
                        graph,
                        materialization_requester,
                        local_node_id,
                        &guest_id,
                        active,
                    )
                    .await
                    {
                        IpcResponse::Standard { ok: true, .. } => Ok(
                            IpcResponse::OperatorTargetComponentMutationAckView {
                                operator_target_component_mutation:
                                    OperatorTargetComponentMutationAckView {
                                        target_node_id: local_node_id.to_string(),
                                        target_hotel: Self::local_hotel_name(
                                            graph,
                                            local_node_id,
                                        )
                                        .unwrap_or_default(),
                                        source_hotel: Self::local_hotel_name(
                                            graph,
                                            local_node_id,
                                        )
                                        .unwrap_or_default(),
                                        guest_id,
                                        operation: if active {
                                            "enable".into()
                                        } else {
                                            "disable".into()
                                        },
                                        ok: true,
                                        active: Some(active),
                                        note: Some(
                                            "derived from the target hotel's canonical component mutation path".into(),
                                        ),
                                    },
                            },
                        ),
                        other => Ok(other),
                    }
                }
                "restart" => {
                    let guest_id = guest_id
                        .ok_or_else(|| anyhow::anyhow!("guest id missing"))?
                        .to_string();
                    match Self::handle_restart_component(
                        graph,
                        materialization_requester,
                        local_node_id,
                        &guest_id,
                    )
                    .await
                    {
                        IpcResponse::Standard { ok: true, .. } => Ok(
                            IpcResponse::OperatorTargetComponentMutationAckView {
                                operator_target_component_mutation:
                                    OperatorTargetComponentMutationAckView {
                                        target_node_id: local_node_id.to_string(),
                                        target_hotel: Self::local_hotel_name(
                                            graph,
                                            local_node_id,
                                        )
                                        .unwrap_or_default(),
                                        source_hotel: Self::local_hotel_name(
                                            graph,
                                            local_node_id,
                                        )
                                        .unwrap_or_default(),
                                        guest_id,
                                        operation: "restart".into(),
                                        ok: true,
                                        active: None,
                                        note: Some(
                                            "derived from the target hotel's canonical component mutation path".into(),
                                        ),
                                    },
                            },
                        ),
                        other => Ok(other),
                    }
                }
                "remove" => {
                    let guest_id = guest_id
                        .ok_or_else(|| anyhow::anyhow!("guest id missing"))?
                        .to_string();
                    match Self::handle_remove_component(graph, local_node_id, &guest_id).await {
                        IpcResponse::Standard { ok: true, .. } => Ok(
                            IpcResponse::OperatorTargetComponentMutationAckView {
                                operator_target_component_mutation:
                                    OperatorTargetComponentMutationAckView {
                                        target_node_id: local_node_id.to_string(),
                                        target_hotel: Self::local_hotel_name(
                                            graph,
                                            local_node_id,
                                        )
                                        .unwrap_or_default(),
                                        source_hotel: Self::local_hotel_name(
                                            graph,
                                            local_node_id,
                                        )
                                        .unwrap_or_default(),
                                        guest_id,
                                        operation: "remove".into(),
                                        ok: true,
                                        active: None,
                                        note: Some(
                                            "derived from the target hotel's canonical component mutation path".into(),
                                        ),
                                    },
                            },
                        ),
                        other => Ok(other),
                    }
                }
                other => anyhow::bail!("unsupported target component mutation [{}]", other),
            };
        }

        let source_hotel = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;
        let guard = registry.read().await;
        let status = guard.get_node(target_node_id).ok_or_else(|| {
            anyhow::anyhow!(
                "mesh target [{target_node_id}] is not currently active in the registry"
            )
        })?;
        let target_hotel = Self::target_hotel_name(graph, status, &source_hotel);
        drop(guard);

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
            surface: match operation {
                "register" => "operator.targets.components.register",
                "set_active" => "operator.targets.components.set_active",
                "restart" => "operator.targets.components.restart",
                "remove" => "operator.targets.components.remove",
                other => anyhow::bail!("unsupported target component mutation [{}]", other),
            }
            .into(),
            request_id: Uuid::new_v4().to_string(),
            source_hotel: source_hotel.clone(),
            target_hotel: target_hotel.clone(),
            target_node_id: target_node_id.to_string(),
            caller_kind: "operator_surface_adapter".into(),
            caller_id: local_node_id.to_string(),
            visibility_scope: "operator".into(),
            grant_scope: "default".into(),
            intent: format!("mutate target component: {operation}"),
            payload: serde_json::json!({
                "target_node_id": target_node_id,
                "manifest": manifest,
                "guest_id": guest_id,
                "active": active,
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
            other => anyhow::bail!("unexpected remote component mutation emit response: {other:?}"),
        }
        let reply = tokio::time::timeout(std::time::Duration::from_secs(2), client.recv_task())
            .await
            .map_err(|_| {
                anyhow::anyhow!("timed out waiting for target component mutation reply")
            })??;
        let IpcResponse::InboundTask { task_json, .. } = reply else {
            anyhow::bail!("unexpected target component mutation reply envelope: {reply:?}");
        };

        if operation == "register" {
            let view: ComponentInventoryEntryView = serde_json::from_str(&task_json)?;
            return Ok(IpcResponse::ComponentInventory {
                components: vec![serde_json::to_value(view).unwrap_or(serde_json::Value::Null)],
            });
        }

        let ack: OperatorTargetComponentMutationAckView = serde_json::from_str(&task_json)?;
        Ok(IpcResponse::OperatorTargetComponentMutationAckView {
            operator_target_component_mutation: ack,
        })
    }

    async fn mutate_operator_target_config(
        registry: &Arc<RwLock<NodeRegistry>>,
        graph: &GraphDomain,
        local_node_id: &str,
        target_node_id: &str,
        key: &str,
        value_json: &str,
    ) -> anyhow::Result<IpcResponse> {
        if !OPERATOR_REMOTE_MUTABLE_CONFIG_KEYS.contains(&key) {
            anyhow::bail!("config key [{key}] is not operator-mutable on remote targets");
        }

        if target_node_id == local_node_id {
            match graph.set_config_value(key, value_json) {
                Ok(()) => {
                    let hotel_name =
                        Self::local_hotel_name(graph, local_node_id).unwrap_or_default();
                    return Ok(IpcResponse::OperatorTargetConfigMutationAckView {
                        operator_target_config_mutation: OperatorTargetConfigMutationAckView {
                            target_node_id: local_node_id.to_string(),
                            target_hotel: hotel_name.clone(),
                            source_hotel: hotel_name,
                            key: key.to_string(),
                            ok: true,
                            note: Some(
                                "derived from the target hotel's bounded operator config mutation path"
                                    .into(),
                            ),
                        },
                    });
                }
                Err(err) => anyhow::bail!("failed to store config key [{key}]: {err}"),
            }
        }

        let source_hotel = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
            anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
        })?;
        let guard = registry.read().await;
        let status = guard.get_node(target_node_id).ok_or_else(|| {
            anyhow::anyhow!(
                "mesh target [{target_node_id}] is not currently active in the registry"
            )
        })?;
        let target_hotel = Self::target_hotel_name(graph, status, &source_hotel);
        drop(guard);

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
            surface: "operator.targets.config.set".into(),
            request_id: Uuid::new_v4().to_string(),
            source_hotel: source_hotel.clone(),
            target_hotel: target_hotel.clone(),
            target_node_id: target_node_id.to_string(),
            caller_kind: "operator_surface_adapter".into(),
            caller_id: local_node_id.to_string(),
            visibility_scope: "operator".into(),
            grant_scope: "default".into(),
            intent: format!("mutate target bounded config key: {key}"),
            payload: serde_json::json!({
                "target_node_id": target_node_id,
                "key": key,
                "value_json": value_json,
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
            other => anyhow::bail!("unexpected remote config mutation emit response: {other:?}"),
        }
        let reply = tokio::time::timeout(std::time::Duration::from_secs(2), client.recv_task())
            .await
            .map_err(|_| anyhow::anyhow!("timed out waiting for target config mutation reply"))??;
        let IpcResponse::InboundTask { task_json, .. } = reply else {
            anyhow::bail!("unexpected target config mutation reply envelope: {reply:?}");
        };
        let ack: OperatorTargetConfigMutationAckView = serde_json::from_str(&task_json)?;
        Ok(IpcResponse::OperatorTargetConfigMutationAckView {
            operator_target_config_mutation: ack,
        })
    }

    async fn mutate_operator_target_secret(
        registry: &Arc<RwLock<NodeRegistry>>,
        graph: &GraphDomain,
        local_node_id: &str,
        operation: &str,
        target_node_id: &str,
        secret_ref: Option<&str>,
        vault_name: Option<&str>,
        plaintext: &str,
        allowed_roles: &[String],
    ) -> anyhow::Result<IpcResponse> {
        if target_node_id == local_node_id {
            let hotel_name = Self::local_hotel_name(graph, local_node_id).unwrap_or_default();
            match operation {
                "rotate" => {
                    let secret_ref = secret_ref
                        .ok_or_else(|| anyhow::anyhow!("secret ref missing for rotate"))?;
                    crate::vault::rotate_secret(graph, secret_ref, plaintext)?;
                    Ok(IpcResponse::OperatorTargetSecretMutationAckView {
                        operator_target_secret_mutation: OperatorTargetSecretMutationAckView {
                            target_node_id: local_node_id.to_string(),
                            target_hotel: hotel_name.clone(),
                            source_hotel: hotel_name,
                            operation: "rotate".into(),
                            secret_ref: Some(secret_ref.to_string()),
                            vault_name: None,
                            ok: true,
                            note: Some(
                                "derived from the target hotel's bounded secret rotation path"
                                    .into(),
                            ),
                        },
                    })
                }
                "add" => {
                    let vault_name =
                        vault_name.ok_or_else(|| anyhow::anyhow!("vault name missing for add"))?;
                    let secret_ref = Self::handle_add_vault_entry(
                        graph,
                        vault_name.to_string(),
                        plaintext.to_string(),
                        allowed_roles.to_vec(),
                    )?;
                    Ok(IpcResponse::OperatorTargetSecretMutationAckView {
                        operator_target_secret_mutation: OperatorTargetSecretMutationAckView {
                            target_node_id: local_node_id.to_string(),
                            target_hotel: hotel_name.clone(),
                            source_hotel: hotel_name,
                            operation: "add".into(),
                            secret_ref: Some(secret_ref),
                            vault_name: Some(vault_name.to_string()),
                            ok: true,
                            note: Some(
                                "derived from the target hotel's bounded vault-entry creation path"
                                    .into(),
                            ),
                        },
                    })
                }
                other => anyhow::bail!("unsupported target secret mutation [{}]", other),
            }
        } else {
            let source_hotel = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
                anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
            })?;
            let guard = registry.read().await;
            let status = guard.get_node(target_node_id).ok_or_else(|| {
                anyhow::anyhow!(
                    "mesh target [{target_node_id}] is not currently active in the registry"
                )
            })?;
            let target_hotel = Self::target_hotel_name(graph, status, &source_hotel);
            drop(guard);

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
                other => {
                    anyhow::bail!("unexpected query reply inbox subscribe response: {other:?}")
                }
            }
            let surface = match operation {
                "rotate" => "operator.targets.secrets.rotate",
                "add" => "operator.targets.secrets.add",
                other => anyhow::bail!("unsupported target secret mutation [{}]", other),
            };
            let task_json = serde_json::to_string(&OperatorSurfaceQueryHandoff {
                handoff_kind: OPERATOR_SURFACE_QUERY_HANDOFF_KIND.into(),
                surface: surface.into(),
                request_id: Uuid::new_v4().to_string(),
                source_hotel: source_hotel.clone(),
                target_hotel: target_hotel.clone(),
                target_node_id: target_node_id.to_string(),
                caller_kind: "operator_surface_adapter".into(),
                caller_id: local_node_id.to_string(),
                visibility_scope: "operator".into(),
                grant_scope: "default".into(),
                intent: format!("mutate target secret surface: {operation}"),
                payload: serde_json::json!({
                    "target_node_id": target_node_id,
                    "secret_ref": secret_ref,
                    "vault_name": vault_name,
                    "plaintext": plaintext,
                    "allowed_roles": allowed_roles,
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
                other => {
                    anyhow::bail!("unexpected remote secret mutation emit response: {other:?}")
                }
            }
            let reply = tokio::time::timeout(std::time::Duration::from_secs(2), client.recv_task())
                .await
                .map_err(|_| {
                    anyhow::anyhow!("timed out waiting for target secret mutation reply")
                })??;
            let IpcResponse::InboundTask { task_json, .. } = reply else {
                anyhow::bail!("unexpected target secret mutation reply envelope: {reply:?}");
            };
            let ack: OperatorTargetSecretMutationAckView = serde_json::from_str(&task_json)?;
            Ok(IpcResponse::OperatorTargetSecretMutationAckView {
                operator_target_secret_mutation: ack,
            })
        }
    }

    async fn mutate_operator_target_role_home(
        registry: &Arc<RwLock<NodeRegistry>>,
        graph: &GraphDomain,
        local_node_id: &str,
        target_node_id: &str,
        agent_id: &str,
        role_name: &str,
        calling_role: &str,
        target_hotel: Option<String>,
    ) -> anyhow::Result<IpcResponse> {
        if target_node_id == local_node_id {
            let hotel_name = Self::local_hotel_name(graph, local_node_id).unwrap_or_default();
            let calling_role_record = graph.get_role_incarnation(agent_id, calling_role);
            let is_admin = calling_role_record
                .ok()
                .flatten()
                .map(|r| r.is_admin || r.role_name == "orchestrator")
                .unwrap_or(false);
            if !is_admin {
                anyhow::bail!(
                    "role '{}' does not have authority to set home_node for other roles",
                    calling_role
                );
            }

            let mut record = graph
                .get_role_incarnation(agent_id, role_name)?
                .ok_or_else(|| {
                    anyhow::anyhow!("role '{}' not found for agent '{}'", role_name, agent_id)
                })?;
            record.home_node = target_hotel.clone();
            graph.upsert_role_incarnation(&record)?;

            Ok(IpcResponse::OperatorTargetRoleHomeAckView {
                operator_target_role_home: OperatorTargetRoleHomeAckView {
                    target_node_id: local_node_id.to_string(),
                    target_hotel: hotel_name.clone(),
                    source_hotel: hotel_name,
                    agent_id: agent_id.to_string(),
                    role_name: role_name.to_string(),
                    home_node: target_hotel,
                    ok: true,
                    note: Some(
                        "derived from the target hotel's canonical role-home placement path".into(),
                    ),
                },
            })
        } else {
            let source_hotel = Self::local_hotel_name(graph, local_node_id).ok_or_else(|| {
                anyhow::anyhow!("local hotel record missing for node [{local_node_id}]")
            })?;
            let guard = registry.read().await;
            let status = guard.get_node(target_node_id).ok_or_else(|| {
                anyhow::anyhow!(
                    "mesh target [{target_node_id}] is not currently active in the registry"
                )
            })?;
            let target_hotel_name = Self::target_hotel_name(graph, status, &source_hotel);
            drop(guard);

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
                other => {
                    anyhow::bail!("unexpected query reply inbox subscribe response: {other:?}")
                }
            }
            let task_json = serde_json::to_string(&OperatorSurfaceQueryHandoff {
                handoff_kind: OPERATOR_SURFACE_QUERY_HANDOFF_KIND.into(),
                surface: "operator.targets.roles.set_home".into(),
                request_id: Uuid::new_v4().to_string(),
                source_hotel: source_hotel.clone(),
                target_hotel: target_hotel_name.clone(),
                target_node_id: target_node_id.to_string(),
                caller_kind: "operator_surface_adapter".into(),
                caller_id: local_node_id.to_string(),
                visibility_scope: "operator".into(),
                grant_scope: "default".into(),
                intent: "mutate target role home placement".into(),
                payload: serde_json::json!({
                    "target_node_id": target_node_id,
                    "agent_id": agent_id,
                    "role_name": role_name,
                    "calling_role": calling_role,
                    "target_hotel": target_hotel,
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
                other => {
                    anyhow::bail!("unexpected remote role home mutation emit response: {other:?}")
                }
            }
            let reply = tokio::time::timeout(std::time::Duration::from_secs(2), client.recv_task())
                .await
                .map_err(|_| {
                    anyhow::anyhow!("timed out waiting for target role-home mutation reply")
                })??;
            let IpcResponse::InboundTask { task_json, .. } = reply else {
                anyhow::bail!("unexpected target role-home mutation reply envelope: {reply:?}");
            };
            let ack: OperatorTargetRoleHomeAckView = serde_json::from_str(&task_json)?;
            Ok(IpcResponse::OperatorTargetRoleHomeAckView {
                operator_target_role_home: ack,
            })
        }
    }
}
