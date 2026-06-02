//! Tool dispatch for the `agent.graph.*` surface.
//!
//! Each function accepts the current `AgentGraphStorage` instance and the
//! raw JSON arguments from the incoming tool call, and returns a JSON-string
//! result to send back as the tool call reply.

use ansible_mesh_core::agent_graph_storage::{
    AgentExperienceTrace, AgentGraphSnapshot, AgentGraphStorage, AgentReflexPreference,
    AgentRoutingPreference, AgentToolPreference,
};
use ansible_mesh_core::resources::{ResourceDeclaration, ResourceType};
use serde_json::{json, Value};
use ulid::Ulid;

/// Dispatch an `agent.graph.*` tool call. Returns a JSON string result.
pub fn dispatch(
    storage: &dyn AgentGraphStorage,
    tool_name: &str,
    args: &Value,
    agent_id: &str,
) -> String {
    match tool_name {
        "agent.graph.read" => agent_graph_read(storage, args),
        "agent.graph.write" => agent_graph_write(storage, args, agent_id),
        "agent.graph.declare" => agent_graph_declare(storage, args, agent_id),
        "agent.graph.recall" => agent_graph_recall(storage, args),
        "agent.graph.sync" => agent_graph_sync(storage, args, agent_id),
        _ => json!({"ok": false, "error": format!("{tool_name}: unsupported agent.graph tool")})
            .to_string(),
    }
}

// ── agent.graph.read ──────────────────────────────────────────────────────────

/// Read agent graph state by entity type.
///
/// Args: `{ "entity": "resource_grants" | "tool_preferences" | "routing_preferences" | "reflex_preferences" | "resource_declarations" }`
fn agent_graph_read(storage: &dyn AgentGraphStorage, args: &Value) -> String {
    let entity = match args.get("entity").and_then(Value::as_str) {
        Some(e) => e,
        None => return json!({"ok": false, "error": "missing 'entity' field"}).to_string(),
    };

    match entity {
        "resource_grants" => match storage.list_resource_grants() {
            Ok(grants) => json!({"ok": true, "resource_grants": grants}).to_string(),
            Err(e) => json!({"ok": false, "error": e.to_string()}).to_string(),
        },
        "tool_preferences" => match storage.list_tool_preferences() {
            Ok(prefs) => json!({"ok": true, "tool_preferences": prefs}).to_string(),
            Err(e) => json!({"ok": false, "error": e.to_string()}).to_string(),
        },
        "routing_preferences" => match storage.list_routing_preferences() {
            Ok(prefs) => json!({"ok": true, "routing_preferences": prefs}).to_string(),
            Err(e) => json!({"ok": false, "error": e.to_string()}).to_string(),
        },
        "reflex_preferences" => match storage.list_reflex_preferences() {
            Ok(prefs) => json!({"ok": true, "reflex_preferences": prefs}).to_string(),
            Err(e) => json!({"ok": false, "error": e.to_string()}).to_string(),
        },
        "resource_declarations" => match storage.list_resource_declarations() {
            Ok(decls) => json!({"ok": true, "resource_declarations": decls}).to_string(),
            Err(e) => json!({"ok": false, "error": e.to_string()}).to_string(),
        },
        other => {
            json!({"ok": false, "error": format!("unknown entity type '{other}'")}).to_string()
        }
    }
}

// ── agent.graph.write ─────────────────────────────────────────────────────────

/// Write or update an entity in the agent graph.
///
/// Args: `{ "entity": "tool_preference", "tool_name": "...", "preference_level": N, "config": {...} }`
/// or:  `{ "entity": "resource_grant", ... }`
fn agent_graph_write(storage: &dyn AgentGraphStorage, args: &Value, agent_id: &str) -> String {
    let entity = match args.get("entity").and_then(Value::as_str) {
        Some(e) => e,
        None => return json!({"ok": false, "error": "missing 'entity' field"}).to_string(),
    };

    match entity {
        "tool_preference" => {
            let tool_name = match args.get("tool_name").and_then(Value::as_str) {
                Some(t) => t,
                None => return json!({"ok": false, "error": "missing 'tool_name'"}).to_string(),
            };
            let preference_level = args
                .get("preference_level")
                .and_then(Value::as_i64)
                .unwrap_or(0) as i32;
            let config_json = args.get("config").cloned().unwrap_or_else(|| json!({}));
            let pref = AgentToolPreference {
                agent_id: agent_id.to_string(),
                tool_name: tool_name.to_string(),
                preference_level,
                config_json,
                updated_at: 0, // upsert_tool_preference sets now_epoch() when 0
            };
            match storage.upsert_tool_preference(&pref) {
                Ok(()) => json!({"ok": true}).to_string(),
                Err(e) => json!({"ok": false, "error": e.to_string()}).to_string(),
            }
        }
        "routing_preference" => {
            let preference_key = match args.get("preference_key").and_then(Value::as_str) {
                Some(v) => v,
                None => {
                    return json!({"ok": false, "error": "missing 'preference_key'"}).to_string()
                }
            };
            let pref = AgentRoutingPreference {
                agent_id: agent_id.to_string(),
                preference_key: preference_key.to_string(),
                stage_kind: args
                    .get("stage_kind")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                capability: args
                    .get("capability")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                provider_hint: args
                    .get("provider_hint")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                model_ref: args
                    .get("model_ref")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                preference_level: args
                    .get("preference_level")
                    .and_then(Value::as_i64)
                    .unwrap_or(0) as i32,
                weight: args.get("weight").and_then(Value::as_i64).unwrap_or(0) as i32,
                config_json: args.get("config").cloned().unwrap_or_else(|| json!({})),
                updated_at: 0,
            };
            match storage.upsert_routing_preference(&pref) {
                Ok(()) => json!({"ok": true}).to_string(),
                Err(e) => json!({"ok": false, "error": e.to_string()}).to_string(),
            }
        }
        "reflex_preference" => {
            let preference_key = match args.get("preference_key").and_then(Value::as_str) {
                Some(v) => v,
                None => {
                    return json!({"ok": false, "error": "missing 'preference_key'"}).to_string()
                }
            };
            let reflexes_json = match args.get("reflexes") {
                Some(v) if v.is_object() => v.clone(),
                Some(_) => {
                    return json!({"ok": false, "error": "'reflexes' must be an object"})
                        .to_string()
                }
                None => return json!({"ok": false, "error": "missing 'reflexes'"}).to_string(),
            };
            let pref = AgentReflexPreference {
                agent_id: agent_id.to_string(),
                preference_key: preference_key.to_string(),
                precedence: args.get("precedence").and_then(Value::as_i64).unwrap_or(70) as i32,
                reflexes_json,
                config_json: args.get("config").cloned().unwrap_or_else(|| json!({})),
                updated_at: 0,
            };
            match storage.upsert_reflex_preference(&pref) {
                Ok(()) => json!({"ok": true}).to_string(),
                Err(e) => json!({"ok": false, "error": e.to_string()}).to_string(),
            }
        }
        other => json!({"ok": false, "error": format!("write not supported for entity '{other}'")})
            .to_string(),
    }
}

// ── agent.graph.declare ───────────────────────────────────────────────────────

/// Add or update a static resource declaration.
///
/// Args: `{ "resource_type": "model_router" | ..., "config_hint": "..." }`
/// Omit `config_hint` or set to `null` to clear it.
fn agent_graph_declare(storage: &dyn AgentGraphStorage, args: &Value, _agent_id: &str) -> String {
    let rt_str = match args.get("resource_type").and_then(Value::as_str) {
        Some(s) => s,
        None => return json!({"ok": false, "error": "missing 'resource_type'"}).to_string(),
    };
    let resource_type: ResourceType = match serde_json::from_value(json!(rt_str)) {
        Ok(rt) => rt,
        Err(_) => {
            return json!({"ok": false, "error": format!("unknown resource_type '{rt_str}'")})
                .to_string()
        }
    };
    let config_hint = args
        .get("config_hint")
        .and_then(Value::as_str)
        .map(str::to_string);

    let decl = ResourceDeclaration {
        resource_type,
        config_hint,
    };
    match storage.upsert_resource_declaration(&decl) {
        Ok(()) => json!({"ok": true}).to_string(),
        Err(e) => json!({"ok": false, "error": e.to_string()}).to_string(),
    }
}

// ── agent.graph.recall ────────────────────────────────────────────────────────

/// Retrieve recent experience entries, optionally filtered by event type.
///
/// Args: `{ "limit": N, "event_type": "tool_call" }` (both optional)
fn agent_graph_recall(storage: &dyn AgentGraphStorage, args: &Value) -> String {
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20) as usize;
    let result = if let Some(et) = args.get("event_type").and_then(Value::as_str) {
        storage.list_traces_by_event_type(et, limit)
    } else {
        storage.list_experience_traces(limit)
    };
    match result {
        Ok(traces) => json!({"ok": true, "traces": traces}).to_string(),
        Err(e) => json!({"ok": false, "error": e.to_string()}).to_string(),
    }
}

// ── agent.graph.sync ──────────────────────────────────────────────────────────

/// Apply an inbound mesh snapshot with LWW conflict resolution.
///
/// Args: an `AgentGraphSnapshot` JSON object (serialised by the sending hotel).
///
/// This is the receive-side of the `EventKind::AgentGraphSync` mesh transport.
/// The hotel delivers it as an `InboundTask` to the `agent-graph` guest; the
/// existing dispatch loop calls here without special-casing.
fn agent_graph_sync(storage: &dyn AgentGraphStorage, args: &Value, _agent_id: &str) -> String {
    let snapshot: AgentGraphSnapshot = match serde_json::from_value(args.clone()) {
        Ok(s) => s,
        Err(e) => {
            return json!({"ok": false, "error": format!("invalid snapshot payload: {e}")})
                .to_string()
        }
    };
    match storage.apply_snapshot(&snapshot) {
        Ok(result) => json!({
            "ok": true,
            "preferences_applied": result.preferences_applied,
            "preferences_skipped": result.preferences_skipped,
            "routing_preferences_applied": result.routing_preferences_applied,
            "routing_preferences_skipped": result.routing_preferences_skipped,
            "reflex_preferences_applied": result.reflex_preferences_applied,
            "reflex_preferences_skipped": result.reflex_preferences_skipped,
            "declarations_applied": result.declarations_applied,
            "declarations_skipped": result.declarations_skipped,
        })
        .to_string(),
        Err(e) => json!({"ok": false, "error": e.to_string()}).to_string(),
    }
}

// ── experience trace recording ────────────────────────────────────────────────

/// Record a tool-call outcome into the experience ledger.
pub fn record_tool_call_trace(
    storage: &dyn AgentGraphStorage,
    agent_id: &str,
    tool_name: &str,
    args: &Value,
    result_json: &Value,
    outcome: &str,
) {
    let trace = AgentExperienceTrace {
        trace_id: Ulid::new().to_string(),
        agent_id: agent_id.to_string(),
        event_type: "tool_call".to_string(),
        input_json: json!({"tool": tool_name, "args": args}),
        output_json: result_json.clone(),
        outcome: outcome.to_string(),
        outcome_detail: None,
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    };
    if let Err(e) = storage.record_experience_trace(&trace) {
        tracing::warn!("Failed to record experience trace: {e}");
    }
}
