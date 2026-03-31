//! Integration smoke for the agent-graph tool surface.
//!
//! Exercises `tools::dispatch` end-to-end through a real `SqliteAgentGraphStorage`
//! instance backed by a temp file. No hotel IPC required — this tests the
//! storage + dispatch stack that the runner exposes to the hotel.
//!
//! Run with: `cargo test -p agent-graph-runner`

use agent_graph_runner::tools;
use ansible_mesh_core::agent_graph_storage::SqliteAgentGraphStorage;
use serde_json::json;
use tempfile::NamedTempFile;

fn open_storage(agent_id: &str) -> (SqliteAgentGraphStorage, NamedTempFile) {
    let f = NamedTempFile::new().unwrap();
    let s = SqliteAgentGraphStorage::open(agent_id, f.path()).unwrap();
    (s, f)
}

fn dispatch_ok(
    storage: &dyn ansible_mesh_core::agent_graph_storage::AgentGraphStorage,
    tool: &str,
    args: serde_json::Value,
    agent_id: &str,
) -> serde_json::Value {
    let raw = tools::dispatch(storage, tool, &args, agent_id);
    let v: serde_json::Value = serde_json::from_str(&raw).expect("dispatch must return valid JSON");
    assert!(
        v.get("ok").and_then(|v| v.as_bool()).unwrap_or(false),
        "tool {tool} returned ok=false: {v}"
    );
    v
}

// ── agent.graph.write + agent.graph.read ──────────────────────────────────────

#[test]
fn write_tool_preference_then_read_it_back() {
    let (storage, _f) = open_storage("smoke-agent");

    dispatch_ok(
        &storage,
        "agent.graph.write",
        json!({
            "entity": "tool_preference",
            "tool_name": "bash.exec",
            "preference_level": 1,
            "config": {"timeout_secs": 30}
        }),
        "smoke-agent",
    );

    let result = dispatch_ok(
        &storage,
        "agent.graph.read",
        json!({"entity": "tool_preferences"}),
        "smoke-agent",
    );

    let prefs = result["tool_preferences"]
        .as_array()
        .expect("tool_preferences array");
    assert_eq!(prefs.len(), 1);
    assert_eq!(prefs[0]["tool_name"], "bash.exec");
    assert_eq!(prefs[0]["preference_level"], 1);
}

// ── agent.graph.declare ───────────────────────────────────────────────────────

#[test]
fn declare_resource_and_read_back() {
    let (storage, _f) = open_storage("smoke-agent");

    dispatch_ok(
        &storage,
        "agent.graph.declare",
        json!({"resource_type": "model_router", "config_hint": "gemini"}),
        "smoke-agent",
    );

    let result = dispatch_ok(
        &storage,
        "agent.graph.read",
        json!({"entity": "resource_declarations"}),
        "smoke-agent",
    );

    let decls = result["resource_declarations"]
        .as_array()
        .expect("declarations array");
    assert_eq!(decls.len(), 1);
    assert_eq!(decls[0]["resource_type"], "model_router");
    assert_eq!(decls[0]["config_hint"], "gemini");
}

// ── agent.graph.recall ────────────────────────────────────────────────────────

#[test]
fn recall_returns_experience_traces() {
    use ansible_mesh_core::agent_graph_storage::AgentExperienceTrace;
    use ansible_mesh_core::agent_graph_storage::AgentGraphStorage;

    let (storage, _f) = open_storage("smoke-agent");

    // Directly record a trace (the IPC loop normally calls record_tool_call_trace;
    // here we populate the ledger manually to test the recall path end-to-end).
    storage
        .record_experience_trace(&AgentExperienceTrace {
            trace_id: "smoke-trace-01".into(),
            agent_id: "smoke-agent".into(),
            event_type: "tool_call".into(),
            input_json: json!({"tool": "bash.exec"}),
            output_json: json!({"ok": true}),
            outcome: "success".into(),
            outcome_detail: None,
            timestamp: 1_000_000,
        })
        .unwrap();

    let result = dispatch_ok(
        &storage,
        "agent.graph.recall",
        json!({"limit": 5}),
        "smoke-agent",
    );

    let traces = result["traces"].as_array().expect("traces array");
    assert!(
        !traces.is_empty(),
        "experience ledger should contain at least one trace"
    );
    assert_eq!(traces[0]["event_type"], "tool_call");
}

// ── agent.graph.sync (LWW merge) ──────────────────────────────────────────────

#[test]
fn sync_inserts_new_preference_from_snapshot() {
    use ansible_mesh_core::agent_graph_storage::{AgentGraphSnapshot, SnapshotPreference};

    let (storage, _f) = open_storage("smoke-agent");

    // No local preference for "new.tool" — snapshot should insert it.
    let snapshot = AgentGraphSnapshot {
        agent_id: "smoke-agent".into(),
        source_node_id: "remote-hotel".into(),
        exported_at: 1_000_000,
        preferences: vec![SnapshotPreference {
            tool_name: "new.tool".into(),
            preference_level: 1,
            config_json: json!({"setting": true}),
            updated_at: 1_000_000,
        }],
        declarations: vec![],
    };

    let merge_args = serde_json::to_value(&snapshot).unwrap();
    let result = dispatch_ok(&storage, "agent.graph.sync", merge_args, "smoke-agent");

    assert_eq!(result["preferences_applied"], 1);
    assert_eq!(result["preferences_skipped"], 0);

    // Verify it is readable.
    let read_result = dispatch_ok(
        &storage,
        "agent.graph.read",
        json!({"entity": "tool_preferences"}),
        "smoke-agent",
    );
    let prefs = read_result["tool_preferences"].as_array().unwrap();
    assert!(prefs.iter().any(|p| p["tool_name"] == "new.tool"));
}

// ── unknown tool returns ok=false ────────────────────────────────────────────

#[test]
fn unknown_tool_returns_error_not_panic() {
    let (storage, _f) = open_storage("smoke-agent");
    let raw = tools::dispatch(
        &storage,
        "agent.graph.nonexistent",
        &json!({}),
        "smoke-agent",
    );
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(v["ok"], false);
    assert!(v["error"].as_str().is_some());
}
