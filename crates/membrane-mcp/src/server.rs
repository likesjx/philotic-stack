//! Axum HTTP server — MCP JSON-RPC 2.0 endpoint.

use axum::{
    Router,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use membrane::envelope::{InboundEnvelope, SenderInfo};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc, oneshot};
use tracing::{info, warn};
use uuid::Uuid;

use crate::auth::{AllotmentTracker, VaultHashCache, VaultResolver, authorize_call};
use crate::protocol::{
    InitializeParams, InitializeResult, JsonRpcRequest, JsonRpcResponse, McpToolDescriptor,
    ServerCapabilities, ServerInfo, ToolCallParams, ToolCallResult, ToolsCapability,
    ToolsListResult, error_code,
};
use crate::routing::SharedRoutingTable;

const DISPATCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Approval-required routes park the HTTP connection for up to 5 minutes.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

// ── Application state ─────────────────────────────────────────────────────────

pub struct MembraneState {
    pub routing_table: SharedRoutingTable,
    pub vault_cache: VaultHashCache,
    pub allotment: AllotmentTracker,
    pub vault: Box<dyn VaultResolver>,
    /// Hotel node ID — used as `final_reply_to` so philote routes replies
    /// back through the local hotel to the mcp-membrane role.
    pub node_id: String,
    /// Channel into the runtime's IPC dispatch loop; HTTP handlers send
    /// envelopes here which the runtime forwards to the hotel as CreateTask.
    pub inbound_tx: mpsc::Sender<InboundEnvelope>,
    /// Pending tool-call responses keyed by turn_id. The HTTP handler parks
    /// a oneshot sender here; `McpMembrane::deliver` fires it when the hotel
    /// pushes back the philote reply.
    pub pending_responses: Arc<Mutex<HashMap<String, oneshot::Sender<String>>>>,
}

impl std::fmt::Debug for MembraneState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MembraneState").finish_non_exhaustive()
    }
}

pub type SharedState = Arc<MembraneState>;

// ── Router builder ────────────────────────────────────────────────────────────

pub fn build_router(state: SharedState) -> Router {
    Router::new()
        .route("/mcp", post(handle_mcp))
        .route("/health", get(handle_health))
        .with_state(state)
}

// ── Health ────────────────────────────────────────────────────────────────────

async fn handle_health() -> impl IntoResponse {
    (StatusCode::OK, "membrane-mcp ok")
}

// ── MCP dispatcher ────────────────────────────────────────────────────────────

async fn handle_mcp(
    State(state): State<SharedState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    axum::Json(req): axum::Json<JsonRpcRequest>,
) -> axum::Json<JsonRpcResponse> {
    let id = req.id.clone().unwrap_or(Value::Null);
    let is_loopback = addr.ip().is_loopback();
    let auth_header = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok());

    let resp = match req.method.as_str() {
        "initialize" => handle_initialize(id, req.params),
        "initialized" => {
            // Notification — no response required per spec, but we ack.
            JsonRpcResponse::ok(id, json!({}))
        }
        "ping" => JsonRpcResponse::ok(id, json!({})),
        "tools/list" => handle_tools_list(&state, id, auth_header).await,
        "tools/call" => {
            handle_tools_call(&state, id, req.params, auth_header, is_loopback).await
        }
        _ => JsonRpcResponse::err(id, error_code::METHOD_NOT_FOUND, "method not found"),
    };

    axum::Json(resp)
}

// ── Method handlers ───────────────────────────────────────────────────────────

fn handle_initialize(id: Value, params: Option<Value>) -> JsonRpcResponse {
    let _params: InitializeParams = match params.and_then(|p| serde_json::from_value(p).ok()) {
        Some(p) => p,
        None => {
            return JsonRpcResponse::err(id, error_code::INVALID_PARAMS, "invalid initialize params");
        }
    };

    JsonRpcResponse::ok(
        id,
        serde_json::to_value(InitializeResult {
            protocol_version: "2024-11-05".into(),
            server_info: ServerInfo {
                name: "philotic-membrane-mcp".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            capabilities: ServerCapabilities {
                tools: ToolsCapability { list_changed: false },
            },
        })
        .unwrap(),
    )
}

async fn handle_tools_list(
    state: &SharedState,
    id: Value,
    auth_header: Option<&str>,
) -> JsonRpcResponse {
    let caller_id = crate::auth::extract_bearer(auth_header);

    let table = state.routing_table.read().await;
    let tools: Vec<McpToolDescriptor> = table.visible_descriptors(caller_id);
    drop(table);

    JsonRpcResponse::ok(
        id,
        serde_json::to_value(ToolsListResult { tools }).unwrap(),
    )
}

async fn handle_tools_call(
    state: &SharedState,
    id: Value,
    params: Option<Value>,
    auth_header: Option<&str>,
    is_loopback: bool,
) -> JsonRpcResponse {
    let params: ToolCallParams = match params.and_then(|p| serde_json::from_value(p).ok()) {
        Some(p) => p,
        None => {
            return JsonRpcResponse::err(id, error_code::INVALID_PARAMS, "invalid tools/call params");
        }
    };

    let tool_name = &params.name;
    let args = params.arguments.unwrap_or(json!({}));

    // Look up the route.
    let table = state.routing_table.read().await;
    let route = match table.get(tool_name) {
        Some(r) => r.clone(),
        None => {
            return JsonRpcResponse::err(
                id,
                error_code::TOOL_NOT_FOUND,
                format!("tool '{}' not found", tool_name),
            );
        }
    };
    drop(table);

    // Authorize the call.
    let caller = match authorize_call(
        tool_name,
        &route.record.security.auth,
        auth_header,
        is_loopback,
        &state.vault_cache,
        state.vault.as_ref(),
        &state.allotment,
    ) {
        Ok(c) => c,
        Err(e) => {
            warn!(tool = tool_name, err = %e, "auth rejected");
            return JsonRpcResponse::err(id, error_code::PERMISSION_DENIED, e.to_string());
        }
    };

    // Determine if operator approval is required for this route.
    let requires_approval = route.record.security.require_approval;
    let timeout = if requires_approval { APPROVAL_TIMEOUT } else { DISPATCH_TIMEOUT };

    // Generate correlation IDs.
    let session_id = format!("mcp-{}", caller.token_id);
    let turn_id = Uuid::new_v4().to_string();

    if requires_approval {
        info!(tool = tool_name, caller_id = %caller.token_id, %turn_id, "dispatching with approval gate — waiting up to {}s", timeout.as_secs());
    } else {
        info!(tool = tool_name, caller_id = %caller.token_id, %turn_id, "dispatching via IPC");
    }

    // Build the inbound envelope that philote will receive.
    let envelope = InboundEnvelope {
        session_id: session_id.clone(),
        turn_id: turn_id.clone(),
        sender: SenderInfo {
            id: Some(caller.token_id.clone()),
            display_name: None,
            username: None,
            is_operator: false,
        },
        content: serde_json::to_string(&json!({
            "tool": tool_name,
            "args": args,
        }))
        .unwrap_or_default(),
        attachments: vec![],
        command: Some(tool_name.clone()),
        reply_to: Some(turn_id.clone()),
        raw_transport: json!({ "transport": "mcp", "tool": tool_name }),
        requires_approval,
        // Route philote replies back to this hotel node, role=mcp-membrane.
        final_reply_to: Some(state.node_id.clone()),
        final_reply_role: Some("mcp-membrane".into()),
        final_reply_guest_id: None,
    };

    // Park a oneshot waiting for the philote reply.
    let (tx, rx) = oneshot::channel::<String>();
    {
        let mut pending = state.pending_responses.lock().await;
        pending.insert(turn_id.clone(), tx);
    }

    // Forward to the hotel via the runtime's IPC dispatch loop.
    if let Err(e) = state.inbound_tx.send(envelope).await {
        let mut pending = state.pending_responses.lock().await;
        pending.remove(&turn_id);
        warn!(tool = tool_name, err = %e, "inbound_tx send failed");
        return JsonRpcResponse::err(id, error_code::DISPATCH_ERROR, "IPC send failed");
    }

    // Await the reply with a timeout (longer for approval-gated routes).
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(content)) => JsonRpcResponse::ok(
            id,
            serde_json::to_value(ToolCallResult::json(serde_json::from_str(&content).unwrap_or(
                json!({ "text": content }),
            )))
            .unwrap(),
        ),
        Ok(Err(_)) => {
            // Sender dropped — runtime disconnected mid-call.
            warn!(tool = tool_name, %turn_id, "pending oneshot sender dropped");
            JsonRpcResponse::err(id, error_code::DISPATCH_ERROR, "IPC connection lost")
        }
        Err(_) => {
            // Timeout — clean up pending entry.
            let mut pending = state.pending_responses.lock().await;
            pending.remove(&turn_id);
            warn!(tool = tool_name, %turn_id, "dispatch timed out");
            JsonRpcResponse::err(id, error_code::DISPATCH_ERROR, "tool call timed out")
        }
    }
}
