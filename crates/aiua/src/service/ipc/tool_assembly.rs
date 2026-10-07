//! Tool assembly: live tool runners, component routes, incarnation selection.
//!
//! Moved verbatim from `ipc/mod.rs` (IPC_DISPATCH_SPLIT); only item
//! visibility was widened so the parent module can reach it.

use super::*;

#[derive(Debug, Clone)]
pub(in crate::service) struct LiveToolRunner {
    pub(super) guest_id: String,
    pub(super) supported_tools: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct LiveRoleSubscriberView {
    guest_id: String,
    role: String,
}

pub(super) async fn live_tool_runners(inboxes: &InboxRegistry) -> Vec<LiveToolRunner> {
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

pub(super) async fn live_role_subscribers(
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

pub(super) fn load_tool_runner_registry(
    graph: &GraphDomain,
) -> anyhow::Result<Vec<ToolRunnerRegistryEntry>> {
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

pub(super) fn merge_tool_runners(
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

pub(super) fn compose_component_route_assembly(
    bindings: &serde_json::Value,
    local_subscribers: &[LiveRoleSubscriberView],
    local_guest_roles: &[String],
    registry: &NodeRegistry,
    local_node_id: &str,
) -> serde_json::Value {
    // Find the highest-precedence `preferred_generation_capability` from agent reflex layers.
    // This lets a philote self-promote text.generate turns to response.generate (Gemini Live)
    // by storing a routing reflex via routing.reflex.set.
    let preferred_gen_cap: Option<String> = bindings
        .get("reflex_policy_agent_layers")
        .and_then(|v| v.as_array())
        .and_then(|layers| {
            layers
                .iter()
                .filter_map(|layer| {
                    let cap = layer
                        .get("reflexes")
                        .and_then(|r| r.get("preferred_generation_capability"))
                        .and_then(|v| v.as_str())?;
                    let precedence = layer
                        .get("precedence")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    Some((precedence, cap.to_string()))
                })
                .max_by_key(|(p, _)| *p)
                .map(|(_, cap)| cap)
        });

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
            .map(|mut route| {
                if capability == "text.generate" {
                    if let Some(cap) = &preferred_gen_cap {
                        if let Some(obj) = route.as_object_mut() {
                            obj.insert("target_capability".to_string(), serde_json::json!(cap));
                        }
                    }
                }
                (capability, route)
            })
        })
        .collect::<serde_json::Map<_, _>>();

    serde_json::json!({
        "execution_routes": execution_routes,
    })
}

pub(super) fn default_component_capabilities(bindings: &serde_json::Value) -> Vec<String> {
    let capabilities = declared_component_capabilities(bindings);
    let rights = bindings
        .get("effective_rights")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if rights.is_empty() {
        return capabilities;
    }

    capabilities
        .into_iter()
        .filter(|capability| has_right(&rights, &component_right(capability)))
        .collect()
}

pub(super) fn select_component_route(
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
    let allow_remote_execution = remote_component_execution_allowed(bindings);

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
                "explicit_pin": true,
            }));
        }

        if allow_remote_execution {
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
                    "explicit_pin": true,
                }));
            }
        }
    }

    // `explicit_pin` distinguishes a genuine operator/reflex `component_routes`
    // pin (`binding.is_some()`) from the hotel's implicit local default
    // (`target_role` fell through to `default_component_role`). Consumed by
    // philote's `resolve_model_execution_target` (routing drill 2026-07-09) so
    // an unconfigured hotel route no longer silently outranks a role's
    // `fallback_tiers` ladder for ladder-governed capabilities.
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
            "explicit_pin": binding.is_some(),
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
            "selection_reason": "local_active_guest_fallback",
            "explicit_pin": binding.is_some(),
        }));
    }

    if allow_remote_execution {
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
                "explicit_pin": binding.is_some(),
            }));
        }
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
        "explicit_pin": binding.is_some(),
    }))
}

pub(super) fn component_implementation_to_role(implementation: &str) -> String {
    let normalized = implementation.trim().to_ascii_lowercase();
    if normalized.starts_with("model.") {
        return normalized;
    }
    let prefix = normalized
        .split(['.', '-', '@', '/'])
        .find(|segment| !segment.is_empty())
        .unwrap_or("gemini");

    match prefix {
        "elevenlabs" => "model.elevenlabs".into(),
        "onnx" | "local" => "model.local".into(),
        _ => "model".into(),
    }
}

pub(super) fn default_component_role(capability: &str) -> &'static str {
    match capability {
        "voice.synthesize" => "model.elevenlabs",
        "voice.transcribe" => "model.local",
        _ => "model",
    }
}

pub(super) fn select_remote_component_advertisement(
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

pub(in crate::service) fn compose_tool_assembly(
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
            let marker = shared_tool_receptor_record(bindings, tool_name);
            serde_json::json!({
                "tool_name": tool_name,
                "description": marker
                    .and_then(|value: &serde_json::Value| value.get("description"))
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!(format!("Execute the {} tool.", tool_name))),
                "input_schema": marker
                    .and_then(|value: &serde_json::Value| value.get("input_schema"))
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({ "type": "object" }))
            })
        })
        .collect::<Vec<_>>();

    let execution_routes = toolset
        .iter()
        .map(|tool_name| {
            // Pinned takes precedence over the shared local-agent allowlist:
            // desktop.observe is in BOTH sets (philote's own assembly routes
            // it local_agent as a metadata stub, while the hotel-side
            // assembly pins it to a real desktop runner). The pre-union
            // hotel routing is preserved deliberately — the allowlist
            // unification (aria-mesh-steward slice 1) merged the lists, it
            // did not adjudicate this routing disagreement.
            if is_local_agent_tool(tool_name) && !is_pinned_tool(tool_name) {
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
                if tool_has_marker(bindings, tool_name, "local_only") {
                    return None;
                }
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
            let marker = shared_tool_receptor_record(bindings, tool_name);
            let tool_markers = tool_ligand_markers(bindings, tool_name);
            (
                tool_name.to_string(),
                serde_json::json!({
                    "policy_class": marker
                        .and_then(|value: &serde_json::Value| value.get("class"))
                        .and_then(serde_json::Value::as_str)
                        .map(|class| format!("tool:{class}"))
                        .unwrap_or_else(|| format!("tool:{tool_name}")),
                    "approval_required": tool_markers.iter().any(|marker| marker == "high_agency"),
                    "credential_scope_reflex": credential_scope_reflex(bindings),
                    "tool_markers": tool_markers,
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

pub(super) fn default_visible_toolset(bindings: &serde_json::Value) -> Vec<String> {
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
    if let Some(classes) = bindings
        .get("allowed_classes")
        .and_then(serde_json::Value::as_array)
    {
        for class in classes.iter().filter_map(serde_json::Value::as_str) {
            for tool in tools_for_allowed_class(class) {
                if !toolset.iter().any(|existing| existing == tool) {
                    toolset.push(tool.to_string());
                }
            }
        }
    }
    let rights = bindings
        .get("effective_rights")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if rights.is_empty() {
        return toolset;
    }

    toolset
        .into_iter()
        .filter(|tool_name| has_right(&rights, &tool_right(tool_name)))
        .collect()
}

pub(super) fn tools_for_allowed_class(class: &str) -> &'static [&'static str] {
    // Shared with philote's session assembly so a class granted in a
    // ToolsetProfileRecord expands identically on both sides of the IPC
    // boundary. See ansible_mesh_core::graph::tools_for_tool_class.
    ansible_mesh_core::graph::tools_for_tool_class(class)
}

pub(super) fn shared_tool_receptor_record<'a>(
    bindings: &'a serde_json::Value,
    tool_name: &str,
) -> Option<&'a serde_json::Value> {
    bindings
        .get("shared_tool_markers")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .find(|marker| {
            marker.get("tool_name").and_then(serde_json::Value::as_str) == Some(tool_name)
        })
}

pub(super) fn tool_ligand_markers(bindings: &serde_json::Value, tool_name: &str) -> Vec<String> {
    shared_tool_receptor_record(bindings, tool_name)
        .and_then(|marker: &serde_json::Value| marker.get("tool_markers"))
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

pub(super) fn tool_has_marker(
    bindings: &serde_json::Value,
    tool_name: &str,
    marker_name: &str,
) -> bool {
    tool_ligand_markers(bindings, tool_name)
        .iter()
        .any(|marker| marker == marker_name)
}

pub(super) fn remote_tool_advertisements(
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

pub(super) fn select_remote_tool_advertisement(
    remote_tool_ads: &[CapabilityAdvertisement],
    tool_name: &str,
    bindings: &serde_json::Value,
) -> Option<CapabilityAdvertisement> {
    if !remote_tool_execution_allowed(bindings) {
        return None;
    }
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

pub(super) fn remote_available_capacity(advertisement: &CapabilityAdvertisement) -> i64 {
    i64::from(advertisement.max_concurrent_jobs.unwrap_or(0))
        - i64::from(advertisement.active_jobs)
        - i64::from(advertisement.queue_depth)
}

pub(super) fn is_local_agent_tool(tool_name: &str) -> bool {
    // Delegates to the shared allowlist so the hotel-side compose_tool_assembly
    // and the agent-side route assembly can never diverge again (they had
    // drifted into two different lists before the aria-mesh-steward slice).
    ansible_mesh_core::local_agent_tools::is_local_agent_tool(tool_name)
}

/// Operational-admin gate for agent-originated heal/steward mutations
/// (aria-mesh-steward slice 1: heal.resolve, heal.close_work_item,
/// session.repair_stale, component.restart).
///
/// Classifies the calling connection's registered identity:
/// - `Ok(false)` — NOT an agent (heal-dispatcher, `phil` CLI, web, operator
///   surface): proceed exactly as before this gate existed.
/// - `Ok(true)` — an agent caller WITH operational admin authority
///   (DEF-058 tier: `is_admin` OR the orchestrator persona).
/// - `Err(refusal)` — an agent caller WITHOUT operational admin authority.
///
/// Agent detection mirrors `is_agent_handoff_caller`: the base philote
/// registers `role == "agent"` with `guest_id == agent_id`; role-incarnation
/// workers register `guest_id == "{agent_id}:{role_name}"` and are matched by
/// their role-incarnation records. Authority resolution: a role worker is
/// judged by its own incarnation record; the single-process base guest hosts
/// every role in-process and is judged by its agent's orchestrator
/// incarnation (its management posture) — the per-session active role is not
/// visible at the IPC layer, so this is deliberately the coarse tier, with
/// philote-side toolset projection as the finer-grained layer.
pub(super) fn steward_agent_admin_gate(
    graph: &GraphDomain,
    current_identity: Option<&GuestIdentity>,
    op: &str,
) -> Result<bool, IpcResponse> {
    let Some(identity) = current_identity else {
        return Ok(false);
    };
    let incarnations = graph
        .list_role_incarnations_by_guest_id(&identity.guest_id)
        .unwrap_or_default();
    let is_agent_caller = identity.role == "agent" || !incarnations.is_empty();
    if !is_agent_caller {
        return Ok(false);
    }
    let authorized = if incarnations.is_empty() {
        graph
            .get_role_incarnation(&identity.guest_id, "orchestrator")
            .ok()
            .flatten()
            .map(|r| r.has_operational_admin_authority())
            .unwrap_or(false)
    } else {
        incarnations
            .iter()
            .any(|r| r.has_operational_admin_authority())
    };
    if authorized {
        Ok(true)
    } else {
        Err(IpcResponse::error(
            op,
            "ADMIN_REQUIRED",
            format!(
                "guest '{}' (role '{}') lacks operational admin authority for mutating \
                 heal/steward operations",
                identity.guest_id, identity.role
            ),
        ))
    }
}

pub(super) fn is_pinned_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "workspace.list"
            | "workspace.read"
            | "workspace.search"
            | "workspace.write"
            | "desktop.observe"
    )
}

pub(super) fn task_runner_kind_for_tool(tool_name: &str) -> Option<&'static str> {
    if tool_name.starts_with("workspace.") {
        return Some("workspace");
    }

    if tool_name.starts_with("shell.") {
        return Some("shell");
    }

    if tool_name.starts_with("desktop.") {
        return Some("desktop");
    }

    None
}

pub(super) fn task_runner_base_config_for_tool(
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

pub(super) fn parse_allowed_incarnations(
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

pub(super) fn parse_routing_preferences(bindings: &serde_json::Value) -> RoutingPreferences {
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

pub(super) fn compose_tool_assembly_from_incarnations(
    bindings: &serde_json::Value,
    incarnations: &[AllowedIncarnation],
) -> serde_json::Value {
    let preferences = parse_routing_preferences(bindings);
    let rights = bindings
        .get("effective_rights")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut toolset = {
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
    if !rights.is_empty() {
        toolset.retain(|tool_name| has_right(&rights, &tool_right(tool_name)));
    }

    let tools_for_model = toolset
        .iter()
        .map(|tool_name| {
            let marker = shared_tool_receptor_record(bindings, tool_name);
            serde_json::json!({
                "tool_name": tool_name,
                "description": marker
                    .and_then(|value: &serde_json::Value| value.get("description"))
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!(format!("Execute the {} tool.", tool_name))),
                "input_schema": marker
                    .and_then(|value: &serde_json::Value| value.get("input_schema"))
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({ "type": "object" }))
            })
        })
        .collect::<Vec<_>>();

    let execution_routes = toolset
        .iter()
        .filter_map(|tool_name| {
            if tool_has_marker(bindings, tool_name, "local_only")
                && incarnations.iter().all(|incarnation| {
                    incarnation
                        .target_node
                        .as_deref()
                        .map(|node| node != "local-aiua-01")
                        .unwrap_or(true)
                })
            {
                return None;
            }
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
            let marker = shared_tool_receptor_record(bindings, tool_name);
            let tool_markers = tool_ligand_markers(bindings, tool_name);
            (
                tool_name.to_string(),
                serde_json::json!({
                    "policy_class": marker
                        .and_then(|value: &serde_json::Value| value.get("class"))
                        .and_then(serde_json::Value::as_str)
                        .map(|class| format!("tool:{class}"))
                        .unwrap_or_else(|| format!("tool:{tool_name}")),
                    "approval_required": tool_markers.iter().any(|marker| marker == "high_agency"),
                    "tool_markers": tool_markers
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

pub(super) fn select_allowed_incarnation<'a>(
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

pub(super) fn incarnation_preference_rank(
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

pub(super) fn selection_reason_for_incarnation(
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
