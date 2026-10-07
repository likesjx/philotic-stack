//! Components: register, inventory, list, activate, restart, remove.
//!
//! Moved verbatim from `ipc/mod.rs` (IPC_DISPATCH_SPLIT); only item
//! visibility was widened so the parent module can reach it.

use super::*;

impl IpcServer {
    pub(in crate::service) async fn handle_register_component(
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

    pub(in crate::service) fn component_inventory_entries(
        graph: &GraphDomain,
        local_node_id: &str,
    ) -> anyhow::Result<Vec<ComponentInventoryEntryView>> {
        let hotel_name = match Self::local_hotel_name(graph, local_node_id) {
            Some(h) => h,
            None => {
                anyhow::bail!("local hotel record not found");
            }
        };

        let guests = graph.list_guests(&hotel_name, false)?;

        // Load tool_runner_registry once to enrich tool-runner entries with capabilities.
        let tool_registry: Vec<serde_json::Value> = graph
            .get_config_value("tool_runner_registry")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Vec<serde_json::Value>>(&s).ok())
            .unwrap_or_default();

        Ok(guests
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
                } else if g.role.contains("datasource") || g.role.contains("listener") {
                    "data"
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

                ComponentInventoryEntryView {
                    guest_id: g.guest_id,
                    role: g.role,
                    hotel: g.hotel_name,
                    command,
                    args: serde_json::from_value(serde_json::Value::Array(args))
                        .unwrap_or_default(),
                    env: serde_json::from_value(env).unwrap_or_default(),
                    component_type: component_type.into(),
                    is_active: g.is_active,
                    auto_start: g.is_active,
                    active_pid: g.active_pid,
                    last_active_at: g.last_active_at,
                    component_config,
                    capabilities,
                }
            })
            .collect())
    }

    pub(super) fn handle_list_components(graph: &GraphDomain, local_node_id: &str) -> IpcResponse {
        match Self::component_inventory_entries(graph, local_node_id) {
            Ok(components) => IpcResponse::ComponentInventory {
                components: components
                    .into_iter()
                    .map(|component| {
                        serde_json::to_value(component).unwrap_or(serde_json::Value::Null)
                    })
                    .collect(),
            },
            Err(err) => IpcResponse::error("list_components", "STORAGE_ERROR", err.to_string()),
        }
    }

    pub(in crate::service) async fn handle_set_component_active(
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

    pub(in crate::service) async fn handle_restart_component(
        graph: &GraphDomain,
        materialization_requester: Option<&dyn GuestMaterializationRequester>,
        local_node_id: &str,
        guest_id: &str,
        reason: RestartReason,
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

        // Flap protection for AUTOMATIC (heal-dispatcher) restarts only. Operator/CLI
        // restarts are deliberate and never budget-limited. We consult the shared
        // respawn budget BEFORE killing so a budget-exhausted guest is left running
        // rather than terminated-with-no-respawn. A missing requester falls through to
        // the NO_MATERIALIZER path below.
        if reason == RestartReason::Heal {
            if let Some(req) = materialization_requester {
                if req.check_heal_restart_budget(guest_id).await == HealRestartVerdict::Denied {
                    warn!(
                        guest_id = %guest_id,
                        "heal-restart skipped: guest exhausted its respawn budget"
                    );
                    return IpcResponse::error(
                        "restart_component",
                        "RESPAWN_BUDGET_EXHAUSTED",
                        format!(
                            "heal restart for {guest_id} skipped: respawn budget exhausted; \
                             restarts paused until a clean window elapses"
                        ),
                    );
                }
            }
        }

        if let Some(req) = materialization_requester {
            match req.restart_guest(guest_id).await {
                Ok(true) => {}
                Ok(false) => {
                    return IpcResponse::error(
                        "restart_component",
                        "SPAWN_FAILED",
                        format!("guest {guest_id} was not re-materialized"),
                    );
                }
                Err(e) => {
                    return IpcResponse::error("restart_component", "SPAWN_FAILED", e.to_string());
                }
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

    pub(in crate::service) async fn handle_remove_component(
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
}
