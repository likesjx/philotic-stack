use serde_json::{json, Value};

use crate::graph::{
    EdgeFilter, EdgeInput, GraphSchema, GraphSpec, Identity, NodeFilter, NodeInput, TraversalDirection,
    TraversalQuery,
};
use crate::store::GraphStore;

/// Dispatch an incoming tool call. Returns the string result content for the IPC reply.
pub fn dispatch(store: &dyn GraphStore, tool_name: &str, args: &Value, identity: &Identity) -> String {
    match tool_name {
        "graph.create" => graph_create(store, args, identity),
        "graph.list" => graph_list(store),
        "graph.schema.get" => graph_schema_get(store, args),
        "graph.schema.update" => graph_schema_update(store, args),
        "graph.node.upsert" => graph_node_upsert(store, args, identity),
        "graph.node.get" => graph_node_get(store, args, identity),
        "graph.node.list" => graph_node_list(store, args, identity),
        "graph.node.delete" => graph_node_delete(store, args),
        "graph.edge.upsert" => graph_edge_upsert(store, args, identity),
        "graph.edge.get" => graph_edge_get(store, args, identity),
        "graph.edge.list" => graph_edge_list(store, args, identity),
        "graph.edge.delete" => graph_edge_delete(store, args),
        "graph.traverse" => graph_traverse(store, args, identity),
        "graph.search" => graph_search(store, args, identity),
        _ => format!("{tool_name}: unsupported graph tool"),
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing required field '{key}'"))
}

fn identity_from_args(args: &Value) -> Identity {
    let id = args
        .get("caller_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let roles = args
        .get("caller_roles")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    Identity { id, roles }
}

fn str_vec(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn ok_json(v: Value) -> String {
    v.to_string()
}

fn err_str(tool: &str, msg: impl std::fmt::Display) -> String {
    json!({ "ok": false, "error": msg.to_string() }).to_string() + &format!(" [{tool}]")
}

// ── Graph management ──────────────────────────────────────────────────────────

fn graph_create(store: &dyn GraphStore, args: &Value, identity: &Identity) -> String {
    let name = match require_str(args, "name") {
        Ok(n) => n.to_string(),
        Err(e) => return err_str("graph.create", e),
    };
    let description = args.get("description").and_then(Value::as_str).map(str::to_string);
    let default_visibility = args
        .get("default_visibility")
        .and_then(Value::as_str)
        .unwrap_or("private")
        .to_string();

    let schema: GraphSchema = args
        .get("schema")
        .and_then(|s| serde_json::from_value(s.clone()).ok())
        .unwrap_or_default();

    let caller = args
        .get("caller_id")
        .and_then(Value::as_str)
        .unwrap_or(&identity.id)
        .to_string();

    match store.create_graph(GraphSpec { name, description, schema, default_visibility, creator: caller }) {
        Ok(graph_id) => ok_json(json!({ "ok": true, "graph_id": graph_id })),
        Err(e) => err_str("graph.create", e),
    }
}

fn graph_list(store: &dyn GraphStore) -> String {
    match store.list_graphs() {
        Ok(graphs) => ok_json(json!({ "ok": true, "graphs": graphs })),
        Err(e) => err_str("graph.list", e),
    }
}

fn graph_schema_get(store: &dyn GraphStore, args: &Value) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("graph.schema.get", e),
    };
    match store.get_graph(&graph_id) {
        Ok(Some(meta)) => ok_json(json!({ "ok": true, "schema": meta.schema })),
        Ok(None) => err_str("graph.schema.get", format!("graph '{graph_id}' not found")),
        Err(e) => err_str("graph.schema.get", e),
    }
}

fn graph_schema_update(store: &dyn GraphStore, args: &Value) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("graph.schema.update", e),
    };
    let schema: GraphSchema = match args.get("schema").and_then(|s| serde_json::from_value(s.clone()).ok()) {
        Some(s) => s,
        None => return err_str("graph.schema.update", "missing or invalid 'schema'"),
    };
    match store.update_schema(&graph_id, schema) {
        Ok(()) => ok_json(json!({ "ok": true })),
        Err(e) => err_str("graph.schema.update", e),
    }
}

// ── Node operations ───────────────────────────────────────────────────────────

fn graph_node_upsert(store: &dyn GraphStore, args: &Value, identity: &Identity) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("graph.node.upsert", e),
    };
    let node_type = args.get("node_type").and_then(Value::as_str).unwrap_or("").to_string();
    let label = match require_str(args, "label") {
        Ok(l) => l.to_string(),
        Err(e) => return err_str("graph.node.upsert", e),
    };
    let content = args.get("content").cloned().unwrap_or(Value::Null);
    let tags = args.get("tags").map(str_vec).unwrap_or_default();
    let visibility = args.get("visibility").map(str_vec).unwrap_or_default();
    let creator = args
        .get("creator")
        .and_then(Value::as_str)
        .unwrap_or(&identity.id)
        .to_string();
    let node_id = args.get("node_id").and_then(Value::as_str).map(str::to_string);

    match store.upsert_node(&graph_id, NodeInput { node_id, node_type, label, content, tags, visibility, creator }) {
        Ok(nid) => ok_json(json!({ "ok": true, "node_id": nid })),
        Err(e) => err_str("graph.node.upsert", e),
    }
}

fn graph_node_get(store: &dyn GraphStore, args: &Value, identity: &Identity) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id,
        Err(e) => return err_str("graph.node.get", e),
    };
    let node_id = match require_str(args, "node_id") {
        Ok(id) => id,
        Err(e) => return err_str("graph.node.get", e),
    };
    let caller = identity_from_args(args);
    let eff_identity = if caller.id != "unknown" { &caller } else { identity };
    match store.get_node(graph_id, node_id, eff_identity) {
        Ok(Some(n)) => ok_json(json!({ "ok": true, "node": n })),
        Ok(None) => ok_json(json!({ "ok": true, "node": null })),
        Err(e) => err_str("graph.node.get", e),
    }
}

fn graph_node_list(store: &dyn GraphStore, args: &Value, identity: &Identity) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("graph.node.list", e),
    };
    let filter = NodeFilter {
        node_type: args.get("node_type").and_then(Value::as_str).map(str::to_string),
        tags: args.get("tags").map(str_vec).filter(|v| !v.is_empty()),
        creator: args.get("creator").and_then(Value::as_str).map(str::to_string),
    };
    let caller = identity_from_args(args);
    let eff_identity = if caller.id != "unknown" { &caller } else { identity };
    match store.list_nodes(&graph_id, &filter, eff_identity) {
        Ok(nodes) => ok_json(json!({ "ok": true, "nodes": nodes })),
        Err(e) => err_str("graph.node.list", e),
    }
}

fn graph_node_delete(store: &dyn GraphStore, args: &Value) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id,
        Err(e) => return err_str("graph.node.delete", e),
    };
    let node_id = match require_str(args, "node_id") {
        Ok(id) => id,
        Err(e) => return err_str("graph.node.delete", e),
    };
    match store.delete_node(graph_id, node_id) {
        Ok(()) => ok_json(json!({ "ok": true })),
        Err(e) => err_str("graph.node.delete", e),
    }
}

// ── Edge operations ───────────────────────────────────────────────────────────

fn graph_edge_upsert(store: &dyn GraphStore, args: &Value, identity: &Identity) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("graph.edge.upsert", e),
    };
    let from_node_id = match require_str(args, "from_node_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("graph.edge.upsert", e),
    };
    let to_node_id = match require_str(args, "to_node_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("graph.edge.upsert", e),
    };
    let edge_type = args.get("edge_type").and_then(Value::as_str).unwrap_or("").to_string();
    let label = args.get("label").and_then(Value::as_str).map(str::to_string);
    let content = args.get("content").cloned().unwrap_or(Value::Null);
    let visibility = args.get("visibility").map(str_vec).unwrap_or_default();
    let creator = args
        .get("creator")
        .and_then(Value::as_str)
        .unwrap_or(&identity.id)
        .to_string();
    let edge_id = args.get("edge_id").and_then(Value::as_str).map(str::to_string);

    match store.upsert_edge(&graph_id, EdgeInput { edge_id, from_node_id, to_node_id, edge_type, label, content, visibility, creator }) {
        Ok(eid) => ok_json(json!({ "ok": true, "edge_id": eid })),
        Err(e) => err_str("graph.edge.upsert", e),
    }
}

fn graph_edge_get(store: &dyn GraphStore, args: &Value, identity: &Identity) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id,
        Err(e) => return err_str("graph.edge.get", e),
    };
    let edge_id = match require_str(args, "edge_id") {
        Ok(id) => id,
        Err(e) => return err_str("graph.edge.get", e),
    };
    let caller = identity_from_args(args);
    let eff_identity = if caller.id != "unknown" { &caller } else { identity };
    match store.get_edge(graph_id, edge_id, eff_identity) {
        Ok(Some(e)) => ok_json(json!({ "ok": true, "edge": e })),
        Ok(None) => ok_json(json!({ "ok": true, "edge": null })),
        Err(e) => err_str("graph.edge.get", e),
    }
}

fn graph_edge_list(store: &dyn GraphStore, args: &Value, identity: &Identity) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("graph.edge.list", e),
    };
    let filter = EdgeFilter {
        from_node_id: args.get("from_node_id").and_then(Value::as_str).map(str::to_string),
        to_node_id: args.get("to_node_id").and_then(Value::as_str).map(str::to_string),
        edge_type: args.get("edge_type").and_then(Value::as_str).map(str::to_string),
        creator: args.get("creator").and_then(Value::as_str).map(str::to_string),
    };
    let caller = identity_from_args(args);
    let eff_identity = if caller.id != "unknown" { &caller } else { identity };
    match store.list_edges(&graph_id, &filter, eff_identity) {
        Ok(edges) => ok_json(json!({ "ok": true, "edges": edges })),
        Err(e) => err_str("graph.edge.list", e),
    }
}

fn graph_edge_delete(store: &dyn GraphStore, args: &Value) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id,
        Err(e) => return err_str("graph.edge.delete", e),
    };
    let edge_id = match require_str(args, "edge_id") {
        Ok(id) => id,
        Err(e) => return err_str("graph.edge.delete", e),
    };
    match store.delete_edge(graph_id, edge_id) {
        Ok(()) => ok_json(json!({ "ok": true })),
        Err(e) => err_str("graph.edge.delete", e),
    }
}

// ── Traversal + Search ────────────────────────────────────────────────────────

fn graph_traverse(store: &dyn GraphStore, args: &Value, identity: &Identity) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("graph.traverse", e),
    };
    let start_node_id = match require_str(args, "start_node_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("graph.traverse", e),
    };
    let direction = match args.get("direction").and_then(Value::as_str).unwrap_or("outbound") {
        "inbound" => TraversalDirection::Inbound,
        "both" => TraversalDirection::Both,
        _ => TraversalDirection::Outbound,
    };
    let max_depth = args
        .get("max_depth")
        .and_then(Value::as_u64)
        .unwrap_or(3)
        .min(10) as u32;
    let edge_types = args.get("edge_types").map(str_vec).filter(|v| !v.is_empty());

    let caller = identity_from_args(args);
    let eff_identity = if caller.id != "unknown" { &caller } else { identity };

    let query = TraversalQuery { start_node_id, direction, max_depth, edge_types };
    match store.traverse(&graph_id, &query, eff_identity) {
        Ok(result) => ok_json(json!({ "ok": true, "result": result })),
        Err(e) => err_str("graph.traverse", e),
    }
}

fn graph_search(store: &dyn GraphStore, args: &Value, identity: &Identity) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("graph.search", e),
    };
    let query = match require_str(args, "query") {
        Ok(q) => q.to_string(),
        Err(e) => return err_str("graph.search", e),
    };
    let caller = identity_from_args(args);
    let eff_identity = if caller.id != "unknown" { &caller } else { identity };
    match store.search_nodes(&graph_id, &query, eff_identity) {
        Ok(nodes) => ok_json(json!({ "ok": true, "nodes": nodes })),
        Err(e) => err_str("graph.search", e),
    }
}
