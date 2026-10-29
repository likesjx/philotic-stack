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

use crate::auth::{
    AllotmentTracker, AuthError, VaultHashCache, VaultResolver, authorize_call, extract_bearer,
    verify_bearer_token,
};
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

/// Protocol versions this server can speak. The client's requested version is
/// echoed when supported; otherwise the newest supported version is advertised.
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-03-26", "2024-11-05"];

// ── Dispatch outcome ──────────────────────────────────────────────────────────

/// Terminal result of one dispatched tool call, delivered through the pending
/// oneshot. `Err` carries a philote business error and surfaces to the MCP
/// client as a successful JSON-RPC response whose result has `isError: true`,
/// per the MCP tool-error convention.
#[derive(Debug)]
pub enum DispatchOutcome {
    Ok(String),
    Err(String),
}

pub struct PendingResponse {
    pub sender: oneshot::Sender<DispatchOutcome>,
    pub native: Option<native_memory::response::ResponseExpectation>,
}
#[derive(Clone)]
pub struct NativePending {
    request_id: Value,
    token_hash: [u8; 32],
    handle: String,
    config: native_memory::oauth::ResourceConfig,
    expires_at: u64,
}
async fn cancel_native(state: &MembraneState, pending: &NativePending) -> Option<bool> {
    use native_memory::authority_control::{Client, Reply, Request};
    let mut client = Client::connect(&pending.config.authority_socket)
        .await
        .ok()?;
    match client
        .send(&Request::Cancel {
            guest: state.guest_id.clone(),
            handle: pending.handle.clone(),
        })
        .await
        .ok()?
    {
        Reply::Cancelled { committed } => Some(committed),
        _ => None,
    }
}
impl From<oneshot::Sender<DispatchOutcome>> for PendingResponse {
    fn from(sender: oneshot::Sender<DispatchOutcome>) -> Self {
        Self {
            sender,
            native: None,
        }
    }
}

/// All unsigned completion paths must use this gate. Invalid native responses
/// leave the pending request intact for a legitimate signed result or timeout.
pub async fn take_unsigned_response(
    state: &MembraneState,
    turn: &str,
) -> Option<oneshot::Sender<DispatchOutcome>> {
    let mut pending = state.pending_responses.lock().await;
    if pending.get(turn).is_some_and(|p| p.native.is_some()) {
        return None;
    }
    pending.remove(turn).map(|p| p.sender)
}
pub async fn resolve_native_response(
    state: &MembraneState,
    turn: &str,
    reply: &native_memory::response::SignedResponse,
    now: u64,
) -> bool {
    let Ok(config) = state
        .native_memory_config
        .as_ref()
        .ok_or(())
        .and_then(|path| native_memory::IngressConfig::load(path).map_err(|_| ()))
    else {
        return false;
    };
    let mut pending = state.pending_responses.lock().await;
    let Some(expectation) = pending.get(turn).and_then(|p| p.native.as_ref()) else {
        return false;
    };
    if expectation.authorize_return(&config, now).is_err() {
        return false;
    }
    let Ok(result) = expectation.verify(reply, &config.response_verifiers, now) else {
        return false;
    };
    let Ok(result) = serde_json::to_string(&result) else {
        return false;
    };
    if let Some(p) = pending.remove(turn) {
        let _ = p.sender.send(DispatchOutcome::Ok(result));
        return true;
    }
    false
}

// ── Application state ─────────────────────────────────────────────────────────

pub struct MembraneState {
    pub native_pending: Arc<Mutex<HashMap<String, NativePending>>>,
    /// Opt-in local file, reread per native call for revocation. Never logged/routed.
    pub native_memory_config: Option<std::path::PathBuf>,
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
    pub pending_responses: Arc<Mutex<HashMap<String, PendingResponse>>>,
    /// Accumulated streaming tokens keyed by turn_id, folded into the final
    /// result when the terminal reply carries no content of its own.
    pub streaming_buffers: Arc<Mutex<HashMap<String, String>>>,
    /// True when running without an IPC socket: `tools/call` cannot dispatch
    /// and fails immediately instead of timing out.
    pub static_mode: bool,
    /// Current exposure tier for this listener — drives the ingress fence gate.
    /// Updated via `update_perimeter` push from the hotel when the perimeter shifts.
    pub ingress_tier: Arc<std::sync::RwLock<ExposureTier>>,
    /// Exposure tier the provisioned endpoint declared, when known. `None` for
    /// legacy/static route-table mode. Together with `ingress_tier` (the hotel
    /// ceiling) this drives the listener bind address: anything other than an
    /// explicitly declared, ceiling-honored wide tier binds loopback-only.
    pub declared_exposure: Arc<std::sync::RwLock<Option<ExposureTier>>>,
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
        .route(
            "/.well-known/oauth-protected-resource",
            get(native_resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/*suffix",
            get(native_resource_metadata),
        )
        .route("/mcp", post(handle_mcp).get(handle_mcp_sse))
        .route("/health", get(handle_health))
        .with_state(state)
}
fn native_oauth(state: &MembraneState) -> Option<native_memory::oauth::ResourceConfig> {
    state
        .native_memory_config
        .as_ref()
        .and_then(|p| native_memory::IngressConfig::load(p).ok())
        .and_then(|c| c.oauth)
}
async fn native_resource_metadata(State(state): State<SharedState>) -> axum::response::Response {
    match native_oauth(&state) {
        Some(c)=>(StatusCode::OK,axum::Json(json!({"resource":c.resource,"authorization_servers":[c.issuer],"scopes_supported":[native_memory::oauth::RECALL,native_memory::oauth::REMEMBER],"bearer_methods_supported":["header"]}))).into_response(),
        None=>StatusCode::NOT_FOUND.into_response(),
    }
}

// ── Ingress fence helper ──────────────────────────────────────────────────────

/// Whether a request may be trusted as local. A loopback peer is not enough:
/// on vps-jane the public TLS proxy (Traefik in Docker, bridged by socat)
/// reaches this listener over 127.0.0.1, so every internet request would look
/// local. A request that carries a reverse-proxy forwarding header came from
/// somewhere else and is treated as remote.
pub(crate) fn is_trusted_loopback(addr: &SocketAddr, headers: &HeaderMap) -> bool {
    addr.ip().is_loopback()
        && !["x-forwarded-for", "forwarded", "x-real-ip"]
            .iter()
            .any(|h| headers.contains_key(*h))
}

/// Run the listener-level ingress fence for a request. Returns `None` when the
/// request may proceed, or a ready-to-send denial response.
fn ingress_fence_gate(
    state: &SharedState,
    headers: &HeaderMap,
    is_loopback: bool,
) -> Option<axum::response::Response> {
    let auth_header = headers.get("authorization").and_then(|v| v.to_str().ok());
    let tier = *state.ingress_tier.read().unwrap();
    let decision = perimeter_core::fence::check_ingress(tier, auth_header, is_loopback);
    match decision {
        perimeter_core::IngressDecision::Allow => None,
        perimeter_core::IngressDecision::Deny { status, message } => Some(
            (
                StatusCode::from_u16(status).unwrap_or(StatusCode::UNAUTHORIZED),
                message,
            )
                .into_response(),
        ),
    }
}

// ── Health ────────────────────────────────────────────────────────────────────

// Health stays open on loopback only; every other source passes the fence.
async fn handle_health(
    State(state): State<SharedState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> axum::response::Response {
    let is_loopback = is_trusted_loopback(&addr, &headers);
    if !is_loopback && let Some(denied) = ingress_fence_gate(&state, &headers, is_loopback) {
        return denied;
    }
    (StatusCode::OK, "membrane-mcp ok").into_response()
}

// ── GET /mcp — Streamable HTTP SSE channel (MCP 2025-03-26) ──────────────────
//
// Clients using the Streamable HTTP transport open this GET endpoint to receive
// server-initiated messages. We don't push server-initiated messages yet, so we
// return a minimal SSE stream with a keepalive comment and then close it.
// Clients will reconnect if they need more events.
async fn handle_mcp_sse(
    State(state): State<SharedState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> axum::response::Response {
    if let Some(denied) = ingress_fence_gate(&state, &headers, is_trusted_loopback(&addr, &headers))
    {
        return denied;
    }
    let body = ": keepalive\n\n";
    let mut headers = HeaderMap::new();
    headers.insert(
        "content-type",
        HeaderValue::from_static("text/event-stream"),
    );
    headers.insert("cache-control", HeaderValue::from_static("no-cache"));
    headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
    (StatusCode::OK, headers, body).into_response()
}

// ── MCP dispatcher ────────────────────────────────────────────────────────────

async fn handle_mcp(
    State(state): State<SharedState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    axum::Json(req): axum::Json<JsonRpcRequest>,
) -> axum::response::Response {
    let is_loopback = is_trusted_loopback(&addr, &headers);
    let auth_header = headers.get("authorization").and_then(|v| v.to_str().ok());
    dispatch_rpc(&state, req, auth_header, is_loopback).await
}

/// Full JSON-RPC dispatch for one request: ingress fence, notification
/// handling, then method routing. Split from the axum handler so the dispatch
/// surface is testable across fence tiers × auth schemes × source addresses
/// without real sockets.
pub(crate) async fn dispatch_rpc(
    state: &SharedState,
    req: JsonRpcRequest,
    auth_header: Option<&str>,
    is_loopback: bool,
) -> axum::response::Response {
    let is_notification = req.id.is_none();
    let id = req.id.clone().unwrap_or(Value::Null);

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
            let mut challenge = HeaderMap::new();
            if status == 401 {
                if let Some(c) = native_oauth(state) {
                    if let Ok(value) = HeaderValue::from_str(&format!(
                        "Bearer resource_metadata=\"{}\"",
                        c.metadata_url
                    )) {
                        challenge.insert("www-authenticate", value);
                    }
                }
            }
            return (
                axum::http::StatusCode::from_u16(status)
                    .unwrap_or(axum::http::StatusCode::UNAUTHORIZED),
                challenge,
                axum::Json(body),
            )
                .into_response();
        }
    }

    if req.method == "notifications/cancelled" {
        if let (Some(token), Some(request_id)) = (
            extract_bearer(auth_header),
            req.params.as_ref().and_then(|p| p.get("requestId")),
        ) {
            let hash = *blake3::hash(token.as_bytes()).as_bytes();
            let selected: Vec<_> = state
                .native_pending
                .lock()
                .await
                .iter()
                .filter(|(_, p)| p.token_hash == hash && p.request_id == *request_id)
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            for (turn, pending) in selected {
                if let Some(committed) = cancel_native(state, &pending).await {
                    state.native_pending.lock().await.remove(&turn);
                    if let Some(response) = state.pending_responses.lock().await.remove(&turn) {
                        let _ = response.sender.send(DispatchOutcome::Err(
                            if committed {
                                "cancelled after commit; write remains committed"
                            } else {
                                "cancelled before commit"
                            }
                            .into(),
                        ));
                    }
                }
            }
        }
        return (StatusCode::ACCEPTED, "").into_response();
    }
    match req.method.as_str() {
        // Notifications — JSON-RPC spec: MUST NOT send a response.
        // "notifications/initialized" is the spec name; bare "initialized" is
        // kept for clients that predate the namespaced form.
        "notifications/initialized"
        | "initialized"
        | "notifications/cancelled"
        | "notifications/progress" => {
            return (StatusCode::ACCEPTED, "").into_response();
        }
        _ => {}
    }

    // If id is absent this is a notification we don't explicitly handle; still no response.
    if is_notification {
        return (StatusCode::ACCEPTED, "").into_response();
    }

    let mut resp = match req.method.as_str() {
        "initialize" => handle_initialize(id, req.params),
        "ping" => JsonRpcResponse::ok(id, json!({})),
        "tools/list" => handle_tools_list(state, id, auth_header, is_loopback).await,
        "tools/call" => handle_tools_call(state, id, req.params, auth_header, is_loopback).await,
        _ => JsonRpcResponse::err(id, error_code::METHOD_NOT_FOUND, "method not found"),
    };

    if resp
        .error
        .as_ref()
        .is_some_and(|e| e.code == error_code::PERMISSION_DENIED)
    {
        if let Some(c) = native_oauth(state) {
            let mut headers = HeaderMap::new();
            if let Ok(value) =
                HeaderValue::from_str(&format!("Bearer resource_metadata=\"{}\"", c.metadata_url))
            {
                headers.insert("www-authenticate", value);
            }
            return (StatusCode::UNAUTHORIZED, headers, axum::Json(resp)).into_response();
        }
    }
    if let Some((client, turn, digest, deadline)) = resp.native_release.take() {
        let Ok(bytes) = serde_json::to_vec(&resp) else {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        };
        let pending = state.native_pending.clone();
        use futures_util::StreamExt;
        let stream =
            crate::native_release::stream(axum::body::Bytes::from(bytes), client, digest, deadline)
                .chain(futures_util::stream::once(async move {
                    pending.lock().await.remove(&turn);
                    Ok(axum::body::Bytes::new())
                }));
        // Linearization: the complete response data frame is handed to HTTP
        // while the hotel fence is still held. Only the following poll releases
        // it. Disconnect/drop before completion leaves uncertainty, not a retry.
        return (
            [
                ("content-type", "application/json"),
                ("cache-control", "no-store"),
            ],
            axum::body::Body::from_stream(stream),
        )
            .into_response();
    }
    axum::Json(resp).into_response()
}

// ── Method handlers ───────────────────────────────────────────────────────────

fn handle_initialize(id: Value, params: Option<Value>) -> JsonRpcResponse {
    // Params are optional — tolerate clients that omit them entirely.
    let params: Option<InitializeParams> = params.and_then(|p| serde_json::from_value(p).ok());

    // Version negotiation: echo the client's requested version when we speak
    // it; otherwise advertise our newest supported version.
    let protocol_version = params
        .as_ref()
        .map(|p| p.protocol_version.as_str())
        .filter(|v| SUPPORTED_PROTOCOL_VERSIONS.contains(v))
        .unwrap_or(SUPPORTED_PROTOCOL_VERSIONS[0])
        .to_string();

    JsonRpcResponse::ok(
        id,
        serde_json::to_value(InitializeResult {
            protocol_version,
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
    is_loopback: bool,
) -> JsonRpcResponse {
    // Prefer config-driven endpoint table; fall back to legacy route table.
    let endpoint = state.endpoint_table.read().await;
    let oauth = native_oauth(state);
    let tools: Vec<McpToolDescriptor> = if let Some(cfg) = endpoint.config() {
        let bearer = extract_bearer(auth_header);
        cfg.tools
            .iter()
            .filter(|tool| {
                if oauth.is_some()
                    && matches!(
                        tool.name.as_str(),
                        "native-memory.remember" | "native-memory.recall"
                    )
                {
                    true
                } else {
                    match cfg.effective_auth(tool) {
                        McpAuthScheme::BearerToken { grants } => bearer
                            .and_then(|token| {
                                verify_bearer_token(
                                    &tool.name,
                                    token,
                                    &grants,
                                    &state.vault_cache,
                                    state.vault.as_ref(),
                                )
                                .ok()
                            })
                            .is_some(),
                        McpAuthScheme::None => is_loopback,
                    }
                }
            })
            .map(|t| McpToolDescriptor {
                name: t.name.clone(),
                description: t.description.clone(),
                input_schema: t.input_schema.clone(),
            })
            .collect()
    } else {
        drop(endpoint);
        let bearer = extract_bearer(auth_header);
        let table = state.routing_table.read().await;
        table.visible_descriptors_by(|route| match &route.record.security.auth {
            McpAuthScheme::BearerToken { grants } => bearer
                .and_then(|token| {
                    verify_bearer_token(
                        &route.record.tool_name,
                        token,
                        grants,
                        &state.vault_cache,
                        state.vault.as_ref(),
                    )
                    .ok()
                })
                .is_some(),
            McpAuthScheme::None => is_loopback,
        })
    };

    let mut result = serde_json::to_value(ToolsListResult { tools }).unwrap();
    if oauth.is_some() {
        if let Some(tools) = result["tools"].as_array_mut() {
            for tool in tools {
                let scope = match tool["name"].as_str() {
                    Some("native-memory.remember") => Some(native_memory::oauth::REMEMBER),
                    Some("native-memory.recall") => Some(native_memory::oauth::RECALL),
                    _ => None,
                };
                if let Some(scope) = scope {
                    tool["securitySchemes"] = json!([{"type":"oauth2","scopes":[scope]}]);
                }
            }
        }
    }
    JsonRpcResponse::ok(id, result)
}

/// Map a classified auth failure to its MCP error code.
fn auth_error_code(e: &AuthError) -> i32 {
    match e {
        AuthError::Missing(_) | AuthError::Invalid(_) => error_code::PERMISSION_DENIED,
        AuthError::Expired(_) => error_code::TOKEN_EXPIRED,
        AuthError::Allotment(_) => error_code::ALLOTMENT_EXCEEDED,
    }
}

/// Await a dispatched call's terminal outcome and map it onto the MCP result
/// shape: business errors become `isError: true` results, approval-window
/// timeouts become `APPROVAL_REQUIRED`, transport failures stay JSON-RPC errors.
/// An upstream MCP server (McpUpstream route) already answers with a
/// `CallToolResult`. Wrapping that again as JSON text would hand the caller an
/// escaped `{"content":[…]}` string, so a value that is exactly that shape —
/// only `content` / `isError` / `structuredContent` / `_meta` keys, and every
/// content item typed — is returned as the result itself.
pub(crate) fn mcp_call_result_passthrough(value: &Value) -> Option<Value> {
    let obj = value.as_object()?;
    let content = obj.get("content")?.as_array()?;
    let known_keys = obj.keys().all(|k| {
        matches!(
            k.as_str(),
            "content" | "isError" | "structuredContent" | "_meta"
        )
    });
    let typed_items = content
        .iter()
        .all(|item| item.get("type").and_then(Value::as_str).is_some());
    (known_keys && typed_items).then(|| value.clone())
}

async fn await_dispatch_outcome(
    state: &SharedState,
    id: Value,
    tool_name: &str,
    turn_id: &str,
    timeout: Duration,
    requires_approval: bool,
    rx: oneshot::Receiver<DispatchOutcome>,
    transform_ok: impl FnOnce(&str) -> Value,
) -> JsonRpcResponse {
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(DispatchOutcome::Ok(content))) => {
            let value = transform_ok(&content);
            let result = mcp_call_result_passthrough(&value)
                .unwrap_or_else(|| serde_json::to_value(ToolCallResult::json(value)).unwrap());
            JsonRpcResponse::ok(id, result)
        }
        Ok(Ok(DispatchOutcome::Err(message))) => JsonRpcResponse::ok(
            id,
            serde_json::to_value(ToolCallResult::error(message)).unwrap(),
        ),
        Ok(Err(_)) => {
            warn!(tool = tool_name, %turn_id, "pending oneshot sender dropped");
            JsonRpcResponse::err(id, error_code::DISPATCH_ERROR, "IPC connection lost")
        }
        Err(_) => {
            {
                let mut pending = state.pending_responses.lock().await;
                pending.remove(turn_id);
            }
            {
                let mut buffers = state.streaming_buffers.lock().await;
                buffers.remove(turn_id);
            }
            warn!(tool = tool_name, %turn_id, requires_approval, "dispatch timed out");
            if requires_approval {
                JsonRpcResponse::err(
                    id,
                    error_code::APPROVAL_REQUIRED,
                    "operator approval was not granted within the approval window",
                )
            } else {
                JsonRpcResponse::err(id, error_code::DISPATCH_ERROR, "tool call timed out")
            }
        }
    }
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

    // Static-only mode has no IPC dispatch path — fail immediately and
    // honestly instead of parking the caller into a 30s timeout.
    if state.static_mode {
        return JsonRpcResponse::err(
            id,
            error_code::DISPATCH_ERROR,
            "this membrane runs in static-only mode (no IPC socket); tools/call cannot dispatch",
        );
    }

    let tool_name = &params.name;
    let oauth = native_oauth(state);
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

        let auth_scheme = endpoint
            .config()
            .map(|cfg| cfg.effective_auth(&tool_spec))
            .unwrap_or(McpAuthScheme::None);
        let caller = match if let Some(c) = &oauth {
            if !matches!(
                tool_name.as_str(),
                "native-memory.remember" | "native-memory.recall"
            ) || extract_bearer(auth_header).is_none()
            {
                Err(AuthError::Missing(
                    "native OAuth authentication required".into(),
                ))
            } else {
                Ok(crate::auth::CallerIdentity {
                    token_id: c.grant_id.clone(),
                    scopes: vec![],
                })
            }
        } else {
            authorize_call(
                tool_name,
                &auth_scheme,
                auth_header,
                is_loopback,
                &state.vault_cache,
                state.vault.as_ref(),
                &state.allotment,
            )
        } {
            Ok(c) => c,
            Err(e) => {
                warn!(tool = tool_name, err = %e, "config-driven auth rejected");
                return JsonRpcResponse::err(id, auth_error_code(&e), e.to_string());
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
        if oauth.is_some() && inbound.target_kind != "native_memory" {
            return JsonRpcResponse::err(id, error_code::PERMISSION_DENIED, "native resource only");
        }
        drop(endpoint);

        // Pre-approval rules are the only approval bypass. Loopback callers get
        // no special treatment: the provisioning turn is the authorization event,
        // and local processes are not implicitly the operator.
        let requires_approval = !preapproved;
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

        let mut envelope = InboundEnvelope {
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
            // The receiving philote runs the tool's handler policy
            // (deterministic ladder, then the declared fallback) before any
            // model turn. It also gets the advertised schema and the raw
            // args so input validation happens against what the caller
            // actually saw in `tools/list`.
            raw_transport: json!({
                "transport": "mcp",
                "tool": tool_name,
                "target_kind": inbound.target_kind,
                "target_id": inbound.target_id,
                // Cross-hotel pin (McpUpstream targets only); the membrane
                // runtime turns a non-null value into an EmitTask to that node.
                "target_node": inbound.target_node,
                "handler": tool_spec.handler,
                "input_schema": tool_spec.input_schema,
                "args": args,
            }),
            requires_approval,
            final_reply_to: Some(state.node_id.clone()),
            final_reply_role: Some("mcp-membrane".into()),
            final_reply_guest_id: Some(state.guest_id.clone()),
            target_node: None,
            target_guest_id: None,
            extra: serde_json::Map::new(),
        };

        let mut native_expectation = None;
        let mut native_permit = None;
        if inbound.target_kind == "native_memory" {
            // Native memory has a separate local issuer/grant policy. A bearer
            // token_id by itself is not delegated authority at the destination.
            if !matches!(auth_scheme, McpAuthScheme::BearerToken { .. }) || !preapproved {
                return JsonRpcResponse::err(
                    id,
                    error_code::INVALID_PARAMS,
                    "native memory requires explicit bearer auth and preapproval",
                );
            }
            let issued = (|| -> Result<_, native_memory::Error> {
                let path = state
                    .native_memory_config
                    .as_ref()
                    .ok_or(native_memory::Error::Unavailable)?;
                let config = native_memory::IngressConfig::load(path)?;
                let verifiers = config.response_verifiers.clone();
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Arguments {
                    event: String,
                    command: native_memory::Command,
                }
                // Use the exact caller args, not an authority-bearing field map.
                let input: Arguments = serde_json::from_value(args.clone())
                    .map_err(|_| native_memory::Error::Invalid)?;
                if inbound.action != format!("native-memory.{}", input.command.operation()) {
                    return Err(native_memory::Error::Denied);
                }
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|_| native_memory::Error::Unavailable)?
                    .as_secs();
                let signed = config.into_ingress().issue(
                    &caller.token_id,
                    input.command,
                    input.event,
                    turn_id.clone(),
                    now,
                )?;
                if Some(signed.claims.destination.as_str()) != inbound.target_node.as_deref()
                    || signed.claims.runner != inbound.target_id
                    || signed.claims.reply_node != state.node_id
                    || signed.claims.reply_guest != state.guest_id
                {
                    return Err(native_memory::Error::Denied);
                }
                let expectation =
                    native_memory::response::ResponseExpectation::new(signed.clone(), &verifiers)?;
                Ok((signed, expectation))
            })();
            let signed = match issued {
                Ok((signed, expectation)) => {
                    native_expectation = Some(expectation);
                    signed
                }
                Err(e) => {
                    return JsonRpcResponse::err(id, error_code::DISPATCH_ERROR, e.to_string());
                }
            };
            let mut routed = native_memory::RoutedRequest::new(signed);
            if let Some(c) = &oauth {
                use native_memory::authority_control::{Client, Reply, Request};
                let Some(token) = extract_bearer(auth_header) else {
                    return JsonRpcResponse::err(
                        id,
                        error_code::PERMISSION_DENIED,
                        "native OAuth required",
                    );
                };
                let admitted = async {
                    let mut client = Client::connect(&c.authority_socket).await?;
                    let reply = client
                        .send(&Request::Admit {
                            guest: state.guest_id.clone(),
                            token: token.into(),
                            envelope: Box::new(routed.signed_request.clone()),
                        })
                        .await?;
                    match reply {
                        Reply::Permit { handle } => Ok(handle),
                        _ => Err(native_memory::Error::Denied),
                    }
                }
                .await;
                let Ok(handle) = admitted else {
                    return JsonRpcResponse::err(
                        id,
                        error_code::PERMISSION_DENIED,
                        "native OAuth denied",
                    );
                };
                let digest = match native_memory::response::request_binding(&routed.signed_request)
                {
                    Ok(v) => v,
                    Err(_) => {
                        return JsonRpcResponse::err(
                            id,
                            error_code::PERMISSION_DENIED,
                            "native request invalid",
                        );
                    }
                };
                native_permit = Some((handle.clone(), digest));
                let token_hash = *blake3::hash(token.as_bytes()).as_bytes();
                let mut pending = state.native_pending.lock().await;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|v| v.as_secs())
                    .unwrap_or(0);
                pending.retain(|_, p| p.expires_at > now);
                if pending.len() >= 128 {
                    return JsonRpcResponse::err(
                        id,
                        error_code::DISPATCH_ERROR,
                        "native request capacity",
                    );
                }
                pending.insert(
                    turn_id.clone(),
                    NativePending {
                        request_id: id.clone(),
                        token_hash,
                        handle: handle.clone(),
                        config: c.clone(),
                        expires_at: routed.signed_request.claims.expires_at,
                    },
                );
                routed.authority_handle = Some(handle);
            }
            envelope.content =
                serde_json::to_string(&routed).expect("native envelope serialization");
            envelope.raw_transport = json!({"transport":"mcp","target_kind":"native_memory","target_id":inbound.target_id,"target_node":inbound.target_node});
        }

        let (tx, rx) = oneshot::channel::<DispatchOutcome>();
        {
            let mut pending = state.pending_responses.lock().await;
            pending.insert(
                turn_id.clone(),
                PendingResponse {
                    sender: tx,
                    native: native_expectation,
                },
            );
        }

        if let Err(e) = state.inbound_tx.send(envelope).await {
            let mut pending = state.pending_responses.lock().await;
            pending.remove(&turn_id);
            warn!(tool = tool_name, err = %e, "inbound_tx send failed");
            return JsonRpcResponse::err(id, error_code::DISPATCH_ERROR, "IPC send failed");
        }

        let mut response = await_dispatch_outcome(
            state,
            id,
            tool_name,
            &turn_id,
            timeout,
            requires_approval,
            rx,
            |content| {
                let transformed = transform::apply_outbound(&tool_spec, content);
                serde_json::from_str(&transformed).unwrap_or(json!({ "text": transformed }))
            },
        )
        .await;
        if let (Some(c), Some((handle, digest))) = (&oauth, native_permit) {
            use native_memory::authority_control::{Client, Reply, Request};
            let released = async {
                let mut client = Client::connect(&c.authority_socket).await?;
                // Conservative bound starts before the hotel acquires its five-second lease.
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(4);
                if response.error.is_some() {
                    let _ = client
                        .send(&Request::Cancel {
                            guest: state.guest_id.clone(),
                            handle,
                        })
                        .await;
                    return Err(native_memory::Error::Denied);
                }
                if !matches!(
                    client
                        .send(&Request::Release {
                            guest: state.guest_id.clone(),
                            handle,
                            digest
                        })
                        .await?,
                    Reply::Ready
                ) {
                    return Err(native_memory::Error::Denied);
                }
                Ok((client, deadline))
            }
            .await;
            match released {
                Ok((client, deadline)) => {
                    response.native_release = Some((client, turn_id.clone(), digest, deadline))
                }
                Err(_) => {
                    state.native_pending.lock().await.remove(&turn_id);
                    return JsonRpcResponse::err(
                        response.id,
                        error_code::PERMISSION_DENIED,
                        "native result release denied",
                    );
                }
            }
        }
        return response;
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
            return JsonRpcResponse::err(id, auth_error_code(&e), e.to_string());
        }
    };

    // Legacy routes have no action mapping, so an upstream target cannot name
    // its remote tool. Refuse rather than mis-dispatching to the agent role.
    if matches!(
        route.record.target,
        ansible_mesh_core::mcp_route::McpRouteTarget::McpUpstream { .. }
            | ansible_mesh_core::mcp_route::McpRouteTarget::NativeMemory { .. }
    ) {
        return JsonRpcResponse::err(
            id,
            error_code::DISPATCH_ERROR,
            format!("tool '{tool_name}': mcp_upstream targets require an endpoint config"),
        );
    }

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
        target_node: None,
        target_guest_id: None,
        extra: serde_json::Map::new(),
    };

    let (tx, rx) = oneshot::channel::<DispatchOutcome>();
    {
        let mut pending = state.pending_responses.lock().await;
        pending.insert(turn_id.clone(), tx.into());
    }

    if let Err(e) = state.inbound_tx.send(envelope).await {
        let mut pending = state.pending_responses.lock().await;
        pending.remove(&turn_id);
        warn!(tool = tool_name, err = %e, "inbound_tx send failed");
        return JsonRpcResponse::err(id, error_code::DISPATCH_ERROR, "IPC send failed");
    }

    await_dispatch_outcome(
        state,
        id,
        tool_name,
        &turn_id,
        timeout,
        requires_approval,
        rx,
        |content| serde_json::from_str(content).unwrap_or(json!({ "text": content })),
    )
    .await
}
