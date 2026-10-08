//! Integrations: bindings, credentials, audit, runner materialization, operator OIDC exchange.
//!
//! Moved verbatim from `ipc/mod.rs` (IPC_DISPATCH_SPLIT); only item
//! visibility was widened so the parent module can reach it.

use super::*;

impl IpcServer {
    /// Resolve a binding's exit policy against the live mesh registry and map
    /// the policy hotel identity to its concrete node id.
    pub(super) async fn integration_binding_entry(
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

    pub(super) async fn materialize_integration_runner(
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

    pub(super) async fn exchange_operator_oidc(
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
}

impl IpcServer {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_register_integration_binding(
        binding: ansible_mesh_core::integration::IntegrationBinding,
        local_node_id: &str,
        graph: &GraphDomain,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        registry: &Arc<RwLock<NodeRegistry>>,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
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
        if let Err(error) = graph.set_config_value("__integration_bindings__", &serialized) {
            return IpcResponse::error(
                "integration_binding",
                "CONFIG_STORE_ERROR",
                error.to_string(),
            );
        }

        let entry = Self::integration_binding_entry(binding, registry, graph, local_node_id).await;
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

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_revoke_integration_binding(
        binding_id: String,
        owner_agent_id: String,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
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

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_get_integration_bindings(
        local_node_id: &str,
        graph: &GraphDomain,
        registry: &Arc<RwLock<NodeRegistry>>,
    ) -> IpcResponse {
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
                Self::integration_binding_entry(binding, registry, graph, local_node_id).await,
            );
        }
        entries.sort_by(|left, right| left.binding.binding_id.cmp(&right.binding.binding_id));
        IpcResponse::IntegrationBindingsState {
            integration_bindings: entries,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_exchange_operator_oidc(
        provider: String,
        authorization_code: String,
        code_verifier: String,
        redirect_uri: String,
        local_node_id: &str,
        socket_path: &str,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
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
                Some(serde_json::to_value(response).unwrap_or_else(|_| serde_json::json!({}))),
            ),
            Err(error) => {
                error!(provider, %error, "governed operator OIDC exchange failed");
                IpcResponse::error("operator_oidc", "OIDC_EXCHANGE_FAILED", error.to_string())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_record_integration_audit(
        audit: ansible_mesh_core::integration::HttpIntegrationAudit,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
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
            Err(error) => {
                IpcResponse::error("integration_audit", "CONFIG_STORE_ERROR", error.to_string())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_get_integration_audit(
        binding_id: Option<String>,
        limit: Option<u32>,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
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

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_provision_integration_credential(
        binding_id: String,
        owner_agent_id: String,
        credential: String,
        local_node_id: &str,
        graph: &GraphDomain,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        registry: &Arc<RwLock<NodeRegistry>>,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
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
        let entry =
            Self::integration_binding_entry(snapshot.clone(), registry, graph, local_node_id).await;
        let Some(execution_node_id) = entry.execution_node_id.clone() else {
            return IpcResponse::error(
                "integration_credential",
                "PLACEMENT_DENIED",
                match entry.placement {
                    ansible_mesh_core::integration::EgressPlacementDecision::Deny { reason } => {
                        reason
                    }
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
                Some(secret_ref) if graph.get_secret(&secret_ref).ok().flatten().is_some() => {
                    if let Err(error) = crate::vault::rotate_secret(graph, &secret_ref, &credential)
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
                    let Some(secret_ref) = operator_target_secret_mutation.secret_ref else {
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
                Some(credential_binding) => credential_binding.secret_ref = vault_ref.clone(),
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
            execution_node_id, rotated, "integration credential provisioned at execution hotel"
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
}
