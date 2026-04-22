use std::path::Path;
use std::sync::Arc;

use axum::{
    extract::{Path as AxumPath, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;
use tracing;

use crate::scanner::{full_scan, ScanConfig};
use crate::schema::*;

use super::ws::ChangeEvent;
use super::AppState;

// ── Embedded UI assets ──

#[derive(RustEmbed)]
#[folder = "ui/"]
struct UiAssets;

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
pub struct UpdateContentBody {
    pub content: String,
    pub format: Option<String>,
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

#[derive(Deserialize)]
pub struct SessionStartBody {
    pub session_id: String,
    pub agent: String,
    pub agent_model: Option<String>,
    pub seam_id: String,
    pub proposal_id: Option<String>,
    pub task_id: Option<String>,
    pub phase: Option<String>,
}

#[derive(Deserialize)]
pub struct SessionCloseBody {
    pub session_id: String,
    pub summary: Option<String>,
    pub files_touched: Option<Vec<String>>,
}

#[derive(Deserialize)]
pub struct DecideBody {
    pub target_node: String,
    pub action: String,
    pub from_value: Option<String>,
    pub to_value: Option<String>,
    pub reason: String,
    pub agent: String,
    pub session: Option<String>,
}

#[derive(Deserialize)]
pub struct TestRunBody {
    pub target_id: String,
    pub test_count: usize,
    pub pass_count: usize,
    pub fail_count: usize,
    pub coverage_pct: Option<f64>,
    pub commit_sha: Option<String>,
    pub duration_ms: Option<u64>,
}

#[derive(Deserialize)]
pub struct MempalaceTurnBody {
    pub agent_id: String,
    pub session_id: String,
    pub turn_transcript: String,
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
        .route("/api/nodes/:id", get(get_node))
        .route(
            "/api/nodes/:id/content",
            get(get_node_content).post(update_node_content),
        )
        .route("/api/nodes/:id/edges", get(get_node_edges))
        .route("/api/nodes/:id/update", post(update_node))
        .route("/api/search", get(search))
        .route("/api/snippets/:node_id", get(get_snippets))
        .route("/api/snippets/:node_id/full", get(get_snippets_full))
        .route("/api/skeleton/:crate_name", get(get_skeleton))
        .route("/api/c4/context/:system_name", get(get_c4_context))
        .route("/api/c4/container/:system_name", get(get_c4_container))
        .route("/api/c4/component/:crate_name", get(get_c4_component))
        .route("/api/c4/proposal/:proposal_id", get(get_c4_proposal))
        .route("/api/c4/seam/:seam_id", get(get_c4_seam))
        .route(
            "/api/diagram/sequence/:function_id",
            get(get_sequence_diagram),
        )
        .route("/api/diagram/state/:enum_id", get(get_state_diagram))
        .route(
            "/api/diagram/interactions/:crate_name",
            get(get_module_interactions),
        )
        .route("/api/proposals", get(list_proposals))
        .route("/api/proposals/:id", get(get_proposal))
        .route("/api/proposals/:id/content", get(get_proposal_content))
        .route("/api/seams", get(list_seams))
        .route("/api/worktrees", get(list_worktrees))
        .route("/api/mutations", get(list_mutations))
        .route("/api/dashboard", get(get_dashboard))
        .route("/api/impact/:target", get(get_impact))
        .route("/api/next_task", get(get_next_task))
        .route("/api/context/:target_id", get(get_context_for))
        .route("/api/session/start", post(post_session_start))
        .route("/api/session/close", post(post_session_close))
        .route("/api/session/cleanup", post(post_session_cleanup))
        .route("/api/health/sessions", get(get_session_health))
        .route("/api/health/proposals", get(get_proposal_health))
        .route("/api/health", get(get_system_health))
        .route("/api/decide", post(post_decide))
        .route("/api/test-run", post(post_test_run))
        .route("/api/scan", post(trigger_scan))
        .route("/api/edges", post(post_upsert_edge))
        .route("/api/mempalace/turn", post(post_mempalace_turn))
        .layer(CorsLayer::permissive())
        .fallback(get(handle_ui_static))
        .with_state(state)
}

#[derive(Deserialize)]
pub struct UpsertEdgeBody {
    pub source_id: String,
    pub target_id: String,
    pub relation: EdgeRelation,
    #[serde(default)]
    pub properties: serde_json::Value,
    #[serde(default)]
    pub worktree: String,
}

// ── Handlers ──

async fn post_mempalace_turn(
    State(state): State<Arc<AppState>>,
    Json(body): Json<MempalaceTurnBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    tracing::info!(
        "Broker received reflexive turn for mempalace Wing [{}]: {} chars in session {}",
        body.agent_id,
        body.turn_transcript.len(),
        body.session_id
    );

    // Write transcript to a persistent file for mempalace to mine
    let convos_dir = Path::new(&state.repo_root)
        .join(".mempalace_convos")
        .join(&body.agent_id);
    if let Err(e) = tokio::fs::create_dir_all(&convos_dir).await {
        tracing::error!("Failed to create convos directory: {}", e);
        return Err(internal_error(anyhow::anyhow!("Create dir failed: {}", e)));
    }

    let file_path = convos_dir.join(format!("{}.md", body.session_id));
    use tokio::io::AsyncWriteExt;

    // Append the turn to the session file
    let mut file = match tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)
        .await
    {
        Ok(f) => f,
        Err(e) => {
            tracing::error!("Failed to open session file: {}", e);
            return Err(internal_error(anyhow::anyhow!("File open failed: {}", e)));
        }
    };

    let payload = format!(
        "\n\n### Turn At: {}\n{}\n",
        chrono::Utc::now().to_rfc3339(),
        body.turn_transcript
    );

    if let Err(e) = file.write_all(payload.as_bytes()).await {
        tracing::error!("Failed to write to session file: {}", e);
        return Err(internal_error(anyhow::anyhow!("Write file failed: {}", e)));
    }

    // Trigger mempalace mine in the background so we don't block the agent
    let convos_path = convos_dir.clone();
    let agent_id_for_log = body.agent_id.clone();
    tokio::spawn(async move {
        tracing::info!("Executing mempalace mine on {:?}", convos_path);
        match tokio::process::Command::new("mempalace")
            .arg("mine")
            .arg(&convos_path)
            .arg("--mode")
            .arg("convos")
            .arg("--yes")
            .output()
            .await
        {
            Ok(output) => {
                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    tracing::error!("Mempalace mine failed: {}", stderr);
                } else {
                    tracing::info!("Mempalace mine complete for Wing [{}]", agent_id_for_log);
                }
            }
            Err(e) => tracing::error!("Failed to spawn mempalace: {}", e),
        }
    });

    let _ = state.change_tx.send(ChangeEvent {
        event_type: "mempalace_turn_received".to_string(),
        payload: serde_json::json!({
            "agent_id": body.agent_id,
            "session_id": body.session_id,
            "size": body.turn_transcript.len(),
            "file": file_path.to_string_lossy(),
        }),
    });

    Ok(Json(serde_json::json!({
        "status": "brokered",
        "wing": format!("wing_{}", body.agent_id),
        "transcript_length": body.turn_transcript.len(),
        "file": file_path.to_string_lossy(),
    })))
}

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
    let diagram =
        crate::plantuml::generate_crate_diagram(&engine, &crate_name).map_err(internal_error)?;
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

    let outgoing = engine
        .get_edges_from(&proposal_id)
        .map_err(internal_error)?;
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

async fn get_proposal_content(
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

    Ok(Json(node_content_response(&node)))
}

async fn get_node_content(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;

    let node = engine
        .get_node(&id)
        .map_err(internal_error)?
        .ok_or_else(|| not_found(&format!("Node '{}' not found", id)))?;

    if !is_graph_content_kind(node.kind) {
        return Err(not_found(&format!(
            "Node '{}' does not support graph-authored content",
            id
        )));
    }

    Ok(Json(node_content_response(&node)))
}

async fn update_node_content(
    State(state): State<Arc<AppState>>,
    AxumPath(id): AxumPath<String>,
    Json(body): Json<UpdateContentBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;

    let mut node = engine
        .get_node(&id)
        .map_err(internal_error)?
        .ok_or_else(|| not_found(&format!("Node '{}' not found", id)))?;

    if !is_graph_content_kind(node.kind) {
        return Err(not_found(&format!(
            "Node '{}' does not support graph-authored content",
            id
        )));
    }

    let mut props = node.properties.as_object().cloned().unwrap_or_default();
    props.insert("content".to_string(), serde_json::json!(body.content));
    props.insert(
        "content_format".to_string(),
        serde_json::json!(body
            .format
            .clone()
            .unwrap_or_else(|| "markdown".to_string())),
    );
    props.insert("content_source".to_string(), serde_json::json!("graph"));
    props.insert(
        "last_updated".to_string(),
        serde_json::json!(chrono::Utc::now().format("%Y-%m-%d").to_string()),
    );
    node.properties = serde_json::Value::Object(props);
    node.updated_at = chrono::Utc::now();

    engine.upsert_node(&node).map_err(internal_error)?;

    let mutation = Mutation {
        id: uuid::Uuid::new_v4().to_string(),
        timestamp: chrono::Utc::now(),
        agent: body.agent.clone(),
        session: body.session.clone(),
        action: "update_content".to_string(),
        target_node: Some(id.clone()),
        from_value: None,
        to_value: Some(format!(
            "{} bytes ({})",
            node_content(&node).len(),
            node_content_format(&node)
        )),
        reason: body.reason.clone(),
        details: serde_json::json!({}),
    };
    engine.record_mutation(&mutation).map_err(internal_error)?;

    let _ = state.change_tx.send(ChangeEvent {
        event_type: "node_content_updated".to_string(),
        payload: serde_json::json!({
            "node_id": id,
            "agent": body.agent,
        }),
    });

    Ok(Json(serde_json::json!({
        "updated": true,
        "node_id": node.id,
        "content_source": "graph",
        "content_format": node_content_format(&node),
        "mutation_id": mutation.id,
    })))
}

fn is_graph_content_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Proposal | NodeKind::Task | NodeKind::Document | NodeKind::Skill | NodeKind::Sver
    )
}

fn node_content(node: &Node) -> String {
    node.properties
        .get("content")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            node.file_path
                .as_ref()
                .and_then(|path| std::fs::read_to_string(path).ok())
        })
        .unwrap_or_default()
}

fn node_content_format(node: &Node) -> String {
    node.properties
        .get("content_format")
        .and_then(|v| v.as_str())
        .unwrap_or("markdown")
        .to_string()
}

fn node_content_source(node: &Node) -> String {
    node.properties
        .get("content_source")
        .and_then(|v| v.as_str())
        .unwrap_or(if node.file_path.is_some() {
            "file_fallback"
        } else {
            "graph"
        })
        .to_string()
}

fn node_content_response(node: &Node) -> serde_json::Value {
    serde_json::json!({
        "id": node.id,
        "name": node.name,
        "content": node_content(node),
        "content_format": node_content_format(node),
        "content_source": node_content_source(node),
    })
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

// ── C4 Architecture Diagram Handlers ──

async fn get_c4_context(
    State(state): State<Arc<AppState>>,
    AxumPath(system_name): AxumPath<String>,
) -> Result<String, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let uml = crate::c4::generate_c4_context(&engine, &system_name).map_err(internal_error)?;
    Ok(uml)
}

async fn get_c4_container(
    State(state): State<Arc<AppState>>,
    AxumPath(system_name): AxumPath<String>,
) -> Result<String, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let uml = crate::c4::generate_c4_container(&engine, &system_name).map_err(internal_error)?;
    Ok(uml)
}

async fn get_c4_component(
    State(state): State<Arc<AppState>>,
    AxumPath(crate_name): AxumPath<String>,
) -> Result<String, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let uml = crate::c4::generate_c4_component(&engine, &crate_name).map_err(internal_error)?;
    Ok(uml)
}

async fn get_c4_proposal(
    State(state): State<Arc<AppState>>,
    AxumPath(proposal_id): AxumPath<String>,
) -> Result<String, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let uml =
        crate::c4::generate_proposal_architecture(&engine, &proposal_id).map_err(internal_error)?;
    Ok(uml)
}

async fn get_c4_seam(
    State(state): State<Arc<AppState>>,
    AxumPath(seam_id): AxumPath<String>,
) -> Result<String, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let uml = crate::c4::generate_seam_detail(&engine, &seam_id).map_err(internal_error)?;
    Ok(uml)
}

// ── Sequence/State Diagram Handlers ──

async fn get_sequence_diagram(
    State(state): State<Arc<AppState>>,
    AxumPath(function_id): AxumPath<String>,
) -> Result<String, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let uml = crate::diagrams::generate_sequence_diagram(&engine, &function_id, 3)
        .map_err(internal_error)?;
    Ok(uml)
}

async fn get_state_diagram(
    State(state): State<Arc<AppState>>,
    AxumPath(enum_id): AxumPath<String>,
) -> Result<String, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let uml = crate::diagrams::generate_state_diagram(&engine, &enum_id).map_err(internal_error)?;
    Ok(uml)
}

async fn get_module_interactions(
    State(state): State<Arc<AppState>>,
    AxumPath(crate_name): AxumPath<String>,
) -> Result<String, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let uml = crate::diagrams::generate_module_interaction(&engine, &crate_name)
        .map_err(internal_error)?;
    Ok(uml)
}

// ── Dashboard, Impact, Next Task handlers ──

async fn get_dashboard(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let report = crate::egress::agent_dashboard(&engine).map_err(internal_error)?;

    Ok(Json(serde_json::json!({
        "active_sessions": report.active_sessions.iter().map(|s| serde_json::json!({
            "id": s.id, "agent": s.agent, "seam_id": s.seam_id,
            "proposal_id": s.proposal_id, "phase": s.phase,
            "started": s.started.to_rfc3339(),
            "last_activity": s.last_activity,
            "tokens_total": s.tokens_total,
            "tokens_input": s.tokens_input,
            "tokens_output": s.tokens_output,
            "elapsed_ms": s.elapsed_ms,
            "harness_id": s.harness_id,
            "workflow_name": s.workflow_name,
            "branch": s.branch,
            "worktree_path": s.worktree_path,
            "files_touched": s.files_touched,
            "lines_changed": s.lines_changed,
            "test_runs": s.test_runs,
        })).collect::<Vec<_>>(),
        "recent_closed": report.recent_closed.iter().map(|s| serde_json::json!({
            "id": s.id, "agent": s.agent, "seam_id": s.seam_id,
            "proposal_id": s.proposal_id, "phase": s.phase,
            "started": s.started.to_rfc3339(),
            "last_activity": s.last_activity,
            "tokens_total": s.tokens_total,
            "tokens_input": s.tokens_input,
            "tokens_output": s.tokens_output,
            "elapsed_ms": s.elapsed_ms,
            "harness_id": s.harness_id,
            "workflow_name": s.workflow_name,
            "branch": s.branch,
            "worktree_path": s.worktree_path,
            "files_touched": s.files_touched,
            "lines_changed": s.lines_changed,
            "test_runs": s.test_runs,
        })).collect::<Vec<_>>(),
        "agents": report.agents.iter().map(|a| serde_json::json!({
            "agent": a.agent, "total_sessions": a.total_sessions,
            "active_sessions": a.active_sessions, "decisions_made": a.decisions_made,
            "last_active": a.last_active.to_rfc3339(),
        })).collect::<Vec<_>>(),
        "verification_progress": report.verification_progress,
        "total_proposals": report.total_proposals,
        "total_sessions": report.total_sessions,
        "total_decisions": report.total_decisions,
    })))
}

async fn get_impact(
    State(state): State<Arc<AppState>>,
    AxumPath(target): AxumPath<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let report = crate::egress::impact_analysis(&engine, &target).map_err(internal_error)?;

    Ok(Json(serde_json::json!({
        "target": report.target,
        "seed_nodes": report.seed_nodes,
        "affected_proposals": report.affected_proposals,
        "affected_seams": report.affected_seams,
        "affected_tests": report.affected_tests,
        "total_reached": report.total_reached,
    })))
}

async fn get_next_task(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let result = crate::egress::next_task(&engine).map_err(internal_error)?;

    match result {
        Some(task) => Ok(Json(serde_json::json!({
            "id": task.id,
            "name": task.name,
            "score": task.score,
            "disposition": task.disposition,
            "reason": task.reason,
            "runner_up": task.runner_up,
            "total_candidates": task.total_candidates,
        }))),
        None => Ok(Json(serde_json::json!({
            "id": null,
            "message": "No unclaimed work items",
        }))),
    }
}

// ── Session Lifecycle, Context, Decide handlers ──

async fn get_context_for(
    State(state): State<Arc<AppState>>,
    AxumPath(target_id): AxumPath<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let ctx = crate::egress::context_for(&engine, &target_id).map_err(internal_error)?;

    Ok(Json(serde_json::json!({
        "target_id": ctx.target_id,
        "target_name": ctx.target_name,
        "target_kind": ctx.target_kind,
        "domain": ctx.domain,
        "status": ctx.status,
        "disposition": ctx.disposition,
        "verification_level": ctx.verification_level,
        "proposal_body": ctx.proposal_body,
        "related_seams": ctx.related_seams.iter().map(|s| serde_json::json!({
            "id": s.id, "name": s.name, "kind": s.kind, "status": s.status,
        })).collect::<Vec<_>>(),
        "related_proposals": ctx.related_proposals.iter().map(|p| serde_json::json!({
            "id": p.id, "name": p.name, "kind": p.kind, "status": p.status,
        })).collect::<Vec<_>>(),
        "implementing_code": ctx.implementing_code.iter().map(|c| serde_json::json!({
            "file_path": c.file_path, "line_start": c.line_start,
            "line_end": c.line_end, "signature": c.signature,
        })).collect::<Vec<_>>(),
        "blocking": ctx.blocking,
        "decisions": ctx.decisions.iter().map(|d| serde_json::json!({
            "id": d.id, "name": d.name, "kind": d.kind, "status": d.status,
        })).collect::<Vec<_>>(),
        "active_sessions": ctx.active_sessions.iter().map(|s| serde_json::json!({
            "id": s.id, "name": s.name, "kind": s.kind, "status": s.status,
        })).collect::<Vec<_>>(),
        "diagram": ctx.diagram,
    })))
}

async fn post_session_start(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SessionStartBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let phase = body.phase.as_deref().unwrap_or("started");
    let agent_model = body.agent_model.as_deref().unwrap_or("unknown");

    // Create workstream node
    let workstream_id = format!("workstream:{}", body.seam_id.replace("seam:", ""));
    let workstream = Node {
        id: workstream_id.clone(),
        kind: NodeKind::Workstream,
        name: format!("Work on {}", body.seam_id),
        properties: serde_json::json!({
            "seam_id": body.seam_id,
            "proposal_id": body.proposal_id,
            "task_id": body.task_id,
            "agent": body.agent,
            "status": "active",
            "phase": phase,
            "started_at": chrono::Utc::now().to_rfc3339(),
        }),
        file_path: None,
        worktree: String::new(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    };
    engine.upsert_node(&workstream).map_err(internal_error)?;

    // Link to seam
    let edge = Edge {
        source_id: workstream_id.clone(),
        target_id: body.seam_id.clone(),
        relation: EdgeRelation::PartOf,
        properties: serde_json::json!({"role": "active_work"}),
        worktree: String::new(),
    };
    engine.upsert_edge(&edge).map_err(internal_error)?;

    // Create session node
    let session = Node {
        id: body.session_id.clone(),
        kind: NodeKind::Session,
        name: format!(
            "Session {} - {}",
            body.agent,
            chrono::Utc::now().format("%Y-%m-%d %H:%M")
        ),
        properties: serde_json::json!({
            "agent": body.agent,
            "agent_model": agent_model,
            "seam_id": body.seam_id,
            "proposal_id": body.proposal_id,
            "task_id": body.task_id,
            "status": "active",
            "phase": phase,
            "start_time": chrono::Utc::now().to_rfc3339(),
        }),
        file_path: None,
        worktree: String::new(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    };
    engine.upsert_node(&session).map_err(internal_error)?;

    // Link session to workstream
    let sess_edge = Edge {
        source_id: body.session_id.clone(),
        target_id: workstream_id.clone(),
        relation: EdgeRelation::WorkingOn,
        properties: serde_json::json!({}),
        worktree: String::new(),
    };
    engine.upsert_edge(&sess_edge).map_err(internal_error)?;

    let _ = state.change_tx.send(ChangeEvent {
        event_type: "session_started".to_string(),
        payload: serde_json::json!({
            "session_id": body.session_id,
            "agent": body.agent,
            "seam_id": body.seam_id,
        }),
    });

    Ok(Json(serde_json::json!({
        "session_id": body.session_id,
        "workstream_id": workstream_id,
        "status": "active",
    })))
}

async fn post_session_close(
    State(state): State<Arc<AppState>>,
    Json(body): Json<SessionCloseBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;

    let session = engine
        .get_node(&body.session_id)
        .map_err(internal_error)?
        .ok_or_else(|| not_found(&format!("Session not found: {}", body.session_id)))?;

    let mut props = session.properties.clone();
    props["status"] = serde_json::json!("closed");
    props["end_time"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
    if let Some(ref summary) = body.summary {
        props["summary"] = serde_json::json!(summary);
    }
    if let Some(ref files) = body.files_touched {
        props["files_touched"] = serde_json::json!(files);
    }

    let mut updated = session.clone();
    updated.properties = props;
    updated.updated_at = chrono::Utc::now();
    engine.upsert_node(&updated).map_err(internal_error)?;
    let closed_workstreams = engine
        .close_linked_workstreams(&updated, "closed", body.summary.as_deref())
        .map_err(internal_error)?;

    let _ = state.change_tx.send(ChangeEvent {
        event_type: "session_closed".to_string(),
        payload: serde_json::json!({
            "session_id": body.session_id,
            "workstream_ids": closed_workstreams,
        }),
    });

    Ok(Json(serde_json::json!({
        "session_id": body.session_id,
        "status": "closed",
        "workstream_ids": closed_workstreams,
    })))
}

async fn post_decide(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DecideBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;

    let decision_id = format!("decision:{}", uuid::Uuid::new_v4());
    let decision = Node {
        id: decision_id.clone(),
        kind: NodeKind::Decision,
        name: format!("{}: {} on {}", body.action, body.agent, body.target_node),
        properties: serde_json::json!({
            "action": body.action,
            "from_value": body.from_value,
            "to_value": body.to_value,
            "reason": body.reason,
            "agent": body.agent,
            "session": body.session,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }),
        file_path: None,
        worktree: String::new(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    };
    engine.upsert_node(&decision).map_err(internal_error)?;

    // Link decision to target node
    let edge = Edge {
        source_id: decision_id.clone(),
        target_id: body.target_node.clone(),
        relation: EdgeRelation::DecidedBy,
        properties: serde_json::json!({}),
        worktree: String::new(),
    };
    engine.upsert_edge(&edge).map_err(internal_error)?;

    // Record mutation
    let mutation = crate::schema::Mutation {
        id: format!("mut:{}", uuid::Uuid::new_v4()),
        timestamp: chrono::Utc::now(),
        agent: Some(body.agent.clone()),
        session: body.session.clone(),
        action: body.action.clone(),
        target_node: Some(body.target_node.clone()),
        from_value: body.from_value.clone(),
        to_value: body.to_value.clone(),
        reason: Some(body.reason.clone()),
        details: serde_json::json!({}),
    };
    engine.record_mutation(&mutation).map_err(internal_error)?;

    let _ = state.change_tx.send(ChangeEvent {
        event_type: "decision_recorded".to_string(),
        payload: serde_json::json!({
            "decision_id": decision_id,
            "target": body.target_node,
            "action": body.action,
        }),
    });

    Ok(Json(serde_json::json!({
        "decision_id": decision_id,
        "target_node": body.target_node,
        "action": body.action,
        "status": "recorded",
    })))
}

async fn post_test_run(
    State(state): State<Arc<AppState>>,
    Json(body): Json<TestRunBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;

    let testrun_id = format!("testrun:{}", uuid::Uuid::new_v4());
    let coverage = body.coverage_pct.unwrap_or(0.0);
    let commit = body.commit_sha.clone().unwrap_or_default();
    let duration = body.duration_ms.unwrap_or(0);

    let testrun = Node {
        id: testrun_id.clone(),
        kind: NodeKind::TestRun,
        name: format!("Test run: {}/{}", body.pass_count, body.test_count),
        properties: serde_json::json!({
            "target_id": body.target_id,
            "test_count": body.test_count,
            "pass_count": body.pass_count,
            "fail_count": body.fail_count,
            "coverage_pct": coverage,
            "commit_sha": commit,
            "duration_ms": duration,
            "timestamp": chrono::Utc::now().to_rfc3339(),
        }),
        file_path: None,
        worktree: String::new(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        embedding: None,
        embedding_model: None,
        embedding_dims: None,
        embedding_updated: None,
        embedding_hash: None,
    };
    engine.upsert_node(&testrun).map_err(internal_error)?;

    // Link test run to target node
    let edge = Edge {
        source_id: testrun_id.clone(),
        target_id: body.target_id.clone(),
        relation: EdgeRelation::TestedBy,
        properties: serde_json::json!({"status": if body.fail_count == 0 { "pass" } else { "fail" }}),
        worktree: String::new(),
    };
    engine.upsert_edge(&edge).map_err(internal_error)?;

    // Record mutation for audit trail
    let mutation = crate::schema::Mutation {
        id: format!("mut:{}", uuid::Uuid::new_v4()),
        timestamp: chrono::Utc::now(),
        agent: None,
        session: None,
        action: "test_run".to_string(),
        target_node: Some(body.target_id.clone()),
        from_value: None,
        to_value: Some(format!("{}/{} passed", body.pass_count, body.test_count)),
        reason: Some(format!(
            "Test run recorded: {}/{} pass",
            body.pass_count, body.test_count
        )),
        details: serde_json::json!({"testrun_id": testrun_id}),
    };
    engine.record_mutation(&mutation).map_err(internal_error)?;

    let _ = state.change_tx.send(ChangeEvent {
        event_type: "test_run_recorded".to_string(),
        payload: serde_json::json!({
            "testrun_id": testrun_id,
            "target": body.target_id,
            "passed": body.pass_count,
            "failed": body.fail_count,
        }),
    });

    Ok(Json(serde_json::json!({
        "testrun_id": testrun_id,
        "target_id": body.target_id,
        "status": if body.fail_count == 0 { "pass" } else { "fail" },
        "passed": body.pass_count,
        "failed": body.fail_count,
    })))
}

// ── Health & Cleanup handlers ──

#[derive(Deserialize)]
pub struct SessionCleanupBody {
    pub max_age_hours: Option<u64>,
}

async fn post_session_cleanup(
    State(state): State<Arc<AppState>>,
    body: Option<Json<SessionCleanupBody>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let max_age_hours = body
        .map(|Json(b)| b.max_age_hours.unwrap_or(4))
        .unwrap_or(4);
    let max_age = chrono::Duration::hours(max_age_hours as i64);

    let cleaned = engine
        .cleanup_stale_sessions(max_age)
        .map_err(internal_error)?;
    let orphaned_workstreams = engine
        .cleanup_orphaned_workstreams()
        .map_err(internal_error)?;

    let _ = state.change_tx.send(super::ws::ChangeEvent {
        event_type: "sessions_cleaned".to_string(),
        payload: serde_json::json!({
            "cleaned_count": cleaned.len(),
            "session_ids": cleaned,
            "orphaned_workstream_ids": orphaned_workstreams,
        }),
    });

    Ok(Json(serde_json::json!({
        "cleaned": cleaned.len(),
        "session_ids": cleaned,
        "orphaned_workstreams_cleaned": orphaned_workstreams.len(),
        "orphaned_workstream_ids": orphaned_workstreams,
        "max_age_hours": max_age_hours,
    })))
}

async fn get_session_health(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let report = crate::egress::session_health_check(&engine).map_err(internal_error)?;

    Ok(Json(serde_json::json!({
        "healthy": report.healthy,
        "active_sessions": report.active_sessions,
        "total_sessions": report.total_sessions,
        "stale_sessions": report.stale_sessions.iter().map(|s| serde_json::json!({
            "id": s.id,
            "agent": s.agent,
            "seam_id": s.seam_id,
            "age_hours": s.age_hours,
            "phase": s.phase,
        })).collect::<Vec<_>>(),
        "overloaded_agents": report.overloaded_agents.iter().map(|a| serde_json::json!({
            "agent": a.agent,
            "session_count": a.session_count,
            "session_ids": a.session_ids,
        })).collect::<Vec<_>>(),
        "orphaned_workstreams": report.orphaned_workstreams,
    })))
}

async fn get_proposal_health(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let report = crate::egress::proposal_health_check(&engine).map_err(internal_error)?;

    Ok(Json(serde_json::json!({
        "healthy": report.healthy,
        "total_proposals": report.total_proposals,
        "missing_disposition": report.missing_disposition.len(),
        "missing_disposition_ids": report.missing_disposition,
        "missing_domain": report.missing_domain.len(),
        "missing_domain_ids": report.missing_domain,
        "no_verification": report.no_verification.len(),
        "no_seams": report.no_seams.len(),
        "no_embedding": report.no_embedding.len(),
        "disposition_counts": report.disposition_counts,
        "verification_counts": report.verification_counts,
    })))
}

async fn get_system_health(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;

    let session_health = crate::egress::session_health_check(&engine).map_err(internal_error)?;
    let proposal_health = crate::egress::proposal_health_check(&engine).map_err(internal_error)?;
    let total_nodes = engine.count_nodes(None).map_err(internal_error)?;
    let total_edges = engine.count_edges().map_err(internal_error)?;

    let overall_healthy = session_health.healthy && proposal_health.healthy;

    Ok(Json(serde_json::json!({
        "healthy": overall_healthy,
        "graph": {
            "total_nodes": total_nodes,
            "total_edges": total_edges,
        },
        "sessions": {
            "healthy": session_health.healthy,
            "active": session_health.active_sessions,
            "stale": session_health.stale_sessions.len(),
            "orphaned_workstreams": session_health.orphaned_workstreams.len(),
        },
        "proposals": {
            "healthy": proposal_health.healthy,
            "total": proposal_health.total_proposals,
            "missing_disposition": proposal_health.missing_disposition.len(),
            "no_verification": proposal_health.no_verification.len(),
            "no_embedding": proposal_health.no_embedding.len(),
        },
    })))
}

async fn post_upsert_edge(
    State(state): State<Arc<AppState>>,
    Json(body): Json<UpsertEdgeBody>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorResponse>)> {
    let engine = state.engine.lock().await;
    let edge = Edge {
        source_id: body.source_id.clone(),
        target_id: body.target_id.clone(),
        relation: body.relation,
        properties: body.properties,
        worktree: body.worktree,
    };
    engine.upsert_edge(&edge).map_err(internal_error)?;

    let _ = state.change_tx.send(ChangeEvent {
        event_type: "edge_upserted".to_string(),
        payload: serde_json::json!({
            "source_id": body.source_id,
            "target_id": body.target_id,
            "relation": body.relation,
        }),
    });

    Ok(Json(serde_json::json!({
        "source_id": body.source_id,
        "target_id": body.target_id,
        "relation": body.relation,
        "upserted": true,
    })))
}

// ── Embedded UI handlers ──

async fn handle_ui_static(uri: axum::http::Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');

    // Serve the requested asset if it exists
    if !path.is_empty() {
        if let Some(asset) = UiAssets::get(path) {
            let mime = asset.metadata.mimetype();
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, mime.to_string())],
                asset.data.into_owned(),
            )
                .into_response();
        }
    }

    // SPA fallback — serve index.html for any unknown path
    match UiAssets::get("index.html") {
        Some(index) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8".to_string())],
            index.data.into_owned(),
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "UI not available").into_response(),
    }
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
