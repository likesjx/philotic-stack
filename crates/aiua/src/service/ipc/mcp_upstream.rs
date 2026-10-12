//! MCP upstreams: registry, catalog reports, credentials, tool catalog.
//!
//! Handler bodies moved verbatim from the `process_request` match in
//! `ipc/mod.rs` (IPC_DISPATCH_SPLIT); parameters keep their declared types.

use super::*;

impl IpcServer {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_register_mcp_upstream(
        config: ansible_mesh_core::mcp_upstream::McpUpstreamConfig,
        local_node_id: &str,
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        registry: &Arc<RwLock<NodeRegistry>>,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
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
        let mut upstream_registry: std::collections::HashMap<String, McpUpstreamConfig> = graph
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
                    return IpcResponse::error("mcp_upstream", "CONFIG_STORE_ERROR", e.to_string());
                }
            }
            Err(e) => {
                return IpcResponse::error("mcp_upstream", "SERIALIZE_ERROR", e.to_string());
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
                if let Err(error) = graph.set_config_value("__integration_bindings__", &serialized)
                {
                    warn!(
                        upstream_id,
                        %error,
                        "failed to persist MCP transport integration binding"
                    );
                }
            }
            let entry =
                Self::integration_binding_entry(binding, registry, graph, local_node_id).await;
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
        let materialized = if let Some(hotel_name) = Self::local_hotel_name(graph, local_node_id) {
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

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_revoke_mcp_upstream(
        upstream_id: String,
        owner_agent_id: String,
        local_node_id: &str,
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
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

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_get_tool_catalog(graph: &GraphDomain) -> IpcResponse {
        match graph.list_abstract_tools() {
            Ok(tool_catalog) => IpcResponse::ToolCatalogState { tool_catalog },
            Err(err) => IpcResponse::Standard {
                ok: false,
                code: "tool_catalog_read_failed".into(),
                message: format!("tool catalog read failed: {err}"),
                corr_id: String::new(),
                data: None,
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_get_mcp_upstreams(graph: &GraphDomain) -> IpcResponse {
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

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_report_mcp_upstream_catalog(
        catalog: ansible_mesh_core::mcp_upstream::McpUpstreamCatalog,
        graph: &GraphDomain,
    ) -> IpcResponse {
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
                return IpcResponse::error("mcp_upstream", "SERIALIZE_ERROR", e.to_string());
            }
        }
        if let Err(e) = graph.set_config_value("__mcp_upstream_catalogs__", &{
            match serde_json::to_string(&catalogs) {
                Ok(j) => j,
                Err(e) => {
                    return IpcResponse::error("mcp_upstream", "SERIALIZE_ERROR", e.to_string());
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

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_provision_mcp_upstream_credential(
        upstream_id: String,
        owner_agent_id: String,
        credential: String,
        local_node_id: &str,
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        registry: &Arc<RwLock<NodeRegistry>>,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
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
        let (execution_node_id, placement) =
            if let Some(binding) = integration_bindings.get(&binding_id).cloned() {
                let entry =
                    Self::integration_binding_entry(binding, registry, graph, local_node_id).await;
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
                    ansible_mesh_core::integration::EgressPlacementDecision::Deny { reason } => {
                        reason
                    }
                    _ => "MCP transport has no execution node".into(),
                },
            );
        };

        let (vault_ref, rotated) = if execution_node_id == local_node_id {
            match config_snapshot.credential_ref.clone() {
                Some(existing_ref) if graph.get_secret(&existing_ref).ok().flatten().is_some() => {
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
                    let Some(secret_ref) = operator_target_secret_mutation.secret_ref else {
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
}
