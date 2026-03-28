use std::path::Path;
use std::sync::Arc;

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;

use crate::schema::*;
use crate::scanner::{full_scan, ScanConfig};

use super::ws::ChangeEvent;
use super::AppState;

// ── Query parameter types ──

#[derive(Deserialize)]
pub struct NodeListQuery {
    pub kind: Option<String>,
    pub worktree: Option<String>,
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub q: Option<String>,
}

#[derive(Deserialize)]
pub struct MutationQuery {
    pub target: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Deserialize)]
pub struct UpdateNodeBody {
    pub properties: serde_json::Value,
    pub agent: Option<String>,
    pub session: Option<String>,
    pub reason: Option<String>,
}

#[derive(Deserialize)]
pub struct ScanBody {
    pub rust_roots: Option<Vec<String>>,
    pub doc_roots: Option<Vec<String>>,
    pub worktree: Option<String>,
}

// ── Response types ──

#[derive(Serialize)]
struct StatusResponse {
    node_counts: serde_json::Value,
    edge_count: usize,
    snippet_count: usize,
    last_scan: Option<String>,
}

#[derive(Serialize)]
struct NodeResponse {
    #[serde(flatten)]
    node: Node,
    #[serde(rename = "_edges")]
    edges: EdgesBundle,
}

#[derive(Serialize)]
struct EdgesBundle {
    outgoing: Vec<Edge>,
    incoming: Vec<Edge>,
}

#[derive(Serialize)]
struct ScanResponse {
    crates: usize,
    modules: usize,
    types: usize,
    functions: usize,
    tests: usize,
    snippets: usize,
    docs: usize,
    commits: usize,
    branches: usize,
    duration_ms: u64,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/status", get(get_status))
        .route("/api/nodes", get(list_nodes))
        .route("/api/nodes/{id}", get(get_node))
        .route("/api/nodes/{id}/edges", get(get_node_edges))
        .route("/api/nodes/{id}/update", post(update_node))
        .route("/api/search", get(search))
        .route("/api/snippets/{node_id}", get(get_snippets))
        .route("/api/snippets/{node_id}/full", get(get_snippets_full))
        .route("/api/skeleton/{crate_name}", get(get_skeleton))
        .route("/api/proposals", get(list_proposals))
        .route("/api/proposals/{id}", get(get_proposal))
        .route("/api/seams", get(list_seams))
        .route("/api/worktrees", get(list_worktrees))
        .route("/api/mutations", get(list_mutations))
        .route("/api/scan", post(trigger_scan))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

// ── Handlers ──

async fn get_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<StatusResponse>, (StatusCode, Json<ErrorResponse>)> {
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
        NodeKind::Worktree,
        NodeKind::Domain,
        NodeKind::Task,
        NodeKind::Decision,
    ];

    let mut counts = serde_json::Map::new();
    for kind in &kinds {
        let count = engine.count_nodes(Some(*kind)).map_err(internal_error)?;
        counts.insert(kind.as_str().to_string(), serde_json::json!(count));
    }
    let total = engine.count_nodes(None).map_err(internal_error)?;
    counts.insert("total".to_string(), serde_json::json!(total));

    let edge_count = engine.count_edges().map_err(internal_error)?;
    let snippet_count = engine.count_snippets().map_err(internal_error)?;

    Ok(Json(StatusResponse {
        node_counts: serde_json::Value::Object(counts),
        edge_count,
        snippet_count,
        last_scan: None,
    }))
}

async fn list_nodes(
    State(state): State<Arc<AppState>>,
    Query(params): Query<NodeListQuery>,
) -> Result<Json<Vec<Node>>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let kind = params.kind.as_deref().and_then(NodeKind::from_str);
    let nodes = engine
        .query_nodes(kind, params.worktree.as_deref())
        .map_err(internal_error)?;
    Ok(Json(nodes))
}

async fn get_node(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<NodeResponse>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let node = engine
        .get_node(&id)
        .map_err(internal_error)?
        .ok_or_else(|| not_found(&format!("Node '{}' not found", id)))?;
    let outgoing = engine.get_edges_from(&id).map_err(internal_error)?;
    let incoming = engine.get_edges_to(&id).map_err(internal_error)?;
    Ok(Json(NodeResponse {
        node,
        edges: EdgesBundle { outgoing, incoming },
    }))
}

async fn get_node_edges(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<EdgesBundle>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let outgoing = engine.get_edges_from(&id).map_err(internal_error)?;
    let incoming = engine.get_edges_to(&id).map_err(internal_error)?;
    Ok(Json(EdgesBundle { outgoing, incoming }))
}

async fn search(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SearchQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let query = params.q.unwrap_or_default();
    if query.is_empty() {
        return Ok(Json(serde_json::json!({ "nodes": [], "snippets": [] })));
    }

    let engine = state.engine.lock().await;

    // Search nodes via FTS
    let all_nodes = engine.query_nodes(None, None).map_err(internal_error)?;
    let matching_nodes: Vec<&Node> = all_nodes
        .iter()
        .filter(|n| n.name.to_lowercase().contains(&query.to_lowercase()))
        .collect();

    // Search snippets via FTS
    let snippets = engine.search_snippets(&query).map_err(internal_error)?;

    Ok(Json(serde_json::json!({
        "nodes": matching_nodes,
        "snippets": snippets,
    })))
}

async fn get_snippets(
    State(state): State<Arc<AppState>>,
    AxumPath(node_id): AxumPath<String>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let snippets = engine
        .get_snippets_for_node(&node_id)
        .map_err(internal_error)?;

    // Return without body
    let result: Vec<serde_json::Value> = snippets
        .into_iter()
        .map(|s| {
            serde_json::json!({
                "id": s.id,
                "node_id": s.node_id,
                "kind": s.kind,
                "signature": s.signature,
                "doc_comment": s.doc_comment,
                "file_path": s.file_path,
                "line_start": s.line_start,
                "line_end": s.line_end,
                "language": s.language,
            })
        })
        .collect();
    Ok(Json(result))
}

async fn get_snippets_full(
    State(state): State<Arc<AppState>>,
    AxumPath(node_id): AxumPath<String>,
) -> Result<Json<Vec<Snippet>>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let snippets = engine
        .get_snippets_for_node(&node_id)
        .map_err(internal_error)?;
    Ok(Json(snippets))
}

async fn get_skeleton(
    State(state): State<Arc<AppState>>,
    AxumPath(crate_name): AxumPath<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let diagram = crate::plantuml::generate_crate_diagram(&engine, &crate_name)
        .map_err(internal_error)?;
    Ok((
        StatusCode::OK,
        [("content-type", "text/plain; charset=utf-8")],
        diagram,
    ))
}

async fn list_proposals(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Node>>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let proposals = engine
        .query_nodes(Some(NodeKind::Proposal), None)
        .map_err(internal_error)?;
    Ok(Json(proposals))
}

async fn get_proposal(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;

    // Try both raw id and prefixed
    let proposal_id = if id.contains(':') {
        id.clone()
    } else {
        format!("proposal:{}", id)
    };

    let node = engine
        .get_node(&proposal_id)
        .map_err(internal_error)?
        .ok_or_else(|| not_found(&format!("Proposal '{}' not found", id)))?;

    let outgoing = engine.get_edges_from(&proposal_id).map_err(internal_error)?;
    let incoming = engine.get_edges_to(&proposal_id).map_err(internal_error)?;

    // Find related seams
    let seam_ids: Vec<&str> = outgoing
        .iter()
        .filter(|e| e.relation == EdgeRelation::Implements)
        .map(|e| e.target_id.as_str())
        .collect();

    let mut seams = Vec::new();
    for sid in &seam_ids {
        if let Some(s) = engine.get_node(sid).map_err(internal_error)? {
            seams.push(s);
        }
    }

    // Find implementing code
    let impl_ids: Vec<&str> = outgoing
        .iter()
        .filter(|e| e.relation == EdgeRelation::ImplementedBy)
        .map(|e| e.target_id.as_str())
        .collect();

    let mut implementing = Vec::new();
    for iid in &impl_ids {
        if let Some(n) = engine.get_node(iid).map_err(internal_error)? {
            implementing.push(n);
        }
    }

    // Find decisions
    let decision_ids: Vec<&str> = incoming
        .iter()
        .filter(|e| e.relation == EdgeRelation::AppliesTo)
        .map(|e| e.source_id.as_str())
        .collect();

    let mut decisions = Vec::new();
    for did in &decision_ids {
        if let Some(d) = engine.get_node(did).map_err(internal_error)? {
            decisions.push(d);
        }
    }

    Ok(Json(serde_json::json!({
        "proposal": node,
        "seams": seams,
        "implementing_code": implementing,
        "decisions": decisions,
        "_edges": {
            "outgoing": outgoing,
            "incoming": incoming,
        }
    })))
}

async fn list_seams(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let seams = engine
        .query_nodes(Some(NodeKind::Seam), None)
        .map_err(internal_error)?;

    let mut results = Vec::new();
    for seam in seams {
        let deps = engine
            .get_edges_from(&seam.id)
            .map_err(internal_error)?
            .into_iter()
            .filter(|e| e.relation == EdgeRelation::DependsOn)
            .map(|e| e.target_id)
            .collect::<Vec<_>>();

        results.push(serde_json::json!({
            "node": seam,
            "dependencies": deps,
        }));
    }
    Ok(Json(results))
}

async fn list_worktrees(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Node>>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let worktrees = engine
        .query_nodes(Some(NodeKind::Worktree), None)
        .map_err(internal_error)?;
    // Also include branches as they represent worktree-like entities
    let branches = engine
        .query_nodes(Some(NodeKind::Branch), None)
        .map_err(internal_error)?;

    let mut all = worktrees;
    all.extend(branches);
    Ok(Json(all))
}

async fn list_mutations(
    State(state): State<Arc<AppState>>,
    Query(params): Query<MutationQuery>,
) -> Result<Json<Vec<Mutation>>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let limit = params.limit.unwrap_or(50);
    let mutations = engine
        .get_mutations(params.target.as_deref(), limit)
        .map_err(internal_error)?;
    Ok(Json(mutations))
}

async fn trigger_scan(
    State(state): State<Arc<AppState>>,
    body: Option<Json<ScanBody>>,
) -> Result<Json<ScanResponse>, (StatusCode, Json<ErrorResponse>)> {
    let mut engine = state.engine.lock().await;

    let config = if let Some(Json(overrides)) = body {
        ScanConfig {
            rust_roots: overrides
                .rust_roots
                .unwrap_or_else(|| state.scan_config.rust_roots.clone()),
            doc_roots: overrides
                .doc_roots
                .unwrap_or_else(|| state.scan_config.doc_roots.clone()),
            git_repo: state.scan_config.git_repo.clone(),
            worktree: overrides
                .worktree
                .unwrap_or_else(|| state.scan_config.worktree.clone()),
        }
    } else {
        ScanConfig {
            rust_roots: state.scan_config.rust_roots.clone(),
            doc_roots: state.scan_config.doc_roots.clone(),
            git_repo: state.scan_config.git_repo.clone(),
            worktree: state.scan_config.worktree.clone(),
        }
    };

    let root = Path::new(&state.repo_root);
    let result = full_scan(root, &config, &mut engine).map_err(internal_error)?;

    // Broadcast scan complete event
    let _ = state.change_tx.send(ChangeEvent {
        event_type: "scan_complete".to_string(),
        payload: serde_json::json!({
            "crates": result.crates,
            "modules": result.modules,
            "types": result.types,
            "duration_ms": result.duration_ms,
        }),
    });

    Ok(Json(ScanResponse {
        crates: result.crates,
        modules: result.modules,
        types: result.types,
        functions: result.functions,
        tests: result.tests,
        snippets: result.snippets,
        docs: result.docs,
        commits: result.commits,
        branches: result.branches,
        duration_ms: result.duration_ms,
    }))
}

async fn update_node(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<UpdateNodeBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;

    let mut node = engine
        .get_node(&id)
        .map_err(internal_error)?
        .ok_or_else(|| not_found(&format!("Node '{}' not found", id)))?;

    // Merge properties
    if let (serde_json::Value::Object(existing), serde_json::Value::Object(updates)) =
        (&mut node.properties, &body.properties)
    {
        for (k, v) in updates {
            existing.insert(k.clone(), v.clone());
        }
    }
    node.updated_at = chrono::Utc::now();

    engine.upsert_node(&node).map_err(internal_error)?;

    // Record the mutation
    let mutation = Mutation {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        agent: body.agent.clone(),
        session: body.session.clone(),
        action: "update_properties".to_string(),
        target_node: Some(id.clone()),
        from_value: None,
        to_value: Some(body.properties.to_string()),
        reason: body.reason.clone(),
        details: serde_json::json!({}),
    };
    engine.record_mutation(&mutation).map_err(internal_error)?;

    // Broadcast node update event
    let _ = state.change_tx.send(ChangeEvent {
        event_type: "node_updated".to_string(),
        payload: serde_json::json!({
            "node_id": id,
            "agent": body.agent,
        }),
    });

    Ok(Json(serde_json::json!({
        "updated": true,
        "node": node,
        "mutation_id": mutation.id,
    })))
}

// ── Error helpers ──

fn internal_error(err: anyhow::Error) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: err.to_string(),
        }),
    )
}

fn not_found(msg: &str) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: msg.to_string(),
        }),
    )
}
