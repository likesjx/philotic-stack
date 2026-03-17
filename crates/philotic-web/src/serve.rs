//! `philotic-web serve` — local management HTTP + WebSocket server.
//!
//! Generates a random session token on startup, prints it, and requires it
//! on every request as `Authorization: Bearer <token>`.
//!
//! REST endpoints:
//!   GET  /api/status
//!   GET  /api/guests
//!   GET  /api/agents
//!   GET  /api/sessions    (stub — returns [] until session table exists)
//!   GET  /api/apartments/:agent_id
//!   POST /api/guests/:guest_id/restart
//!   POST /api/guests/:guest_id/stop
//!
//! WebSocket:
//!   GET  /ws?token=<token>  — live push of guest/session state changes

use anyhow::Result;
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
    Router,
};
use rand::Rng;
use rust_embed::RustEmbed;
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::Arc,
};
use tokio::sync::broadcast;
use tower_http::cors::{AllowOrigin, CorsLayer};

// ── Embedded UI assets ────────────────────────────────────────────────────────

#[derive(RustEmbed)]
#[folder = "ui-dist/"]
struct UiAssets;

// ── State shared across request handlers ─────────────────────────────────────

#[derive(Clone)]
struct AppState {
    token: Arc<String>,
    port: u16,
    db_path: Arc<PathBuf>,
    config_path: Arc<PathBuf>,
    /// Broadcast channel for WebSocket push events
    tx: broadcast::Sender<String>,
}

// ── Public entry point ────────────────────────────────────────────────────────

pub async fn run(
    port: u16,
    db: Option<PathBuf>,
    config: Option<PathBuf>,
    allow_origins: Option<String>,
) -> Result<()> {
    let db_path = db.unwrap_or_else(|| {
        match crate::init::active_profile() {
            Some(_) => crate::init::profile_dir().join("context.db"),
            None => PathBuf::from("aiua_context.db"),
        }
    });
    let config_path = config.unwrap_or_else(|| {
        match crate::init::active_profile() {
            Some(_) => crate::init::profile_dir().join("config.json"),
            None => PathBuf::from("mesh-config.json"),
        }
    });

    // Generate session token
    let token: String = {
        let bytes: [u8; 24] = rand::thread_rng().gen();
        format!("philotic-{}", hex::encode(bytes))
    };

    // Broadcast channel for WebSocket events (capacity 256)
    let (tx, _) = broadcast::channel::<String>(256);

    let state = AppState {
        token: Arc::new(token.clone()),
        port,
        db_path: Arc::new(db_path),
        config_path: Arc::new(config_path),
        tx,
    };

    // CORS — localhost only; UI is embedded and served from the same origin
    let cors = build_cors(allow_origins.as_deref());

    let app = Router::new()
        // API routes
        .route("/api/status",                      get(handle_status))
        .route("/api/guests",                      get(handle_guests))
        .route("/api/agents",                      get(handle_agents))
        .route("/api/sessions",                    get(handle_sessions))
        .route("/api/apartments/:agent_id",        get(handle_apartment))
        .route("/api/guests/:guest_id/restart",    post(handle_guest_restart))
        .route("/api/guests/:guest_id/stop",       post(handle_guest_stop))
        .route("/ws",                              get(handle_ws))
        // Embedded UI — index.html gets token injected; all other assets served as-is
        .route("/",                                get(handle_index))
        .fallback(get(handle_static))
        .layer(cors)
        .with_state(state);

    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    println!("philotic-web serve");
    println!("──────────────────────────────────────────");
    println!("  http://127.0.0.1:{port}");
    println!();
    println!("  Press Ctrl-C to stop.");

    // Auto-open the embedded desktop in the default browser
    let _ = tokio::process::Command::new("open")
        .arg(format!("http://127.0.0.1:{port}"))
        .spawn();

    axum::serve(listener, app).await?;
    Ok(())
}

// ── Embedded UI handlers ──────────────────────────────────────────────────────

/// Serve `index.html` with the session token injected as a JS global.
/// The desktop reads `window.__PHILOTIC_AIUA_TOKEN__` on startup and
/// auto-connects — the token is never visible in the URL or browser history.
async fn handle_index(State(state): State<AppState>) -> Response {
    let html = match UiAssets::get("index.html") {
        Some(f) => String::from_utf8_lossy(f.data.as_ref()).into_owned(),
        None => return (StatusCode::NOT_FOUND, "UI not built").into_response(),
    };

    let injection = format!(
        "<script>window.__PHILOTIC_AIUA_TOKEN__={:?};window.__PHILOTIC_AIUA_URL__={:?};</script>",
        state.token.as_str(),
        format!("http://127.0.0.1:{}", state.port),
    );

    let html = html.replace("</head>", &format!("{injection}</head>"));

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

/// Serve any other embedded asset (JS, CSS, icons, etc.).
/// Falls back to `index.html` for SPA client-side routes.
async fn handle_static(State(state): State<AppState>, uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');

    if let Some(asset) = UiAssets::get(path) {
        let mime = asset.metadata.mimetype();
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, mime)],
            asset.data.into_owned(),
        )
            .into_response()
    } else {
        // SPA fallback — let the client-side router handle it
        handle_index(State(state)).await
    }
}

// ── Auth helper ───────────────────────────────────────────────────────────────

fn check_auth(headers: &HeaderMap, state: &AppState) -> bool {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|t| t == state.token.as_str())
        .unwrap_or(false)
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, Json(json!({"error": "unauthorized"}))).into_response()
}

// ── GET /api/status ───────────────────────────────────────────────────────────

async fn handle_status(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    let daemon_alive = crate::start::read_pid()
        .map(|pid| crate::start::process_alive(pid))
        .unwrap_or(false);

    let hotel = read_hotel_name(&state.config_path);

    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "hotel": hotel,
        "daemon": if daemon_alive { "running" } else { "stopped" },
        "db": state.db_path.display().to_string(),
    }))
    .into_response()
}

// ── GET /api/guests ───────────────────────────────────────────────────────────

async fn handle_guests(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    match query_guests(&state.db_path) {
        Ok(guests) => Json(guests).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

fn query_guests(db_path: &PathBuf) -> Result<Vec<Value>> {
    let conn = Connection::open(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT guest_id, role, is_active, last_seen, active_pid FROM materialized_guests ORDER BY last_seen DESC"
    )?;

    let rows = stmt.query_map([], |row| {
        let guest_id: String = row.get(0)?;
        let role: String = row.get(1)?;
        let is_active: bool = row.get(2)?;
        let last_seen: Option<String> = row.get(3)?;
        let active_pid: Option<String> = row.get(4)?;

        // Check if process is actually alive
        let pid_alive = active_pid
            .as_ref()
            .and_then(|p| p.parse::<u32>().ok())
            .map(|pid| crate::start::process_alive(pid))
            .unwrap_or(false);

        let status = if is_active && pid_alive {
            "running"
        } else if is_active {
            "stopped"
        } else {
            "inactive"
        };

        Ok(json!({
            "guest_id": guest_id,
            "name": role_to_name(&role),
            "role": role,
            "pid": active_pid,
            "status": status,
            "last_seen": last_seen,
        }))
    })?;

    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn role_to_name(role: &str) -> String {
    // Convert "model.gemini" → "Gemini", "agent.jane" → "Jane", etc.
    role.split('.')
        .last()
        .map(|s| {
            let mut c = s.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().to_string() + c.as_str(),
            }
        })
        .unwrap_or_else(|| role.to_string())
}

// ── GET /api/agents ───────────────────────────────────────────────────────────

async fn handle_agents(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    let agents = query_agents(&state.db_path, &state.config_path);
    Json(agents).into_response()
}

fn query_agents(db_path: &PathBuf, config_path: &PathBuf) -> Vec<Value> {
    // Start with agent_identities from SQLite
    let mut agents: Vec<Value> = Vec::new();

    if let Ok(conn) = Connection::open(db_path) {
        if let Ok(mut stmt) = conn.prepare(
            "SELECT agent_id, persona_name, bundle_json FROM agent_identities ORDER BY agent_id"
        ) {
            if let Ok(rows) = stmt.query_map([], |row| {
                let agent_id: String = row.get(0)?;
                let persona_name: String = row.get(1)?;
                let bundle_json: String = row.get(2)?;
                Ok((agent_id, persona_name, bundle_json))
            }) {
                for row in rows.flatten() {
                    let (agent_id, persona_name, bundle_json) = row;
                    let bundle: Value = serde_json::from_str(&bundle_json).unwrap_or(Value::Null);
                    agents.push(json!({
                        "agent_id": agent_id,
                        "persona_name": persona_name,
                        "system_prompt": bundle.get("system_prompt"),
                        "toolset_tags": bundle.get("toolset_tags").and_then(|v| v.as_array()).cloned().unwrap_or_default(),
                        "active_session": false,  // TODO: join with sessions when table exists
                    }));
                }
            }
        }
    }

    // Supplement with mesh-config.json for any agents not yet in the DB
    if let Ok(raw) = std::fs::read_to_string(config_path) {
        if let Ok(cfg) = serde_json::from_str::<Value>(&raw) {
            if let Some(hotels) = cfg.get("hotels").and_then(|h| h.as_object()) {
                for (_hotel, hotel_val) in hotels {
                    if let Some(hotel_agents) =
                        hotel_val.get("agents").and_then(|a| a.as_object())
                    {
                        for (key, agent) in hotel_agents {
                            let agent_id = agent
                                .get("agent_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or(key.as_str())
                                .to_string();

                            // Skip if already in DB results
                            if agents.iter().any(|a| a["agent_id"] == agent_id) {
                                continue;
                            }

                            agents.push(json!({
                                "agent_id": agent_id,
                                "persona_name": agent.get("persona_name"),
                                "system_prompt": agent.get("system_prompt"),
                                "toolset_tags": agent.get("toolset_tags").and_then(|v| v.as_array()).cloned().unwrap_or_default(),
                                "active_session": false,
                            }));
                        }
                    }
                }
            }
        }
    }

    agents
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
        Ok(None) => (StatusCode::NOT_FOUND, Json(json!({"error": "no apartment data"}))).into_response(),
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
    match ipc_guest_action(&guest_id, "restart").await {
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

    match ipc_guest_action(&guest_id, "stop").await {
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

async fn ipc_guest_action(guest_id: &str, action: &str) -> Result<()> {
    use philotic_client::{GuestIdentity, IpcRequest, PhiloticClient};

    let socket = crate::start::socket_path("aiua");

    let identity = GuestIdentity {
        guest_id: "philotic-web-mgmt".into(),
        role: "management".into(),
        supported_tools: vec![],
    };
    let mut client = PhiloticClient::connect_at(&socket, identity).await?;

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

// ── WebSocket /ws ─────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct WsQuery {
    token: Option<String>,
}

async fn handle_ws(
    ws: WebSocketUpgrade,
    Query(params): Query<WsQuery>,
    State(state): State<AppState>,
) -> Response {
    // Auth via query param (browsers can't set headers on WS upgrade)
    let authed = params
        .token
        .as_deref()
        .map(|t| t == state.token.as_str())
        .unwrap_or(false);

    if !authed {
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

    let origins: Vec<axum::http::HeaderValue> = allow_origins
        .unwrap_or("http://localhost:5173")
        .split(',')
        .filter_map(|o| o.trim().parse().ok())
        .collect();

    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
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
