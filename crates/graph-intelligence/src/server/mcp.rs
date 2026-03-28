use std::path::Path;
use std::sync::Arc;

use axum::{extract::State, routing::post, Json, Router};
use serde::{Deserialize, Serialize};

use crate::scanner::{full_scan, ScanConfig};
use crate::schema::*;

use super::ws::ChangeEvent;
use super::AppState;

// ── JSON-RPC 2.0 types ──

#[derive(Deserialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    params: Option<serde_json::Value>,
    id: serde_json::Value,
}

#[derive(Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    result: Option<serde_json::Value>,
    error: Option<JsonRpcError>,
    id: serde_json::Value,
}

#[derive(Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
}

// ── MCP Tool Definitions ──

fn tool_definitions() -> serde_json::Value {
    serde_json::json!({
        "tools": [
            {
                "name": "graph_status",
                "description": "Get overall project graph status — node counts, last scan time, metrics",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "graph_query",
                "description": "Query nodes by kind and optional filters. Returns nodes with their properties.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "description": "Node kind filter (proposal, seam, crate, module, type, function, etc.)" },
                        "worktree": { "type": "string", "description": "Worktree filter" },
                        "status": { "type": "string", "description": "Status filter (for proposals)" }
                    }
                }
            },
            {
                "name": "graph_node",
                "description": "Get a single node by ID with all its edges and related nodes",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Node ID (e.g., 'proposal:desktop-membrane', 'crate:aiua', 'type:GraphDomain')" }
                    },
                    "required": ["id"]
                }
            },
            {
                "name": "graph_skeleton",
                "description": "Generate a PlantUML class diagram for a crate showing its types, traits, and relationships",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "crate_name": { "type": "string", "description": "Crate name (e.g., 'aiua', 'ansible-mesh-core')" }
                    },
                    "required": ["crate_name"]
                }
            },
            {
                "name": "graph_snippet",
                "description": "Get code snippets for a node. Returns signatures by default; pass full=true for complete source.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "node_id": { "type": "string", "description": "Node ID to get snippets for" },
                        "full": { "type": "boolean", "description": "Include full body (default: false, returns signatures only)" }
                    },
                    "required": ["node_id"]
                }
            },
            {
                "name": "graph_search",
                "description": "Full-text search across all nodes and code snippets",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "Search text" }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "graph_proposals",
                "description": "List all proposals with their current status, domain, and active seams",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "graph_decide",
                "description": "Record a traced decision about a node (e.g., change proposal status, record an architectural decision)",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "target_node": { "type": "string", "description": "Node ID the decision applies to" },
                        "action": { "type": "string", "description": "What action was taken (e.g., 'status_change', 'decision', 'defer')" },
                        "from_value": { "type": "string", "description": "Previous value (if changing)" },
                        "to_value": { "type": "string", "description": "New value (if changing)" },
                        "reason": { "type": "string", "description": "Why this decision was made" },
                        "agent": { "type": "string", "description": "Agent making the decision" },
                        "session": { "type": "string", "description": "Session ID" }
                    },
                    "required": ["target_node", "action", "reason"]
                }
            },
            {
                "name": "graph_scan",
                "description": "Trigger a full rescan of the codebase, docs, and git state",
                "inputSchema": { "type": "object", "properties": {} }
            }
        ]
    })
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/mcp", post(handle_mcp))
        .with_state(state)
}

async fn handle_mcp(
    State(state): State<Arc<AppState>>,
    Json(req): Json<JsonRpcRequest>,
) -> Json<JsonRpcResponse> {
    if req.jsonrpc != "2.0" {
        return Json(JsonRpcResponse {
            jsonrpc: "2.0",
            result: None,
            error: Some(JsonRpcError {
                code: -32600,
                message: "Invalid JSON-RPC version".to_string(),
            }),
            id: req.id,
        });
    }

    let result = match req.method.as_str() {
        "tools/list" => Ok(tool_definitions()),
        "tools/call" => {
            let params = req.params.unwrap_or(serde_json::json!({}));
            let tool_name = params
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let arguments = params
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            execute_tool(&state, tool_name, &arguments).await
        }
        "initialize" => Ok(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "graph-intelligence",
                "version": "0.1.0"
            }
        })),
        _ => Err(JsonRpcError {
            code: -32601,
            message: format!("Method '{}' not found", req.method),
        }),
    };

    match result {
        Ok(value) => Json(JsonRpcResponse {
            jsonrpc: "2.0",
            result: Some(value),
            error: None,
            id: req.id,
        }),
        Err(err) => Json(JsonRpcResponse {
            jsonrpc: "2.0",
            result: None,
            error: Some(err),
            id: req.id,
        }),
    }
}

async fn execute_tool(
    state: &AppState,
    tool_name: &str,
    arguments: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    match tool_name {
        "graph_status" => tool_graph_status(state).await,
        "graph_query" => tool_graph_query(state, arguments).await,
        "graph_node" => tool_graph_node(state, arguments).await,
        "graph_skeleton" => tool_graph_skeleton(state, arguments).await,
        "graph_snippet" => tool_graph_snippet(state, arguments).await,
        "graph_search" => tool_graph_search(state, arguments).await,
        "graph_proposals" => tool_graph_proposals(state).await,
        "graph_decide" => tool_graph_decide(state, arguments).await,
        "graph_scan" => tool_graph_scan(state).await,
        _ => Err(JsonRpcError {
            code: -32602,
            message: format!("Unknown tool: {}", tool_name),
        }),
    }
}

fn mcp_err(msg: &str) -> JsonRpcError {
    JsonRpcError {
        code: -32603,
        message: msg.to_string(),
    }
}

async fn tool_graph_status(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let engine = state.engine.lock().await;

    let kinds = [
        NodeKind::Proposal,
        NodeKind::Seam,
        NodeKind::Crate,
        NodeKind::Module,
        NodeKind::Type,
        NodeKind::Function,
        NodeKind::Test,
        NodeKind::Commit,
        NodeKind::Branch,
    ];

    let mut counts = serde_json::Map::new();
    for kind in &kinds {
        let count = engine.count_nodes(Some(*kind)).map_err(|e| mcp_err(&e.to_string()))?;
        counts.insert(kind.as_str().to_string(), serde_json::json!(count));
    }
    let total = engine.count_nodes(None).map_err(|e| mcp_err(&e.to_string()))?;
    counts.insert("total".to_string(), serde_json::json!(total));

    let edges = engine.count_edges().map_err(|e| mcp_err(&e.to_string()))?;
    let snippets = engine.count_snippets().map_err(|e| mcp_err(&e.to_string()))?;

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&serde_json::json!({
                "node_counts": counts,
                "edge_count": edges,
                "snippet_count": snippets,
            })).unwrap_or_default()
        }]
    }))
}

async fn tool_graph_query(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let engine = state.engine.lock().await;

    let kind = args
        .get("kind")
        .and_then(|v| v.as_str())
        .and_then(NodeKind::from_str);
    let worktree = args.get("worktree").and_then(|v| v.as_str());
    let status_filter = args.get("status").and_then(|v| v.as_str());

    let mut nodes = engine
        .query_nodes(kind, worktree)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // Apply status filter if provided (checks properties.status)
    if let Some(status) = status_filter {
        nodes.retain(|n| {
            n.properties
                .get("status")
                .and_then(|v| v.as_str())
                .map(|s| s == status)
                .unwrap_or(false)
        });
    }

    let text = serde_json::to_string_pretty(&nodes).unwrap_or_default();
    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    }))
}

async fn tool_graph_node(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let id = args
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: id"))?;

    let engine = state.engine.lock().await;
    let node = engine
        .get_node(id)
        .map_err(|e| mcp_err(&e.to_string()))?
        .ok_or_else(|| mcp_err(&format!("Node '{}' not found", id)))?;

    let outgoing = engine.get_edges_from(id).map_err(|e| mcp_err(&e.to_string()))?;
    let incoming = engine.get_edges_to(id).map_err(|e| mcp_err(&e.to_string()))?;

    let text = serde_json::to_string_pretty(&serde_json::json!({
        "node": node,
        "edges": {
            "outgoing": outgoing,
            "incoming": incoming,
        }
    }))
    .unwrap_or_default();

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    }))
}

async fn tool_graph_skeleton(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let crate_name = args
        .get("crate_name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: crate_name"))?;

    let engine = state.engine.lock().await;
    let diagram = crate::plantuml::generate_crate_diagram(&engine, crate_name)
        .map_err(|e| mcp_err(&e.to_string()))?;

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": diagram }]
    }))
}

async fn tool_graph_snippet(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let node_id = args
        .get("node_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: node_id"))?;
    let full = args.get("full").and_then(|v| v.as_bool()).unwrap_or(false);

    let engine = state.engine.lock().await;
    let snippets = engine
        .get_snippets_for_node(node_id)
        .map_err(|e| mcp_err(&e.to_string()))?;

    let result: Vec<serde_json::Value> = if full {
        snippets.iter().map(|s| serde_json::to_value(s).unwrap_or_default()).collect()
    } else {
        snippets
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "kind": s.kind,
                    "signature": s.signature,
                    "doc_comment": s.doc_comment,
                    "file_path": s.file_path,
                    "line_start": s.line_start,
                    "line_end": s.line_end,
                })
            })
            .collect()
    };

    let text = serde_json::to_string_pretty(&result).unwrap_or_default();
    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    }))
}

async fn tool_graph_search(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: query"))?;

    let engine = state.engine.lock().await;

    // Search nodes by name
    let all_nodes = engine
        .query_nodes(None, None)
        .map_err(|e| mcp_err(&e.to_string()))?;
    let matching_nodes: Vec<&Node> = all_nodes
        .iter()
        .filter(|n| n.name.to_lowercase().contains(&query.to_lowercase()))
        .collect();

    // Search snippets via FTS
    let snippets = engine
        .search_snippets(query)
        .map_err(|e| mcp_err(&e.to_string()))?;

    let text = serde_json::to_string_pretty(&serde_json::json!({
        "nodes": matching_nodes,
        "snippets": snippets.iter().map(|s| serde_json::json!({
            "id": s.id,
            "node_id": s.node_id,
            "kind": s.kind,
            "signature": s.signature,
            "file_path": s.file_path,
        })).collect::<Vec<_>>(),
    }))
    .unwrap_or_default();

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    }))
}

async fn tool_graph_proposals(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let engine = state.engine.lock().await;
    let proposals = engine
        .query_nodes(Some(NodeKind::Proposal), None)
        .map_err(|e| mcp_err(&e.to_string()))?;

    let mut results = Vec::new();
    for p in &proposals {
        let edges = engine.get_edges_from(&p.id).map_err(|e| mcp_err(&e.to_string()))?;
        let active_seams: Vec<&str> = edges
            .iter()
            .filter(|e| e.relation == EdgeRelation::Implements)
            .map(|e| e.target_id.as_str())
            .collect();

        results.push(serde_json::json!({
            "id": p.id,
            "name": p.name,
            "status": p.properties.get("status"),
            "domain": p.properties.get("domain"),
            "active_seams": active_seams,
        }));
    }

    let text = serde_json::to_string_pretty(&results).unwrap_or_default();
    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    }))
}

async fn tool_graph_decide(
    state: &AppState,
    args: &serde_json::Value,
) -> Result<serde_json::Value, JsonRpcError> {
    let target_node = args
        .get("target_node")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: target_node"))?;
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: action"))?;
    let reason = args
        .get("reason")
        .and_then(|v| v.as_str())
        .ok_or_else(|| mcp_err("Missing required parameter: reason"))?;

    let from_value = args.get("from_value").and_then(|v| v.as_str());
    let to_value = args.get("to_value").and_then(|v| v.as_str());
    let agent = args.get("agent").and_then(|v| v.as_str());
    let session = args.get("session").and_then(|v| v.as_str());

    let engine = state.engine.lock().await;

    let mutation = Mutation {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        agent: agent.map(String::from),
        session: session.map(String::from),
        action: action.to_string(),
        target_node: Some(target_node.to_string()),
        from_value: from_value.map(String::from),
        to_value: to_value.map(String::from),
        reason: Some(reason.to_string()),
        details: serde_json::json!({}),
    };

    engine
        .record_mutation(&mutation)
        .map_err(|e| mcp_err(&e.to_string()))?;

    // If action is status_change and to_value is set, update the node properties
    if action == "status_change" {
        if let Some(new_status) = to_value {
            if let Some(mut node) = engine
                .get_node(target_node)
                .map_err(|e| mcp_err(&e.to_string()))?
            {
                if let serde_json::Value::Object(ref mut map) = node.properties {
                    map.insert(
                        "status".to_string(),
                        serde_json::Value::String(new_status.to_string()),
                    );
                }
                node.updated_at = chrono::Utc::now();
                engine
                    .upsert_node(&node)
                    .map_err(|e| mcp_err(&e.to_string()))?;
            }
        }
    }

    // Broadcast mutation event
    let _ = state.change_tx.send(ChangeEvent {
        event_type: "mutation_recorded".to_string(),
        payload: serde_json::json!({
            "mutation_id": mutation.id,
            "target_node": target_node,
            "action": action,
        }),
    });

    Ok(serde_json::json!({
        "content": [{
            "type": "text",
            "text": format!("Decision recorded: {} on {} — {}", action, target_node, reason)
        }]
    }))
}

async fn tool_graph_scan(state: &AppState) -> Result<serde_json::Value, JsonRpcError> {
    let mut engine = state.engine.lock().await;

    let config = ScanConfig {
        rust_roots: state.scan_config.rust_roots.clone(),
        doc_roots: state.scan_config.doc_roots.clone(),
        git_repo: state.scan_config.git_repo.clone(),
        worktree: state.scan_config.worktree.clone(),
    };

    let root = Path::new(&state.repo_root);
    let result = full_scan(root, &config, &mut engine).map_err(|e| mcp_err(&e.to_string()))?;

    let _ = state.change_tx.send(ChangeEvent {
        event_type: "scan_complete".to_string(),
        payload: serde_json::json!({
            "crates": result.crates,
            "modules": result.modules,
            "types": result.types,
            "duration_ms": result.duration_ms,
        }),
    });

    let text = serde_json::to_string_pretty(&serde_json::json!({
        "crates": result.crates,
        "modules": result.modules,
        "types": result.types,
        "functions": result.functions,
        "tests": result.tests,
        "snippets": result.snippets,
        "docs": result.docs,
        "commits": result.commits,
        "branches": result.branches,
        "duration_ms": result.duration_ms,
    }))
    .unwrap_or_default();

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    }))
}
