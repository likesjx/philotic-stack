use serde_json::{json, Value};

use crate::graph::{
    ColumnSpec, EdgeFilter, EdgeInput, GraphSchema, GraphSpec, Identity, NodeFilter, NodeInput,
    RowInput, RowQuery, TableSpec, TraversalDirection, TraversalQuery,
};
use crate::store::{GraphStore, GraphTableStore, TableStore};

/// Dispatch an incoming tool call. Returns the string result content for the IPC reply.
pub fn dispatch(
    store: &dyn GraphTableStore,
    tool_name: &str,
    args: &Value,
    identity: &Identity,
) -> String {
    match tool_name {
        // Graph tools
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
        "graph.export" => graph_export(store, args, identity),
        // Table tools
        "table.create" => table_create(store, args),
        "table.schema.get" => table_schema_get(store, args),
        "table.update" => table_update(store, args),
        "table.drop" => table_drop(store, args),
        "table.list" => table_list(store, args),
        "table.row.insert" => table_row_insert(store, args),
        "table.row.get" => table_row_get(store, args),
        "table.row.update" => table_row_update(store, args),
        "table.row.delete" => table_row_delete(store, args),
        "table.query" => table_query(store, args),
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
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Identity { id, roles }
}

fn str_vec(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn ok_json(v: Value) -> String {
    v.to_string()
}

fn err_str(tool: &str, msg: impl std::fmt::Display) -> String {
    json!({ "ok": false, "error": msg.to_string() }).to_string() + &format!(" [{tool}]")
}

// ── Graph management ──────────────────────────────────────────────────────────

fn graph_create(store: &dyn GraphTableStore, args: &Value, identity: &Identity) -> String {
    let name = match require_str(args, "name") {
        Ok(n) => n.to_string(),
        Err(e) => return err_str("graph.create", e),
    };
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string);
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

    match store.create_graph(GraphSpec {
        name,
        description,
        schema,
        default_visibility,
        creator: caller,
    }) {
        Ok(graph_id) => ok_json(json!({ "ok": true, "graph_id": graph_id })),
        Err(e) => err_str("graph.create", e),
    }
}

fn graph_list(store: &dyn GraphTableStore) -> String {
    match store.list_graphs() {
        Ok(graphs) => ok_json(json!({ "ok": true, "graphs": graphs })),
        Err(e) => err_str("graph.list", e),
    }
}

fn graph_schema_get(store: &dyn GraphTableStore, args: &Value) -> String {
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

fn graph_schema_update(store: &dyn GraphTableStore, args: &Value) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("graph.schema.update", e),
    };
    let schema: GraphSchema = match args
        .get("schema")
        .and_then(|s| serde_json::from_value(s.clone()).ok())
    {
        Some(s) => s,
        None => return err_str("graph.schema.update", "missing or invalid 'schema'"),
    };
    match store.update_schema(&graph_id, schema) {
        Ok(()) => ok_json(json!({ "ok": true })),
        Err(e) => err_str("graph.schema.update", e),
    }
}

// ── Node operations ───────────────────────────────────────────────────────────

fn graph_node_upsert(store: &dyn GraphTableStore, args: &Value, identity: &Identity) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("graph.node.upsert", e),
    };
    let node_type = args
        .get("node_type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
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
    let node_id = args
        .get("node_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let table_ref = args
        .get("table_ref")
        .and_then(Value::as_str)
        .map(str::to_string);

    match store.upsert_node(
        &graph_id,
        NodeInput {
            node_id,
            node_type,
            label,
            content,
            tags,
            visibility,
            creator,
            table_ref,
        },
    ) {
        Ok(nid) => ok_json(json!({ "ok": true, "node_id": nid })),
        Err(e) => err_str("graph.node.upsert", e),
    }
}

fn graph_node_get(store: &dyn GraphTableStore, args: &Value, identity: &Identity) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id,
        Err(e) => return err_str("graph.node.get", e),
    };
    let node_id = match require_str(args, "node_id") {
        Ok(id) => id,
        Err(e) => return err_str("graph.node.get", e),
    };
    let caller = identity_from_args(args);
    let eff_identity = if caller.id != "unknown" {
        &caller
    } else {
        identity
    };
    match store.get_node(graph_id, node_id, eff_identity) {
        Ok(Some(n)) => ok_json(json!({ "ok": true, "node": n })),
        Ok(None) => ok_json(json!({ "ok": true, "node": null })),
        Err(e) => err_str("graph.node.get", e),
    }
}

fn graph_node_list(store: &dyn GraphTableStore, args: &Value, identity: &Identity) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("graph.node.list", e),
    };
    let filter = NodeFilter {
        node_type: args
            .get("node_type")
            .and_then(Value::as_str)
            .map(str::to_string),
        tags: args.get("tags").map(str_vec).filter(|v| !v.is_empty()),
        creator: args
            .get("creator")
            .and_then(Value::as_str)
            .map(str::to_string),
    };
    let caller = identity_from_args(args);
    let eff_identity = if caller.id != "unknown" {
        &caller
    } else {
        identity
    };
    match store.list_nodes(&graph_id, &filter, eff_identity) {
        Ok(nodes) => ok_json(json!({ "ok": true, "nodes": nodes })),
        Err(e) => err_str("graph.node.list", e),
    }
}

fn graph_node_delete(store: &dyn GraphTableStore, args: &Value) -> String {
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

fn graph_edge_upsert(store: &dyn GraphTableStore, args: &Value, identity: &Identity) -> String {
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
    let edge_type = args
        .get("edge_type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let label = args
        .get("label")
        .and_then(Value::as_str)
        .map(str::to_string);
    let content = args.get("content").cloned().unwrap_or(Value::Null);
    let visibility = args.get("visibility").map(str_vec).unwrap_or_default();
    let creator = args
        .get("creator")
        .and_then(Value::as_str)
        .unwrap_or(&identity.id)
        .to_string();
    let edge_id = args
        .get("edge_id")
        .and_then(Value::as_str)
        .map(str::to_string);

    match store.upsert_edge(
        &graph_id,
        EdgeInput {
            edge_id,
            from_node_id,
            to_node_id,
            edge_type,
            label,
            content,
            visibility,
            creator,
        },
    ) {
        Ok(eid) => ok_json(json!({ "ok": true, "edge_id": eid })),
        Err(e) => err_str("graph.edge.upsert", e),
    }
}

fn graph_edge_get(store: &dyn GraphTableStore, args: &Value, identity: &Identity) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id,
        Err(e) => return err_str("graph.edge.get", e),
    };
    let edge_id = match require_str(args, "edge_id") {
        Ok(id) => id,
        Err(e) => return err_str("graph.edge.get", e),
    };
    let caller = identity_from_args(args);
    let eff_identity = if caller.id != "unknown" {
        &caller
    } else {
        identity
    };
    match store.get_edge(graph_id, edge_id, eff_identity) {
        Ok(Some(e)) => ok_json(json!({ "ok": true, "edge": e })),
        Ok(None) => ok_json(json!({ "ok": true, "edge": null })),
        Err(e) => err_str("graph.edge.get", e),
    }
}

fn graph_edge_list(store: &dyn GraphTableStore, args: &Value, identity: &Identity) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("graph.edge.list", e),
    };
    let filter = EdgeFilter {
        from_node_id: args
            .get("from_node_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        to_node_id: args
            .get("to_node_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        edge_type: args
            .get("edge_type")
            .and_then(Value::as_str)
            .map(str::to_string),
        creator: args
            .get("creator")
            .and_then(Value::as_str)
            .map(str::to_string),
    };
    let caller = identity_from_args(args);
    let eff_identity = if caller.id != "unknown" {
        &caller
    } else {
        identity
    };
    match store.list_edges(&graph_id, &filter, eff_identity) {
        Ok(edges) => ok_json(json!({ "ok": true, "edges": edges })),
        Err(e) => err_str("graph.edge.list", e),
    }
}

fn graph_edge_delete(store: &dyn GraphTableStore, args: &Value) -> String {
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

fn graph_traverse(store: &dyn GraphTableStore, args: &Value, identity: &Identity) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("graph.traverse", e),
    };
    let start_node_id = match require_str(args, "start_node_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("graph.traverse", e),
    };
    let direction = match args
        .get("direction")
        .and_then(Value::as_str)
        .unwrap_or("outbound")
    {
        "inbound" => TraversalDirection::Inbound,
        "both" => TraversalDirection::Both,
        _ => TraversalDirection::Outbound,
    };
    let max_depth = args
        .get("max_depth")
        .and_then(Value::as_u64)
        .unwrap_or(3)
        .min(10) as u32;
    let edge_types = args
        .get("edge_types")
        .map(str_vec)
        .filter(|v| !v.is_empty());

    let caller = identity_from_args(args);
    let eff_identity = if caller.id != "unknown" {
        &caller
    } else {
        identity
    };

    let query = TraversalQuery {
        start_node_id,
        direction,
        max_depth,
        edge_types,
    };
    match store.traverse(&graph_id, &query, eff_identity) {
        Ok(result) => ok_json(json!({ "ok": true, "result": result })),
        Err(e) => err_str("graph.traverse", e),
    }
}

fn graph_search(store: &dyn GraphTableStore, args: &Value, identity: &Identity) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("graph.search", e),
    };
    let query = match require_str(args, "query") {
        Ok(q) => q.to_string(),
        Err(e) => return err_str("graph.search", e),
    };
    let caller = identity_from_args(args);
    let eff_identity = if caller.id != "unknown" {
        &caller
    } else {
        identity
    };
    match store.search_nodes(&graph_id, &query, eff_identity) {
        Ok(nodes) => ok_json(json!({ "ok": true, "nodes": nodes })),
        Err(e) => err_str("graph.search", e),
    }
}

// ── Export ────────────────────────────────────────────────────────────────────

/// `graph.export` — return a visibility-filtered snapshot of a graph as JSON.
///
/// If `root_node_id` is provided the result is a subgraph rooted there,
/// traversed in both directions up to `max_depth` (default 10, cap 20).
/// Without `root_node_id` the entire graph is returned.
///
/// Response: `{ ok, graph_id, node_count, edge_count, nodes: [...], edges: [...] }`
fn graph_export(store: &dyn GraphTableStore, args: &Value, identity: &Identity) -> String {
    let graph_id = match require_str(args, "graph_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("graph.export", e),
    };

    let caller = identity_from_args(args);
    let eff_identity = if caller.id != "unknown" {
        &caller
    } else {
        identity
    };

    let (nodes, edges) = if let Some(root) = args.get("root_node_id").and_then(Value::as_str) {
        let max_depth = args
            .get("max_depth")
            .and_then(Value::as_u64)
            .unwrap_or(10)
            .min(20) as u32;
        let edge_types = args
            .get("edge_types")
            .map(str_vec)
            .filter(|v| !v.is_empty());
        let query = TraversalQuery {
            start_node_id: root.to_string(),
            direction: TraversalDirection::Both,
            max_depth,
            edge_types,
        };
        match store.traverse(&graph_id, &query, eff_identity) {
            Ok(r) => (r.nodes, r.edges),
            Err(e) => return err_str("graph.export", e),
        }
    } else {
        let nodes = match store.list_nodes(&graph_id, &NodeFilter::default(), eff_identity) {
            Ok(n) => n,
            Err(e) => return err_str("graph.export", e),
        };
        let edges = match store.list_edges(&graph_id, &EdgeFilter::default(), eff_identity) {
            Ok(e) => e,
            Err(e) => return err_str("graph.export", e),
        };
        (nodes, edges)
    };

    ok_json(json!({
        "ok": true,
        "graph_id": graph_id,
        "node_count": nodes.len(),
        "edge_count": edges.len(),
        "nodes": nodes,
        "edges": edges,
    }))
}

// ── Table operations ───────────────────────────────────────────────────────────

fn parse_columns(args: &Value, key: &str) -> Vec<ColumnSpec> {
    args.get(key)
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default()
}

fn table_create(store: &dyn GraphTableStore, args: &Value) -> String {
    let name = match require_str(args, "name") {
        Ok(n) => n.to_string(),
        Err(e) => return err_str("table.create", e),
    };
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string);
    let columns = parse_columns(args, "columns");
    let graph_id = args
        .get("graph_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let creator = args
        .get("creator")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    match store.create_table(TableSpec {
        name,
        description,
        columns,
        graph_id,
        creator,
    }) {
        Ok(tid) => ok_json(json!({ "ok": true, "table_id": tid })),
        Err(e) => err_str("table.create", e),
    }
}

fn table_schema_get(store: &dyn GraphTableStore, args: &Value) -> String {
    let table_id = match require_str(args, "table_id") {
        Ok(id) => id,
        Err(e) => return err_str("table.schema.get", e),
    };
    match store.get_table(table_id) {
        Ok(Some(meta)) => ok_json(json!({ "ok": true, "table": meta })),
        Ok(None) => err_str("table.schema.get", format!("table '{table_id}' not found")),
        Err(e) => err_str("table.schema.get", e),
    }
}

fn table_update(store: &dyn GraphTableStore, args: &Value) -> String {
    let table_id = match require_str(args, "table_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("table.update", e),
    };
    let name = args.get("name").and_then(Value::as_str).map(str::to_string);
    let description = args
        .get("description")
        .and_then(Value::as_str)
        .map(str::to_string);
    let columns = args.get("columns").map(|_| parse_columns(args, "columns"));

    match store.update_table(&table_id, name, description, columns) {
        Ok(()) => ok_json(json!({ "ok": true })),
        Err(e) => err_str("table.update", e),
    }
}

fn table_drop(store: &dyn GraphTableStore, args: &Value) -> String {
    let table_id = match require_str(args, "table_id") {
        Ok(id) => id,
        Err(e) => return err_str("table.drop", e),
    };
    match store.drop_table(table_id) {
        Ok(()) => ok_json(json!({ "ok": true })),
        Err(e) => err_str("table.drop", e),
    }
}

fn table_list(store: &dyn GraphTableStore, args: &Value) -> String {
    let graph_id = args.get("graph_id").and_then(Value::as_str);
    match store.list_tables(graph_id) {
        Ok(tables) => ok_json(json!({ "ok": true, "tables": tables })),
        Err(e) => err_str("table.list", e),
    }
}

fn table_row_insert(store: &dyn GraphTableStore, args: &Value) -> String {
    let table_id = match require_str(args, "table_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("table.row.insert", e),
    };
    let data = args.get("data").cloned().unwrap_or(Value::Null);
    let creator = args
        .get("creator")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let row_id = args
        .get("row_id")
        .and_then(Value::as_str)
        .map(str::to_string);

    match store.insert_row(
        &table_id,
        RowInput {
            row_id,
            data,
            creator,
        },
    ) {
        Ok(rid) => ok_json(json!({ "ok": true, "row_id": rid })),
        Err(e) => err_str("table.row.insert", e),
    }
}

fn table_row_get(store: &dyn GraphTableStore, args: &Value) -> String {
    let table_id = match require_str(args, "table_id") {
        Ok(id) => id,
        Err(e) => return err_str("table.row.get", e),
    };
    let row_id = match require_str(args, "row_id") {
        Ok(id) => id,
        Err(e) => return err_str("table.row.get", e),
    };
    match store.get_row(table_id, row_id) {
        Ok(Some(row)) => ok_json(json!({ "ok": true, "row": row })),
        Ok(None) => ok_json(json!({ "ok": true, "row": null })),
        Err(e) => err_str("table.row.get", e),
    }
}

fn table_row_update(store: &dyn GraphTableStore, args: &Value) -> String {
    let table_id = match require_str(args, "table_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("table.row.update", e),
    };
    let row_id = match require_str(args, "row_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("table.row.update", e),
    };
    let patch = args.get("patch").cloned().unwrap_or(Value::Null);

    match store.update_row(&table_id, &row_id, patch) {
        Ok(()) => ok_json(json!({ "ok": true })),
        Err(e) => err_str("table.row.update", e),
    }
}

fn table_row_delete(store: &dyn GraphTableStore, args: &Value) -> String {
    let table_id = match require_str(args, "table_id") {
        Ok(id) => id,
        Err(e) => return err_str("table.row.delete", e),
    };
    let row_id = match require_str(args, "row_id") {
        Ok(id) => id,
        Err(e) => return err_str("table.row.delete", e),
    };
    match store.delete_row(table_id, row_id) {
        Ok(()) => ok_json(json!({ "ok": true })),
        Err(e) => err_str("table.row.delete", e),
    }
}

fn table_query(store: &dyn GraphTableStore, args: &Value) -> String {
    let table_id = match require_str(args, "table_id") {
        Ok(id) => id.to_string(),
        Err(e) => return err_str("table.query", e),
    };
    let filter = args.get("filter").cloned();
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .map(|n| n.min(1000) as u32);
    let offset = args.get("offset").and_then(Value::as_u64).map(|n| n as u32);

    let query = RowQuery {
        filter,
        limit,
        offset,
    };
    match store.query_rows(&table_id, &query) {
        Ok(rows) => ok_json(json!({ "ok": true, "rows": rows })),
        Err(e) => err_str("table.query", e),
    }
}
