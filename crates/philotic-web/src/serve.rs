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
//!   GET  /api/agents/:agent_id/routing-policies
//!   POST /api/routing-policies/:proposal_id/disposition
//!   PATCH /api/agents/:agent_id/roles/:role_name
//!   GET  /api/skills
//!   GET  /api/mesh/targets
//!   GET  /api/mesh/targets/:target_node_id/status
//!   GET  /api/mesh/targets/:target_node_id/guests
//!   GET  /api/mesh/targets/:target_node_id/agents
//!   POST /api/mesh/targets/:target_node_id/agents/:agent_id/chat
//!   GET  /api/config
//!   GET  /api/config/telegram
//!   GET  /api/config/gemini
//!   GET  /api/components
//!   GET  /api/component-templates
//!   POST /api/components
//!   GET  /api/components/:guest_id
//!   PATCH /api/components/:guest_id
//!   DELETE /api/components/:guest_id
//!   POST /api/components/:guest_id/enable
//!   POST /api/components/:guest_id/disable
//!   POST /api/components/:guest_id/restart
//!   GET  /api/graphs
//!   GET  /api/graphs/:graph_id
//!   GET  /api/secrets
//!   POST /api/agents/:agent_id/roles/:role_name/skills  (assign skill)
//!   DELETE /api/agents/:agent_id/roles/:role_name/skills/:skill_name  (revoke skill)
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
    routing::{delete, get, post, put},
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
    ComponentManifest, CronJob, CronJobSource, DesktopMembraneAgentView, DesktopMembraneGuestView,
    DesktopMembraneStatusView, GuestIdentity, IpcRequest, IpcResponse, LeaseEnvelope,
    OperatorTargetAgentInventoryView, OperatorTargetGuestInventoryView, OperatorTargetStatusView,
    OperatorTargetView, PhiloticClient, OPERATOR_CHAT_REPLY_ROLE,
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

#[derive(serde::Deserialize)]
struct SetRoutingPolicyDispositionBody {
    state: String,
    reason: String,
}
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct ComponentInventoryEntry {
    guest_id: String,
    role: String,
    hotel: String,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: HashMap<String, String>,
    component_type: String,
    is_active: bool,
    auto_start: bool,
    #[serde(default)]
    active_pid: Option<String>,
    #[serde(default)]
    last_active_at: Option<u64>,
    #[serde(default)]
    component_config: Value,
    #[serde(default)]
    capabilities: Vec<String>,
}

#[derive(Clone, Debug, serde::Serialize)]
struct ComponentTemplateFieldView {
    key: String,
    label: String,
    target: String,
    input_kind: String,
    required: bool,
    #[serde(default)]
    secret: bool,
    #[serde(default)]
    vault_only: bool,
    #[serde(default)]
    placeholder: Option<String>,
    #[serde(default)]
    help: Option<String>,
    #[serde(default)]
    default_value: Option<Value>,
}

#[derive(Clone, Debug, serde::Serialize)]
struct ComponentTemplateDependencyView {
    key: String,
    label: String,
    location: String,
    required: bool,
    #[serde(default)]
    secret: bool,
    #[serde(default)]
    vault_only: bool,
    help: String,
}

#[derive(Clone, Debug, serde::Serialize)]
struct ComponentTemplateView {
    id: String,
    label: String,
    description: String,
    command: String,
    role: String,
    #[serde(default)]
    env_fields: Vec<ComponentTemplateFieldView>,
    #[serde(default)]
    component_config_fields: Vec<ComponentTemplateFieldView>,
    #[serde(default)]
    dependencies: Vec<ComponentTemplateDependencyView>,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct PatchComponentBody {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    hotel: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    env: Option<HashMap<String, String>>,
    #[serde(default)]
    component_config: Option<Value>,
    #[serde(default)]
    auto_start: Option<bool>,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct DeleteComponentBody {
    confirm_guest_id: String,
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
        .route(
            "/api/agents/:agent_id",
            axum::routing::patch(handle_agent_patch),
        )
        .route("/api/agents/:agent_id/roles", get(handle_agent_roles))
        .route(
            "/api/agents/:agent_id/roles/:role_name",
            axum::routing::patch(handle_role_patch),
        )
        .route("/api/agents/:agent_id/rules", get(handle_agent_rules))
        .route(
            "/api/agents/:agent_id/routing-policies",
            get(handle_agent_routing_policies),
        )
        .route(
            "/api/routing-policies/:proposal_id/disposition",
            post(handle_routing_policy_disposition),
        )
        .route("/api/skills", get(handle_skills))
        .route("/api/toolsets", get(handle_toolsets))
        .route("/api/config", get(handle_config))
        .route("/api/config/telegram", get(handle_config_telegram))
        .route("/api/config/gemini", get(handle_config_gemini))
        .route(
            "/api/components",
            get(handle_components).post(handle_component_create),
        )
        .route("/api/component-templates", get(handle_component_templates))
        .route(
            "/api/components/:guest_id",
            get(handle_component_detail)
                .patch(handle_component_patch)
                .delete(handle_component_delete),
        )
        .route(
            "/api/components/:guest_id/enable",
            post(handle_component_enable),
        )
        .route(
            "/api/components/:guest_id/disable",
            post(handle_component_disable),
        )
        .route(
            "/api/components/:guest_id/restart",
            post(handle_component_restart),
        )
        .route("/api/graphs", get(handle_graphs))
        .route("/api/graphs/:graph_id", get(handle_graph_detail))
        .route("/api/cron", get(handle_cron_list).post(handle_cron_create))
        .route("/api/cron/:job_id", delete(handle_cron_delete))
        .route("/api/cron/:job_id/enable", post(handle_cron_enable))
        .route("/api/cron/:job_id/disable", post(handle_cron_disable))
        .route("/api/secrets", get(handle_secrets))
        .route("/api/secrets/rotate", post(handle_secret_rotate))
        .route("/api/vault", post(handle_vault_add))
        .route("/api/config/:key", put(handle_config_put))
        .route(
            "/api/agents/:agent_id/roles/:role_name/skills",
            post(handle_assign_skill),
        )
        .route(
            "/api/agents/:agent_id/roles/:role_name/skills/:skill_name",
            delete(handle_revoke_skill),
        )
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
    let mut client = connect_client_with_identity(
        &socket,
        GuestIdentity {
            guest_id: reply_guest_id.clone(),
            role: OPERATOR_CHAT_REPLY_ROLE.into(),
            supported_tools: vec![],
        },
    )
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
    // Load role incarnations, then enrich each with its toolset profile data.
    let role_data = match ipc_list_role_incarnations(&state.socket, &agent_id).await {
        Ok(d) => d,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": e.to_string()})),
            )
                .into_response()
        }
    };
    let roles_arr = role_data
        .get("roles")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut enriched_roles = Vec::new();
    for role in roles_arr {
        let profile_name = role
            .get("toolset_profile")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let profile = if !profile_name.is_empty() {
            ipc_get_toolset_profile(&state.socket, profile_name)
                .await
                .ok()
                .flatten()
        } else {
            None
        };
        let mut role_obj = role.clone();
        if let (Some(obj), Some(p)) = (role_obj.as_object_mut(), profile) {
            obj.insert("toolset".to_string(), p);
        }
        enriched_roles.push(role_obj);
    }
    Json(json!({ "agent_id": agent_id, "roles": enriched_roles })).into_response()
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

async fn handle_agent_routing_policies(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_list_routing_policies(&state.socket, &agent_id).await {
        Ok(policies) => Json(policies).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn handle_routing_policy_disposition(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(proposal_id): Path<String>,
    Json(body): Json<SetRoutingPolicyDispositionBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_set_routing_policy_disposition(&state.socket, &proposal_id, &body.state, &body.reason)
        .await
    {
        Ok(value) => Json(value).into_response(),
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

// ── GET /api/toolsets ─────────────────────────────────────────────────────────

async fn handle_toolsets(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_list_toolset_profiles(&state.socket).await {
        Ok(profiles) => Json(profiles).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── Cron endpoints ────────────────────────────────────────────────────────────

async fn handle_cron_list(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_list_cron_jobs(&state.socket).await {
        Ok(jobs) => Json(jobs).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
struct CreateCronBody {
    schedule: String,
    target_role: String,
    payload: String,
    #[serde(default)]
    target_node_id: Option<String>,
    #[serde(default)]
    guaranteed: bool,
    #[serde(default)]
    enabled: bool,
}

async fn handle_cron_create(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<CreateCronBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let job = CronJob {
        id: uuid::Uuid::new_v4().to_string(),
        schedule: body.schedule,
        target_role: body.target_role,
        target_node_id: body.target_node_id,
        payload: body.payload,
        guaranteed: body.guaranteed,
        enabled: body.enabled,
        last_fired_epoch: None,
        next_fire_at: now_ms,
        created_at: now_ms,
        created_by: CronJobSource::Operator,
    };
    match ipc_register_cron_job(&state.socket, job.clone()).await {
        Ok(()) => {
            let event = json!({ "type": "cron:created", "payload": { "job_id": job.id } });
            let _ = state.tx.send(event.to_string());
            Json(json!({"ok": true, "job_id": job.id})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn handle_cron_delete(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_remove_cron_job(&state.socket, &job_id).await {
        Ok(()) => {
            let event = json!({ "type": "cron:deleted", "payload": { "job_id": job_id } });
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

async fn handle_cron_enable(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_set_cron_enabled(&state.socket, &job_id, true).await {
        Ok(()) => {
            let event =
                json!({ "type": "cron:updated", "payload": { "job_id": job_id, "enabled": true } });
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

async fn handle_cron_disable(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(job_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_set_cron_enabled(&state.socket, &job_id, false).await {
        Ok(()) => {
            let event = json!({ "type": "cron:updated", "payload": { "job_id": job_id, "enabled": false } });
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

// ── GET /api/config ───────────────────────────────────────────────────────────

async fn handle_config(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    // Read well-known operator-facing config keys. Values are returned as-is
    // (already JSON-encoded strings in the context graph).
    let keys = &["execution_host", "vault_registry", "tool_runner_registry"];
    let mut out = serde_json::Map::new();
    for key in keys {
        match ipc_get_config(&state.socket, key).await {
            Ok(Some(val)) => {
                out.insert(key.to_string(), val);
            }
            Ok(None) => {
                out.insert(key.to_string(), Value::Null);
            }
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
            Ok(Some(val)) => {
                meta.insert(key.to_string(), val);
            }
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

// ── GET /api/graphs ───────────────────────────────────────────────────────────

async fn handle_graphs(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_list_graph_instances(&state.socket).await {
        Ok(instances) => Json(instances).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── GET /api/graphs/:graph_id ─────────────────────────────────────────────────

async fn handle_graph_detail(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(graph_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_list_graph_instances(&state.socket).await {
        Ok(instances) => {
            let found = instances
                .into_iter()
                .find(|g| g.get("graph_id").and_then(|v| v.as_str()) == Some(graph_id.as_str()));
            match found {
                Some(g) => Json(g).into_response(),
                None => (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "graph instance not found"})),
                )
                    .into_response(),
            }
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── GET /api/secrets ──────────────────────────────────────────────────────────
//
// Returns a read-only inventory of known secret refs: vault registry entries
// (vault name → secret_ref) plus known config-key secret refs (telegram, gemini).
// Values are never returned — only metadata about what refs are configured.

async fn handle_secrets(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    // Vault registry: {vault_name: string, secret_ref: string}[]
    let vault_registry = match ipc_get_config(&state.socket, "vault_registry").await {
        Ok(Some(v)) => v.as_array().cloned().unwrap_or_default(),
        _ => vec![],
    };
    let vault_entries: Vec<Value> = vault_registry
        .into_iter()
        .filter_map(|entry| {
            let name = entry
                .get("vault_name")
                .or_else(|| entry.get("name"))
                .and_then(|v| v.as_str())?
                .to_string();
            let secret_ref = entry
                .get("secret_ref")
                .and_then(|v| v.as_str())?
                .to_string();
            Some(json!({ "kind": "vault_token", "name": name, "secret_ref": secret_ref }))
        })
        .collect();

    // Named config-key secret refs
    let named_refs = [
        ("gemini_oauth_access_token", "gemini_oauth_access_token_ref"),
        (
            "gemini_oauth_refresh_token",
            "gemini_oauth_refresh_token_ref",
        ),
        ("telegram_bot_token", "telegram_bot_token"),
    ];
    let mut named_entries: Vec<Value> = Vec::new();
    for (label, key) in &named_refs {
        let configured = match ipc_get_config(&state.socket, key).await {
            Ok(Some(_)) => true,
            _ => false,
        };
        named_entries.push(
            json!({ "kind": "config_ref", "name": label, "key": key, "configured": configured }),
        );
    }

    Json(json!({
        "vault_entries": vault_entries,
        "config_refs": named_entries,
    }))
    .into_response()
}

// ── PUT /api/config/:key ──────────────────────────────────────────────────────
//
// Allowed keys for operator mutation (prevents arbitrary config overwrites).
const MUTABLE_CONFIG_KEYS: &[&str] = &["telegram_bot_token", "execution_host", "vault_registry"];

#[derive(serde::Deserialize)]
struct SetConfigBody {
    value: Value,
}

async fn handle_config_put(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(key): Path<String>,
    Json(body): Json<SetConfigBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    if !MUTABLE_CONFIG_KEYS.contains(&key.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("config key '{}' is not operator-mutable", key)})),
        )
            .into_response();
    }
    let value_json = match serde_json::to_string(&body.value) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("invalid value: {e}")})),
            )
                .into_response()
        }
    };
    match ipc_set_config(&state.socket, &key, &value_json).await {
        Ok(()) => {
            let event = json!({ "type": "config:updated", "payload": { "key": key } });
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

// ── POST /api/secrets/rotate ──────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct RotateSecretBody {
    secret_ref: String,
    plaintext: String,
}

async fn handle_secret_rotate(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<RotateSecretBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_rotate_secret(&state.socket, &body.secret_ref, &body.plaintext).await {
        Ok(()) => Json(json!({"ok": true})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── POST /api/vault ───────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct AddVaultEntryBody {
    vault_name: String,
    plaintext: String,
    #[serde(default)]
    allowed_roles: Vec<String>,
}

async fn handle_vault_add(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<AddVaultEntryBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_add_vault_entry(
        &state.socket,
        &body.vault_name,
        &body.plaintext,
        body.allowed_roles,
    )
    .await
    {
        Ok(secret_ref) => {
            let event = json!({ "type": "vault:entry-added", "payload": { "vault_name": body.vault_name, "secret_ref": secret_ref } });
            let _ = state.tx.send(event.to_string());
            Json(json!({"ok": true, "secret_ref": secret_ref})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── POST /api/agents/:agent_id/roles/:role_name/skills ────────────────────────

#[derive(serde::Deserialize)]
struct AssignSkillBody {
    skill_name: String,
}

async fn handle_assign_skill(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path((agent_id, role_name)): Path<(String, String)>,
    Json(body): Json<AssignSkillBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_assign_skill(&state.socket, &agent_id, &role_name, &body.skill_name).await {
        Ok(result) => {
            let event = json!({ "type": "skill:assigned", "payload": { "agent_id": agent_id, "role_name": role_name, "skill_name": body.skill_name } });
            let _ = state.tx.send(event.to_string());
            Json(result).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── DELETE /api/agents/:agent_id/roles/:role_name/skills/:skill_name ──────────

async fn handle_revoke_skill(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path((agent_id, role_name, skill_name)): Path<(String, String, String)>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_revoke_skill(&state.socket, &agent_id, &role_name, &skill_name).await {
        Ok(result) => {
            let event = json!({ "type": "skill:revoked", "payload": { "agent_id": agent_id, "role_name": role_name, "skill_name": skill_name } });
            let _ = state.tx.send(event.to_string());
            Json(result).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── PATCH /api/agents/:agent_id ───────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct PatchAgentBody {
    #[serde(default)]
    persona_name: Option<String>,
    #[serde(default)]
    soul_text: Option<String>,
    #[serde(default)]
    identity_text: Option<String>,
    #[serde(default)]
    user_context_text: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
    #[serde(default)]
    default_toolset: Option<Vec<String>>,
    #[serde(default)]
    default_skillset: Option<Vec<String>>,
}

#[derive(serde::Deserialize)]
struct PatchRoleBody {
    #[serde(default)]
    guest_id: Option<String>,
    #[serde(default)]
    toolset_profile: Option<String>,
    #[serde(default)]
    role_identity_addendum: Option<String>,
    #[serde(default)]
    role_manifest: Option<String>,
    #[serde(default)]
    is_admin: Option<bool>,
    #[serde(default)]
    inactive_ttl_seconds: Option<u64>,
    #[serde(default)]
    iteration_cap: Option<u32>,
    #[serde(default)]
    approval_policy: Option<String>,
    #[serde(default)]
    model_profile: Option<String>,
    #[serde(default)]
    context_window_policy: Option<String>,
}

async fn handle_agent_patch(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(agent_id): Path<String>,
    Json(body): Json<PatchAgentBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_patch_agent_bundle(
        &state.socket,
        &agent_id,
        body.persona_name,
        body.soul_text,
        body.identity_text,
        body.user_context_text,
        body.system_prompt,
        body.default_toolset,
        body.default_skillset,
    )
    .await
    {
        Ok(agent) => {
            let event = json!({ "type": "agent:updated", "payload": { "agent_id": agent_id } });
            let _ = state.tx.send(event.to_string());
            Json(agent).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn handle_role_patch(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path((agent_id, role_name)): Path<(String, String)>,
    Json(body): Json<PatchRoleBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_patch_role(
        &state.socket,
        &agent_id,
        &role_name,
        body.guest_id,
        body.toolset_profile,
        body.role_identity_addendum,
        body.role_manifest,
        body.is_admin,
        body.inactive_ttl_seconds,
        body.iteration_cap,
        body.approval_policy,
        body.model_profile,
        body.context_window_policy,
    )
    .await
    {
        Ok(role) => {
            let event = json!({
                "type": "role:updated",
                "payload": { "agent_id": agent_id, "role_name": role_name }
            });
            let _ = state.tx.send(event.to_string());
            Json(role).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn ipc_patch_agent_bundle(
    socket: &str,
    agent_id: &str,
    persona_name: Option<String>,
    soul_text: Option<String>,
    identity_text: Option<String>,
    user_context_text: Option<String>,
    system_prompt: Option<String>,
    default_toolset: Option<Vec<String>>,
    default_skillset: Option<Vec<String>>,
) -> Result<Value> {
    let mut client = connect_management_client(socket, "philotic-web-patch-agent").await?;
    match client
        .send_request(IpcRequest::PatchAgentBundle {
            agent_id: agent_id.to_string(),
            persona_name,
            soul_text,
            identity_text,
            user_context_text,
            system_prompt,
            default_toolset,
            default_skillset,
        })
        .await?
    {
        IpcResponse::AgentUpdated { agent } => Ok(serde_json::to_value(agent)?),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected patch_agent_bundle response: {other:?}")),
    }
}

async fn ipc_patch_role(
    socket: &str,
    agent_id: &str,
    role_name: &str,
    guest_id: Option<String>,
    toolset_profile: Option<String>,
    role_identity_addendum: Option<String>,
    role_manifest: Option<String>,
    is_admin: Option<bool>,
    inactive_ttl_seconds: Option<u64>,
    iteration_cap: Option<u32>,
    approval_policy: Option<String>,
    model_profile: Option<String>,
    context_window_policy: Option<String>,
) -> Result<Value> {
    let role_data = ipc_list_role_incarnations(socket, agent_id).await?;
    let existing = role_data
        .get("roles")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|role| role.get("role_name").and_then(Value::as_str) == Some(role_name))
        .cloned()
        .ok_or_else(|| anyhow!("role [{role_name}] not configured for agent [{agent_id}]"))?;

    let existing_guest_id = existing
        .get("guest_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("role [{role_name}] missing guest_id"))?;
    let existing_toolset_profile = existing
        .get("toolset_profile")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("role [{role_name}] missing toolset_profile"))?;
    let existing_is_admin = existing
        .get("is_admin")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let existing_inactive_ttl_seconds =
        existing.get("inactive_ttl_seconds").and_then(Value::as_u64);
    let turn_loop_config = existing
        .get("turn_loop_config")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let mut client = connect_management_client(socket, "philotic-web-patch-role").await?;
    match client
        .send_request(IpcRequest::ConfigureRole {
            agent_id: agent_id.to_string(),
            role_name: role_name.to_string(),
            guest_id: guest_id.unwrap_or_else(|| existing_guest_id.to_string()),
            calling_role: "orchestrator".into(),
            toolset_profile: toolset_profile
                .unwrap_or_else(|| existing_toolset_profile.to_string()),
            role_identity_addendum: Some(role_identity_addendum.unwrap_or_else(|| {
                existing
                    .get("role_identity_addendum")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })),
            role_manifest: Some(role_manifest.unwrap_or_else(|| {
                existing
                    .get("role_manifest")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })),
            is_admin: is_admin.unwrap_or(existing_is_admin),
            inactive_ttl_seconds: Some(
                inactive_ttl_seconds
                    .or(existing_inactive_ttl_seconds)
                    .unwrap_or(0),
            )
            .filter(|ttl| *ttl > 0),
            iteration_cap: Some(iteration_cap.unwrap_or_else(|| {
                turn_loop_config
                    .get("iteration_cap")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or(0)
            }))
            .filter(|cap| *cap > 0),
            approval_policy: Some(approval_policy.unwrap_or_else(|| {
                turn_loop_config
                    .get("approval_policy")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            }))
            .filter(|policy| !policy.trim().is_empty()),
            model_profile: Some(model_profile.unwrap_or_else(|| {
                turn_loop_config
                    .get("model_profile")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            }))
            .filter(|profile| !profile.trim().is_empty()),
            context_window_policy: Some(context_window_policy.unwrap_or_else(|| {
                turn_loop_config
                    .get("context_window_policy")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            }))
            .filter(|policy| !policy.trim().is_empty()),
        })
        .await?
    {
        IpcResponse::ConfigureRoleOk { .. } => {
            let refreshed = ipc_list_role_incarnations(socket, agent_id).await?;
            refreshed
                .get("roles")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .find(|role| role.get("role_name").and_then(Value::as_str) == Some(role_name))
                .cloned()
                .ok_or_else(|| anyhow!("role [{role_name}] missing after patch"))
        }
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected patch_role response: {other:?}")),
    }
}

// ── GET /api/components ───────────────────────────────────────────────────────

async fn handle_components(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_list_components(&state.socket).await {
        Ok(components) => Json(components).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── GET /api/component-templates ─────────────────────────────────────────────

async fn handle_component_templates(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    Json(component_templates()).into_response()
}

// ── POST /api/components ──────────────────────────────────────────────────────

async fn handle_component_create(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(manifest): Json<ComponentManifest>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_register_component(&state.socket, manifest).await {
        Ok(component) => {
            let event = json!({ "type": "component:created", "payload": { "guest_id": component.guest_id } });
            let _ = state.tx.send(event.to_string());
            (StatusCode::CREATED, Json(component)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── GET /api/components/:guest_id ─────────────────────────────────────────────

async fn handle_component_detail(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(guest_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_get_component(&state.socket, &guest_id).await {
        Ok(component) => Json(component).into_response(),
        Err(e) if e.to_string().contains("component not found") => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "component not found"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── PATCH /api/components/:guest_id ───────────────────────────────────────────

async fn handle_component_patch(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(guest_id): Path<String>,
    Json(body): Json<PatchComponentBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_get_component(&state.socket, &guest_id).await {
        Ok(current) => {
            let manifest = ComponentManifest {
                guest_id: guest_id.clone(),
                role: body.role.unwrap_or(current.role),
                hotel: body.hotel.unwrap_or(current.hotel),
                command: body.command.unwrap_or(current.command),
                args: body.args.unwrap_or(current.args),
                env: body.env.unwrap_or(current.env),
                component_config: body.component_config.unwrap_or(current.component_config),
                auto_start: body.auto_start.unwrap_or(current.auto_start),
            };
            match ipc_register_component(&state.socket, manifest).await {
                Ok(component) => {
                    let event =
                        json!({ "type": "component:updated", "payload": { "guest_id": guest_id } });
                    let _ = state.tx.send(event.to_string());
                    Json(component).into_response()
                }
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": e.to_string()})),
                )
                    .into_response(),
            }
        }
        Err(e) if e.to_string().contains("component not found") => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "component not found"})),
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── DELETE /api/components/:guest_id ─────────────────────────────────────────

async fn handle_component_delete(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(guest_id): Path<String>,
    Json(body): Json<DeleteComponentBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    if body.confirm_guest_id != guest_id {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "confirmation text must exactly match guest_id"})),
        )
            .into_response();
    }
    match ipc_remove_component(&state.socket, &guest_id).await {
        Ok(_) => {
            let event = json!({ "type": "component:deleted", "payload": { "guest_id": guest_id } });
            let _ = state.tx.send(event.to_string());
            Json(json!({"ok": true, "guest_id": guest_id})).into_response()
        }
        Err(e)
            if e.to_string().contains("GUEST_NOT_FOUND")
                || e.to_string().contains("component not found") =>
        {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "component not found"})),
            )
                .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── POST /api/components/:guest_id/enable ─────────────────────────────────────

async fn handle_component_enable(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(guest_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_set_component_active(&state.socket, &guest_id, true).await {
        Ok(_) => {
            let event = json!({ "type": "component:enabled", "payload": { "guest_id": guest_id } });
            let _ = state.tx.send(event.to_string());
            Json(json!({"ok": true, "guest_id": guest_id, "active": true})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── POST /api/components/:guest_id/disable ────────────────────────────────────

async fn handle_component_disable(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(guest_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_set_component_active(&state.socket, &guest_id, false).await {
        Ok(_) => {
            let event =
                json!({ "type": "component:disabled", "payload": { "guest_id": guest_id } });
            let _ = state.tx.send(event.to_string());
            Json(json!({"ok": true, "guest_id": guest_id, "active": false})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── POST /api/components/:guest_id/restart ────────────────────────────────────

async fn handle_component_restart(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(guest_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_restart_component(&state.socket, &guest_id).await {
        Ok(_) => {
            let event =
                json!({ "type": "component:restarted", "payload": { "guest_id": guest_id } });
            let _ = state.tx.send(event.to_string());
            Json(json!({"ok": true, "guest_id": guest_id})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
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
    match client
        .send_request(IpcRequest::QueryOperatorTargets)
        .await?
    {
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

async fn ipc_list_routing_policies(socket: &str, agent_id: &str) -> Result<Vec<Value>> {
    let mut client = connect_management_client(socket, "philotic-web-routing-policies").await?;
    match client
        .send_request(IpcRequest::ListRoutingPolicies {
            agent_id: agent_id.to_string(),
        })
        .await?
    {
        IpcResponse::RoutingPolicyList { policies } => Ok(policies),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected routing policies response: {other:?}")),
    }
}

async fn ipc_set_routing_policy_disposition(
    socket: &str,
    proposal_id: &str,
    state: &str,
    reason: &str,
) -> Result<Value> {
    let mut client =
        connect_management_client(socket, "philotic-web-routing-policy-disposition").await?;
    match client
        .send_request(IpcRequest::SetRoutingPolicyDisposition {
            proposal_id: proposal_id.to_string(),
            state: state.to_string(),
            reason: reason.to_string(),
            source_tool: Some("philotic-web".to_string()),
        })
        .await?
    {
        IpcResponse::Standard {
            ok: true,
            data: Some(value),
            ..
        } => Ok(value),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected routing policy disposition response: {other:?}"
        )),
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

async fn ipc_get_toolset_profile(socket: &str, profile_name: &str) -> Result<Option<Value>> {
    let mut client = connect_management_client(socket, "philotic-web-toolsets").await?;
    match client
        .send_request(IpcRequest::GetToolsetProfile {
            profile_name: profile_name.to_string(),
        })
        .await?
    {
        IpcResponse::Standard { ok: true, data, .. } => Ok(data),
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected toolset_profile response: {other:?}")),
    }
}

async fn ipc_list_toolset_profiles(socket: &str) -> Result<Vec<Value>> {
    let mut client = connect_management_client(socket, "philotic-web-toolsets").await?;
    match client
        .send_request(IpcRequest::ListToolsetProfiles {})
        .await?
    {
        IpcResponse::Standard {
            ok: true,
            data: Some(d),
            ..
        } => Ok(d.as_array().cloned().unwrap_or_default()),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected toolset_profiles response: {other:?}")),
    }
}

async fn ipc_list_cron_jobs(socket: &str) -> Result<Vec<Value>> {
    let mut client = connect_management_client(socket, "philotic-web-cron").await?;
    match client.send_request(IpcRequest::ListCronJobs).await? {
        IpcResponse::CronJobList { jobs } => Ok(jobs
            .into_iter()
            .map(|j| serde_json::to_value(j).unwrap_or(serde_json::Value::Null))
            .collect()),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected cron list response: {other:?}")),
    }
}

async fn ipc_register_cron_job(socket: &str, job: CronJob) -> Result<()> {
    let mut client = connect_management_client(socket, "philotic-web-cron").await?;
    match client
        .send_request(IpcRequest::RegisterCronJob { job })
        .await?
    {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        IpcResponse::Error(msg) => Err(anyhow!(msg)),
        other => Err(anyhow!("unexpected register_cron response: {other:?}")),
    }
}

async fn ipc_remove_cron_job(socket: &str, job_id: &str) -> Result<()> {
    let mut client = connect_management_client(socket, "philotic-web-cron").await?;
    match client
        .send_request(IpcRequest::RemoveCronJob {
            job_id: job_id.to_string(),
        })
        .await?
    {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        IpcResponse::Error(msg) => Err(anyhow!(msg)),
        other => Err(anyhow!("unexpected remove_cron response: {other:?}")),
    }
}

async fn ipc_set_cron_enabled(socket: &str, job_id: &str, enabled: bool) -> Result<()> {
    let req = if enabled {
        IpcRequest::EnableCronJob {
            job_id: job_id.to_string(),
        }
    } else {
        IpcRequest::DisableCronJob {
            job_id: job_id.to_string(),
        }
    };
    let mut client = connect_management_client(socket, "philotic-web-cron").await?;
    match client.send_request(req).await? {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        IpcResponse::Error(msg) => Err(anyhow!(msg)),
        other => Err(anyhow!(
            "unexpected cron enable/disable response: {other:?}"
        )),
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
        IpcResponse::ConfigData {
            value_json: Some(raw),
            ..
        } => {
            let parsed: Value = serde_json::from_str(&raw).unwrap_or_else(|_| Value::String(raw));
            Ok(Some(parsed))
        }
        IpcResponse::ConfigData {
            value_json: None, ..
        } => Ok(None),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected config data response: {other:?}")),
    }
}

async fn ipc_set_config(socket: &str, key: &str, value_json: &str) -> Result<()> {
    let mut client = connect_management_client(socket, "philotic-web-config-write").await?;
    match client
        .send_request(IpcRequest::SetConfig {
            key: key.to_string(),
            value_json: value_json.to_string(),
        })
        .await?
    {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected set_config response: {other:?}")),
    }
}

async fn ipc_rotate_secret(socket: &str, secret_ref: &str, plaintext: &str) -> Result<()> {
    let mut client = connect_management_client(socket, "philotic-web-vault-write").await?;
    match client
        .send_request(IpcRequest::RotateSecret {
            secret_ref: secret_ref.to_string(),
            plaintext: plaintext.to_string(),
        })
        .await?
    {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected rotate_secret response: {other:?}")),
    }
}

async fn ipc_add_vault_entry(
    socket: &str,
    vault_name: &str,
    plaintext: &str,
    allowed_roles: Vec<String>,
) -> Result<String> {
    let mut client = connect_management_client(socket, "philotic-web-vault-write").await?;
    match client
        .send_request(IpcRequest::AddVaultEntry {
            vault_name: vault_name.to_string(),
            plaintext: plaintext.to_string(),
            allowed_roles,
        })
        .await?
    {
        IpcResponse::Standard { ok: true, data, .. } => {
            let secret_ref = data
                .as_ref()
                .and_then(|d| d.get("secret_ref"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            Ok(secret_ref)
        }
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected add_vault_entry response: {other:?}")),
    }
}

async fn ipc_list_graph_instances(socket: &str) -> Result<Vec<Value>> {
    let mut client = connect_management_client(socket, "philotic-web-graphs").await?;
    match client
        .send_request(IpcRequest::ListGraphInstances {})
        .await?
    {
        IpcResponse::GraphInstanceList { instances } => Ok(instances),
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected list_graph_instances response: {other:?}"
        )),
    }
}

async fn ipc_assign_skill(
    socket: &str,
    agent_id: &str,
    role_name: &str,
    skill_name: &str,
) -> Result<Value> {
    let mut client = connect_management_client(socket, "philotic-web-skills-mgmt").await?;
    match client
        .send_request(IpcRequest::AssignSkill {
            agent_id: agent_id.to_string(),
            role_name: role_name.to_string(),
            skill_name: skill_name.to_string(),
        })
        .await?
    {
        IpcResponse::SkillAssigned {
            role_name,
            skill_name,
            operation,
        } => Ok(
            json!({"ok": true, "role_name": role_name, "skill_name": skill_name, "operation": operation}),
        ),
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected assign_skill response: {other:?}")),
    }
}

async fn ipc_revoke_skill(
    socket: &str,
    agent_id: &str,
    role_name: &str,
    skill_name: &str,
) -> Result<Value> {
    let mut client = connect_management_client(socket, "philotic-web-skills-mgmt").await?;
    match client
        .send_request(IpcRequest::RevokeSkill {
            agent_id: agent_id.to_string(),
            role_name: role_name.to_string(),
            skill_name: skill_name.to_string(),
        })
        .await?
    {
        IpcResponse::SkillAssigned {
            role_name,
            skill_name,
            operation,
        } => Ok(
            json!({"ok": true, "role_name": role_name, "skill_name": skill_name, "operation": operation}),
        ),
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected revoke_skill response: {other:?}")),
    }
}

async fn ipc_list_components(socket: &str) -> Result<Vec<ComponentInventoryEntry>> {
    let mut client = connect_management_client(socket, "philotic-web-components").await?;
    match client.send_request(IpcRequest::ListComponents {}).await? {
        IpcResponse::ComponentInventory { components } => components
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<ComponentInventoryEntry>, _>>()
            .map_err(Into::into),
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected list_components response: {other:?}")),
    }
}

async fn ipc_get_component(socket: &str, guest_id: &str) -> Result<ComponentInventoryEntry> {
    let components = ipc_list_components(socket).await?;
    components
        .into_iter()
        .find(|component| component.guest_id == guest_id)
        .ok_or_else(|| anyhow!("component not found"))
}

async fn ipc_register_component(
    socket: &str,
    manifest: ComponentManifest,
) -> Result<ComponentInventoryEntry> {
    let guest_id = manifest.guest_id.clone();
    let mut client = connect_management_client(socket, "philotic-web-components").await?;
    match client
        .send_request(IpcRequest::RegisterComponent { manifest })
        .await?
    {
        IpcResponse::ComponentRegistered { .. } => ipc_get_component(socket, &guest_id).await,
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected register_component response: {other:?}")),
    }
}

async fn ipc_set_component_active(socket: &str, guest_id: &str, active: bool) -> Result<()> {
    let mut client = connect_management_client(socket, "philotic-web-components").await?;
    match client
        .send_request(IpcRequest::SetComponentActive {
            guest_id: guest_id.to_string(),
            active,
        })
        .await?
    {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected set_component_active response: {other:?}"
        )),
    }
}

async fn ipc_restart_component(socket: &str, guest_id: &str) -> Result<()> {
    let mut client = connect_management_client(socket, "philotic-web-components").await?;
    match client
        .send_request(IpcRequest::RestartComponent {
            guest_id: guest_id.to_string(),
        })
        .await?
    {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected restart_component response: {other:?}")),
    }
}

async fn ipc_remove_component(socket: &str, guest_id: &str) -> Result<()> {
    let mut client = connect_management_client(socket, "philotic-web-components").await?;
    match client
        .send_request(IpcRequest::RemoveComponent {
            guest_id: guest_id.to_string(),
        })
        .await?
    {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected remove_component response: {other:?}")),
    }
}

fn component_templates() -> Vec<ComponentTemplateView> {
    vec![
        ComponentTemplateView {
            id: "membrane-telegram".into(),
            label: "Telegram Provider".into(),
            description: "Telegram ingress/egress provider. System socket and node env vars are hotel-owned; operator-authored fields should stay focused on agent targeting and token/config references.".into(),
            command: "membrane-telegram".into(),
            role: "membrane".into(),
            env_fields: vec![
                ComponentTemplateFieldView {
                    key: "PHILOTIC_TARGET_AGENT_ID".into(),
                    label: "Target Agent ID".into(),
                    target: "env".into(),
                    input_kind: "string".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    placeholder: Some("agent-bjork-01".into()),
                    help: Some("Use for a single-agent membrane instance. Hotel-seeded shared membranes may instead rely on PHILOTIC_AGENT_ROSTER.".into()),
                    default_value: None,
                },
                ComponentTemplateFieldView {
                    key: "PHILOTIC_TELEGRAM_BOT_TOKEN_KEY".into(),
                    label: "Telegram Token Config Key".into(),
                    target: "env".into(),
                    input_kind: "string".into(),
                    required: true,
                    secret: true,
                    vault_only: true,
                    placeholder: Some("telegram_bot_token".into()),
                    help: Some("Name of the hotel config key that resolves the bot token. Store the actual token in the vault/config surface, not in this component manifest.".into()),
                    default_value: Some(json!("telegram_bot_token")),
                },
                ComponentTemplateFieldView {
                    key: "PHILOTIC_TELEGRAM_API_BASE_URL".into(),
                    label: "Telegram API Base URL".into(),
                    target: "env".into(),
                    input_kind: "string".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    placeholder: Some("https://api.telegram.org".into()),
                    help: Some("Optional override for alternate Telegram API endpoints or local testing.".into()),
                    default_value: None,
                },
                ComponentTemplateFieldView {
                    key: "PHILOTIC_TELEGRAM_FILE_API_BASE_URL".into(),
                    label: "Telegram File API Base URL".into(),
                    target: "env".into(),
                    input_kind: "string".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    placeholder: Some("https://api.telegram.org".into()),
                    help: Some("Optional override for file downloads when the API base differs.".into()),
                    default_value: None,
                },
            ],
            component_config_fields: vec![],
            dependencies: vec![
                ComponentTemplateDependencyView {
                    key: "telegram_bot_token".into(),
                    label: "Telegram Bot Token".into(),
                    location: "hotel-config".into(),
                    required: true,
                    secret: true,
                    vault_only: true,
                    help: "Create or rotate this through the vault/config surface. The component should reference the config key, not store the token directly.".into(),
                },
            ],
        },
        ComponentTemplateView {
            id: "membrane-discord".into(),
            label: "Discord Provider".into(),
            description: "Discord provider membrane. Guest identity, hotel socket, and node id are hotel-owned; the operator should provide the target agent plus Discord registration/config references.".into(),
            command: "membrane-discord".into(),
            role: "membrane".into(),
            env_fields: vec![
                ComponentTemplateFieldView {
                    key: "PHILOTIC_TARGET_AGENT_ID".into(),
                    label: "Target Agent ID".into(),
                    target: "env".into(),
                    input_kind: "string".into(),
                    required: true,
                    secret: false,
                    vault_only: false,
                    placeholder: Some("agent-01".into()),
                    help: Some("Agent served by this Discord provider instance.".into()),
                    default_value: None,
                },
                ComponentTemplateFieldView {
                    key: "DISCORD_BOT_TOKEN_KEY".into(),
                    label: "Discord Token Config Key".into(),
                    target: "env".into(),
                    input_kind: "string".into(),
                    required: true,
                    secret: true,
                    vault_only: true,
                    placeholder: Some("discord_bot_token".into()),
                    help: Some("Hotel config key used to resolve the Discord bot token. Keep the actual token in vault/config, not here.".into()),
                    default_value: Some(json!("discord_bot_token")),
                },
                ComponentTemplateFieldView {
                    key: "DISCORD_APPLICATION_ID".into(),
                    label: "Discord Application ID".into(),
                    target: "env".into(),
                    input_kind: "string".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    placeholder: Some("123456789012345678".into()),
                    help: Some("Needed for slash command registration and application-scoped Discord features.".into()),
                    default_value: None,
                },
            ],
            component_config_fields: vec![],
            dependencies: vec![
                ComponentTemplateDependencyView {
                    key: "discord_bot_token".into(),
                    label: "Discord Bot Token".into(),
                    location: "hotel-config".into(),
                    required: true,
                    secret: true,
                    vault_only: true,
                    help: "Store the bot token in the vault/config surface and reference it by config key from the component.".into(),
                },
            ],
        },
        ComponentTemplateView {
            id: "model-controller-gemini".into(),
            label: "Gemini Model Controller".into(),
            description: "Gemini-backed model controller. The component itself is usually system-light; most meaningful configuration lives in hotel config and secret refs.".into(),
            command: "model-controller-gemini".into(),
            role: "model".into(),
            env_fields: vec![
                ComponentTemplateFieldView {
                    key: "PHILOTIC_GEMINI_BASE_URL".into(),
                    label: "Gemini Base URL Override".into(),
                    target: "env".into(),
                    input_kind: "string".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    placeholder: Some("https://generativelanguage.googleapis.com".into()),
                    help: Some("Optional per-component override; hotel config usually owns this.".into()),
                    default_value: None,
                },
                ComponentTemplateFieldView {
                    key: "PHILOTIC_GEMINI_OAUTH_ACCESS_TOKEN_REF".into(),
                    label: "Gemini OAuth Access Token Ref".into(),
                    target: "env".into(),
                    input_kind: "secret_ref".into(),
                    required: false,
                    secret: true,
                    vault_only: true,
                    placeholder: Some("secret://hotel/default/gemini/oauth-access".into()),
                    help: Some("Optional env override for OAuth token resolution. Prefer secret refs over raw tokens.".into()),
                    default_value: None,
                },
                ComponentTemplateFieldView {
                    key: "PHILOTIC_GEMINI_OAUTH_PROJECT_ID".into(),
                    label: "Gemini OAuth Project ID".into(),
                    target: "env".into(),
                    input_kind: "string".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    placeholder: Some("my-gcp-project".into()),
                    help: Some("Optional OAuth project override.".into()),
                    default_value: None,
                },
            ],
            component_config_fields: vec![],
            dependencies: vec![
                ComponentTemplateDependencyView {
                    key: "gemini_api_key_ref".into(),
                    label: "Gemini API Key Ref".into(),
                    location: "hotel-config".into(),
                    required: false,
                    secret: true,
                    vault_only: true,
                    help: "Use a secret_ref-backed hotel config entry for API-key auth.".into(),
                },
                ComponentTemplateDependencyView {
                    key: "gemini_oauth_access_token_ref".into(),
                    label: "Gemini OAuth Access Token Ref".into(),
                    location: "hotel-config".into(),
                    required: false,
                    secret: true,
                    vault_only: true,
                    help: "Preferred for refreshable OAuth auth. Store the token in vault and place the secret_ref in hotel config.".into(),
                },
                ComponentTemplateDependencyView {
                    key: "gemini_oauth_project_id".into(),
                    label: "Gemini OAuth Project ID".into(),
                    location: "hotel-config".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    help: "Companion config when OAuth auth is used.".into(),
                },
            ],
        },
        ComponentTemplateView {
            id: "model-controller-elevenlabs".into(),
            label: "ElevenLabs Voice Controller".into(),
            description: "ElevenLabs-backed model controller. The API key should always arrive via secret ref rather than plaintext manifest fields.".into(),
            command: "model-controller-elevenlabs".into(),
            role: "model.elevenlabs".into(),
            env_fields: vec![ComponentTemplateFieldView {
                key: "PHILOTIC_MODEL_CONTROLLER_INLINE_AUDIO".into(),
                label: "Allow Inline Audio".into(),
                target: "env".into(),
                input_kind: "boolean".into(),
                required: false,
                secret: false,
                vault_only: false,
                placeholder: None,
                help: Some("Enable inline PCM audio inputs when the transport supports them.".into()),
                default_value: Some(json!(false)),
            }],
            component_config_fields: vec![],
            dependencies: vec![
                ComponentTemplateDependencyView {
                    key: "elevenlabs_api_key_ref".into(),
                    label: "ElevenLabs API Key Ref".into(),
                    location: "hotel-config".into(),
                    required: true,
                    secret: true,
                    vault_only: true,
                    help: "Store the ElevenLabs key in vault and place the secret_ref in hotel config.".into(),
                },
                ComponentTemplateDependencyView {
                    key: "elevenlabs_voice_id".into(),
                    label: "Default Voice ID".into(),
                    location: "hotel-config".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    help: "Optional default voice selection for synthesis tasks.".into(),
                },
            ],
        },
        ComponentTemplateView {
            id: "model-controller-mlx".into(),
            label: "MLX Local Model Controller".into(),
            description: "Local MLX-backed controller. The important operator-owned shape lives in component_config, especially the model fleet definition.".into(),
            command: "model-controller-mlx".into(),
            role: "model.mlx".into(),
            env_fields: vec![
                ComponentTemplateFieldView {
                    key: "PHILOTIC_MLX_CONFIG".into(),
                    label: "Fleet Config File Path".into(),
                    target: "env".into(),
                    input_kind: "string".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    placeholder: Some("/Users/jaredlikes/.philotic/mlx/fleet.json".into()),
                    help: Some("Optional file-path override. Prefer component_config for hotel-managed components so the manifest stays self-contained.".into()),
                    default_value: None,
                },
            ],
            component_config_fields: vec![
                ComponentTemplateFieldView {
                    key: "health_check_interval_secs".into(),
                    label: "Health Check Interval (secs)".into(),
                    target: "component_config".into(),
                    input_kind: "integer".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    placeholder: Some("300".into()),
                    help: Some("How often the MLX provider checks fleet health in the background.".into()),
                    default_value: Some(json!(300)),
                },
                ComponentTemplateFieldView {
                    key: "python_path".into(),
                    label: "Python Interpreter".into(),
                    target: "component_config".into(),
                    input_kind: "string".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    placeholder: Some("/opt/homebrew/bin/python3".into()),
                    help: Some("Required when any fleet model uses managed mode or transcription.".into()),
                    default_value: None,
                },
                ComponentTemplateFieldView {
                    key: "models".into(),
                    label: "MLX Models JSON".into(),
                    target: "component_config".into(),
                    input_kind: "mlx_models".into(),
                    required: true,
                    secret: false,
                    vault_only: false,
                    placeholder: Some(
                        "[\n  {\n    \"class\": \"text\",\n    \"repo_id\": \"mlx-community/Qwen2.5-7B-Instruct-4bit\",\n    \"mode\": \"attached\",\n    \"host\": \"127.0.0.1\",\n    \"port\": 8091,\n    \"priority\": 100,\n    \"extra_args\": [],\n    \"server_variant\": \"mlx_lm\"\n  }\n]".into(),
                    ),
                    help: Some("Fleet definition array for MLX models. This is the real shape the controller consumes, so the structured field stays JSON rather than pretending it is a few flat strings.".into()),
                    default_value: Some(json!([])),
                },
            ],
            dependencies: vec![],
        },
        ComponentTemplateView {
            id: "model-controller-onnx".into(),
            label: "ONNX Local Model Controller".into(),
            description: "Local ONNX-backed controller with an Ollama-compatible sidecar. These knobs are mostly env-driven and do not require raw secret entry.".into(),
            command: "model-controller-onnx".into(),
            role: "model.local".into(),
            env_fields: vec![
                ComponentTemplateFieldView {
                    key: "PHILOTIC_ONNX_SIDECAR_ONLY".into(),
                    label: "Sidecar Only".into(),
                    target: "env".into(),
                    input_kind: "boolean".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    placeholder: None,
                    help: Some("Skip hotel IPC registration and only serve the local HTTP sidecar.".into()),
                    default_value: Some(json!(false)),
                },
                ComponentTemplateFieldView {
                    key: "PHILOTIC_ONNX_SIDECAR_ADDR".into(),
                    label: "Sidecar Address".into(),
                    target: "env".into(),
                    input_kind: "string".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    placeholder: Some("127.0.0.1:11435".into()),
                    help: Some("Bind address for the Ollama-compatible sidecar.".into()),
                    default_value: Some(json!("127.0.0.1:11435")),
                },
                ComponentTemplateFieldView {
                    key: "PHILOTIC_ONNX_EMBED_REPO".into(),
                    label: "Embedding Repo".into(),
                    target: "env".into(),
                    input_kind: "string".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    placeholder: Some("onnx-community/embeddinggemma-300m-ONNX".into()),
                    help: Some("Hugging Face repo id for the embedding model.".into()),
                    default_value: Some(json!("onnx-community/embeddinggemma-300m-ONNX")),
                },
                ComponentTemplateFieldView {
                    key: "PHILOTIC_ONNX_WHISPER_REPO".into(),
                    label: "Whisper Repo".into(),
                    target: "env".into(),
                    input_kind: "string".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    placeholder: Some("onnx-community/whisper-small".into()),
                    help: Some("Hugging Face repo id for the transcription model.".into()),
                    default_value: Some(json!("onnx-community/whisper-small")),
                },
                ComponentTemplateFieldView {
                    key: "PHILOTIC_ONNX_PREFER_QUANTIZED".into(),
                    label: "Prefer Quantized Models".into(),
                    target: "env".into(),
                    input_kind: "boolean".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    placeholder: None,
                    help: Some("Use quantized ONNX variants when available.".into()),
                    default_value: Some(json!(true)),
                },
            ],
            component_config_fields: vec![],
            dependencies: vec![],
        },
        ComponentTemplateView {
            id: "agent-graph-runner".into(),
            label: "Agent Graph Runner".into(),
            description: "Per-agent cognitive graph guest. The agent id is required; the graph DB path is optional unless you want an explicit storage location.".into(),
            command: "agent-graph-runner".into(),
            role: "agent-graph".into(),
            env_fields: vec![
                ComponentTemplateFieldView {
                    key: "PHILOTIC_AGENT_ID".into(),
                    label: "Owning Agent ID".into(),
                    target: "env".into(),
                    input_kind: "string".into(),
                    required: true,
                    secret: false,
                    vault_only: false,
                    placeholder: Some("agent-jane-01".into()),
                    help: Some("Agent whose cognitive graph this guest serves.".into()),
                    default_value: None,
                },
                ComponentTemplateFieldView {
                    key: "PHILOTIC_GRAPH_RUNNER_ID".into(),
                    label: "Agent Graph Guest ID".into(),
                    target: "env".into(),
                    input_kind: "string".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    placeholder: Some("agent-graph-agent-jane-01".into()),
                    help: Some("Optional override for the guest id used during IPC registration.".into()),
                    default_value: None,
                },
                ComponentTemplateFieldView {
                    key: "PHILOTIC_AGENT_GRAPH_DB".into(),
                    label: "Agent Graph DB Path".into(),
                    target: "env".into(),
                    input_kind: "string".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    placeholder: Some("~/.philotic/agent-graph-agent-jane-01.db".into()),
                    help: Some("Optional explicit SQLite path for the per-agent graph store.".into()),
                    default_value: None,
                },
            ],
            component_config_fields: vec![],
            dependencies: vec![],
        },
        ComponentTemplateView {
            id: "tool-runner".into(),
            label: "Tool Runner".into(),
            description: "Generic tool execution guest. Most deployments only need the canonical hotel socket and node id, which the hotel should provide.".into(),
            command: "tool-runner".into(),
            role: "tool".into(),
            env_fields: vec![],
            component_config_fields: vec![],
            dependencies: vec![],
        },
        ComponentTemplateView {
            id: "graph-runner".into(),
            label: "Graph Runner".into(),
            description: "Graph query/tool guest. The hotel usually injects the graph runner id automatically; operator-authored config is minimal.".into(),
            command: "graph-runner".into(),
            role: "tool.graph".into(),
            env_fields: vec![
                ComponentTemplateFieldView {
                    key: "PHILOTIC_GRAPH_RUNNER_ID".into(),
                    label: "Graph Runner ID".into(),
                    target: "env".into(),
                    input_kind: "string".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    placeholder: Some("local-telegram:graph-runner".into()),
                    help: Some("Optional explicit runner id when not hotel-seeded.".into()),
                    default_value: None,
                },
            ],
            component_config_fields: vec![],
            dependencies: vec![],
        },
    ]
}

fn new_operator_chat_id(prefix: &str) -> String {
    let mut rng = rand::thread_rng();
    let suffix = format!("{:016x}", rng.r#gen::<u64>());
    format!("{prefix}-{suffix}")
}

async fn connect_management_client(socket: &str, guest_id: &str) -> Result<PhiloticClient> {
    connect_client_with_identity(
        socket,
        GuestIdentity {
            guest_id: guest_id.into(),
            role: "management".into(),
            supported_tools: vec![],
        },
    )
    .await
}

async fn connect_client_with_identity(
    socket: &str,
    identity: GuestIdentity,
) -> Result<PhiloticClient> {
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
