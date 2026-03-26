//! `philotic-web serve` — local management HTTP + WebSocket server.
//!
//! Generates a random session token on startup, binds it to a same-origin
//! session cookie for the embedded UI, and still accepts
//! `Authorization: Bearer <token>` as an explicit transitional compatibility
//! path.
//!
//! REST endpoints:
//!   GET  /api/status
//!   GET  /api/guests
//!   GET  /api/agents
//!   GET  /api/agents/:agent_id/roles
//!   GET  /api/agents/:agent_id/rules
//!   GET  /api/skills
//!   GET  /api/mesh/targets
//!   GET  /api/mesh/targets/:target_node_id/status
//!   GET  /api/mesh/targets/:target_node_id/guests
//!   GET  /api/mesh/targets/:target_node_id/agents
//!   POST /api/mesh/targets/:target_node_id/agents/:agent_id/chat
//!   GET  /api/config
//!   GET  /api/config/telegram
//!   GET  /api/config/gemini
//!   GET  /api/sessions    (stub — returns [] until session table exists)
//!   GET  /api/apartments/:agent_id   (disabled by default for the desktop membrane)
//!   POST /api/guests/:guest_id/restart
//!   POST /api/guests/:guest_id/stop
//!
//! WebSocket:
//!   GET  /ws  — live push of guest/session state changes
//!              auth via same-origin cookie

use anyhow::{anyhow, bail, Context, Result};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use rand::Rng;
use rusqlite::Connection;
use rust_embed::RustEmbed;
use serde_json::{json, Value};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{broadcast, watch, Mutex};
use tower_http::cors::{AllowOrigin, CorsLayer};

use philotic_client::{
    DesktopMembraneAgentView, DesktopMembraneGuestView, DesktopMembraneStatusView,
    GuestIdentity, IpcRequest, IpcResponse, LeaseEnvelope,
    OperatorTargetAgentInventoryView, OperatorTargetGuestInventoryView,
    OperatorTargetStatusView, OperatorTargetView, PhiloticClient,
    OPERATOR_CHAT_REPLY_ROLE,
};

// ── Embedded UI assets ────────────────────────────────────────────────────────

#[derive(RustEmbed)]
#[folder = "ui-dist/"]
struct UiAssets;

const AUTH_COOKIE_NAME: &str = "philotic_session";
const AUTH_COOKIE_MAX_AGE_SECS: u64 = 60 * 60 * 8;
const HEADER_COOP: &str = "cross-origin-opener-policy";
const HEADER_CORP: &str = "cross-origin-resource-policy";

// ── State shared across request handlers ─────────────────────────────────────

#[derive(Clone)]
struct AppState {
    token: Arc<String>,
    db_path: PathBuf,
    /// IPC socket path for the connected hotel
    socket: Arc<String>,
    /// Broadcast channel for WebSocket push events
    tx: broadcast::Sender<String>,
}

#[derive(Clone)]
struct DesktopMembraneLeaseHandle {
    client: Arc<Mutex<PhiloticClient>>,
    lease_key: Arc<String>,
}

#[derive(serde::Deserialize)]
struct OperatorChatTurnBody {
    #[serde(default)]
    operator_session_id: Option<String>,
    #[serde(default)]
    conversation_id: Option<String>,
    content: String,
}

#[derive(Clone, serde::Serialize)]
struct OperatorChatAcceptedView {
    accepted: bool,
    target_node_id: String,
    target_agent_id: String,
    operator_session_id: String,
    conversation_id: String,
    session_id: String,
    turn_id: String,
    delivery_kind: String,
}

// ── Public entry point ────────────────────────────────────────────────────────

pub async fn run(
    port: u16,
    _db: Option<PathBuf>,
    config: Option<PathBuf>,
    allow_origins: Option<String>,
) -> Result<()> {
    let db_path = _db.unwrap_or_else(|| match crate::init::active_profile() {
        Some(_) => crate::init::profile_dir().join("context.db"),
        None => PathBuf::from("aiua_context.db"),
    });
    let config_path = config.unwrap_or_else(|| match crate::init::active_profile() {
        Some(_) => crate::init::profile_dir().join("config.json"),
        None => PathBuf::from("mesh-config.json"),
    });
    let hotel = read_hotel_name(&config_path);
    let socket = crate::start::socket_path(&hotel);
    let lease_key = desktop_membrane_lease_key(&hotel);
    let lease_handle = acquire_desktop_membrane_lease(&socket, &lease_key, port).await?;

    // Generate session token
    let token: String = {
        let bytes: [u8; 24] = rand::thread_rng().gen();
        format!("philotic-{}", hex::encode(bytes))
    };

    // Broadcast channel for WebSocket events (capacity 256)
    let (tx, _) = broadcast::channel::<String>(256);

    let state = AppState {
        token: Arc::new(token.clone()),
        db_path,
        socket: Arc::new(socket),
        tx,
    };

    // CORS — localhost only; UI is embedded and served from the same origin
    let cors = build_cors(allow_origins.as_deref());

    let app = Router::new()
        // API routes
        .route("/api/status", get(handle_status))
        .route("/api/guests", get(handle_guests))
        .route("/api/agents", get(handle_agents))
        .route("/api/agents/:agent_id/roles", get(handle_agent_roles))
        .route("/api/agents/:agent_id/rules", get(handle_agent_rules))
        .route("/api/skills", get(handle_skills))
        .route("/api/config", get(handle_config))
        .route("/api/config/telegram", get(handle_config_telegram))
        .route("/api/config/gemini", get(handle_config_gemini))
        .route("/api/mesh/targets", get(handle_mesh_targets))
        .route(
            "/api/mesh/targets/:target_node_id/status",
            get(handle_mesh_target_status),
        )
        .route(
            "/api/mesh/targets/:target_node_id/guests",
            get(handle_mesh_target_guests),
        )
        .route(
            "/api/mesh/targets/:target_node_id/agents",
            get(handle_mesh_target_agents),
        )
        .route(
            "/api/mesh/targets/:target_node_id/agents/:agent_id/chat",
            post(handle_mesh_target_agent_chat),
        )
        .route("/api/sessions", get(handle_sessions))
        .route("/api/apartments/:agent_id", get(handle_apartment))
        .route("/api/guests/:guest_id/restart", post(handle_guest_restart))
        .route("/api/guests/:guest_id/stop", post(handle_guest_stop))
        .route("/ws", get(handle_ws))
        // Embedded UI — index.html gets token injected; cookie auth bootstrap; all other assets served as-is
        .route("/", get(handle_index))
        .fallback(get(handle_static))
        .layer(cors)
        .with_state(state);

    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let renew_handle = tokio::spawn(run_desktop_membrane_lease_renewal(
        lease_handle.clone(),
        shutdown_tx.clone(),
    ));

    println!("philotic-web serve");
    println!("──────────────────────────────────────────");
    println!("  http://127.0.0.1:{port}");
    println!();
    println!("  Debug token: {token}");
    println!();
    println!("  Press Ctrl-C to stop.");

    // Auto-open the embedded desktop in the default browser
    let _ = tokio::process::Command::new("open")
        .arg(format!("http://127.0.0.1:{port}"))
        .spawn();

    let shutdown_reason = wait_for_shutdown(shutdown_rx);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_reason)
        .await?;

    let _ = shutdown_tx.send(true);
    let _ = renew_handle.await;
    release_desktop_membrane_lease(&lease_handle).await?;
    Ok(())
}

fn desktop_membrane_lease_key(hotel: &str) -> String {
    format!("desktop:{hotel}:operator-surface")
}

async fn acquire_desktop_membrane_lease(
    socket: &str,
    lease_key: &str,
    port: u16,
) -> Result<DesktopMembraneLeaseHandle> {
    let identity = GuestIdentity {
        guest_id: format!("philotic-web-membrane-{port}"),
        role: "management".into(),
        supported_tools: vec![],
    };
    let mut client = PhiloticClient::connect_at(&socket, identity)
        .await
        .with_context(|| format!("connect to hotel at {socket} for desktop membrane lease"))?;

    let response = client
        .send_request(IpcRequest::AcquireDesktopMembraneLease {
            lease_key: lease_key.to_string(),
            port,
        })
        .await
        .context("acquire desktop membrane lease")?;

    let lease = match response {
        IpcResponse::DesktopMembraneLease {
            desktop_granted: true,
            desktop_lease: Some(lease),
        } => lease,
        IpcResponse::DesktopMembraneLease {
            desktop_granted: false,
            desktop_lease: Some(lease),
        } => {
            bail!(
                "desktop membrane lease [{}] is already held by [{}] (epoch {})",
                lease.lease_scope,
                lease.owner_guest_id,
                lease.lease_epoch
            );
        }
        other => {
            return Err(anyhow!(
                "unexpected desktop membrane lease response: {other:?}"
            ))
        }
    };

    println!(
        "  Desktop membrane lease: {} (owner {}, epoch {})",
        lease.lease_scope, lease.owner_guest_id, lease.lease_epoch
    );

    Ok(DesktopMembraneLeaseHandle {
        client: Arc::new(Mutex::new(client)),
        lease_key: Arc::new(lease.lease_scope),
    })
}

async fn release_desktop_membrane_lease(handle: &DesktopMembraneLeaseHandle) -> Result<()> {
    let mut client = handle.client.lock().await;
    let response = client
        .send_request(IpcRequest::ReleaseDesktopMembraneLease {
            lease_key: handle.lease_key.as_str().to_string(),
        })
        .await
        .context("release desktop membrane lease")?;

    match response {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        IpcResponse::Standard { message, .. } => {
            Err(anyhow!("desktop membrane lease release failed: {message}"))
        }
        other => Err(anyhow!(
            "unexpected desktop membrane lease release response: {other:?}"
        )),
    }
}

async fn run_desktop_membrane_lease_renewal(
    handle: DesktopMembraneLeaseHandle,
    shutdown_tx: watch::Sender<bool>,
) {
    let mut shutdown_rx = shutdown_tx.subscribe();
    let mut epoch = match current_desktop_membrane_lease(&handle).await {
        Ok(lease) => lease.lease_epoch,
        Err(err) => {
            eprintln!("desktop membrane lease startup check failed: {err:#}");
            let _ = shutdown_tx.send(true);
            return;
        }
    };
    let mut interval = tokio::time::interval(Duration::from_secs(15));

    loop {
        tokio::select! {
            _ = interval.tick() => {
                match renew_desktop_membrane_lease(&handle, epoch).await {
                    Ok(lease) => {
                        epoch = lease.lease_epoch;
                    }
                    Err(err) => {
                        eprintln!("desktop membrane lease renewal lost: {err:#}");
                        let _ = shutdown_tx.send(true);
                        return;
                    }
                }
            }
            changed = shutdown_rx.changed() => {
                if changed.is_ok() && *shutdown_rx.borrow() {
                    return;
                }
            }
        }
    }
}

async fn current_desktop_membrane_lease(
    handle: &DesktopMembraneLeaseHandle,
) -> Result<LeaseEnvelope> {
    let mut client = handle.client.lock().await;
    let response = client
        .send_request(IpcRequest::GetDesktopMembraneLeaseOwner {
            lease_key: handle.lease_key.as_str().to_string(),
        })
        .await
        .context("query desktop membrane lease owner")?;

    match response {
        IpcResponse::DesktopMembraneLeaseStatus {
            desktop_active: true,
            desktop_lease: Some(lease),
        } => Ok(lease),
        IpcResponse::DesktopMembraneLeaseStatus { .. } => Err(anyhow!(
            "desktop membrane lease [{}] is no longer active",
            handle.lease_key
        )),
        other => Err(anyhow!(
            "unexpected desktop membrane lease status response: {other:?}"
        )),
    }
}

async fn renew_desktop_membrane_lease(
    handle: &DesktopMembraneLeaseHandle,
    lease_epoch: u64,
) -> Result<LeaseEnvelope> {
    let mut client = handle.client.lock().await;
    let response = client
        .send_request(IpcRequest::RenewDesktopMembraneLease {
            lease_key: handle.lease_key.as_str().to_string(),
            lease_epoch,
        })
        .await
        .context("renew desktop membrane lease")?;

    match response {
        IpcResponse::DesktopMembraneLease {
            desktop_granted: true,
            desktop_lease: Some(lease),
        } => Ok(lease),
        IpcResponse::DesktopMembraneLease {
            desktop_granted: false,
            desktop_lease: Some(lease),
        } => Err(anyhow!(
            "desktop membrane lease [{}] moved to [{}] (epoch {})",
            lease.lease_scope,
            lease.owner_guest_id,
            lease.lease_epoch
        )),
        IpcResponse::DesktopMembraneLease {
            desktop_granted: false,
            desktop_lease: None,
        } => Err(anyhow!(
            "desktop membrane lease [{}] renew was denied without an owner",
            handle.lease_key
        )),
        other => Err(anyhow!(
            "unexpected desktop membrane lease renew response: {other:?}"
        )),
    }
}

async fn wait_for_shutdown(mut shutdown_rx: watch::Receiver<bool>) {
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        changed = shutdown_rx.changed() => {
            if changed.is_ok() && *shutdown_rx.borrow() {
                return;
            }
        }
    }
}

// ── Embedded UI handlers ──────────────────────────────────────────────────────

/// Serve `index.html` and bind the session token as a same-origin cookie.
async fn handle_index(State(state): State<AppState>) -> Response {
    let html = match UiAssets::get("index.html") {
        Some(f) => String::from_utf8_lossy(f.data.as_ref()).into_owned(),
        None => return (StatusCode::NOT_FOUND, "UI not built").into_response(),
    };

    let mut response = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response();
    let headers = response.headers_mut();
    headers.insert(header::SET_COOKIE, auth_cookie_header(&state));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(no_store_header_value()),
    );
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        HeaderName::from_static(HEADER_COOP),
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        HeaderName::from_static(HEADER_CORP),
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; connect-src 'self' ws: wss:; object-src 'none'; frame-ancestors 'none'; base-uri 'self'",
        ),
    );
    response
}

/// Serve any other embedded asset (JS, CSS, icons, etc.).
/// Falls back to `index.html` for SPA client-side routes.
async fn handle_static(State(state): State<AppState>, uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    if let Some(asset) = UiAssets::get(path) {
        let mime = asset.metadata.mimetype();
        let mut response = (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mime)],
            asset.data.into_owned(),
        )
            .into_response();
        let headers = response.headers_mut();
        headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        headers.insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        );
        headers.insert(
            HeaderName::from_static(HEADER_COOP),
            HeaderValue::from_static("same-origin"),
        );
        headers.insert(
            HeaderName::from_static(HEADER_CORP),
            HeaderValue::from_static("same-origin"),
        );
        response
    } else {
        // SPA fallback — let the client-side router handle it
        handle_index(State(state)).await
    }
}

// ── Auth helper ───────────────────────────────────────────────────────────────

fn check_auth(headers: &HeaderMap, state: &AppState) -> bool {
    header_bearer_token(headers)
        .or_else(|| cookie_token(headers, AUTH_COOKIE_NAME))
        .map(|token| token == state.token.as_str())
        .unwrap_or(false)
}

fn header_bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

fn cookie_token<'a>(headers: &'a HeaderMap, cookie_name: &str) -> Option<&'a str> {
    let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
    cookie_header.split(';').map(str::trim).find_map(|pair| {
        let (name, value) = pair.split_once('=')?;
        (name == cookie_name).then_some(value)
    })
}

fn auth_cookie_header(state: &AppState) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{AUTH_COOKIE_NAME}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={AUTH_COOKIE_MAX_AGE_SECS}",
        state.token
    ))
    .expect("session cookie should be a valid header value")
}

fn no_store_header_value() -> &'static str {
    "no-store, no-cache, must-revalidate, private"
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error": "unauthorized"})),
    )
        .into_response()
}

// ── GET /api/status ───────────────────────────────────────────────────────────

async fn handle_status(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    let status = match ipc_desktop_membrane_status(&state.socket).await {
        Ok(status) => status,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response();
        }
    };

    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "hotel": status.hotel,
        "daemon": status.daemon,
    }))
    .into_response()
}

// ── GET /api/guests ───────────────────────────────────────────────────────────

async fn handle_guests(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    match ipc_desktop_membrane_guests(&state.socket).await {
        Ok(guests) => Json(guests).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── GET /api/agents ───────────────────────────────────────────────────────────

async fn handle_agents(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    match ipc_desktop_membrane_agents(&state.socket).await {
        Ok(agents) => Json(agents).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── GET /api/mesh/targets ────────────────────────────────────────────────────

async fn handle_mesh_targets(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    match ipc_desktop_membrane_targets(&state.socket).await {
        Ok(targets) => Json(targets).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn handle_mesh_target_status(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(target_node_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    match ipc_desktop_membrane_target_status(&state.socket, &target_node_id).await {
        Ok(status) => Json(status).into_response(),
        Err(err) => {
            let status_code = if err
                .to_string()
                .contains("not currently active in the registry")
            {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status_code, Json(json!({"error": err.to_string()}))).into_response()
        }
    }
}

async fn handle_mesh_target_guests(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(target_node_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    match ipc_desktop_membrane_target_guests(&state.socket, &target_node_id).await {
        Ok(guests) => Json(guests).into_response(),
        Err(err) => {
            let status_code = if err
                .to_string()
                .contains("not currently active in the registry")
            {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status_code, Json(json!({"error": err.to_string()}))).into_response()
        }
    }
}

async fn handle_mesh_target_agents(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(target_node_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    match ipc_desktop_membrane_target_agents(&state.socket, &target_node_id).await {
        Ok(agents) => Json(agents).into_response(),
        Err(err) => {
            let status_code = if err
                .to_string()
                .contains("not currently active in the registry")
            {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status_code, Json(json!({"error": err.to_string()}))).into_response()
        }
    }
}

async fn handle_mesh_target_agent_chat(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path((target_node_id, agent_id)): Path<(String, String)>,
    axum::Json(body): axum::Json<OperatorChatTurnBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    let operator_session_id = body
        .operator_session_id
        .unwrap_or_else(|| "desktop-membrane".into());

    let targets = match ipc_desktop_membrane_targets(&state.socket).await {
        Ok(targets) => targets,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response()
        }
    };
    let Some(target) = targets
        .iter()
        .find(|target| target.target_node_id == target_node_id)
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("mesh target [{target_node_id}] is not currently active in the registry")})),
        )
            .into_response();
    };
    let Some(local_target) = targets.iter().find(|target| target.is_local) else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "local operator target is unavailable"})),
        )
            .into_response();
    };
    let local_node_id = local_target.target_node_id.clone();

    let conversation_id = body
        .conversation_id
        .unwrap_or_else(|| format!("operator-chat:{operator_session_id}:{agent_id}"));
    let session_id = conversation_id.clone();
    let turn_id = new_operator_chat_id("operator-chat-turn");
    let accepted = OperatorChatAcceptedView {
        accepted: true,
        target_node_id: target_node_id.clone(),
        target_agent_id: agent_id.clone(),
        operator_session_id: operator_session_id.clone(),
        conversation_id: conversation_id.clone(),
        session_id: session_id.clone(),
        turn_id: turn_id.clone(),
        delivery_kind: if target.is_local {
            "local-direct".into()
        } else {
            "router-routed".into()
        },
    };

    let tx = state.tx.clone();
    let socket = state.socket.as_ref().clone();
    let accepted_for_error = accepted.clone();
    tokio::spawn(async move {
        if let Err(err) = stream_operator_chat_turn(
            socket,
            tx.clone(),
            local_node_id,
            target_node_id,
            agent_id,
            operator_session_id,
            conversation_id,
            session_id,
            turn_id,
            body.content,
        )
        .await
        {
            let _ = tx.send(
                json!({
                    "type": "operator_chat:error",
                    "payload": {
                        "target_node_id": accepted_for_error.target_node_id,
                        "target_agent_id": accepted_for_error.target_agent_id,
                        "operator_session_id": accepted_for_error.operator_session_id,
                        "conversation_id": accepted_for_error.conversation_id,
                        "session_id": accepted_for_error.session_id,
                        "turn_id": accepted_for_error.turn_id,
                        "message": err.to_string(),
                    }
                })
                .to_string(),
            );
        }
    });

    (StatusCode::ACCEPTED, Json(json!(accepted))).into_response()
}

async fn stream_operator_chat_turn(
    socket: String,
    tx: broadcast::Sender<String>,
    local_node_id: String,
    target_node_id: String,
    agent_id: String,
    operator_session_id: String,
    conversation_id: String,
    session_id: String,
    turn_id: String,
    content: String,
) -> Result<()> {
    let reply_guest_id = new_operator_chat_id("operator-chat");
    let mut client = connect_client_with_identity(&socket, GuestIdentity {
        guest_id: reply_guest_id.clone(),
        role: OPERATOR_CHAT_REPLY_ROLE.into(),
        supported_tools: vec![],
    })
    .await?;

    match client
        .send_request(IpcRequest::SubscribeInbox {
            role: OPERATOR_CHAT_REPLY_ROLE.into(),
        })
        .await?
    {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("unexpected operator chat inbox subscribe response: {other:?}"),
    }

    match client
        .send_request(IpcRequest::EmitTask {
            target_node: target_node_id.clone(),
            target_role: "agent".into(),
            target_guest_id: Some(agent_id.clone()),
            task_json: json!({
                "source": "operator_chat",
                "transport": "operator_chat",
                "session_id": session_id,
                "turn_id": turn_id,
                "chat_id": conversation_id,
                "content": content,
                "final_reply_to": local_node_id,
                "final_reply_role": OPERATOR_CHAT_REPLY_ROLE,
                "final_reply_guest_id": reply_guest_id
            })
            .to_string(),
        })
        .await?
    {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("unexpected operator chat emit response: {other:?}"),
    }

    loop {
        let inbound = tokio::time::timeout(Duration::from_secs(30), client.recv_task())
            .await
            .map_err(|_| anyhow!("timed out waiting for operator chat reply"))??;
        let IpcResponse::InboundTask { task_json, .. } = inbound else {
            bail!("unexpected operator chat reply envelope: {inbound:?}");
        };
        let payload: Value = serde_json::from_str(&task_json)?;
        let action = payload
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("send_reply");
        match action {
            "turn_event" => {
                let _ = tx.send(
                    json!({
                        "type": "operator_chat:turn_event",
                        "payload": {
                            "target_node_id": target_node_id,
                            "target_agent_id": agent_id,
                            "operator_session_id": operator_session_id,
                            "conversation_id": payload.get("chat_id").and_then(Value::as_str).unwrap_or(&conversation_id),
                            "session_id": payload.get("session_id").and_then(Value::as_str).unwrap_or(&session_id),
                            "turn_id": payload.get("turn_id").and_then(Value::as_str).unwrap_or(&turn_id),
                            "event": payload.get("event").and_then(Value::as_str).unwrap_or("unknown")
                        }
                    })
                    .to_string(),
                );
            }
            "partial_reply" => {
                let _ = tx.send(
                    json!({
                        "type": "operator_chat:partial_reply",
                        "payload": {
                            "target_node_id": target_node_id,
                            "target_agent_id": agent_id,
                            "operator_session_id": operator_session_id,
                            "conversation_id": payload.get("chat_id").and_then(Value::as_str).unwrap_or(&conversation_id),
                            "session_id": payload.get("session_id").and_then(Value::as_str).unwrap_or(&session_id),
                            "turn_id": payload.get("turn_id").and_then(Value::as_str).unwrap_or(&turn_id),
                            "content": payload.get("content").and_then(Value::as_str).unwrap_or_default()
                        }
                    })
                    .to_string(),
                );
            }
            "send_reply" => {
                let _ = tx.send(
                    json!({
                        "type": "operator_chat:reply",
                        "payload": {
                            "target_node_id": target_node_id,
                            "target_agent_id": agent_id,
                            "operator_session_id": operator_session_id,
                            "conversation_id": payload.get("chat_id").and_then(Value::as_str).unwrap_or(&conversation_id),
                            "session_id": payload.get("session_id").and_then(Value::as_str).unwrap_or(&session_id),
                            "turn_id": payload.get("turn_id").and_then(Value::as_str).unwrap_or(&turn_id),
                            "reply_action": action,
                            "content": payload.get("content").and_then(Value::as_str).unwrap_or_default()
                        }
                    })
                    .to_string(),
                );
                return Ok(());
            }
            other => {
                let _ = tx.send(
                    json!({
                        "type": "operator_chat:event",
                        "payload": {
                            "target_node_id": target_node_id,
                            "target_agent_id": agent_id,
                            "operator_session_id": operator_session_id,
                            "conversation_id": payload.get("chat_id").and_then(Value::as_str).unwrap_or(&conversation_id),
                            "session_id": payload.get("session_id").and_then(Value::as_str).unwrap_or(&session_id),
                            "turn_id": payload.get("turn_id").and_then(Value::as_str).unwrap_or(&turn_id),
                            "action": other,
                            "payload": payload
                        }
                    })
                    .to_string(),
                );
            }
        }
    }
}

// ── GET /api/sessions ─────────────────────────────────────────────────────────

async fn handle_sessions(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    // Sessions are held in RAM by philote — not persisted to SQLite yet.
    // Return empty list; the desktop handles this gracefully.
    Json(json!([])).into_response()
}

// ── GET /api/apartments/:agent_id ─────────────────────────────────────────────

async fn handle_apartment(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    match query_apartment(&state.db_path, &agent_id) {
        Ok(Some(apt)) => Json(apt).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "no apartment data"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

fn query_apartment(db_path: &PathBuf, agent_id: &str) -> Result<Option<Value>> {
    let conn = Connection::open(db_path)?;

    // Get agent identity for persona_name
    let persona_name: Option<String> = conn
        .query_row(
            "SELECT persona_name FROM agent_identities WHERE agent_id = ?1",
            [agent_id],
            |row| row.get(0),
        )
        .ok();

    // Get all apartment rows for this agent, keyed by memory_type
    let mut stmt = conn.prepare(
        "SELECT memory_type, content, created_at FROM memory_apartments WHERE agent_id = ?1 ORDER BY created_at DESC"
    )?;

    let mut sections: HashMap<String, Value> = HashMap::new();
    let rows = stmt.query_map([agent_id], |row| {
        let memory_type: String = row.get(0)?;
        let content: String = row.get(1)?;
        let created_at: Option<String> = row.get(2)?;
        Ok((memory_type, content, created_at))
    })?;

    for row in rows.flatten() {
        let (memory_type, content, _) = row;
        // Only keep the most recent entry per memory_type (rows ordered DESC)
        if !sections.contains_key(&memory_type) {
            let parsed: Value = serde_json::from_str(&content).unwrap_or(Value::String(content));
            sections.insert(memory_type, parsed);
        }
    }

    if sections.is_empty() && persona_name.is_none() {
        return Ok(None);
    }

    let mut result = serde_json::Map::new();
    result.insert("agent_id".into(), Value::String(agent_id.to_string()));
    if let Some(name) = persona_name {
        result.insert("persona_name".into(), Value::String(name));
    }
    for (k, v) in sections {
        result.insert(k, v);
    }

    Ok(Some(Value::Object(result)))
}

// ── POST /api/guests/:guest_id/restart ────────────────────────────────────────

async fn handle_guest_restart(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(guest_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    // Signal the hotel via IPC to restart the guest
    match ipc_guest_action(&state.socket, &guest_id, "restart").await {
        Ok(_) => {
            let event = json!({ "type": "guest:started", "payload": { "guest_id": guest_id } });
            let _ = state.tx.send(event.to_string());
            Json(json!({"ok": true})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── POST /api/guests/:guest_id/stop ──────────────────────────────────────────

async fn handle_guest_stop(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(guest_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    match ipc_guest_action(&state.socket, &guest_id, "stop").await {
        Ok(_) => {
            let event = json!({ "type": "guest:stopped", "payload": { "guest_id": guest_id } });
            let _ = state.tx.send(event.to_string());
            Json(json!({"ok": true})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── GET /api/agents/:agent_id/roles ──────────────────────────────────────────

async fn handle_agent_roles(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_list_role_incarnations(&state.socket, &agent_id).await {
        Ok(roles) => Json(roles).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── GET /api/agents/:agent_id/rules ──────────────────────────────────────────

async fn handle_agent_rules(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_list_rules(&state.socket, &agent_id).await {
        Ok(rules) => Json(rules).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── GET /api/skills ───────────────────────────────────────────────────────────

async fn handle_skills(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_list_skills(&state.socket).await {
        Ok(skills) => Json(skills).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── GET /api/config ───────────────────────────────────────────────────────────

async fn handle_config(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    // Read well-known operator-facing config keys. Values are returned as-is
    // (already JSON-encoded strings in the context graph).
    let keys = &[
        "execution_host",
        "vault_registry",
        "tool_runner_registry",
    ];
    let mut out = serde_json::Map::new();
    for key in keys {
        match ipc_get_config(&state.socket, key).await {
            Ok(Some(val)) => { out.insert(key.to_string(), val); }
            Ok(None) => { out.insert(key.to_string(), Value::Null); }
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("reading {key}: {e}")})),
                )
                    .into_response();
            }
        }
    }
    Json(Value::Object(out)).into_response()
}

// ── GET /api/config/telegram ──────────────────────────────────────────────────

async fn handle_config_telegram(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    // Return token ref name only — never the token value.
    let token_ref = match ipc_get_config(&state.socket, "telegram_bot_token").await {
        Ok(Some(val)) => {
            // The value is the token itself. Surface only that it is set and its first 8 chars as a hint.
            let hint = val
                .as_str()
                .map(|s| format!("{}…", &s[..s.len().min(8)]))
                .unwrap_or_else(|| "(set)".into());
            Some(hint)
        }
        Ok(None) => None,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    Json(json!({
        "token_configured": token_ref.is_some(),
        "token_hint": token_ref,
    }))
    .into_response()
}

// ── GET /api/config/gemini ────────────────────────────────────────────────────

async fn handle_config_gemini(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    // Surface OAuth metadata only — no token values.
    let meta_keys = &[
        "gemini_oauth_project_id",
        "gemini_oauth_scope",
        "gemini_oauth_token_type",
        "gemini_oauth_access_token_expires_at",
    ];
    let ref_keys = &[
        "gemini_oauth_access_token_ref",
        "gemini_oauth_refresh_token_ref",
    ];
    let mut meta = serde_json::Map::new();
    for key in meta_keys {
        match ipc_get_config(&state.socket, key).await {
            Ok(Some(val)) => { meta.insert(key.to_string(), val); }
            Ok(None) => {}
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("reading {key}: {e}")})),
                )
                    .into_response();
            }
        }
    }
    let mut refs_configured = serde_json::Map::new();
    for key in ref_keys {
        let configured = match ipc_get_config(&state.socket, key).await {
            Ok(v) => v.is_some(),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": format!("reading {key}: {e}")})),
                )
                    .into_response();
            }
        };
        refs_configured.insert(key.to_string(), Value::Bool(configured));
    }
    Json(json!({
        "oauth_metadata": meta,
        "token_refs_configured": refs_configured,
    }))
    .into_response()
}

// ── IPC helpers ───────────────────────────────────────────────────────────────

async fn ipc_guest_action(socket: &str, guest_id: &str, action: &str) -> Result<()> {
    let mut client = connect_management_client(socket, "philotic-web-mgmt").await?;

    // Publish a management action to the hotel
    let payload = serde_json::json!({
        "action": action,
        "guest_id": guest_id,
    });
    client
        .send_request(IpcRequest::PublishMessage {
            target_role: "management.guest_action".into(),
            payload,
        })
        .await?;

    Ok(())
}

async fn ipc_desktop_membrane_status(socket: &str) -> Result<DesktopMembraneStatusView> {
    let mut client = connect_management_client(socket, "philotic-web-status").await?;
    match client
        .send_request(IpcRequest::GetDesktopMembraneStatus)
        .await?
    {
        IpcResponse::DesktopMembraneStatusView { membrane_status } => Ok(membrane_status),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected desktop membrane status response: {other:?}"
        )),
    }
}

async fn ipc_desktop_membrane_guests(socket: &str) -> Result<Vec<DesktopMembraneGuestView>> {
    let mut client = connect_management_client(socket, "philotic-web-guests").await?;
    match client
        .send_request(IpcRequest::ListDesktopMembraneGuests)
        .await?
    {
        IpcResponse::DesktopMembraneGuestsView { membrane_guests } => Ok(membrane_guests),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected desktop membrane guests response: {other:?}"
        )),
    }
}

async fn ipc_desktop_membrane_agents(socket: &str) -> Result<Vec<DesktopMembraneAgentView>> {
    let mut client = connect_management_client(socket, "philotic-web-agents").await?;
    match client
        .send_request(IpcRequest::ListDesktopMembraneAgents)
        .await?
    {
        IpcResponse::DesktopMembraneAgentsView { membrane_agents } => Ok(membrane_agents),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected desktop membrane agents response: {other:?}"
        )),
    }
}

async fn ipc_desktop_membrane_targets(socket: &str) -> Result<Vec<OperatorTargetView>> {
    let mut client = connect_management_client(socket, "philotic-web-mesh-targets").await?;
    match client.send_request(IpcRequest::QueryOperatorTargets).await? {
        IpcResponse::OperatorTargetsView { operator_targets } => Ok(operator_targets),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected desktop membrane targets response: {other:?}"
        )),
    }
}

async fn ipc_desktop_membrane_target_status(
    socket: &str,
    target_node_id: &str,
) -> Result<OperatorTargetStatusView> {
    let mut client = connect_management_client(socket, "philotic-web-mesh-target-status").await?;
    match client
        .send_request(IpcRequest::QueryOperatorTargetStatus {
            target_node_id: target_node_id.to_string(),
        })
        .await?
    {
        IpcResponse::OperatorTargetStatusView {
            operator_target_status,
        } => Ok(operator_target_status),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected desktop membrane target status response: {other:?}"
        )),
    }
}

async fn ipc_desktop_membrane_target_guests(
    socket: &str,
    target_node_id: &str,
) -> Result<OperatorTargetGuestInventoryView> {
    let mut client = connect_management_client(socket, "philotic-web-mesh-target-guests").await?;
    match client
        .send_request(IpcRequest::QueryOperatorTargetGuests {
            target_node_id: target_node_id.to_string(),
        })
        .await?
    {
        IpcResponse::OperatorTargetGuestsView {
            operator_target_guests,
        } => Ok(operator_target_guests),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected desktop membrane target guests response: {other:?}"
        )),
    }
}

async fn ipc_desktop_membrane_target_agents(
    socket: &str,
    target_node_id: &str,
) -> Result<OperatorTargetAgentInventoryView> {
    let mut client = connect_management_client(socket, "philotic-web-mesh-target-agents").await?;
    match client
        .send_request(IpcRequest::QueryOperatorTargetAgents {
            target_node_id: target_node_id.to_string(),
        })
        .await?
    {
        IpcResponse::OperatorTargetAgentsView {
            operator_target_agents,
        } => Ok(operator_target_agents),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected desktop membrane target agents response: {other:?}"
        )),
    }
}

async fn ipc_list_role_incarnations(socket: &str, agent_id: &str) -> Result<Value> {
    let mut client = connect_management_client(socket, "philotic-web-roles").await?;
    match client
        .send_request(IpcRequest::ListRoleIncarnations {
            agent_id: agent_id.to_string(),
        })
        .await?
    {
        IpcResponse::Standard {
            ok: true,
            data: Some(d),
            ..
        } => Ok(d),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected role incarnations response: {other:?}")),
    }
}

async fn ipc_list_rules(socket: &str, agent_id: &str) -> Result<Vec<Value>> {
    let mut client = connect_management_client(socket, "philotic-web-rules").await?;
    match client
        .send_request(IpcRequest::ListRules {
            agent_id: agent_id.to_string(),
        })
        .await?
    {
        IpcResponse::RuleList { rules } => Ok(rules),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected rules response: {other:?}")),
    }
}

async fn ipc_list_skills(socket: &str) -> Result<Vec<Value>> {
    let mut client = connect_management_client(socket, "philotic-web-skills").await?;
    match client.send_request(IpcRequest::ListSkills {}).await? {
        IpcResponse::SkillList { skills } => Ok(skills),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected skills response: {other:?}")),
    }
}

async fn ipc_get_config(socket: &str, key: &str) -> Result<Option<Value>> {
    let mut client = connect_management_client(socket, "philotic-web-config").await?;
    match client
        .send_request(IpcRequest::GetConfig {
            key: key.to_string(),
        })
        .await?
    {
        IpcResponse::ConfigData { value_json: Some(raw), .. } => {
            let parsed: Value = serde_json::from_str(&raw)
                .unwrap_or_else(|_| Value::String(raw));
            Ok(Some(parsed))
        }
        IpcResponse::ConfigData { value_json: None, .. } => Ok(None),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected config data response: {other:?}")),
    }
}

fn new_operator_chat_id(prefix: &str) -> String {
    let mut rng = rand::thread_rng();
    let suffix = format!("{:016x}", rng.r#gen::<u64>());
    format!("{prefix}-{suffix}")
}

async fn connect_management_client(socket: &str, guest_id: &str) -> Result<PhiloticClient> {
    connect_client_with_identity(socket, GuestIdentity {
        guest_id: guest_id.into(),
        role: "management".into(),
        supported_tools: vec![],
    })
    .await
}

async fn connect_client_with_identity(socket: &str, identity: GuestIdentity) -> Result<PhiloticClient> {
    PhiloticClient::connect_at(socket, identity)
        .await
        .map_err(Into::into)
}

// ── WebSocket /ws ─────────────────────────────────────────────────────────────

async fn handle_ws(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    let cookie_authed = cookie_token(&headers, AUTH_COOKIE_NAME)
        .map(|t| t == state.token.as_str())
        .unwrap_or(false);

    if !cookie_authed {
        return unauthorized();
    }

    ws.on_upgrade(move |socket| ws_handler(socket, state))
}

async fn ws_handler(mut socket: WebSocket, state: AppState) {
    let mut rx = state.tx.subscribe();

    // Send a welcome ping so the client knows it's live
    let welcome = json!({"type": "connected", "payload": {"server": "philotic-web"}});
    let _ = socket.send(Message::Text(welcome.to_string().into())).await;

    loop {
        tokio::select! {
            // Forward broadcast events to this client
            Ok(msg) = rx.recv() => {
                if socket.send(Message::Text(msg.into())).await.is_err() {
                    break;
                }
            }
            // Handle incoming client messages (ping/close)
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(data))) => {
                        let _ = socket.send(Message::Pong(data)).await;
                    }
                    _ => {}
                }
            }
        }
    }
}

// ── CORS ──────────────────────────────────────────────────────────────────────

fn build_cors(allow_origins: Option<&str>) -> CorsLayer {
    use axum::http::{header, Method};

    let layer = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE, header::COOKIE]);

    if let Some(origins) = allow_origins {
        let parsed: Vec<HeaderValue> = origins
            .split(',')
            .filter_map(|origin| origin.trim().parse().ok())
            .collect();
        layer.allow_origin(AllowOrigin::list(parsed))
    } else {
        layer
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn read_hotel_name(config_path: &PathBuf) -> String {
    std::fs::read_to_string(config_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| {
            v.get("hotels")
                .and_then(|h| h.as_object())
                .and_then(|m| m.keys().next().cloned())
        })
        .unwrap_or_else(|| "default".to_string())
}
