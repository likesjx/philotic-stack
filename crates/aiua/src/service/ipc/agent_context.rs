//! Agent context: placement markers, reflex policy, agent-graph snapshots, delivery/reply-owner context.
//!
//! Moved verbatim from `ipc/mod.rs` (IPC_DISPATCH_SPLIT); only item
//! visibility was widened so the parent module can reach it.

use super::*;

pub(super) fn normalize_marker_text(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch.is_ascii_whitespace() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
}

pub(super) fn collect_role_receptor_markers(values: &[&str]) -> Vec<String> {
    let mut markers = BTreeSet::new();
    const STOPWORDS: &[&str] = &[
        "the", "and", "for", "with", "from", "that", "this", "into", "role", "focus", "uses",
        "use", "work", "mode", "lens", "agent", "same", "self", "your", "their",
    ];
    for value in values {
        let normalized = normalize_marker_text(value);
        for token in normalized.split_whitespace() {
            if token.len() < 4 || STOPWORDS.contains(&token) {
                continue;
            }
            markers.insert(token.to_string());
        }
    }
    markers.into_iter().collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::service) struct ToolRunnerRegistryEntry {
    pub(super) guest_id: String,
    pub(super) supported_tools: Vec<String>,
    pub(super) last_seen_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AllowedIncarnation {
    pub(super) incarnation_id: String,
    pub(super) runner_id: Option<String>,
    pub(super) hotel_id: Option<String>,
    pub(super) environment_id: Option<String>,
    pub(super) target_node: Option<String>,
    pub(super) target_role: Option<String>,
    pub(super) supported_tools: Vec<String>,
    pub(super) execution_mode: String,
    pub(super) availability_state: String,
    pub(super) selection_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::service) struct LocalDeliveryProvenanceHint {
    pub(in crate::service) guest_id: String,
    pub(in crate::service) updated_at: u64,
    pub(in crate::service) marker_kind: Option<String>,
    pub(in crate::service) marker_strength: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::service) struct PlacementMarkerPolicy {
    pub(in crate::service) ttl_secs: u64,
    pub(in crate::service) supersede_on_newer_active_incarnation_conflict: bool,
    pub(in crate::service) permit_parking_when_unregistered: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct RoutingPreferences {
    pub(super) preferred_tool_runner_incarnation: Option<String>,
    pub(super) preferred_tool_runner: Option<String>,
    pub(super) preferred_hotel_id: Option<String>,
    pub(super) preferred_environment_id: Option<String>,
}

pub(in crate::service) fn agent_graph_db_path(agent_id: &str) -> PathBuf {
    std::env::var("PHILOTIC_AGENT_GRAPH_DB")
        .map(|value| PathBuf::from(value.replace("{agent_id}", agent_id)))
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home)
                .join(".philotic")
                .join(format!("agent-graph-{agent_id}.db"))
        })
}

pub(super) fn load_agent_graph_routing_preferences(
    agent_id: &str,
) -> Option<Vec<serde_json::Value>> {
    let path = agent_graph_db_path(agent_id);
    if !path.exists() {
        return None;
    }
    let storage = SqliteAgentGraphStorage::open(agent_id, &path).ok()?;
    let preferences = storage.list_routing_preferences().ok()?;
    Some(
        preferences
            .into_iter()
            .filter_map(|preference| serde_json::to_value(preference).ok())
            .collect(),
    )
}

pub(super) fn load_shared_model_markers(graph: &GraphDomain) -> Option<Vec<serde_json::Value>> {
    let mut markers = graph
        .list_abstract_models()
        .ok()?
        .into_iter()
        .filter_map(|record| serde_json::to_value(record).ok())
        .collect::<Vec<_>>();
    markers.sort_by(|left, right| {
        left.get("model_ref")
            .and_then(serde_json::Value::as_str)
            .cmp(&right.get("model_ref").and_then(serde_json::Value::as_str))
    });
    Some(markers)
}

pub(super) fn load_shared_tool_markers(graph: &GraphDomain) -> Option<Vec<serde_json::Value>> {
    let mut markers = graph
        .list_abstract_tools()
        .ok()?
        .into_iter()
        .filter_map(|record| serde_json::to_value(record).ok())
        .collect::<Vec<_>>();
    markers.sort_by(|left, right| {
        left.get("tool_name")
            .and_then(serde_json::Value::as_str)
            .cmp(&right.get("tool_name").and_then(serde_json::Value::as_str))
    });
    Some(markers)
}

pub(super) fn load_shared_skill_markers(graph: &GraphDomain) -> Option<Vec<serde_json::Value>> {
    let mut markers = graph
        .list_abstract_skills()
        .ok()?
        .into_iter()
        .filter_map(|record| serde_json::to_value(record).ok())
        .collect::<Vec<_>>();
    markers.sort_by(|left, right| {
        left.get("skill_name")
            .and_then(serde_json::Value::as_str)
            .cmp(&right.get("skill_name").and_then(serde_json::Value::as_str))
    });
    Some(markers)
}

pub(super) fn latest_routing_policy_reflex_dispositions(
    graph: &GraphDomain,
    agent_id: &str,
) -> HashMap<String, serde_json::Value> {
    let mut dispositions = HashMap::new();
    let Ok(policies) = graph.list_routing_policies(agent_id) else {
        return dispositions;
    };
    for policy in policies {
        let Some(preference_key) = policy.learned_reflex_preference_key.clone() else {
            continue;
        };
        let decided_at = policy.operator_disposition.decided_at;
        let replace = dispositions
            .get(&preference_key)
            .and_then(|existing| existing.get("decided_at"))
            .and_then(|value| value.as_u64())
            .map(|existing| decided_at >= existing)
            .unwrap_or(true);
        if replace {
            dispositions.insert(
                preference_key,
                serde_json::json!({
                    "proposal_id": policy.proposal_id,
                    "state": policy.operator_disposition.state,
                    "reason": policy.operator_disposition.reason,
                    "decided_at": decided_at,
                }),
            );
        }
    }
    dispositions
}

pub(super) fn load_agent_graph_reflex_preferences(
    graph: &GraphDomain,
    agent_id: &str,
) -> Option<(
    Vec<serde_json::Value>,
    Vec<serde_json::Value>,
    Vec<serde_json::Value>,
)> {
    let path = agent_graph_db_path(agent_id);
    if !path.exists() {
        return None;
    }
    let storage = SqliteAgentGraphStorage::open(agent_id, &path).ok()?;
    let preferences = storage.list_reflex_preferences().ok()?;
    let dispositions = latest_routing_policy_reflex_dispositions(graph, agent_id);
    let mut layers = Vec::new();
    let mut suppressions = Vec::new();
    let mut rewards = Vec::new();
    for preference in preferences {
        let mut precedence = preference.precedence;
        if let Some(disposition) = dispositions.get(&preference.preference_key) {
            match disposition.get("state").and_then(|value| value.as_str()) {
                Some("rejected") => {
                    suppressions.push(serde_json::json!({
                        "preference_key": preference.preference_key,
                        "reason": "suppressed_by_rejected_routing_policy",
                        "routing_policy": disposition,
                    }));
                    continue;
                }
                Some("approved") => {
                    precedence += 5;
                    rewards.push(serde_json::json!({
                        "preference_key": preference.preference_key,
                        "reason": "reinforced_by_approved_routing_policy",
                        "precedence_bonus": 5,
                        "routing_policy": disposition,
                    }));
                }
                _ => {}
            }
        }
        layers.push(serde_json::json!({
                    "policy_scope": "agent_learned",
                    "policy_source": "agent_graph",
                    "origin_class": "agent_learned",
                    "precedence": precedence,
                    "reason": preference.config_json.get("reason").cloned().unwrap_or(serde_json::Value::Null),
                    "preference_key": preference.preference_key,
                    "config": preference.config_json,
                    "regulatory_system": if precedence > preference.precedence { "reward" } else { "baseline" },
                    "reflexes": preference.reflexes_json,
        }));
    }
    Some((layers, suppressions, rewards))
}

pub(in crate::service) fn infer_marker_strength(
    explicit_strength: Option<&str>,
    marker_kind: Option<&str>,
) -> Option<&'static str> {
    match explicit_strength {
        Some("weak") => Some("weak"),
        Some("medium") => Some("medium"),
        Some("strong") => Some("strong"),
        Some(_) => Some("medium"),
        None => match marker_kind {
            Some("receptor_ingress") | Some("membrane_ingress") => Some("weak"),
            Some("transport_continuity") => Some("medium"),
            Some("role_handoff") => Some("strong"),
            None | Some(_) => Some("medium"),
        },
    }
}

pub(in crate::service) fn infer_placement_risk_level(
    marker_kind: Option<&str>,
    marker_source: Option<&str>,
    marker_strength: Option<&str>,
) -> &'static str {
    let inferred_strength = infer_marker_strength(marker_strength, marker_kind);
    match (marker_kind, marker_source, inferred_strength) {
        (Some("receptor_ingress"), _, _) | (Some("membrane_ingress"), _, _) => "elevated",
        (Some("role_handoff"), _, Some("strong")) => "low",
        (Some("transport_continuity"), Some("operator_chat"), Some(level))
            if matches!(level, "strong" | "medium") =>
        {
            "guarded"
        }
        (Some("transport_continuity"), _, Some(level)) if matches!(level, "strong" | "medium") => {
            "guarded"
        }
        (_, _, Some("weak")) => "elevated",
        _ => "guarded",
    }
}

pub(in crate::service) fn placement_marker_policy(
    marker_kind: Option<&str>,
    marker_strength: Option<&str>,
) -> PlacementMarkerPolicy {
    let inferred_strength = infer_marker_strength(marker_strength, marker_kind);
    match marker_kind {
        Some("receptor_ingress") | Some("membrane_ingress") => PlacementMarkerPolicy {
            ttl_secs: std::cmp::max(1, LOCAL_DELIVERY_PROVENANCE_TTL_SECS / 2),
            supersede_on_newer_active_incarnation_conflict: true,
            permit_parking_when_unregistered: false,
        },
        Some("role_handoff") => PlacementMarkerPolicy {
            ttl_secs: LOCAL_DELIVERY_PROVENANCE_TTL_SECS.saturating_mul(2),
            supersede_on_newer_active_incarnation_conflict: false,
            permit_parking_when_unregistered: true,
        },
        Some("transport_continuity") => PlacementMarkerPolicy {
            ttl_secs: LOCAL_DELIVERY_PROVENANCE_TTL_SECS,
            supersede_on_newer_active_incarnation_conflict: false,
            permit_parking_when_unregistered: !matches!(inferred_strength, Some("weak")),
        },
        None | Some(_) => PlacementMarkerPolicy {
            ttl_secs: LOCAL_DELIVERY_PROVENANCE_TTL_SECS,
            supersede_on_newer_active_incarnation_conflict: true,
            permit_parking_when_unregistered: matches!(
                inferred_strength,
                Some("medium") | Some("strong")
            ),
        },
    }
}

pub(super) fn remote_execution_allowed(bindings: &serde_json::Value) -> bool {
    bindings
        .get("effective_posture")
        .and_then(|posture| posture.get("remote_execution_allowed"))
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
}

pub(super) fn remote_tool_execution_allowed(bindings: &serde_json::Value) -> bool {
    bindings
        .get("effective_reflexes")
        .and_then(|reflexes| reflexes.get("remote_tool_reflex"))
        .and_then(serde_json::Value::as_str)
        .map(|value| value == "allow")
        .or_else(|| {
            bindings
                .get("effective_right_policy")
                .and_then(|policy| policy.get("remote_tool_execution"))
                .and_then(serde_json::Value::as_str)
                .map(|value| value == "allow")
        })
        .unwrap_or_else(|| remote_execution_allowed(bindings))
}

pub(super) fn remote_component_execution_allowed(bindings: &serde_json::Value) -> bool {
    bindings
        .get("effective_reflexes")
        .and_then(|reflexes| reflexes.get("remote_component_reflex"))
        .and_then(serde_json::Value::as_str)
        .map(|value| value == "allow")
        .or_else(|| {
            bindings
                .get("effective_right_policy")
                .and_then(|policy| policy.get("remote_component_execution"))
                .and_then(serde_json::Value::as_str)
                .map(|value| value == "allow")
        })
        .unwrap_or_else(|| remote_execution_allowed(bindings))
}

pub(super) fn credential_scope_reflex(bindings: &serde_json::Value) -> &'static str {
    bindings
        .get("effective_reflexes")
        .and_then(|reflexes| reflexes.get("credential_scope_reflex"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            bindings
                .get("effective_right_policy")
                .and_then(|policy| policy.get("credential_scope"))
                .and_then(serde_json::Value::as_str)
        })
        .and_then(|value| match value {
            "local_only" => Some("local_only"),
            "local_scoped" => Some("local_scoped"),
            "mesh_scoped" => Some("mesh_scoped"),
            _ => None,
        })
        .unwrap_or("local_scoped")
}

pub(super) fn effective_reflexes_from_placement_risk(
    placement_risk_level: &str,
) -> serde_json::Value {
    match placement_risk_level {
        "elevated" => serde_json::json!({
            "remote_tool_reflex": "deny",
            "remote_component_reflex": "deny",
            "credential_scope_reflex": "local_only",
        }),
        "low" => serde_json::json!({
            "remote_tool_reflex": "allow",
            "remote_component_reflex": "allow",
            "credential_scope_reflex": "mesh_scoped",
        }),
        _ => serde_json::json!({
            "remote_tool_reflex": "deny",
            "remote_component_reflex": "allow",
            "credential_scope_reflex": "local_scoped",
        }),
    }
}

pub(super) fn merge_reflex_overrides(
    inferred_reflexes: serde_json::Value,
    overrides: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut reflexes = inferred_reflexes;
    let Some(overrides) = overrides.and_then(serde_json::Value::as_object) else {
        return reflexes;
    };
    let Some(obj) = reflexes.as_object_mut() else {
        return reflexes;
    };
    for key in [
        "remote_tool_reflex",
        "remote_component_reflex",
        "credential_scope_reflex",
    ] {
        if let Some(value) = overrides.get(key) {
            obj.insert(key.to_string(), value.clone());
        }
    }
    reflexes
}

pub(super) fn push_normalized_reflex_policy_records(
    layers: &mut Vec<serde_json::Value>,
    records: Option<&serde_json::Value>,
    fallback_scope: &str,
    fallback_source: &str,
    fallback_origin_class: &str,
    fallback_precedence: u64,
) {
    let Some(records) = records.and_then(serde_json::Value::as_array) else {
        return;
    };
    for record in records {
        let Some(obj) = record.as_object() else {
            continue;
        };
        let Some(reflexes) = obj.get("reflexes").and_then(serde_json::Value::as_object) else {
            continue;
        };
        layers.push(serde_json::json!({
            "policy_scope": obj
                .get("policy_scope")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(fallback_scope),
            "policy_source": obj
                .get("policy_source")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(fallback_source),
            "origin_class": obj
                .get("origin_class")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(fallback_origin_class),
            "precedence": obj
                .get("precedence")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(fallback_precedence),
            "reason": obj.get("reason").cloned().unwrap_or(serde_json::Value::Null),
            "reflexes": serde_json::Value::Object(reflexes.clone()),
        }));
    }
}

pub(super) fn normalized_reflex_policy_records(
    summary_json: &serde_json::Value,
    bindings: &serde_json::Value,
    placement_risk_level: &str,
) -> Vec<serde_json::Value> {
    let mut layers = vec![serde_json::json!({
        "policy_scope": "placement_inferred",
        "policy_source": "hotel_runtime",
        "origin_class": "inferred",
        "precedence": 10,
        "reason": format!(
            "derived from placement_risk_level={placement_risk_level} runtime provenance"
        ),
        "reflexes": effective_reflexes_from_placement_risk(placement_risk_level),
    })];

    push_normalized_reflex_policy_records(
        &mut layers,
        bindings.get("reflex_policy_defaults"),
        "hotel_default",
        "hotel_bindings",
        "hotel_default",
        40,
    );
    push_normalized_reflex_policy_records(
        &mut layers,
        bindings.get("reflex_policy_agent_layers"),
        "agent_learned",
        "agent_graph",
        "agent_learned",
        70,
    );
    push_normalized_reflex_policy_records(
        &mut layers,
        summary_json.get("reflex_policy_records"),
        "session_override",
        "session_summary",
        "session_override",
        100,
    );
    if summary_json.get("reflex_policy_records").is_none() {
        if let Some(overrides) = summary_json.get("reflex_overrides") {
            layers.push(serde_json::json!({
                "policy_scope": "session_override",
                "policy_source": "legacy_reflex_overrides",
                "origin_class": "legacy_bridge",
                "precedence": 100,
                "reason": "legacy reflex_overrides bridge",
                "reflexes": overrides,
            }));
        }
    }

    layers.sort_by_key(|layer| {
        layer
            .get("precedence")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(100)
    });
    layers
}

pub(super) fn effective_reflexes_from_policy_records(
    policy_records: &[serde_json::Value],
) -> serde_json::Value {
    let mut effective = serde_json::json!({});
    for layer in policy_records {
        let Some(reflexes) = layer.get("reflexes") else {
            continue;
        };
        effective = merge_reflex_overrides(effective, Some(reflexes));
    }
    effective
}

pub(super) fn load_agent_graph_snapshot(
    agent_id: &str,
    source_node_id: &str,
) -> Option<serde_json::Value> {
    let path = agent_graph_db_path(agent_id);
    if !path.exists() {
        return None;
    }
    let storage = SqliteAgentGraphStorage::open(agent_id, &path).ok()?;
    let snapshot = storage.export_snapshot(source_node_id).ok()?;
    serde_json::to_value(snapshot).ok()
}

pub(in crate::service) fn attach_agent_graph_snapshot(
    task_json: &str,
    agent_id: Option<&str>,
    source_node_id: &str,
) -> String {
    let Some(agent_id) = agent_id else {
        return task_json.to_string();
    };
    let Some(snapshot) = load_agent_graph_snapshot(agent_id, source_node_id) else {
        return task_json.to_string();
    };
    let Ok(mut payload) = serde_json::from_str::<serde_json::Value>(task_json) else {
        return task_json.to_string();
    };
    let Some(obj) = payload.as_object_mut() else {
        return task_json.to_string();
    };
    if obj.contains_key("agent_graph_snapshot") {
        return task_json.to_string();
    }
    obj.insert("agent_graph_snapshot".to_string(), snapshot);
    serde_json::to_string(&payload).unwrap_or_else(|_| task_json.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::service) struct AgentTaskContext {
    pub(in crate::service) agent_id: String,
    pub(super) authority_hotel: Option<String>,
}

pub(in crate::service) fn infer_agent_context_for_task(
    graph: &GraphDomain,
    target_role: &str,
    target_guest_id: Option<&str>,
    task_json: &str,
) -> Option<AgentTaskContext> {
    if target_role != "agent" {
        return None;
    }

    if let Ok(payload) = serde_json::from_str::<serde_json::Value>(task_json) {
        if let Some(agent_id) = payload.get("agent_id").and_then(serde_json::Value::as_str) {
            return Some(AgentTaskContext {
                agent_id: agent_id.to_string(),
                authority_hotel: payload
                    .get("authority_hotel")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .or_else(|| lookup_agent_authority_hotel(graph, agent_id)),
            });
        }
        if let Some(session_id) = payload
            .get("session_id")
            .and_then(serde_json::Value::as_str)
        {
            if let Ok(Some(session)) = graph.get_session(session_id) {
                if let Some(agent_id) = session.primary_agent_id {
                    return Some(AgentTaskContext {
                        authority_hotel: lookup_agent_authority_hotel(graph, &agent_id),
                        agent_id,
                    });
                }
            }
        }
    }

    let guest_id = target_guest_id?;
    graph
        .list_role_incarnations_by_guest_id(guest_id)
        .ok()
        .and_then(|mut roles| roles.drain(..).next())
        .map(|role| AgentTaskContext {
            authority_hotel: lookup_agent_authority_hotel(graph, &role.agent_id),
            agent_id: role.agent_id,
        })
}

pub(super) fn apply_embedded_agent_graph_snapshot(
    task_json: &str,
) -> anyhow::Result<Option<String>> {
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(task_json) else {
        return Ok(None);
    };
    let Some(snapshot_value) = payload.get("agent_graph_snapshot") else {
        return Ok(None);
    };
    let snapshot: AgentGraphSnapshot = serde_json::from_value(snapshot_value.clone())?;
    let path = agent_graph_db_path(&snapshot.agent_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let storage = SqliteAgentGraphStorage::open(&snapshot.agent_id, &path)?;
    storage.apply_snapshot(&snapshot)?;
    Ok(Some(snapshot.agent_id))
}

/// The mesh node hosting `agent_id` according to gossiped peer rosters
/// (`HotelStateSync`), for agents this hotel's graph has no identity for.
pub(in crate::service) fn peer_agent_node_from_roster<'a>(
    states: impl Iterator<Item = &'a ansible_mesh_core::registry::RemoteHotelState>,
    agent_id: &str,
) -> Option<String> {
    ansible_mesh_core::registry::best_host_for_agent(states, agent_id)
        .map(|state| state.node_id.clone())
}

pub(in crate::service) fn lookup_agent_authority_hotel(
    graph: &GraphDomain,
    agent_id: &str,
) -> Option<String> {
    graph
        .get_agent_identity(agent_id)
        .ok()
        .flatten()
        .map(|identity| identity.authority_hotel)
}

pub(super) fn attach_delivery_context(
    graph: &GraphDomain,
    local_node_id: &str,
    target_role: &str,
    target_guest_id: Option<&str>,
    task_json: &str,
) -> String {
    let Ok(mut payload) = serde_json::from_str::<serde_json::Value>(task_json) else {
        return task_json.to_string();
    };
    let Some(obj) = payload.as_object_mut() else {
        return task_json.to_string();
    };
    obj.entry("delivery_node_id".to_string())
        .or_insert_with(|| serde_json::json!(local_node_id));
    if let Some(hotel_name) = IpcServer::local_hotel_name(graph, local_node_id) {
        obj.entry("delivery_hotel".to_string())
            .or_insert_with(|| serde_json::json!(hotel_name));
    }
    obj.entry("delivery_target_role".to_string())
        .or_insert_with(|| serde_json::json!(target_role));
    if let Some(target_guest_id) = target_guest_id {
        // The router has already resolved the canonical destination for this
        // envelope. Replace any stale embedded delivery hint so a role switch
        // cannot split the user turn and model response across two philotes.
        obj.insert(
            "delivery_target_guest_id".to_string(),
            serde_json::json!(target_guest_id),
        );
    }
    serde_json::to_string(&payload).unwrap_or_else(|_| task_json.to_string())
}

/// Payload field naming the agent that owns a membrane-bound task no seat was
/// addressed by. Membrane seats serving a different agent drop the task.
pub(crate) const REPLY_OWNER_AGENT_ID_FIELD: &str = "reply_owner_agent_id";

/// Stamps [`REPLY_OWNER_AGENT_ID_FIELD`] on a membrane-bound task that carries
/// no seat target, taken from the emitter's registered identity — never from
/// the payload, which any guest could forge.
///
/// A seat-less membrane task is delivered to EVERY local membrane seat, and a
/// Telegram DM chat id is the same under every bot token. Seats filtered by
/// parsing the agent out of the session id, which `cron:<job_id>` sessions
/// never name — so each daily Bjork cron brief also went out through the
/// Coach bot (2026-09-18). The emitter is the authority on who is replying.
///
/// Only a registered `agent` whose guest id (`agent-x` or `agent-x:<role>`)
/// resolves to a known agent identity is stamped; anything else (subagents
/// with UUID ids, infra guests) has the field removed so seats fall back to
/// the session-id check.
pub(super) fn stamp_reply_owner_agent(
    graph: &GraphDomain,
    emitter: Option<&GuestIdentity>,
    target_role: &str,
    target_guest_id: Option<&str>,
    task_json: String,
) -> String {
    if target_role != "membrane" || target_guest_id.is_some() {
        return task_json;
    }
    let Ok(mut payload) = serde_json::from_str::<serde_json::Value>(&task_json) else {
        return task_json;
    };
    let Some(obj) = payload.as_object_mut() else {
        return task_json;
    };
    let owner = emitter
        .and_then(emitter_agent_id)
        .filter(|agent_id| matches!(graph.get_agent_identity(agent_id), Ok(Some(_))));
    match owner {
        Some(agent_id) => {
            obj.insert(
                REPLY_OWNER_AGENT_ID_FIELD.to_string(),
                serde_json::json!(agent_id),
            );
        }
        None => {
            if obj.remove(REPLY_OWNER_AGENT_ID_FIELD).is_none() {
                return task_json;
            }
        }
    }
    serde_json::to_string(&payload).unwrap_or(task_json)
}

/// The agent a philote connection speaks for, from how it registered (see
/// `philote::main::role_registration`): a base philote is role `agent` with
/// guest id `{agent_id}`; a role incarnation is role `role:{agent_id}:{role}`
/// with guest id `{agent_id}:{role}`. The two must agree.
pub(super) fn emitter_agent_id(identity: &GuestIdentity) -> Option<&str> {
    let guest_agent = identity.guest_id.split(':').next()?;
    let role_agent = if identity.role == "agent" {
        guest_agent
    } else {
        identity.role.strip_prefix("role:")?.split(':').next()?
    };
    (!guest_agent.is_empty() && guest_agent == role_agent).then_some(guest_agent)
}

/// Guest-record roles that can never consume `role="agent"` deliveries. Used to reject
/// poisoned placement-provenance hints (see `guest_can_fill_agent_placement`): tool and
/// datasource runners such as `life-graph-runner` are dispatch TARGETS of an agent's
/// tool invokes, never a placement for the agent's own turn traffic.
pub(in crate::service) fn is_non_agent_infra_role(role: &str) -> bool {
    matches!(
        role,
        "tool" | "datasource" | "gateway" | "membrane" | "model" | "proxy"
    ) || role.ends_with("-runner")
}

pub(in crate::service) fn is_response_like_agent_action(action: &str) -> bool {
    matches!(
        action,
        "model_response"
            | "tool_result"
            | "datasource_response"
            | "paracrine_response"
            | "approval_resolution"
            | "handoff_bundle"
    )
}

pub(super) fn explicit_response_guest_from_payload(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("return_route")
        .and_then(|route| route.get("guest_id"))
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            payload
                .get("return_route")
                .and_then(|route| route.get("guest"))
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            payload
                .get("return_route")
                .and_then(|route| route.get("reply_guest_id"))
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            payload
                .get("delivery_target_guest_id")
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            payload
                .get("reply_guest_id")
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| payload.get("agent_id").and_then(serde_json::Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

/// True when `task_json` is a `send_reply` (`FinalReplyPayload`) whose
/// `session_id` names an isolated cron session (`cron:<job_id>`, see
/// [`ansible_mesh_core::cron::cron_session_id`]) for a job with
/// `silent_ok = true`, and whose `content` matches the Hermes `[SILENT]`
/// convention ([`ansible_mesh_core::cron::is_silent_cron_reply`]).
///
/// This is the single chokepoint gating cron-reply suppression: every
/// `send_reply` a philote turn emits — regardless of which tool/branch
/// produced it — passes through `IpcRequest::EmitTask`, so checking here
/// once covers the whole delivery path without touching the many unrelated
/// `deliver_inbound_task` call sites. Only `action == "send_reply"` is
/// gated; the cron *fire* itself (the prompt `CronTicker::fire` delivers to
/// the target role) never reaches this arm; it goes straight to
/// `deliver_inbound_task`/park from the ticker, not through `EmitTask`.
///
/// Fails open (returns `false`) whenever the job can't be resolved — an
/// unknown/removed job, a non-cron session, or a malformed payload never
/// suppresses delivery.
pub(super) fn silent_cron_reply_suppressed(graph: &GraphDomain, task_json: &str) -> bool {
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(task_json) else {
        return false;
    };
    if payload.get("action").and_then(serde_json::Value::as_str) != Some("send_reply") {
        return false;
    }
    let Some(session_id) = payload
        .get("session_id")
        .and_then(serde_json::Value::as_str)
    else {
        return false;
    };
    let Some(job_id) = session_id.strip_prefix("cron:") else {
        return false;
    };
    let Some(content) = payload.get("content").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let Ok(Some(job)) = graph.get_cron_job(job_id) else {
        return false;
    };
    job.silent_ok && ansible_mesh_core::cron::is_silent_cron_reply(content)
}

pub(super) fn response_like_agent_action_for_task(
    target_role: &str,
    target_guest_id: Option<&str>,
    task_json: &str,
) -> Option<String> {
    if target_role != "agent" || target_guest_id.is_some() {
        return None;
    }
    let payload = serde_json::from_str::<serde_json::Value>(task_json).ok()?;
    let action = payload.get("action").and_then(serde_json::Value::as_str)?;
    is_response_like_agent_action(action).then(|| action.to_string())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn infer_response_target_guest_id_for_agent_task(
    graph: &GraphDomain,
    local_node_id: &str,
    target_role: &str,
    target_guest_id: Option<&str>,
    task_json: &str,
    live_agent_guests: &[String],
    heal_queue: Option<&dyn ansible_mesh_core::heal_queue::HealQueueStorage>,
) -> Option<String> {
    response_like_agent_action_for_task(target_role, target_guest_id, task_json)?;
    let payload = serde_json::from_str::<serde_json::Value>(task_json).ok()?;

    if let Some(guest_id) = explicit_response_guest_from_payload(&payload) {
        return Some(guest_id);
    }

    let session_id = payload
        .get("session_id")
        .and_then(serde_json::Value::as_str)?;
    let session = graph.get_session(session_id).ok().flatten()?;

    let is_registered = |guest_id: &str| live_agent_guests.iter().any(|live| live == guest_id);

    if let Some(active_guest_id) = session.active_incarnation_id.clone() {
        if is_registered(&active_guest_id) {
            return Some(active_guest_id);
        }

        // RC-2 (2026-07-09 stuck-turn forensic): the active incarnation can be a
        // non-agent infra guest — a tool/datasource/gateway/model/*-runner such as
        // vps-jane:life-graph-runner — when the session's last hop was a tool
        // invoke rather than an agent turn. `is_registered` above correctly misses
        // it (infra guests never subscribe under role="agent"), but the old
        // fallback below treated *any* unregistered active incarnation as "just
        // not live yet" and silently redirected the response to the local
        // orchestrator — the same family of bug PR #174 fixed in
        // `resolve_agent_route`/`guest_can_fill_agent_placement`, never mirrored
        // here. That silently dropped cross-hotel tool RESULTs at the wrong
        // guest and orphaned the delegated turn. Apply the same discipline: never
        // treat a poisoned non-agent infra guest as a legitimate "not live yet"
        // agent — resolve the real agent target instead (or reject loudly).
        if !IpcServer::guest_can_fill_agent_placement(graph, local_node_id, &active_guest_id) {
            // File the heal event on *detection*, not on the no-fallback branch:
            // the poisoning is the A3-countable anomaly regardless of whether we
            // recover it (redirect to the primary agent) or reject it. The RC-4
            // requirement is that RC-2 stops filing ZERO heal rows — the common
            // case has `primary_agent_id` set, so if we only filed on the reject
            // path this would count nothing in production. `push_classified`
            // flood-collapses on `(guest_id, pattern_tag)`, so emitting on every
            // recovered tool-result is cheap; `active_guest_id` is stable across
            // sessions so A3 aggregates the poisoned guest correctly.
            let recovers_to = session.primary_agent_id.clone();
            let message = match recovers_to.as_deref() {
                Some(primary) => format!(
                    "[cross_hotel_misroute] response-like action for session [{session_id}] \
                     targeted non-agent infra guest [{active_guest_id}]; recovered by routing \
                     to primary agent [{primary}]"
                ),
                None => format!(
                    "[cross_hotel_misroute] response-like action for session [{session_id}] \
                     targeted non-agent infra guest [{active_guest_id}] with no resolvable \
                     agent fallback (primary_agent_id unset)"
                ),
            };
            warn!(
                guest_id = active_guest_id.as_str(),
                session_id = session_id,
                recovered = recovers_to.is_some(),
                "infer_response_target_guest_id_for_agent_task detected poisoned non-agent infra response target"
            );
            if let Some(hq) = heal_queue {
                match hq.push_classified(
                    &active_guest_id,
                    &message,
                    "medium",
                    "cross_hotel_misroute",
                ) {
                    Ok(Some(id)) => info!(
                        id = %id,
                        guest_id = active_guest_id.as_str(),
                        "cross-hotel tool-response mis-route pushed to heal queue"
                    ),
                    Ok(None) => debug!(
                        guest_id = active_guest_id.as_str(),
                        "cross-hotel mis-route collapsed into recent heal entry (flood window)"
                    ),
                    Err(err) => warn!(
                        error = %err,
                        "failed to push cross-hotel mis-route to heal queue"
                    ),
                }
            }

            if let Some(primary_agent_id) = recovers_to {
                return Some(primary_agent_id);
            }
            return None;
        }

        // Active incarnation isn't actually live right now (e.g. a single-process
        // philote handling all roles under its base agent_id) — mirror the inbound
        // resolver's fallback so the response isn't silently parked ledger-only.
        if let Some(orchestrator_guest_id) =
            IpcServer::resolve_orchestrator_guest_id(graph, &session, live_agent_guests)
        {
            warn!(
                "Active incarnation [{}] is not registered for session [{}]; routing response to orchestrator guest [{}] instead.",
                active_guest_id, session_id, orchestrator_guest_id
            );
            return Some(orchestrator_guest_id);
        }
    }

    session.primary_agent_id
}

pub(super) fn declared_component_capabilities(bindings: &serde_json::Value) -> Vec<String> {
    let mut capabilities =
        BTreeSet::from(["media.analyze".to_string(), "text.generate".to_string()]);

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

pub(super) fn project_effective_rights(bindings: &serde_json::Value) -> Vec<String> {
    let toolset = bindings
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
    let skillset = bindings
        .get("effective_skillset")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    // Only project rights when there is an explicit toolset or skillset configured.
    // When both are empty the session has no profile yet; returning empty lets
    // downstream consumers (philote's default_visible_toolset) use their own
    // defaults without the rights filter stripping them out.
    if toolset.is_empty() && skillset.is_empty() {
        return Vec::new();
    }

    let mut rights = Vec::new();
    rights.extend(toolset.iter().map(|tool_name| tool_right(tool_name)));
    rights.extend(skillset.iter().map(|skill_name| skill_right(skill_name)));
    // Also project rights for tools that come from allowed_classes expansion.
    // Without this, class-expanded tools (e.g. life_graph → life.*) would have no
    // projected right and would be filtered out by the rights check in
    // compose_tool_assembly_from_incarnations even though the class is explicitly allowed.
    if let Some(classes) = bindings
        .get("allowed_classes")
        .and_then(serde_json::Value::as_array)
    {
        for class in classes {
            if let Some(class_str) = class.as_str() {
                for tool_name in tools_for_allowed_class(class_str) {
                    rights.push(tool_right(tool_name));
                }
            }
        }
    }
    rights.extend(
        declared_component_capabilities(bindings)
            .into_iter()
            .map(|capability| component_right(&capability)),
    );
    normalize_rights(rights)
}
