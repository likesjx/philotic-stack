//! Axum HTTP server — MCP JSON-RPC 2.0 endpoint.

use axum::{
    Router,
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{info, warn};
use uuid::Uuid;

use crate::auth::{AllotmentTracker, VaultHashCache, VaultResolver, authorize_call};
use crate::dispatch::Dispatcher;
use crate::protocol::{
    InitializeParams, InitializeResult, JsonRpcRequest, JsonRpcResponse, McpToolDescriptor,
    ServerCapabilities, ServerInfo, ToolCallParams, ToolCallResult, ToolsCapability,
    ToolsListResult, error_code,
};
use crate::routing::SharedRoutingTable;

// ── Application state ─────────────────────────────────────────────────────────

pub struct MembraneState {
    pub routing_table: SharedRoutingTable,
    pub vault_cache: VaultHashCache,
    pub allotment: AllotmentTracker,
    pub dispatcher: Box<dyn Dispatcher>,
    pub vault: Box<dyn VaultResolver>,
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

    // Check require_approval flag.
    if route.record.security.require_approval {
        info!(tool = tool_name, caller_id = %caller.token_id, "call requires operator approval");
        return JsonRpcResponse::err(
            id,
            error_code::APPROVAL_REQUIRED,
            "this tool requires operator approval before execution",
        );
    }

    // Dispatch.
    let correlation_id = Uuid::new_v4().to_string();
    info!(tool = tool_name, caller_id = %caller.token_id, %correlation_id, "dispatching");

    match state
        .dispatcher
        .dispatch(&route, args, &caller, &correlation_id)
        .await
    {
        Ok(result) => JsonRpcResponse::ok(
            id,
            serde_json::to_value(ToolCallResult::json(result)).unwrap(),
        ),
        Err(e) => {
            warn!(tool = tool_name, err = %e, "dispatch error");
            JsonRpcResponse::err(id, error_code::DISPATCH_ERROR, e.to_string())
        }
    }
}
