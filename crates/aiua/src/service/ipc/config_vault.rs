//! Config and vault: config get/set, secrets, rotation, vault entries.
//!
//! Moved verbatim from `ipc/mod.rs` (IPC_DISPATCH_SPLIT); only item
//! visibility was widened so the parent module can reach it.

use super::*;

impl IpcServer {
    pub(in crate::service) fn handle_add_vault_entry(
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
}

impl IpcServer {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn handle_get_config(
        key: String,
        local_node_id: &str,
        graph: &GraphDomain,
        inboxes: &InboxRegistry,
        registry: &Arc<RwLock<NodeRegistry>>,
    ) -> IpcResponse {
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
            return Self::handle_memory_delta_digest(graph, local_node_id, window_hours).await;
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

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_get_secret(
        secret_ref: String,
        graph: &GraphDomain,
        current_identity: &mut Option<GuestIdentity>,
    ) -> IpcResponse {
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

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_set_config(
        key: String,
        value_json: String,
        graph: &GraphDomain,
    ) -> IpcResponse {
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

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_rotate_secret(
        secret_ref: String,
        plaintext: String,
        graph: &GraphDomain,
    ) -> IpcResponse {
        info!("RotateSecret requested for ref: {}", secret_ref);
        match crate::vault::rotate_secret(graph, &secret_ref, &plaintext) {
            Ok(()) => IpcResponse::success("secret", None),
            Err(e) => IpcResponse::error("secret", "SECRET_ERROR", e.to_string()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn handle_add_vault_entry_request(
        vault_name: String,
        plaintext: String,
        allowed_roles: Vec<String>,
        secret_kind: Option<String>,
        graph: &GraphDomain,
    ) -> IpcResponse {
        info!("AddVaultEntry requested: {}", vault_name);
        match Self::handle_add_vault_entry(graph, vault_name, plaintext, allowed_roles, secret_kind)
        {
            Ok(secret_ref) => IpcResponse::success(
                "vault",
                Some(serde_json::json!({ "secret_ref": secret_ref })),
            ),
            Err(e) => IpcResponse::error("vault", "VAULT_ERROR", e.to_string()),
        }
    }
}
