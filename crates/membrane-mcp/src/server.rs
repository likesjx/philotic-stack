//! Axum HTTP server — MCP JSON-RPC 2.0 endpoint.

use axum::{
    Router,
    extract::{ConnectInfo, State},
    http::{HeaderMap, HeaderValue, StatusCode},
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
use crate::routing::{SharedEndpointTable, SharedRoutingTable};
use crate::transform;
use ansible_mesh_core::ExposureTier;
use ansible_mesh_core::mcp_route::McpAuthScheme;

const DISPATCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Approval-required routes park the HTTP connection for up to 5 minutes.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);

// ── Application state ─────────────────────────────────────────────────────────

pub struct MembraneState {
    /// Legacy per-agent route table (UpdateMcpRoutes path).
    pub routing_table: SharedRoutingTable,
    /// Config-driven endpoint table (ProvisionMcpEndpoint path). Takes
    /// priority over `routing_table` when an endpoint config is present.
    pub endpoint_table: SharedEndpointTable,
    pub vault_cache: VaultHashCache,
    pub allotment: AllotmentTracker,
    pub vault: Box<dyn VaultResolver>,
    /// Hotel node ID — used as `final_reply_to` so replies route back
    /// through the local hotel to the mcp-membrane role.
    pub node_id: String,
    /// Concrete membrane guest ID for this listener. Reply routing must target
    /// the specific endpoint guest, not the broad mcp-membrane role.
    pub guest_id: String,
    /// Channel into the runtime's IPC dispatch loop.
    pub inbound_tx: mpsc::Sender<InboundEnvelope>,
    /// Pending tool-call responses keyed by turn_id.
    pub pending_responses: Arc<Mutex<HashMap<String, oneshot::Sender<String>>>>,
    /// Current exposure tier for this listener — drives the ingress fence gate.
    /// Updated via `update_perimeter` push from the hotel when the perimeter shifts.
    pub ingress_tier: Arc<std::sync::RwLock<ExposureTier>>,
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
        .route("/mcp", post(handle_mcp).get(handle_mcp_sse))
        .route("/health", get(handle_health))
        .with_state(state)
}

// ── Health ────────────────────────────────────────────────────────────────────

async fn handle_health() -> impl IntoResponse {
    (StatusCode::OK, "membrane-mcp ok")
}

// ── GET /mcp — Streamable HTTP SSE channel (MCP 2025-03-26) ──────────────────
//
// Clients using the Streamable HTTP transport open this GET endpoint to receive
// server-initiated messages. We don't push server-initiated messages yet, so we
// return a minimal SSE stream with a keepalive comment and then close it.
// Clients will reconnect if they need more events.
async fn handle_mcp_sse() -> impl IntoResponse {
    let body = ": keepalive\n\n";
    let mut headers = HeaderMap::new();
    headers.insert(
        "content-type",
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert("cache-control", HeaderValue::from_static("no-cache"));
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    (StatusCode::OK, headers, body)
}

// ── MCP dispatcher ────────────────────────────────────────────────────────────

async fn handle_mcp(
    State(state): State<SharedState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    axum::Json(req): axum::Json<JsonRpcRequest>,
) -> axum::response::Response {
    let is_notification = req.id.is_none();
    let id = req.id.clone().unwrap_or(Value::Null);
    let is_loopback = addr.ip().is_loopback();
    let auth_header = headers.get("authorization").and_then(|v| v.to_str().ok());

    // Ingress fence: listener-level check before any per-route auth.
    {
        let tier = *state.ingress_tier.read().unwrap();
        let decision = perimeter_core::fence::check_ingress(tier, auth_header, is_loopback);
        if !decision.is_allowed() {
            let perimeter_core::IngressDecision::Deny { status, message } = decision else {
                unreachable!()
            };
            let body = serde_json::json!({
                "jsonrpc": "2.0",
                "id": req.id,
                "error": { "code": -32001, "message": message }
            });
            return (
                axum::http::StatusCode::from_u16(status)
                    .unwrap_or(axum::http::StatusCode::UNAUTHORIZED),
                axum::Json(body),
            )
                .into_response();
        }
    }

    match req.method.as_str() {
        // Notifications — JSON-RPC spec: MUST NOT send a response.
        "initialized" | "notifications/cancelled" | "notifications/progress" => {
            return (StatusCode::ACCEPTED, "").into_response();
        }
        _ => {}
    }

    // If id is absent this is a notification we don't explicitly handle; still no response.
    if is_notification {
        return (StatusCode::ACCEPTED, "").into_response();
    }

    let resp = match req.method.as_str() {
        "initialize" => handle_initialize(id, req.params),
        "ping" => JsonRpcResponse::ok(id, json!({})),
        "tools/list" => handle_tools_list(&state, id, auth_header).await,
        "tools/call" => handle_tools_call(&state, id, req.params, auth_header, is_loopback).await,
        _ => JsonRpcResponse::err(id, error_code::METHOD_NOT_FOUND, "method not found"),
    };

    axum::Json(resp).into_response()
}

// ── Method handlers ───────────────────────────────────────────────────────────

fn handle_initialize(id: Value, params: Option<Value>) -> JsonRpcResponse {
    // Params are optional — tolerate clients that omit them entirely.
    let _params: Option<InitializeParams> = params.and_then(|p| serde_json::from_value(p).ok());

    JsonRpcResponse::ok(
        id,
        serde_json::to_value(InitializeResult {
            protocol_version: "2024-11-05".into(),
            server_info: ServerInfo {
                name: "philotic-membrane-mcp".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            capabilities: ServerCapabilities {
                tools: ToolsCapability {
                    list_changed: false,
                },
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
    // Prefer config-driven endpoint table; fall back to legacy route table.
    let endpoint = state.endpoint_table.read().await;
    let tools: Vec<McpToolDescriptor> = if endpoint.is_active() {
        endpoint.tool_descriptors()
    } else {
        drop(endpoint);
        let caller_id = crate::auth::extract_bearer(auth_header);
        let table = state.routing_table.read().await;
        table.visible_descriptors(caller_id)
    };

    JsonRpcResponse::ok(id, serde_json::to_value(ToolsListResult { tools }).unwrap())
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
            return JsonRpcResponse::err(
                id,
                error_code::INVALID_PARAMS,
                "invalid tools/call params",
            );
        }
    };

    let tool_name = &params.name;
    let args = params.arguments.unwrap_or(json!({}));

    // ── Config-driven path (McpEndpointConfig) ────────────────────────────
    let endpoint = state.endpoint_table.read().await;
    if endpoint.is_active() {
        let tool_spec = match endpoint.find_tool(tool_name) {
            Some(s) => s.clone(),
            None => {
                return JsonRpcResponse::err(
                    id,
                    error_code::TOOL_NOT_FOUND,
                    format!("tool '{}' not found", tool_name),
                );
            }
        };

        let auth_scheme = tool_spec.auth.clone().unwrap_or(McpAuthScheme::None);
        let caller = match authorize_call(
            tool_name,
            &auth_scheme,
            auth_header,
            is_loopback,
            &state.vault_cache,
            state.vault.as_ref(),
            &state.allotment,
        ) {
            Ok(c) => c,
            Err(e) => {
                warn!(tool = tool_name, err = %e, "config-driven auth rejected");
                return JsonRpcResponse::err(id, error_code::PERMISSION_DENIED, e.to_string());
            }
        };

        // Pre-approval check: apply_inbound tells us the action; check rules.
        let inbound = match transform::apply_inbound(&tool_spec, &args) {
            Ok(r) => r,
            Err(e) => {
                return JsonRpcResponse::err(id, error_code::INVALID_PARAMS, e);
            }
        };

        let preapproved = endpoint.is_preapproved(&inbound.action);
        drop(endpoint);

        let requires_approval = !preapproved && !is_loopback;
        let timeout = if requires_approval {
            APPROVAL_TIMEOUT
        } else {
            DISPATCH_TIMEOUT
        };

        let turn_id = Uuid::new_v4().to_string();
        let session_id = format!("mcp-{}-{}", caller.token_id, &turn_id[..8]);

        info!(
            tool = tool_name,
            caller_id = %caller.token_id,
            action = %inbound.action,
            target_kind = %inbound.target_kind,
            target_id = %inbound.target_id,
            %turn_id,
            preapproved,
            "dispatching via transform engine"
        );

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
                "action": inbound.action,
                "payload": inbound.payload,
                "target_kind": inbound.target_kind,
                "target_id": inbound.target_id,
            }))
            .unwrap_or_default(),
            attachments: vec![],
            command: Some(inbound.action.clone()),
            reply_to: Some(turn_id.clone()),
            raw_transport: json!({
                "transport": "mcp",
                "tool": tool_name,
                "target_kind": inbound.target_kind,
                "target_id": inbound.target_id,
            }),
            requires_approval,
            final_reply_to: Some(state.node_id.clone()),
            final_reply_role: Some("mcp-membrane".into()),
            final_reply_guest_id: Some(state.guest_id.clone()),
        };

        let (tx, rx) = oneshot::channel::<String>();
        {
            let mut pending = state.pending_responses.lock().await;
            pending.insert(turn_id.clone(), tx);
        }

        if let Err(e) = state.inbound_tx.send(envelope).await {
            let mut pending = state.pending_responses.lock().await;
            pending.remove(&turn_id);
            warn!(tool = tool_name, err = %e, "inbound_tx send failed");
            return JsonRpcResponse::err(id, error_code::DISPATCH_ERROR, "IPC send failed");
        }

        return match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(content)) => {
                let transformed = transform::apply_outbound(&tool_spec, &content);
                let result_value =
                    serde_json::from_str(&transformed).unwrap_or(json!({ "text": transformed }));
                JsonRpcResponse::ok(
                    id,
                    serde_json::to_value(ToolCallResult::json(result_value)).unwrap(),
                )
            }
            Ok(Err(_)) => {
                warn!(tool = tool_name, %turn_id, "pending oneshot sender dropped");
                JsonRpcResponse::err(id, error_code::DISPATCH_ERROR, "IPC connection lost")
            }
            Err(_) => {
                let mut pending = state.pending_responses.lock().await;
                pending.remove(&turn_id);
                warn!(tool = tool_name, %turn_id, "dispatch timed out");
                JsonRpcResponse::err(id, error_code::DISPATCH_ERROR, "tool call timed out")
            }
        };
    }
    drop(endpoint);

    // ── Legacy route-table path (UpdateMcpRoutes) ─────────────────────────
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

    let requires_approval = route.record.security.require_approval;
    let timeout = if requires_approval {
        APPROVAL_TIMEOUT
    } else {
        DISPATCH_TIMEOUT
    };

    let session_id = format!("mcp-{}", caller.token_id);
    let turn_id = Uuid::new_v4().to_string();

    if requires_approval {
        info!(tool = tool_name, caller_id = %caller.token_id, %turn_id, "dispatching with approval gate");
    } else {
        info!(tool = tool_name, caller_id = %caller.token_id, %turn_id, "dispatching via legacy route");
    }

    let (route_target_id, route_target_node) = {
        use ansible_mesh_core::mcp_route::McpRouteTarget;
        match &route.record.target {
            McpRouteTarget::Philote {
                agent_id,
                target_node,
            } => (Some(agent_id.clone()), target_node.clone()),
            _ => (None, None),
        }
    };

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
        raw_transport: json!({
            "transport": "mcp",
            "tool": tool_name,
            "target_id": route_target_id,
            "target_node": route_target_node,
        }),
        requires_approval,
        final_reply_to: Some(state.node_id.clone()),
        final_reply_role: Some("mcp-membrane".into()),
        final_reply_guest_id: Some(state.guest_id.clone()),
    };

    let (tx, rx) = oneshot::channel::<String>();
    {
        let mut pending = state.pending_responses.lock().await;
        pending.insert(turn_id.clone(), tx);
    }

    if let Err(e) = state.inbound_tx.send(envelope).await {
        let mut pending = state.pending_responses.lock().await;
        pending.remove(&turn_id);
        warn!(tool = tool_name, err = %e, "inbound_tx send failed");
        return JsonRpcResponse::err(id, error_code::DISPATCH_ERROR, "IPC send failed");
    }

    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(content)) => JsonRpcResponse::ok(
            id,
            serde_json::to_value(ToolCallResult::json(
                serde_json::from_str(&content).unwrap_or(json!({ "text": content })),
            ))
            .unwrap(),
        ),
        Ok(Err(_)) => {
            warn!(tool = tool_name, %turn_id, "pending oneshot sender dropped");
            JsonRpcResponse::err(id, error_code::DISPATCH_ERROR, "IPC connection lost")
        }
        Err(_) => {
            let mut pending = state.pending_responses.lock().await;
            pending.remove(&turn_id);
            warn!(tool = tool_name, %turn_id, "dispatch timed out");
            JsonRpcResponse::err(id, error_code::DISPATCH_ERROR, "tool call timed out")
        }
    }
}
