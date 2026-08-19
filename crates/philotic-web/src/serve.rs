//! `philotic-web serve` — local management HTTP + WebSocket server.
//!
//! Generates a hotel-issued bootstrap token on startup and requires an
//! explicit bootstrap/login ceremony before issuing a bounded same-origin
//! operator session cookie for the embedded UI.
//!
//! Environment / config knobs:
//!   PHILOTIC_WEB_BIND        (or config `web_bind`)        — bind host IP, default 127.0.0.1.
//!                            Set to a Tailscale interface IP to expose to the tailnet.
//!   PHILOTIC_WEB_EDGE_TOKEN  (or config `web_edge_token`)  — bearer token for device/edge
//!                            clients (constant-time compared) accepted alongside operator
//!                            session cookies.
//!   PHILOTIC_WEB_EDGE_INVITE (or config `web_edge_invite`) — operator-issued invite code
//!                            gating POST /api/edge/enroll; unset = enrollment disabled.
//!   PHILOTIC_WEB_ROSTER      (or config `web_roster`)      — JSON mesh roster fallback for
//!                            GET /api/mesh/roster when the aiua GetMeshRoster IPC is
//!                            unreachable; also supplies philotic-web base URLs per node.
//!
//! When the bind is non-loopback, every /api route and /ws require a valid operator
//! session (cookie or bearer session token) or the edge bearer token. GET /health stays
//! unauthenticated on every tier (client latency probes).
//!
//! REST endpoints:
//!   GET  /health   (unauthenticated — node_id + version for latency probes)
//!   POST /api/edge/enroll   (invite-code gated — edge device enrollment)
//!   GET  /api/edge/ws       (edge bearer token — edge-protocol WebSocket, see serve/edge.rs)
//!   GET  /api/mesh/roster
//!   GET  /api/auth/status
//!   POST /api/auth/bootstrap
//!   POST /api/auth/challenges
//!   POST /api/auth/oidc/start
//!   POST /api/auth/logout
//!   GET  /auth/oidc/:provider/callback
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
//!   GET  /api/mesh/targets/:target_node_id/components
//!   GET  /api/mesh/targets/:target_node_id/components/:guest_id
//!   POST /api/mesh/targets/:target_node_id/components
//!   PATCH /api/mesh/targets/:target_node_id/components/:guest_id
//!   DELETE /api/mesh/targets/:target_node_id/components/:guest_id
//!   POST /api/mesh/targets/:target_node_id/components/:guest_id/enable
//!   POST /api/mesh/targets/:target_node_id/components/:guest_id/disable
//!   POST /api/mesh/targets/:target_node_id/components/:guest_id/restart
//!   GET  /api/mesh/targets/:target_node_id/config
//!   PUT  /api/mesh/targets/:target_node_id/config/:key
//!   GET  /api/mesh/targets/:target_node_id/secrets
//!   POST /api/mesh/targets/:target_node_id/secrets/rotate
//!   POST /api/mesh/targets/:target_node_id/vault
//!   GET  /api/mesh/targets/:target_node_id/best-place-to-run
//!   PUT  /api/mesh/targets/:target_node_id/agents/:agent_id/roles/:role_name/home
//!   POST /api/mesh/targets/:target_node_id/agents/:agent_id/chat
//!   GET  /api/event-log
//!   GET  /api/config
//!   GET  /api/config/telegram
//!   GET  /api/config/gemini
//!   GET  /api/model-catalog
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
//!   GET  /api/edge/sessions   (edge-bearer; hotel session history via ListOperatorSessions)
//!   GET  /api/edge/sessions/:session_id/turns   (edge-bearer; ListSessionTurns)
//!   GET  /api/edge/lifegraph/lens/:lens   (edge-bearer; life.recall named strategy)
//!   GET  /api/edge/lifegraph/node/:node_id   (edge-bearer; life.view.node)
//!   GET  /api/edge/lifegraph/neighborhood/:node_id   (edge-bearer; life.view.neighborhood)
//!   GET  /api/edge/memory/recall   (edge-bearer; memory.recall.structured — provenance-bearing)
//!   GET  /api/apartments/:agent_id   (disabled by default for the desktop membrane)
//!   POST /api/guests/:guest_id/restart
//!   POST /api/guests/:guest_id/stop
//!
//! WebSocket:
//!   GET  /ws  — live push of guest/session state changes
//!              auth via same-origin cookie
//!   GET  /api/edge/ws — edge-mesh protocol termination (philotic-edge-protocol
//!              JSON envelopes; per-device bearer auth; see serve/edge.rs)

pub(crate) mod edge;

use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::event::{EventEnvelope, EventKind, EventPayload};
use ansible_mesh_core::model_manager::{project_model_catalog, ModelCatalogProjection};
use ansible_mesh_core::provider_keys::{provider_key_spec, provider_key_specs, ProviderKeySpec};
use ansible_mesh_core::sqlite_storage::{SqliteEventStorage, SqliteGraphStorage};
use ansible_mesh_core::storage::{
    EventStorage, ProjectedExternalIdentityRecord, ProjectedUserIdentityRecord,
};
use anyhow::{anyhow, bail, Context, Result};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        ConnectInfo, Path, Query, Request, State,
    },
    http::{header, HeaderMap, HeaderName, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Json, Response},
    routing::{delete, get, post, put},
    Router,
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use perimeter_core::{classify_bind_addr, ExposureTier};
use rand::Rng;
use reqwest::Url;
use rusqlite::{Connection, OptionalExtension};
use rust_embed::RustEmbed;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{broadcast, watch, Mutex};
use tower_http::cors::{AllowOrigin, CorsLayer};

use philotic_client::{
    ComponentInventoryEntryView, ComponentManifest, CronJob, CronJobSource,
    DesktopMembraneAgentView, DesktopMembraneGuestView, DesktopMembraneStatusView, GuestIdentity,
    IpcRequest, IpcResponse, LeaseEnvelope, MeshRosterEntryView, OperatorSessionView,
    OperatorTargetAgentInventoryView, OperatorTargetComponentInventoryView,
    OperatorTargetComponentMutationAckView, OperatorTargetConfigMutationAckView,
    OperatorTargetConfigView, OperatorTargetGuestInventoryView, OperatorTargetPlacementView,
    OperatorTargetRoleHomeAckView, OperatorTargetSecretInventoryView,
    OperatorTargetSecretMutationAckView, OperatorTargetStatusView, OperatorTargetView,
    PhiloticClient, ResponseRoutePolicyView, SessionTurnView, OPERATOR_CHAT_REPLY_ROLE,
    OPERATOR_REMOTE_CONFIG_KEYS, OPERATOR_REMOTE_MUTABLE_CONFIG_KEYS,
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
pub(crate) struct AppState {
    bootstrap_token: Arc<String>,
    db_path: PathBuf,
    /// Mesh config path — consulted for `web_roster` / `web_edge_token` fallbacks
    config_path: Arc<PathBuf>,
    hotel: Arc<String>,
    /// IPC socket path for the connected hotel
    socket: Arc<String>,
    /// Broadcast channel for WebSocket push events
    tx: broadcast::Sender<String>,
    /// Bearer token for device/edge clients (PHILOTIC_WEB_EDGE_TOKEN / config `web_edge_token`)
    edge_token: Option<Arc<String>>,
    /// Exposure tier of the listener bind address (perimeter-core classification)
    exposure_tier: ExposureTier,
    /// Edge-tier state: enrolled-device registry + per-node outbound rings
    edge: edge::EdgeState,
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

#[derive(Clone, Debug, Default, serde::Deserialize)]
struct EventLogQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    source: Option<String>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
struct EventLogEntry {
    id: String,
    timestamp: u64,
    source: String,
    event_type: String,
    summary: String,
    details: Value,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
struct OperatorSessionRecord {
    session_id: String,
    session_token: String,
    user_id: String,
    display_name: String,
    issuing_hotel: String,
    surface_kind: String,
    posture: String,
    issued_at: i64,
    expires_at: i64,
    status: String,
    auth_method: String,
    bootstrap_id: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct BootstrapAuthBody {
    bootstrap_token: String,
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct OidcStartBody {
    provider: String,
    #[serde(default)]
    return_path: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct OidcCallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct OperatorAuthChallengeRecord {
    challenge_id: String,
    user_id: String,
    intended_surface: String,
    auth_path: String,
    verifier_kind: String,
    verifier_hint: Option<String>,
    bind_label: Option<String>,
    challenge_nonce: String,
    exchange_secret: Option<String>,
    issued_at: i64,
    expires_at: i64,
    status: String,
    consumed_at: Option<i64>,
}

#[derive(Debug, serde::Deserialize)]
struct CreateAuthChallengeBody {
    auth_path: String,
    verifier_kind: String,
    #[serde(default)]
    verifier_hint: Option<String>,
    #[serde(default)]
    bind_label: Option<String>,
}

#[derive(Debug, serde::Serialize)]
struct OperatorAuthChallengeView {
    challenge_id: String,
    user_id: String,
    intended_surface: String,
    auth_path: String,
    verifier_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    verifier_hint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bind_label: Option<String>,
    challenge_nonce: String,
    issued_at: i64,
    expires_at: i64,
    status: String,
}

#[derive(Debug, serde::Serialize)]
struct OidcStartView {
    provider: String,
    authorization_url: String,
    challenge_id: String,
    state: String,
    redirect_uri: String,
    expires_at: i64,
}

#[derive(Debug, serde::Serialize)]
struct AuthStatusView {
    authenticated: bool,
    hotel: String,
    #[serde(default)]
    oidc_providers: Vec<OidcProviderStatusView>,
    #[serde(default)]
    root_user_key_refs: Vec<RootUserKeyRefStatusView>,
    #[serde(default)]
    external_identity_links: Vec<ExternalIdentityLinkStatusView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<OperatorSessionStatusView>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct OperatorUserRecord {
    user_id: String,
    mesh_principal_id: String,
    display_name: String,
    preferred_name: Option<String>,
    primary_email: Option<String>,
    home_hotel: String,
    status: String,
    onboarding_state: String,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
struct OperatorUserSettingsView {
    user_id: String,
    mesh_principal_id: String,
    display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    preferred_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    primary_email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timezone: Option<String>,
    home_hotel: String,
    status: String,
    onboarding_state: String,
    #[serde(default)]
    external_identity_links: Vec<ExternalIdentityLinkStatusView>,
    #[serde(skip_serializing_if = "Option::is_none")]
    projected_identity: Option<ProjectedUserIdentityRecord>,
}

#[derive(Debug, serde::Deserialize)]
struct PatchOperatorUserBody {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    preferred_name: Option<String>,
    #[serde(default)]
    primary_email: Option<String>,
    #[serde(default)]
    timezone: Option<String>,
    #[serde(default)]
    onboarding_state: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
struct OidcProviderStatusView {
    provider: String,
    label: String,
    configured: bool,
    /// Omitted entirely for unauthenticated callers — see `handle_auth_status`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    callback_url: String,
}

#[derive(Debug, Clone)]
struct OidcProviderSettings {
    client_id: Option<String>,
    client_secret_ref: Option<String>,
}

#[derive(Debug, Clone)]
struct OidcResolvedConfig {
    public_base_url: String,
    public_base_source: String,
    google: OidcProviderSettings,
    github: OidcProviderSettings,
}

#[derive(Debug, serde::Serialize)]
struct OidcProviderConfigView {
    provider: String,
    label: String,
    client_id: Option<String>,
    client_id_source: String,
    secret_ref: Option<String>,
    secret_ref_source: String,
    secret_configured: bool,
    callback_url: String,
}

#[derive(Debug, serde::Serialize)]
struct OidcConfigView {
    public_base_url: String,
    public_base_source: String,
    providers: Vec<OidcProviderConfigView>,
}

#[derive(Debug, serde::Serialize)]
struct OperatorSessionStatusView {
    session_id: String,
    user_id: String,
    display_name: String,
    posture: String,
    issued_at: i64,
    expires_at: i64,
    auth_method: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct RootUserKeyRefStatusView {
    user_id: String,
    key_purpose: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    vault_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    public_fingerprint: Option<String>,
    rotation_state: String,
    source_kind: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct ExternalIdentityLinkRecord {
    link_id: String,
    user_id: String,
    provider: String,
    provider_subject: String,
    email: Option<String>,
    login: Option<String>,
    display_name: Option<String>,
    verified_at: i64,
    last_seen_at: i64,
    created_at: i64,
    updated_at: i64,
}

#[derive(Debug, Clone, serde::Serialize)]
struct ExternalIdentityLinkStatusView {
    user_id: String,
    provider: String,
    provider_subject: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    login: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    verified_at: i64,
    last_seen_at: i64,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct ConfirmGuestActionBody {
    confirm_guest_id: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct ConfirmSecretRefBody {
    secret_ref: String,
    plaintext: String,
    confirm_secret_ref: String,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct ConfirmVaultEntryBody {
    vault_name: String,
    plaintext: String,
    #[serde(default)]
    allowed_roles: Vec<String>,
    confirm_vault_name: String,
}

fn require_exact_confirmation(provided: &str, expected: &str, label: &str) -> Result<(), Response> {
    if provided == expected {
        Ok(())
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("confirmation text must exactly match {}", label)})),
        )
            .into_response())
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

pub async fn run(
    port: u16,
    _db: Option<PathBuf>,
    config: Option<PathBuf>,
    allow_origins: Option<String>,
    open_path: Option<String>,
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
    let config_json = read_config_json(&config_path);
    let bind_host = resolve_web_bind(
        env_trimmed("PHILOTIC_WEB_BIND").as_deref(),
        config_json
            .as_ref()
            .and_then(|v| v.get("web_bind"))
            .and_then(Value::as_str),
    )?;
    // Conservative classification: we do not probe for a public IP here, so an
    // unspecified bind (0.0.0.0) classifies as Lan with a logged warning.
    let bind_classification = classify_bind_addr(bind_host, false);
    let exposure_tier = bind_classification.tier();
    let edge_token = resolve_edge_token(
        env_trimmed("PHILOTIC_WEB_EDGE_TOKEN"),
        config_json
            .as_ref()
            .and_then(|v| v.get("web_edge_token"))
            .and_then(Value::as_str)
            .map(str::to_string),
    );
    let edge_token_configured = edge_token.is_some();
    let edge_invite = resolve_edge_invite(
        env_trimmed("PHILOTIC_WEB_EDGE_INVITE"),
        config_json
            .as_ref()
            .and_then(|v| v.get("web_edge_invite"))
            .and_then(Value::as_str)
            .map(str::to_string),
    );
    let socket = crate::start::socket_path(&hotel);
    let socket_for_lease_recovery = socket.clone();
    let lease_key = desktop_membrane_lease_key(&hotel);
    let lease_handle = acquire_desktop_membrane_lease(&socket, &lease_key, port).await?;

    // Generate bootstrap token for the first login ceremony.
    let bootstrap_token: String = {
        let bytes: [u8; 24] = rand::thread_rng().gen();
        format!("philotic-{}", hex::encode(bytes))
    };

    // Broadcast channel for WebSocket events (capacity 256)
    let (tx, _) = broadcast::channel::<String>(256);

    // Edge device registry lives next to the hotel context DB.
    let edge_state =
        edge::EdgeState::load(db_path.with_file_name("edge-devices.json"), edge_invite);
    let edge_enrollment_enabled = edge_state.enrollment_enabled();
    let edge_device_count = edge_state.device_count();

    let state = AppState {
        bootstrap_token: Arc::new(bootstrap_token.clone()),
        db_path,
        config_path: Arc::new(config_path.clone()),
        hotel: Arc::new(hotel),
        socket: Arc::new(socket),
        tx,
        edge_token: edge_token.map(Arc::new),
        exposure_tier,
        edge: edge_state,
    };

    ensure_operator_auth_tables(&state.db_path, &state.hotel)?;

    // Process-wide edge retention: keeps replay rings fed for enrolled edge
    // nodes even while they have no live WS session (see serve/edge.rs docs).
    let _edge_retainer = edge::spawn_edge_retainer(state.edge.clone(), state.tx.clone());

    // lifegraph-change-push: when an observer role is configured, subscribe
    // it and mint retained LifeGraphChange frames for enrolled edge devices.
    // Env-gated so hotels without an edge surface run zero extra IPC.
    let observer_role = std::env::var(edge::LIFEGRAPH_CHANGE_OBSERVER_ENV)
        .unwrap_or_default()
        .trim()
        .to_string();
    if !observer_role.is_empty() {
        let _lifegraph_observer = edge::spawn_lifegraph_change_observer(
            state.edge.clone(),
            state.socket.to_string(),
            observer_role,
        );
    }

    // CORS — localhost only; UI is embedded and served from the same origin
    let cors = build_cors(allow_origins.as_deref());

    let app = Router::new()
        // Unauthenticated lightweight probe endpoint — allowed on every tier
        .route("/health", get(handle_health))
        // Edge-mesh tier (see serve/edge.rs): invite-code-gated enrollment plus
        // the bearer-authenticated edge-protocol WebSocket termination
        .route("/api/edge/enroll", post(edge::handle_edge_enroll))
        .route("/api/edge/agents", get(edge::handle_edge_agents))
        .route("/api/edge/sessions", get(edge::handle_edge_sessions))
        .route(
            "/api/edge/sessions/:session_id/turns",
            get(edge::handle_edge_session_turns),
        )
        .route(
            "/api/edge/lifegraph/lens/:lens",
            get(edge::handle_edge_lifegraph_lens),
        )
        .route(
            "/api/edge/lifegraph/node/:node_id",
            get(edge::handle_edge_lifegraph_node),
        )
        .route(
            "/api/edge/lifegraph/neighborhood/:node_id",
            get(edge::handle_edge_lifegraph_neighborhood),
        )
        .route(
            "/api/edge/lifegraph/observe",
            post(edge::handle_edge_lifegraph_observe)
                .layer(axum::extract::DefaultBodyLimit::max(2 * 1024 * 1024)),
        )
        .route(
            "/api/edge/memory/recall",
            get(edge::handle_edge_memory_recall),
        )
        .route(
            "/api/edge/blob",
            post(edge::handle_edge_blob)
                .layer(axum::extract::DefaultBodyLimit::max(25 * 1024 * 1024)),
        )
        .route("/api/edge/ws", get(edge::handle_edge_ws))
        // API routes
        .route("/api/mesh/roster", get(handle_mesh_roster))
        .route("/api/auth/status", get(handle_auth_status))
        .route(
            "/api/auth/user",
            get(handle_auth_user_get).patch(handle_auth_user_patch),
        )
        .route("/api/auth/bootstrap", post(handle_auth_bootstrap))
        .route("/api/auth/challenges", post(handle_create_auth_challenge))
        .route("/api/auth/oidc/start", post(handle_auth_oidc_start))
        .route("/api/auth/logout", post(handle_auth_logout))
        .route(
            "/auth/oidc/:provider/callback",
            get(handle_auth_oidc_callback),
        )
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
            "/api/user-profile",
            get(handle_user_profile_get).patch(handle_user_profile_patch),
        )
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
        .route("/api/config/oidc", get(handle_config_oidc))
        .route("/api/model-catalog", get(handle_model_catalog))
        .route("/api/provider-configs", get(handle_provider_configs))
        .route(
            "/api/provider-configs/:provider",
            get(handle_provider_config_get).post(handle_provider_configure),
        )
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
        .route("/api/event-log", get(handle_event_log))
        .route("/api/config/:key", put(handle_config_put))
        .route(
            "/api/agents/:agent_id/roles/:role_name/skills",
            post(handle_assign_skill),
        )
        .route(
            "/api/agents/:agent_id/roles/:role_name/skills/:skill_name",
            delete(handle_revoke_skill),
        )
        .route("/api/mesh/invite", post(handle_mesh_invite_create))
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
            "/api/mesh/targets/:target_node_id/components",
            get(handle_mesh_target_components),
        )
        .route(
            "/api/mesh/targets/:target_node_id/components/:guest_id",
            get(handle_mesh_target_component_detail),
        )
        .route(
            "/api/mesh/targets/:target_node_id/components",
            post(handle_mesh_target_component_create),
        )
        .route(
            "/api/mesh/targets/:target_node_id/components/:guest_id",
            axum::routing::patch(handle_mesh_target_component_patch)
                .delete(handle_mesh_target_component_delete),
        )
        .route(
            "/api/mesh/targets/:target_node_id/components/:guest_id/enable",
            post(handle_mesh_target_component_enable),
        )
        .route(
            "/api/mesh/targets/:target_node_id/components/:guest_id/disable",
            post(handle_mesh_target_component_disable),
        )
        .route(
            "/api/mesh/targets/:target_node_id/components/:guest_id/restart",
            post(handle_mesh_target_component_restart),
        )
        .route(
            "/api/mesh/targets/:target_node_id/config",
            get(handle_mesh_target_config),
        )
        .route(
            "/api/mesh/targets/:target_node_id/config/:key",
            put(handle_mesh_target_config_put),
        )
        .route(
            "/api/mesh/targets/:target_node_id/secrets",
            get(handle_mesh_target_secrets),
        )
        .route(
            "/api/mesh/targets/:target_node_id/secrets/rotate",
            post(handle_mesh_target_secret_rotate),
        )
        .route(
            "/api/mesh/targets/:target_node_id/vault",
            post(handle_mesh_target_vault_add),
        )
        .route(
            "/api/mesh/targets/:target_node_id/best-place-to-run",
            get(handle_mesh_target_best_place_to_run),
        )
        .route(
            "/api/mesh/targets/:target_node_id/agents/:agent_id/roles/:role_name/home",
            put(handle_mesh_target_role_home_put),
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
        .route("/setup-guide", get(handle_setup_guide))
        // Embedded UI — index.html gets token injected; cookie auth bootstrap; all other assets served as-is
        .route("/", get(handle_index))
        .fallback(get(handle_static))
        .layer(cors)
        // Outermost: perimeter fence for non-loopback binds (runs before CORS)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            edge_fence_middleware,
        ))
        .with_state(state);

    let addr = SocketAddr::new(bind_host, port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("bind philotic-web listener on {addr}"))?;
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let renew_handle = tokio::spawn(run_desktop_membrane_lease_renewal(
        lease_handle.clone(),
        shutdown_tx.clone(),
        socket_for_lease_recovery,
        port,
    ));

    let display_host = display_host_for(bind_host);
    println!("philotic-web serve");
    println!("──────────────────────────────────────────");
    println!("  bind: {addr} (exposure tier: {exposure_tier:?})");
    if let Some(warning) = bind_classification.warning() {
        println!("  bind warning: {warning}");
    }
    if exposure_tier != ExposureTier::Local {
        if edge_token_configured {
            println!("  non-loopback bind: /api and /ws require an operator session; edge bearer tokens (PHILOTIC_WEB_EDGE_TOKEN / enrollment tokens) open /api/edge/* only");
        } else {
            println!("  non-loopback bind: /api and /ws require an operator session — no PHILOTIC_WEB_EDGE_TOKEN configured, only enrolled device tokens can open /api/edge/*");
        }
    }
    if edge_enrollment_enabled {
        println!(
            "  edge enrollment: enabled (POST /api/edge/enroll, {edge_device_count} device(s) enrolled)"
        );
    } else {
        println!("  edge enrollment: disabled (set PHILOTIC_WEB_EDGE_INVITE to enable)");
    }
    println!("  http://{display_host}:{port}");
    println!();
    println!("  Bootstrap token: {bootstrap_token}");
    println!();
    println!("  Press Ctrl-C to stop.");

    let open_path = normalized_open_path(open_path.as_deref());
    let browser_url = format!("http://{display_host}:{port}{open_path}");

    // Auto-open the embedded desktop in the default browser — only when the
    // listener is actually reachable over loopback.
    if bind_host.is_loopback() || bind_host.is_unspecified() {
        let _ = tokio::process::Command::new("open")
            .arg(&browser_url)
            .spawn();
    }

    let shutdown_reason = wait_for_shutdown(shutdown_rx);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
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
    socket: String,
    port: u16,
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
                        // A dropped renewal usually means the hotel restarted
                        // (broken UDS pipe). Surviving that is a resilience
                        // requirement: reconnect and re-acquire with backoff
                        // instead of shutting the operator surface down.
                        eprintln!(
                            "desktop membrane lease renewal lost ({err:#}); entering re-acquire loop"
                        );
                        match reacquire_desktop_membrane_lease(
                            &handle,
                            &mut shutdown_rx,
                            &socket,
                            port,
                        )
                        .await
                        {
                            Some(new_epoch) => {
                                epoch = new_epoch;
                                interval.reset();
                            }
                            // Shutdown requested while re-acquiring.
                            None => return,
                        }
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

/// Reconnect to the hotel and re-acquire the desktop membrane lease, retrying
/// every 5s until it succeeds or shutdown is requested. On success the fresh
/// IPC client replaces the one inside `handle` (all lease calls go through
/// that shared mutex) and the new lease epoch is returned.
async fn reacquire_desktop_membrane_lease(
    handle: &DesktopMembraneLeaseHandle,
    shutdown_rx: &mut watch::Receiver<bool>,
    socket: &str,
    port: u16,
) -> Option<u64> {
    loop {
        if *shutdown_rx.borrow() {
            return None;
        }
        match try_reacquire_once(handle, socket, port).await {
            Ok(epoch) => {
                println!("desktop membrane lease re-acquired after hotel restart (epoch {epoch})");
                return Some(epoch);
            }
            Err(err) => {
                eprintln!("desktop membrane lease re-acquire failed ({err:#}); retrying in 5s");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(5)) => {}
                    changed = shutdown_rx.changed() => {
                        if changed.is_ok() && *shutdown_rx.borrow() {
                            return None;
                        }
                    }
                }
            }
        }
    }
}

/// One re-acquire attempt: fresh IPC connection + AcquireDesktopMembraneLease,
/// swapping the new client into the shared handle on success. Returns the new
/// lease epoch.
async fn try_reacquire_once(
    handle: &DesktopMembraneLeaseHandle,
    socket: &str,
    port: u16,
) -> Result<u64> {
    let identity = GuestIdentity {
        guest_id: format!("philotic-web-membrane-{port}"),
        role: "management".into(),
        supported_tools: vec![],
    };
    let mut client = PhiloticClient::connect_at(socket, identity)
        .await
        .with_context(|| format!("reconnect to hotel at {socket} for desktop membrane lease"))?;
    let response = client
        .send_request(IpcRequest::AcquireDesktopMembraneLease {
            lease_key: handle.lease_key.as_str().to_string(),
            port,
        })
        .await
        .context("re-acquire desktop membrane lease")?;
    let lease = match response {
        IpcResponse::DesktopMembraneLease {
            desktop_granted: true,
            desktop_lease: Some(lease),
        } => lease,
        IpcResponse::DesktopMembraneLease {
            desktop_granted: false,
            desktop_lease: Some(lease),
        } => bail!(
            "desktop membrane lease [{}] is held by [{}] (epoch {})",
            lease.lease_scope,
            lease.owner_guest_id,
            lease.lease_epoch
        ),
        other => bail!("unexpected desktop membrane lease response: {other:?}"),
    };
    *handle.client.lock().await = client;
    Ok(lease.lease_epoch)
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

/// Serve the embedded desktop shell for both locked and unlocked states.
///
/// The desktop itself is responsible for presenting a locked posture and
/// routing bootstrap/auth workflows into System Settings. The server remains
/// the authority on session issuance and API access, but it should not replace
/// the desktop with a parallel HTML login applet.
async fn handle_index(headers: HeaderMap, State(state): State<AppState>) -> Response {
    serve_index_for_session(&state, current_operator_session(&headers, &state).as_ref()).await
}

async fn serve_index_for_session(
    _state: &AppState,
    session: Option<&OperatorSessionRecord>,
) -> Response {
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
    if let Some(session) = session {
        headers.insert(
            header::SET_COOKIE,
            session_cookie_header(&session.session_token),
        );
    }
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(no_store_header_value()),
    );
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
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

async fn handle_setup_guide() -> Response {
    let html = r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1.0" />
  <title>Philotic Setup Guide</title>
  <style>
    :root {
      color-scheme: light;
      --bg: #f4efe4;
      --panel: rgba(255, 252, 247, 0.92);
      --text: #1d1b18;
      --muted: #665f57;
      --accent: #1d6b57;
      --accent-2: #c46a2a;
      --border: rgba(29, 27, 24, 0.1);
      --shadow: 0 24px 60px rgba(58, 46, 34, 0.12);
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      font-family: "Avenir Next", "Segoe UI", sans-serif;
      color: var(--text);
      background:
        radial-gradient(circle at top left, rgba(196, 106, 42, 0.16), transparent 34%),
        radial-gradient(circle at top right, rgba(29, 107, 87, 0.18), transparent 30%),
        linear-gradient(180deg, #fbf7f0 0%, var(--bg) 100%);
      min-height: 100vh;
    }
    main {
      max-width: 980px;
      margin: 0 auto;
      padding: 48px 24px 64px;
    }
    .hero, .panel {
      background: var(--panel);
      border: 1px solid var(--border);
      border-radius: 28px;
      box-shadow: var(--shadow);
      backdrop-filter: blur(12px);
    }
    .hero {
      padding: 32px;
      margin-bottom: 24px;
    }
    .eyebrow {
      text-transform: uppercase;
      letter-spacing: 0.14em;
      font-size: 12px;
      color: var(--accent);
      font-weight: 700;
      margin: 0 0 12px;
    }
    h1 {
      font-size: clamp(32px, 4vw, 54px);
      line-height: 0.96;
      margin: 0 0 14px;
      max-width: 10ch;
    }
    .lede {
      font-size: 18px;
      line-height: 1.6;
      color: var(--muted);
      max-width: 60ch;
      margin: 0;
    }
    .actions {
      display: flex;
      flex-wrap: wrap;
      gap: 12px;
      margin-top: 24px;
    }
    .actions a {
      text-decoration: none;
      padding: 12px 18px;
      border-radius: 999px;
      font-weight: 700;
    }
    .actions a.primary {
      background: var(--accent);
      color: white;
    }
    .actions a.secondary {
      background: rgba(29, 107, 87, 0.08);
      color: var(--accent);
      border: 1px solid rgba(29, 107, 87, 0.18);
    }
    .grid {
      display: grid;
      gap: 18px;
      grid-template-columns: repeat(auto-fit, minmax(260px, 1fr));
    }
    .panel {
      padding: 24px;
    }
    .panel h2 {
      margin: 0 0 12px;
      font-size: 22px;
    }
    .panel p, .panel li {
      color: var(--muted);
      line-height: 1.6;
    }
    ol, ul {
      margin: 0;
      padding-left: 20px;
    }
    .callout {
      margin-top: 24px;
      padding: 18px 20px;
      border-left: 4px solid var(--accent-2);
      background: rgba(196, 106, 42, 0.08);
      border-radius: 18px;
      color: var(--text);
    }
    code {
      font-family: "SFMono-Regular", "Menlo", monospace;
      font-size: 0.95em;
      background: rgba(29, 27, 24, 0.06);
      padding: 2px 6px;
      border-radius: 6px;
    }
  </style>
</head>
<body>
  <main>
    <section class="hero">
      <p class="eyebrow">Philotic Setup</p>
      <h1>Finish the operator handoff.</h1>
      <p class="lede">
        Onboarding got the hotel alive. This guide is the part where we stop pretending the rest is obvious and
        make the model, workspace, and component setup explicit.
      </p>
      <div class="actions">
        <a class="primary" href="/">Open Management UI</a>
        <a class="secondary" href="/api/component-templates">View Component Templates</a>
      </div>
    </section>

    <section class="grid">
      <article class="panel">
        <h2>1. Confirm the hotel is healthy</h2>
        <ol>
          <li>Use the main dashboard to confirm the daemon, guests, and agents are visible.</li>
          <li>If you just finished onboarding, check that the hotel you seeded is the one you expect.</li>
          <li>If anything looks stale, run <code>phil status --hotel &lt;hotel&gt;</code> in the terminal for a second opinion.</li>
        </ol>
      </article>

      <article class="panel">
        <h2>2. Configure your model path</h2>
        <p>The management UI and <code>phil keys</code> keep model setup legible instead of hidden in config folklore.</p>
        <ul>
          <li>Provider keys: use <code>phil keys configure openrouter</code>, or the provider config API at <code>/api/provider-configs/openrouter</code>.</li>
          <li>Provider status: use <code>phil keys status</code> or <code>/api/provider-configs</code>; secret values stay hidden.</li>
          <li>Remote Gemini path: confirm the configured default model and the Gemini secret/config entries.</li>
          <li>OpenRouter path: configure <code>openrouter_api_key_ref</code>, default model, fallback models, and route through the vault/config surface.</li>
          <li>MLX path: add or edit the <code>model-controller-mlx</code> component and define its model fleet.</li>
          <li>Local controller path: use the local model controller surfaces when you want a local-first or offline-ish route.</li>
        </ul>
      </article>

      <article class="panel">
        <h2>3. Point the agent at real local files</h2>
        <p>
          The agent workspace matters. Philotic uses it for both identity file import
          and as the bash working directory fallback, so this is the seam where “local files”
          becomes a real operating mode rather than a vague intention.
        </p>
        <ul>
          <li>Set the agent workspace/import path to the folder you actually want the operator to work from.</li>
          <li>Keep <code>AGENTS.md</code>, <code>IDENTITY.md</code>, and related context files there if you want the agent bundle to hydrate from disk.</li>
          <li>Use the dashboard’s agent editing surfaces to revise this without re-running bootstrap.</li>
        </ul>
      </article>

      <article class="panel">
        <h2>4. Keep the explanation honest</h2>
        <p>
          If a human has to guess where models live, which workspace is canonical, or why a local controller exists,
          the setup is still under-authored. This guide is the first corrective, not the final word.
        </p>
        <ul>
          <li>Add a small skill bundle for your operator personas once the basic model path is stable.</li>
          <li>Use this UI session to note friction points for Anj and Mauricio rather than trying to solve the whole platform in one turn.</li>
          <li>Prefer one explanatory panel that tells the truth over three “smart” surfaces that assume it.</li>
        </ul>
      </article>
    </section>

    <div class="callout">
      Best next move: get one operator through this guide with a real local-files workspace and one model path
      configured end-to-end. That gives you signal. Everything else is still tempting architecture perfume.
    </div>
  </main>
</body>
</html>"#;

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

/// Serve any other embedded asset (JS, CSS, icons, etc.).
/// Falls back to `index.html` for SPA client-side routes.
async fn handle_static(
    headers: HeaderMap,
    State(state): State<AppState>,
    uri: axum::http::Uri,
) -> Response {
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
        // The MIME here is guessed from the embedded asset's extension, so a
        // mistyped or attacker-influenced asset must not be sniffable into
        // script by the browser.
        headers.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
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
        handle_index(headers, State(state)).await
    }
}

// ── Auth helper ───────────────────────────────────────────────────────────────

/// Pre-login disclosure is deliberately minimal.
///
/// This route must stay reachable without a session — the login screen needs
/// to know which OIDC providers to offer — but everything beyond that is
/// operator data. An unauthenticated caller gets only `authenticated`,
/// `hotel`, and each provider's id/label/configured flag.
///
/// Withheld until authenticated:
/// - `root_user_key_refs` — the vault master key's *storage location*
///   (`keychain://…`, `env://PHILOTIC_VAULT_MASTER_KEY/…`) and a `sha256:`
///   fingerprint of the key itself.
/// - `external_identity_links` — the operator's email, provider subject,
///   login, and display name.
/// - `callback_url` — deployment topology; not needed to render a button.
///
/// The bind tier is not a substitute for this check: at `Local` tier the
/// perimeter fence allows every request, and at `Mesh` tier loopback peers
/// bypass it, so any local process (or a DNS-rebound browser page) reaches
/// this handler with no session at all.
async fn handle_auth_status(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let session = current_operator_session(&headers, &state);
    let authenticated = session.is_some();
    let mut oidc_providers = list_oidc_provider_statuses(&state.socket, Some(&headers)).await;

    let (root_user_key_refs, external_identity_links) = if authenticated {
        (
            list_root_user_key_refs(&state.db_path, &state.hotel).unwrap_or_default(),
            list_external_identity_links(&state.db_path, &state.hotel).unwrap_or_default(),
        )
    } else {
        for provider in &mut oidc_providers {
            provider.callback_url = String::new();
        }
        (Vec::new(), Vec::new())
    };

    Json(AuthStatusView {
        authenticated,
        hotel: (*state.hotel).clone(),
        oidc_providers,
        root_user_key_refs,
        external_identity_links,
        session: session.map(|session| OperatorSessionStatusView {
            session_id: session.session_id,
            user_id: session.user_id,
            display_name: session.display_name,
            posture: session.posture,
            issued_at: session.issued_at,
            expires_at: session.expires_at,
            auth_method: session.auth_method,
        }),
    })
    .into_response()
}

async fn handle_auth_bootstrap(
    State(state): State<AppState>,
    Json(body): Json<BootstrapAuthBody>,
) -> Response {
    // Constant-time: this mints an admin session, and every other credential
    // comparison in this crate already uses `constant_time_eq`. `String`'s `!=`
    // short-circuits on the first differing byte.
    if !constant_time_eq(
        body.bootstrap_token.as_bytes(),
        state.bootstrap_token.as_bytes(),
    ) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid bootstrap token"})),
        )
            .into_response();
    }

    let display_name = body
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Operator");

    let session = match issue_operator_session(
        &state.db_path,
        &state.hotel,
        display_name,
        "bootstrap_token",
        Some("startup-bootstrap".into()),
    ) {
        Ok(session) => session,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response();
        }
    };

    let oidc_providers = list_oidc_provider_statuses(&state.socket, None).await;
    let mut response = Json(AuthStatusView {
        authenticated: true,
        hotel: (*state.hotel).clone(),
        oidc_providers,
        root_user_key_refs: list_root_user_key_refs(&state.db_path, &state.hotel)
            .unwrap_or_default(),
        external_identity_links: list_external_identity_links(&state.db_path, &state.hotel)
            .unwrap_or_default(),
        session: Some(OperatorSessionStatusView {
            session_id: session.session_id.clone(),
            user_id: session.user_id.clone(),
            display_name: session.display_name.clone(),
            posture: session.posture.clone(),
            issued_at: session.issued_at,
            expires_at: session.expires_at,
            auth_method: session.auth_method.clone(),
        }),
    })
    .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie_header(&session.session_token),
    );
    response
}

async fn handle_auth_user_get(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let session = match current_operator_session(&headers, &state) {
        Some(session) => session,
        None => return unauthorized(),
    };

    match load_operator_user_settings_view(&state, &session).await {
        Ok(view) => Json(view).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

async fn handle_auth_user_patch(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<PatchOperatorUserBody>,
) -> Response {
    let session = match current_operator_session(&headers, &state) {
        Some(session) => session,
        None => return unauthorized(),
    };

    if body.display_name.is_some() || body.timezone.is_some() {
        let hotel_name = state.hotel.as_ref().clone();
        if let Err(err) = ipc_patch_user_profile(
            &state.socket,
            &hotel_name,
            body.timezone.clone(),
            body.display_name.clone(),
        )
        .await
        {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response();
        }
    }

    match patch_operator_user_record(
        &state.db_path,
        &state.hotel,
        &session.user_id,
        body.display_name,
        body.preferred_name,
        body.primary_email,
        body.onboarding_state,
    ) {
        Ok(_) => {}
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response();
        }
    }

    let event = json!({ "type": "operator_user:updated", "user_id": session.user_id });
    let _ = state.tx.send(event.to_string());

    match load_operator_user_settings_view(&state, &session).await {
        Ok(view) => Json(view).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

async fn handle_auth_oidc_start(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<OidcStartBody>,
) -> Response {
    let provider_key = body.provider.trim().to_ascii_lowercase();
    let resolved = match load_oidc_resolved_config(&state.socket, Some(&headers)).await {
        Ok(resolved) => resolved,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": err.to_string()})),
            )
                .into_response();
        }
    };
    if oidc_loopback_bootstrap_only(&resolved) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": "local loopback membranes should use the bootstrap/back-door path; configure oidc_public_base_url for a real public ingress if you want OIDC here"
            })),
        )
            .into_response();
    }
    let provider = match oidc_provider_config_from_resolved(&provider_key, resolved) {
        Ok(Some(provider)) => provider,
        Ok(None) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("unsupported provider [{}]", provider_key)})),
            )
                .into_response();
        }
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": err.to_string()})),
            )
                .into_response();
        }
    };

    if !provider.configured {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("provider [{}] is not configured", provider.id)})),
        )
            .into_response();
    }

    let return_path = sanitize_return_path(body.return_path.as_deref());
    let code_verifier = oidc_code_verifier();
    let code_challenge = pkce_code_challenge(&code_verifier);
    let challenge = match issue_operator_auth_challenge(
        &state.db_path,
        &state.hotel,
        "desktop_membrane",
        "oidc",
        &provider.id,
        Some(provider.redirect_uri.clone()),
        return_path.clone(),
        Some(code_verifier),
    ) {
        Ok(challenge) => challenge,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response();
        }
    };

    let authorization_url =
        match provider.authorization_url(&challenge.challenge_nonce, &code_challenge) {
            Ok(url) => url,
            Err(err) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": err.to_string()})),
                )
                    .into_response();
            }
        };

    Json(OidcStartView {
        provider: provider.id,
        authorization_url,
        challenge_id: challenge.challenge_id,
        state: challenge.challenge_nonce,
        redirect_uri: provider.redirect_uri,
        expires_at: challenge.expires_at,
    })
    .into_response()
}

async fn handle_create_auth_challenge(
    State(state): State<AppState>,
    Json(body): Json<CreateAuthChallengeBody>,
) -> Response {
    let auth_path = body.auth_path.trim().to_ascii_lowercase();
    let verifier_kind = body.verifier_kind.trim().to_ascii_lowercase();
    if !matches!(auth_path.as_str(), "membrane_challenge" | "oidc") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "auth_path must be one of: membrane_challenge, oidc"})),
        )
            .into_response();
    }
    if verifier_kind.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "verifier_kind must not be empty"})),
        )
            .into_response();
    }

    let verifier_hint = body
        .verifier_hint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    // `bind_label` is the post-login redirect target (see the OIDC callback), so
    // it must be a site-relative path. Reject rather than silently coerce, so a
    // caller passing an absolute URL learns it was wrong instead of being
    // quietly sent to `/`. Defence in depth only — the callback re-sanitizes.
    let bind_label = match body.bind_label.as_deref().map(str::trim) {
        None => None,
        Some("") => None,
        Some(value) => match sanitize_return_path(Some(value)) {
            Some(path) => Some(path),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": "bind_label must be a site-relative path beginning with a \
                                  single '/' (absolute and scheme-relative URLs are rejected)"
                    })),
                )
                    .into_response();
            }
        },
    };

    let challenge = match issue_operator_auth_challenge(
        &state.db_path,
        &state.hotel,
        "desktop_membrane",
        &auth_path,
        &verifier_kind,
        verifier_hint,
        bind_label,
        None,
    ) {
        Ok(challenge) => challenge,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response();
        }
    };

    (
        StatusCode::CREATED,
        Json(OperatorAuthChallengeView {
            challenge_id: challenge.challenge_id,
            user_id: challenge.user_id,
            intended_surface: challenge.intended_surface,
            auth_path: challenge.auth_path,
            verifier_kind: challenge.verifier_kind,
            verifier_hint: challenge.verifier_hint,
            bind_label: challenge.bind_label,
            challenge_nonce: challenge.challenge_nonce,
            issued_at: challenge.issued_at,
            expires_at: challenge.expires_at,
            status: challenge.status,
        }),
    )
        .into_response()
}

async fn handle_auth_oidc_callback(
    headers: HeaderMap,
    Path(provider): Path<String>,
    Query(query): Query<OidcCallbackQuery>,
    State(state): State<AppState>,
) -> Response {
    if let Some(error) = query.error.as_deref() {
        return oidc_callback_error_response(
            StatusCode::BAD_REQUEST,
            format!("OIDC callback failed: {error}"),
        );
    }

    let state_token = match query
        .state
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => value,
        None => {
            return oidc_callback_error_response(
                StatusCode::BAD_REQUEST,
                "OIDC callback missing state.".into(),
            );
        }
    };
    let code = match query
        .code
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => value,
        None => {
            return oidc_callback_error_response(
                StatusCode::BAD_REQUEST,
                "OIDC callback missing authorization code.".into(),
            );
        }
    };

    let provider_key = provider.trim().to_ascii_lowercase();
    let provider = match oidc_provider_config(&state.socket, &provider_key, Some(&headers)).await {
        Ok(Some(provider)) if provider.configured => provider,
        Ok(Some(_)) => {
            return oidc_callback_error_response(
                StatusCode::BAD_REQUEST,
                format!("OIDC provider [{}] is not configured.", provider_key),
            );
        }
        Ok(None) => {
            return oidc_callback_error_response(
                StatusCode::BAD_REQUEST,
                format!("Unsupported OIDC provider [{}].", provider_key),
            );
        }
        Err(err) => return oidc_callback_error_response(StatusCode::BAD_REQUEST, err.to_string()),
    };

    let pending = match resolve_operator_auth_challenge_by_nonce(&state.db_path, state_token) {
        Ok(Some(challenge)) => challenge,
        Ok(None) => {
            return oidc_callback_error_response(
                StatusCode::BAD_REQUEST,
                "OIDC state was not recognized.".into(),
            );
        }
        Err(err) => {
            return oidc_callback_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                err.to_string(),
            );
        }
    };

    if pending.auth_path != "oidc"
        || pending.verifier_kind != provider.id
        || pending.status != "pending"
        || pending.expires_at <= now_epoch_secs()
    {
        return oidc_callback_error_response(
            StatusCode::BAD_REQUEST,
            "OIDC callback did not match a valid pending challenge.".into(),
        );
    }

    let code_verifier = match pending.exchange_secret.as_deref() {
        Some(value) => value,
        None => {
            return oidc_callback_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "OIDC challenge was missing PKCE verifier state.".into(),
            );
        }
    };

    let userinfo =
        match exchange_oidc_identity_via_hotel(&state.socket, &provider, code, code_verifier).await
        {
            Ok(userinfo) => userinfo,
            Err(err) => {
                return oidc_callback_error_response(
                    StatusCode::BAD_GATEWAY,
                    format!("governed OIDC exchange failed: {err}"),
                );
            }
        };
    let identity = match oidc_identity_from_userinfo(&provider, &userinfo) {
        Ok(identity) => identity,
        Err(err) => {
            return oidc_callback_error_response(
                StatusCode::BAD_GATEWAY,
                format!("OIDC identity response was invalid: {err}"),
            );
        }
    };

    // Authorization gate. The exchange above only proves the caller controls an
    // account at the provider — it says nothing about whether they may operate
    // *this* hotel. Everything past this point binds the identity to the root
    // operator user and mints an admin session, so the check belongs here,
    // before any state is written.
    let allowlist_raw = oidc_config_string(&state.socket, OIDC_SUBJECT_ALLOWLIST_KEY)
        .await
        .unwrap_or(None);
    if let Err(reason) = oidc_identity_allowed(&identity, allowlist_raw.as_deref()) {
        eprintln!(
            "rejected OIDC login for {}:{} — {reason}",
            identity.provider, identity.provider_subject
        );
        return oidc_callback_error_response(StatusCode::FORBIDDEN, reason);
    }

    if let Err(err) =
        upsert_operator_external_identity_link(&state.db_path, &state.hotel, &identity)
    {
        return oidc_callback_error_response(StatusCode::INTERNAL_SERVER_ERROR, err.to_string());
    }

    let challenge = match consume_operator_auth_challenge(&state.db_path, &pending.challenge_id) {
        Ok(Some(challenge)) => challenge,
        Ok(None) => {
            return oidc_callback_error_response(
                StatusCode::BAD_REQUEST,
                "OIDC challenge was already consumed or expired.".into(),
            );
        }
        Err(err) => {
            return oidc_callback_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                err.to_string(),
            );
        }
    };

    let session = match issue_operator_session(
        &state.db_path,
        &state.hotel,
        &identity.display_name,
        &format!("oidc_{}", provider.id),
        Some(challenge.challenge_id.clone()),
    ) {
        Ok(session) => session,
        Err(err) => {
            return oidc_callback_error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                err.to_string(),
            );
        }
    };

    // Re-sanitize at redirect time, not only at issuance.
    //
    // `bind_label` is stored from the `POST /api/auth/challenges` body, which
    // is unauthenticated, and lands here as a `Location` header on the same
    // response that sets a freshly minted admin session cookie. Validating it
    // only when the challenge was created leaves the stored value trusted, so
    // anything that can write a challenge row picks the post-login destination.
    // `sanitize_return_path` requires a single leading `/`, which rejects both
    // absolute URLs (`https://evil.example`) and scheme-relative ones (`//evil`).
    let redirect_path =
        sanitize_return_path(challenge.bind_label.as_deref()).unwrap_or_else(|| "/".into());
    let mut response = (
        StatusCode::SEE_OTHER,
        [(header::LOCATION, redirect_path.as_str())],
        "",
    )
        .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie_header(&session.session_token),
    );
    response
}

async fn handle_auth_logout(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if let Some(token) =
        header_bearer_token(&headers).or_else(|| cookie_token(&headers, AUTH_COOKIE_NAME))
    {
        let _ = revoke_operator_session(&state.db_path, token);
    }

    let mut response = Json(json!({"ok": true})).into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, clear_session_cookie_header());
    response
}

/// Operator-session gate for the admin API surface. Edge bearer tokens are
/// deliberately NOT accepted here: an edge device holds a low-trust,
/// chat-scoped credential (see the trust-tier notes in
/// `philotic-edge-protocol`), so it must never unlock secrets/config/vault
/// mutation or the operator /ws firehose. Edge bearers are honoured only on
/// the `/api/edge/*` surface (see [`edge_fence_allows`] and
/// `edge::handle_edge_ws`).
fn check_auth(headers: &HeaderMap, state: &AppState) -> bool {
    current_operator_session(headers, state).is_some()
}

/// True when the request carries a valid edge bearer token: either the shared
/// PHILOTIC_WEB_EDGE_TOKEN / config `web_edge_token`, or a per-device token
/// issued at enrollment (POST /api/edge/enroll). Constant-time comparisons.
/// Grants access to the `/api/edge/*` surface only — never the operator API.
fn edge_bearer_authorized(headers: &HeaderMap, state: &AppState) -> bool {
    edge::edge_bearer_identity(headers, state).is_some()
}

/// Constant-time byte comparison. Only the length is allowed to leak via timing.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

// ── Perimeter fence (non-loopback binds) ──────────────────────────────────────

async fn edge_fence_middleware(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    if edge_fence_allows(
        &state,
        request.method(),
        request.uri().path(),
        request.headers(),
        peer.ip(),
    ) {
        next.run(request).await
    } else {
        unauthorized()
    }
}

/// Gate for non-loopback binds.
///
/// Tier classification comes from `perimeter_core::classify_bind_addr`. The route
/// policy is intentionally stricter than `perimeter_core::fence::check_ingress`
/// (which is a structural pre-gate and allows Lan unauthenticated): philotic-web
/// is an operator surface, so any non-Local bind requires a *valid* operator
/// session (cookie or bearer session token) on every /api route and /ws. Edge
/// bearer tokens are scoped to `/api/edge/*` only — a device credential must
/// never open the operator admin surface. Two behaviours mirror the perimeter
/// fence: loopback peers bypass the gate up to Mesh tier (loopback callers are
/// local hotel tooling), and Internet tier gets no loopback bypass.
///
/// Non-API surfaces (static UI shell, /health latency probe, the OIDC browser
/// callback) stay reachable; they carry no sensitive data without a session.
fn edge_fence_allows(
    state: &AppState,
    method: &Method,
    path: &str,
    headers: &HeaderMap,
    peer: IpAddr,
) -> bool {
    if state.exposure_tier == ExposureTier::Local {
        return true;
    }
    if method == Method::OPTIONS {
        // CORS preflight requests carry no credentials by design
        return true;
    }
    if path == "/api/edge/enroll" {
        // Enrollment carries its own credential: the operator-issued invite
        // code, validated (and throttled) by the handler (serve/edge.rs).
        // Devices enrolling for the first time cannot yet hold any session
        // or bearer token — but a static invite is not strong enough for a
        // public-facing bind, so Internet tier keeps it fenced.
        return state.exposure_tier <= ExposureTier::Mesh;
    }
    if path.starts_with("/api/edge/") {
        // The edge surface (currently /api/edge/ws) accepts the edge bearer
        // or an operator session; the WS handler re-validates bearer-only.
        return edge_bearer_authorized(headers, state) || check_auth(headers, state);
    }
    if !(path.starts_with("/api/") || path == "/ws") {
        return true;
    }
    if peer.is_loopback() && state.exposure_tier <= ExposureTier::Mesh {
        return true;
    }
    check_auth(headers, state)
}

// ── Bind resolution ───────────────────────────────────────────────────────────

const DEFAULT_WEB_BIND: &str = "127.0.0.1";

/// Resolve the listener bind host: PHILOTIC_WEB_BIND env wins, then the config
/// file `web_bind` key, then loopback.
fn resolve_web_bind(env_value: Option<&str>, config_value: Option<&str>) -> Result<IpAddr> {
    let chosen = env_value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .or_else(|| config_value.map(str::trim).filter(|v| !v.is_empty()))
        .unwrap_or(DEFAULT_WEB_BIND);
    parse_bind_host(chosen)
}

fn parse_bind_host(raw: &str) -> Result<IpAddr> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("bind host is empty");
    }
    if trimmed.eq_ignore_ascii_case("localhost") {
        return Ok(IpAddr::V4(Ipv4Addr::LOCALHOST));
    }
    trimmed.parse::<IpAddr>().with_context(|| {
        format!(
            "invalid PHILOTIC_WEB_BIND host {trimmed:?} — expected an IP address \
             such as 127.0.0.1, 0.0.0.0, or a Tailscale interface IP"
        )
    })
}

/// Host suitable for printed URLs: unspecified binds are reachable via loopback,
/// IPv6 hosts need brackets.
fn display_host_for(bind: IpAddr) -> String {
    if bind.is_unspecified() {
        return DEFAULT_WEB_BIND.to_string();
    }
    match bind {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    }
}

/// Resolve the edge bearer token: PHILOTIC_WEB_EDGE_TOKEN env wins, then the
/// config file `web_edge_token` key.
fn resolve_edge_token(env_value: Option<String>, config_value: Option<String>) -> Option<String> {
    normalize_non_empty_option(env_value).or_else(|| normalize_non_empty_option(config_value))
}

/// Resolve the edge enrollment invite code: PHILOTIC_WEB_EDGE_INVITE env wins,
/// then the config file `web_edge_invite` key. `None` disables enrollment.
fn resolve_edge_invite(env_value: Option<String>, config_value: Option<String>) -> Option<String> {
    normalize_non_empty_option(env_value).or_else(|| normalize_non_empty_option(config_value))
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

fn session_cookie_header(session_token: &str) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{AUTH_COOKIE_NAME}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={AUTH_COOKIE_MAX_AGE_SECS}",
        session_token
    ))
    .expect("session cookie should be a valid header value")
}

fn clear_session_cookie_header() -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{AUTH_COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"
    ))
    .expect("cleared session cookie should be a valid header value")
}

fn no_store_header_value() -> &'static str {
    "no-store, no-cache, must-revalidate, private"
}

fn normalized_open_path(open_path: Option<&str>) -> String {
    match open_path.map(str::trim).filter(|value| !value.is_empty()) {
        Some(path) if path.starts_with('/') => path.to_string(),
        Some(path) => format!("/{path}"),
        None => "/".into(),
    }
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

async fn handle_mesh_target_components(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(target_node_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    match ipc_desktop_membrane_target_components(&state.socket, &target_node_id).await {
        Ok(components) => Json(components).into_response(),
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

async fn handle_mesh_target_component_detail(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path((target_node_id, guest_id)): Path<(String, String)>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    match ipc_desktop_membrane_target_component(&state.socket, &target_node_id, &guest_id).await {
        Ok(component) => Json(component).into_response(),
        Err(err) if err.to_string().contains("component not found") => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "component not found"})),
        )
            .into_response(),
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

async fn handle_mesh_target_component_create(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(target_node_id): Path<String>,
    Json(manifest): Json<ComponentManifest>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_register_target_component(&state.socket, &target_node_id, manifest).await {
        Ok(component) => {
            let event = json!({
                "type": "component:created",
                "payload": { "guest_id": component.guest_id, "target_node_id": target_node_id }
            });
            let _ = state.tx.send(event.to_string());
            (StatusCode::CREATED, Json(component)).into_response()
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

async fn handle_mesh_target_component_patch(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path((target_node_id, guest_id)): Path<(String, String)>,
    Json(body): Json<PatchComponentBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_desktop_membrane_target_component(&state.socket, &target_node_id, &guest_id).await {
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
            match ipc_register_target_component(&state.socket, &target_node_id, manifest).await {
                Ok(component) => {
                    let event = json!({
                        "type": "component:updated",
                        "payload": { "guest_id": guest_id, "target_node_id": target_node_id }
                    });
                    let _ = state.tx.send(event.to_string());
                    Json(component).into_response()
                }
                Err(err) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": err.to_string()})),
                )
                    .into_response(),
            }
        }
        Err(err) if err.to_string().contains("component not found") => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "component not found"})),
        )
            .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

async fn handle_mesh_target_component_delete(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path((target_node_id, guest_id)): Path<(String, String)>,
    Json(body): Json<DeleteComponentBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    if let Err(response) = require_exact_confirmation(&body.confirm_guest_id, &guest_id, "guest_id")
    {
        return response;
    }
    match ipc_remove_target_component(&state.socket, &target_node_id, &guest_id).await {
        Ok(_) => {
            let event = json!({
                "type": "component:deleted",
                "payload": { "guest_id": guest_id, "target_node_id": target_node_id }
            });
            let _ = state.tx.send(event.to_string());
            Json(json!({"ok": true, "guest_id": guest_id, "target_node_id": target_node_id}))
                .into_response()
        }
        Err(err)
            if err.to_string().contains("GUEST_NOT_FOUND")
                || err.to_string().contains("component not found") =>
        {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error": "component not found"})),
            )
                .into_response()
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

async fn handle_mesh_target_component_enable(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path((target_node_id, guest_id)): Path<(String, String)>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_set_target_component_active(&state.socket, &target_node_id, &guest_id, true).await {
        Ok(_) => {
            let event = json!({
                "type": "component:enabled",
                "payload": { "guest_id": guest_id, "target_node_id": target_node_id }
            });
            let _ = state.tx.send(event.to_string());
            Json(json!({"ok": true, "guest_id": guest_id, "active": true, "target_node_id": target_node_id})).into_response()
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

async fn handle_mesh_target_component_disable(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path((target_node_id, guest_id)): Path<(String, String)>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_set_target_component_active(&state.socket, &target_node_id, &guest_id, false).await {
        Ok(_) => {
            let event = json!({
                "type": "component:disabled",
                "payload": { "guest_id": guest_id, "target_node_id": target_node_id }
            });
            let _ = state.tx.send(event.to_string());
            Json(json!({"ok": true, "guest_id": guest_id, "active": false, "target_node_id": target_node_id})).into_response()
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

async fn handle_mesh_target_component_restart(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path((target_node_id, guest_id)): Path<(String, String)>,
    Json(body): Json<ConfirmGuestActionBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    if let Err(response) = require_exact_confirmation(&body.confirm_guest_id, &guest_id, "guest_id")
    {
        return response;
    }
    match ipc_restart_target_component(&state.socket, &target_node_id, &guest_id).await {
        Ok(_) => {
            let event = json!({
                "type": "component:restarted",
                "payload": { "guest_id": guest_id, "target_node_id": target_node_id }
            });
            let _ = state.tx.send(event.to_string());
            Json(json!({"ok": true, "guest_id": guest_id, "target_node_id": target_node_id}))
                .into_response()
        }
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

async fn handle_mesh_target_config(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(target_node_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    match ipc_desktop_membrane_target_config(&state.socket, &target_node_id).await {
        Ok(config) => Json(config).into_response(),
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

async fn handle_mesh_target_config_put(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path((target_node_id, key)): Path<(String, String)>,
    Json(body): Json<SetConfigBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    if !OPERATOR_REMOTE_MUTABLE_CONFIG_KEYS.contains(&key.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("config key '{}' is not operator-mutable on remote targets", key)})),
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
    match ipc_set_target_config(&state.socket, &target_node_id, &key, &value_json).await {
        Ok(ack) => {
            let event = json!({
                "type": "config:updated",
                "payload": { "key": key, "target_node_id": target_node_id }
            });
            let _ = state.tx.send(event.to_string());
            Json(ack).into_response()
        }
        Err(err) => {
            let status_code = if err
                .to_string()
                .contains("not currently active in the registry")
            {
                StatusCode::NOT_FOUND
            } else if err.to_string().contains("not operator-mutable") {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            (status_code, Json(json!({"error": err.to_string()}))).into_response()
        }
    }
}

async fn handle_mesh_target_secrets(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(target_node_id): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_desktop_membrane_target_secrets(&state.socket, &target_node_id).await {
        Ok(secrets) => Json(secrets).into_response(),
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

async fn handle_mesh_target_secret_rotate(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(target_node_id): Path<String>,
    Json(body): Json<ConfirmSecretRefBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    if let Err(response) =
        require_exact_confirmation(&body.confirm_secret_ref, &body.secret_ref, "secret_ref")
    {
        return response;
    }
    match ipc_rotate_target_secret(
        &state.socket,
        &target_node_id,
        &body.secret_ref,
        &body.plaintext,
    )
    .await
    {
        Ok(ack) => {
            let event = json!({
                "type": "secret:rotated",
                "payload": { "target_node_id": target_node_id, "secret_ref": body.secret_ref }
            });
            let _ = state.tx.send(event.to_string());
            Json(ack).into_response()
        }
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

async fn handle_mesh_target_vault_add(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(target_node_id): Path<String>,
    Json(body): Json<ConfirmVaultEntryBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    if let Err(response) =
        require_exact_confirmation(&body.confirm_vault_name, &body.vault_name, "vault_name")
    {
        return response;
    }
    match ipc_add_target_vault_entry(
        &state.socket,
        &target_node_id,
        &body.vault_name,
        &body.plaintext,
        body.allowed_roles,
    )
    .await
    {
        Ok(ack) => {
            let event = json!({
                "type": "vault:entry-added",
                "payload": { "target_node_id": target_node_id, "vault_name": body.vault_name, "secret_ref": ack.secret_ref }
            });
            let _ = state.tx.send(event.to_string());
            (StatusCode::CREATED, Json(ack)).into_response()
        }
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

async fn handle_mesh_target_best_place_to_run(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(target_node_id): Path<String>,
    Query(query): Query<PlacementQuery>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match ipc_desktop_membrane_target_placement(
        &state.socket,
        &target_node_id,
        query.agent_id,
        query.role_name,
        query.tool_name,
        query.required_markers,
        query.prefer_locality,
    )
    .await
    {
        Ok(view) => Json(view).into_response(),
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

async fn handle_mesh_target_role_home_put(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path((target_node_id, agent_id, role_name)): Path<(String, String, String)>,
    Json(body): Json<SetRoleHomeBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    let confirmation_target = format!("{}:{}", agent_id, role_name);
    if let Err(response) = require_exact_confirmation(
        &body.confirm_role_binding,
        &confirmation_target,
        "agent_id:role_name",
    ) {
        return response;
    }
    match ipc_set_target_role_home(
        &state.socket,
        &target_node_id,
        &agent_id,
        &role_name,
        "orchestrator",
        body.target_hotel,
    )
    .await
    {
        Ok(ack) => {
            let event = json!({
                "type": "role:home-updated",
                "payload": { "target_node_id": target_node_id, "agent_id": agent_id, "role_name": role_name, "home_node": ack.home_node, "reason": body.reason }
            });
            let _ = state.tx.send(event.to_string());
            Json(ack).into_response()
        }
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
    let conversation_id = body
        .conversation_id
        .unwrap_or_else(|| format!("operator-chat:{operator_session_id}:{agent_id}"));

    match submit_operator_chat_turn(
        &state,
        &target_node_id,
        &agent_id,
        &operator_session_id,
        conversation_id,
        body.content,
        None,
        Vec::new(),
    )
    .await
    {
        Ok(accepted) => (StatusCode::ACCEPTED, Json(json!(accepted))).into_response(),
        Err(err) => (err.status, Json(json!({"error": err.message}))).into_response(),
    }
}

/// Error surfaced by [`submit_operator_chat_turn`] — carries the HTTP status
/// the REST handler maps it to (the edge WS path renders it as a TurnEvent).
struct OperatorChatSubmitError {
    status: StatusCode,
    message: String,
}

/// Shared submission path for operator chat turns: validates the mesh target,
/// then spawns [`stream_operator_chat_turn`] which drives the hotel IPC leg
/// (SubscribeInbox reply role + EmitTask to the target agent) and fans the
/// resulting turn events out on the broadcast bus. Used by the REST chat
/// handler and the edge WebSocket mux (serve/edge.rs).
/// Namespace a chat `conversation_id` into a hotel `session_id` scoped to the
/// caller.
///
/// The conversation id is chosen by the client and is deliberately opaque —
/// devices pick their own (`conv-e2e`, a UUID) and expect it echoed back. It
/// used to become the hotel `session_id` verbatim, which meant the id space was
/// shared by every caller: `/api/edge/sessions` hands each device the full list
/// of session ids, so a device could submit a turn addressed at another
/// device's — or the desktop operator's — conversation. That turn would be
/// written into the victim's history, and the reply, carrying that
/// conversation's accumulated context, delivered back to the submitter.
///
/// `operator_session_id` is derived server-side from the authenticated identity
/// (`edge:{node_id}` from the Hello handshake, or `desktop-membrane`), never
/// from client input, so prefixing it makes cross-caller addressing
/// *unrepresentable* rather than merely rejected.
///
/// Ids already carrying the caller's own prefix — the default-minted
/// `operator-chat:{marker}:{agent}` — pass through unchanged, so existing
/// conversations keep their session id.
fn scoped_operator_session_id(operator_session_id: &str, conversation_id: &str) -> String {
    let prefix = format!("operator-chat:{operator_session_id}:");
    if conversation_id.starts_with(&prefix) {
        conversation_id.to_string()
    } else {
        format!("{prefix}{conversation_id}")
    }
}

async fn submit_operator_chat_turn(
    state: &AppState,
    target_node_id: &str,
    agent_id: &str,
    operator_session_id: &str,
    conversation_id: String,
    content: String,
    message_kind: Option<String>,
    attachments: Vec<Value>,
) -> Result<OperatorChatAcceptedView, OperatorChatSubmitError> {
    let targets = ipc_desktop_membrane_targets(&state.socket)
        .await
        .map_err(|err| OperatorChatSubmitError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: err.to_string(),
        })?;
    let Some(target) = targets
        .iter()
        .find(|target| target.target_node_id == target_node_id)
    else {
        return Err(OperatorChatSubmitError {
            status: StatusCode::NOT_FOUND,
            message: format!(
                "mesh target [{target_node_id}] is not currently active in the registry"
            ),
        });
    };
    let Some(local_target) = targets.iter().find(|target| target.is_local) else {
        return Err(OperatorChatSubmitError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "local operator target is unavailable".into(),
        });
    };
    let local_node_id = local_target.target_node_id.clone();

    let session_id = scoped_operator_session_id(operator_session_id, &conversation_id);
    let turn_id = new_operator_chat_id("operator-chat-turn");
    let accepted = OperatorChatAcceptedView {
        accepted: true,
        target_node_id: target_node_id.to_string(),
        target_agent_id: agent_id.to_string(),
        operator_session_id: operator_session_id.to_string(),
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
    let target_node_id = target_node_id.to_string();
    let agent_id = agent_id.to_string();
    let operator_session_id = operator_session_id.to_string();
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
            content,
            message_kind,
            attachments,
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

    Ok(accepted)
}

// ── GET /api/event-log ────────────────────────────────────────────────────────

async fn handle_event_log(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(query): Query<EventLogQuery>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    match query_event_log(&state.db_path, limit, query.source.as_deref()) {
        Ok(entries) => Json(entries).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
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
    message_kind: Option<String>,
    attachments: Vec<Value>,
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
            task_json: {
                let mut task = json!({
                    "source": "operator_chat",
                    "transport": "operator_chat",
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": conversation_id,
                    "content": content,
                    "final_reply_to": local_node_id,
                    "final_reply_role": OPERATOR_CHAT_REPLY_ROLE,
                    "final_reply_guest_id": reply_guest_id
                });
                // "voice"/"audio" sets philote's had_voice_input, which lets
                // the agent's own voice_response_policy synthesize a persona
                // voice reply — the same marker membranes stamp on voice notes.
                if let Some(kind) = &message_kind {
                    task["message_kind"] = json!(kind);
                }
                if !attachments.is_empty() {
                    task["attachments"] = json!(attachments);
                }
                task.to_string()
            },
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
            // The hotel pushes unsolicited status broadcasts (e.g. MuninnStatus)
            // to every subscribed IPC client. They are not reply tasks — skip
            // them instead of failing the turn (model-router's runtime does the
            // same for its inbound leg).
            eprintln!(
                "philotic-web: operator chat reply leg skipping non-task IPC push: {inbound:?}"
            );
            continue;
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
            "voice_chunk" => {
                let _ = tx.send(
                    json!({
                        "type": "operator_chat:voice_chunk",
                        "payload": {
                            "target_node_id": target_node_id,
                            "target_agent_id": agent_id,
                            "operator_session_id": operator_session_id,
                            "conversation_id": payload.get("chat_id").and_then(Value::as_str).unwrap_or(&conversation_id),
                            "session_id": payload.get("session_id").and_then(Value::as_str).unwrap_or(&session_id),
                            "turn_id": payload.get("turn_id").and_then(Value::as_str).unwrap_or(&turn_id),
                            "content": payload.get("content").and_then(Value::as_str).unwrap_or_default(),
                            "audio_artifact": payload.get("audio_artifact").cloned().unwrap_or(Value::Null),
                            "chunk_seq": payload.get("chunk_seq").cloned().unwrap_or(Value::Null),
                            "is_final": payload.get("is_final").cloned().unwrap_or(Value::Null)
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
                            "content": payload.get("content").and_then(Value::as_str).unwrap_or_default(),
                            // Synthesized persona-voice replies ride the same
                            // send_reply payload as an inline audio envelope;
                            // forward it so edge/WS consumers can play it.
                            "audio_artifact": payload.get("audio_artifact").cloned().unwrap_or(Value::Null),
                            "send_text_caption": payload.get("send_text_caption").cloned().unwrap_or(Value::Null)
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

fn query_event_log(
    db_path: &PathBuf,
    limit: usize,
    source_filter: Option<&str>,
) -> Result<Vec<EventLogEntry>> {
    let normalized_source = source_filter.map(|value| value.trim().to_ascii_lowercase());
    let router_limit = limit.max(20);
    let mesh_limit = limit.max(20);
    let mut entries = Vec::new();

    if normalized_source
        .as_deref()
        .map(|value| value == "router")
        .unwrap_or(true)
    {
        let router_path = router_trace_path(db_path);
        entries.extend(query_router_event_log(&router_path, router_limit)?);
    }

    if normalized_source
        .as_deref()
        .map(|value| value == "mesh")
        .unwrap_or(true)
    {
        entries.extend(query_mesh_event_log(db_path, mesh_limit)?);
    }

    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp).then_with(|| a.id.cmp(&b.id)));
    entries.truncate(limit);
    Ok(entries)
}

fn router_trace_path(db_path: &PathBuf) -> PathBuf {
    db_path
        .parent()
        .map(|dir| dir.join("router_traces.db"))
        .unwrap_or_else(|| PathBuf::from("router_traces.db"))
}

fn query_router_event_log(path: &PathBuf, limit: usize) -> Result<Vec<EventLogEntry>> {
    if !path.exists() {
        return Ok(vec![]);
    }

    let conn = Connection::open(path)?;
    let mut stmt = conn.prepare(
        "SELECT trace_id, agent_id, session_id, turn_id, provider_id, model_id,
                task_kind, outcome, failure_code, latency_ms, token_count, timestamp
         FROM router_traces ORDER BY timestamp DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |row| {
        let trace_id: String = row.get(0)?;
        let agent_id: String = row.get(1)?;
        let session_id: String = row.get(2)?;
        let turn_id: String = row.get(3)?;
        let provider_id: String = row.get(4)?;
        let model_id: Option<String> = row.get(5)?;
        let task_kind: String = row.get(6)?;
        let outcome: String = row.get(7)?;
        let failure_code: Option<String> = row.get(8)?;
        let latency_ms: Option<i64> = row.get(9)?;
        let token_count: Option<i64> = row.get(10)?;
        let timestamp: i64 = row.get(11)?;
        Ok(EventLogEntry {
            id: trace_id,
            timestamp: timestamp as u64,
            source: "router".into(),
            event_type: format!("router.{outcome}"),
            summary: summarize_router_event(
                &task_kind,
                &provider_id,
                &outcome,
                failure_code.as_deref(),
            ),
            details: json!({
                "agent_id": agent_id,
                "session_id": session_id,
                "turn_id": turn_id,
                "provider_id": provider_id,
                "model_id": model_id,
                "task_kind": task_kind,
                "outcome": outcome,
                "failure_code": failure_code,
                "latency_ms": latency_ms.map(|value| value as u64),
                "token_count": token_count.map(|value| value as u64),
            }),
        })
    })?;

    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    Ok(entries)
}

fn query_mesh_event_log(path: &PathBuf, limit: usize) -> Result<Vec<EventLogEntry>> {
    let conn = Connection::open(path)?;
    let mut stmt = conn.prepare(
        "SELECT event_id, source_node_id, target_node_id, source_agent_id, target_agent_id,
                kind, corr_id, attempt, created_at, expires_at, payload_type, payload_json
         FROM mesh_events ORDER BY created_at DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |row| {
        let event_id: String = row.get(0)?;
        let source_node_id: String = row.get(1)?;
        let target_node_id: Option<String> = row.get(2)?;
        let source_agent_id: String = row.get(3)?;
        let target_agent_id: Option<String> = row.get(4)?;
        let kind: String = row.get(5)?;
        let corr_id: String = row.get(6)?;
        let attempt: i64 = row.get(7)?;
        let created_at: i64 = row.get(8)?;
        let expires_at: Option<i64> = row.get(9)?;
        let payload_type: String = row.get(10)?;
        let payload_json: String = row.get(11)?;
        let parsed_payload =
            serde_json::from_str::<Value>(&payload_json).unwrap_or(Value::String(payload_json));
        Ok(EventLogEntry {
            id: event_id,
            timestamp: created_at as u64,
            source: "mesh".into(),
            event_type: format!("mesh.{kind}"),
            summary: summarize_mesh_event(&kind, &source_node_id, target_node_id.as_deref()),
            details: json!({
                "source_node_id": source_node_id,
                "target_node_id": target_node_id,
                "source_agent_id": source_agent_id,
                "target_agent_id": target_agent_id,
                "kind": kind,
                "corr_id": corr_id,
                "attempt": attempt as u64,
                "expires_at": expires_at.map(|value| value as u64),
                "payload_type": payload_type,
                "payload": parsed_payload,
            }),
        })
    })?;

    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    Ok(entries)
}

fn summarize_router_event(
    task_kind: &str,
    provider_id: &str,
    outcome: &str,
    failure_code: Option<&str>,
) -> String {
    match (outcome, failure_code) {
        ("failure", Some(code)) if !code.is_empty() => {
            format!("{task_kind} via {provider_id} failed ({code})")
        }
        ("failure", _) => format!("{task_kind} via {provider_id} failed"),
        ("tool_call", _) => format!("{task_kind} via {provider_id} requested tool call"),
        _ => format!("{task_kind} via {provider_id} {outcome}"),
    }
}

fn summarize_mesh_event(kind: &str, source_node_id: &str, target_node_id: Option<&str>) -> String {
    match target_node_id {
        Some(target) if !target.is_empty() => format!("{kind} {source_node_id} -> {target}"),
        _ => format!("{kind} from {source_node_id}"),
    }
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
    #[serde(default)]
    silent_ok: bool,
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
        silent_ok: body.silent_ok,
        // Newly registered jobs always want isolated cron sessions; the
        // `RegisterCronJob` IPC handler re-asserts this regardless, but
        // setting it here too keeps the constructed value honest.
        session_target: ansible_mesh_core::cron::CronSessionTarget::Isolated,
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
    let mut out = serde_json::Map::new();
    for key in OPERATOR_REMOTE_CONFIG_KEYS {
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
    // Return presence only — never the token value, not even a prefix.
    //
    // This previously surfaced the first 8 characters as a "hint". A Telegram
    // bot token is `<numeric bot id>:<secret>`, so those leading characters are
    // the identifying half of the credential, not an opaque fragment. Every
    // sibling config route reports a boolean and a ref; this one now matches.
    // (The old slice was also a latent panic: `&s[..8]` splits a non-ASCII
    // value mid-codepoint.)
    let token_ref = match ipc_get_config(&state.socket, "telegram_bot_token").await {
        Ok(Some(_)) => Some("(set)".to_string()),
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

// ── GET /api/config/oidc ──────────────────────────────────────────────────────

async fn handle_config_oidc(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    let resolved = match load_oidc_resolved_config(&state.socket, Some(&headers)).await {
        Ok(config) => config,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response();
        }
    };

    Json(OidcConfigView {
        public_base_url: resolved.public_base_url.clone(),
        public_base_source: resolved.public_base_source.clone(),
        providers: vec![
            OidcProviderConfigView {
                provider: "google".into(),
                label: "Google".into(),
                client_id: resolved.google.client_id.clone(),
                client_id_source: oidc_value_source(
                    oidc_config_string(&state.socket, "oidc_google_client_id")
                        .await
                        .ok()
                        .flatten()
                        .is_some(),
                    env_trimmed("PHILOTIC_OIDC_GOOGLE_CLIENT_ID").is_some(),
                ),
                secret_ref: resolved.google.client_secret_ref.clone(),
                secret_ref_source: oidc_value_source(
                    resolved.google.client_secret_ref.is_some(),
                    false,
                ),
                secret_configured: resolved.google.client_secret_ref.is_some(),
                callback_url: format!("{}/auth/oidc/google/callback", resolved.public_base_url),
            },
            OidcProviderConfigView {
                provider: "github".into(),
                label: "GitHub".into(),
                client_id: resolved.github.client_id.clone(),
                client_id_source: oidc_value_source(
                    oidc_config_string(&state.socket, "oidc_github_client_id")
                        .await
                        .ok()
                        .flatten()
                        .is_some(),
                    env_trimmed("PHILOTIC_OIDC_GITHUB_CLIENT_ID").is_some(),
                ),
                secret_ref: resolved.github.client_secret_ref.clone(),
                secret_ref_source: oidc_value_source(
                    resolved.github.client_secret_ref.is_some(),
                    false,
                ),
                secret_configured: resolved.github.client_secret_ref.is_some(),
                callback_url: format!("{}/auth/oidc/github/callback", resolved.public_base_url),
            },
        ],
    })
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
// (vault name → secret_ref) plus known config-key secret refs.
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
    let mut named_refs = vec![
        ("gemini_oauth_access_token", "gemini_oauth_access_token_ref"),
        (
            "gemini_oauth_refresh_token",
            "gemini_oauth_refresh_token_ref",
        ),
        ("oidc_google_client_secret", "oidc_google_client_secret_ref"),
        ("oidc_github_client_secret", "oidc_github_client_secret_ref"),
        ("telegram_bot_token", "telegram_bot_token"),
    ];
    named_refs.extend(
        provider_key_specs()
            .iter()
            .map(|spec| (spec.vault_name, spec.api_key_ref_key)),
    );
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
const BASE_MUTABLE_CONFIG_KEYS: &[&str] = &[
    "telegram_bot_token",
    "execution_host",
    "vault_registry",
    "oidc_public_base_url",
    "oidc_google_client_id",
    "oidc_google_client_secret_ref",
    "oidc_github_client_id",
    "oidc_github_client_secret_ref",
    OIDC_SUBJECT_ALLOWLIST_KEY,
];

#[derive(serde::Deserialize)]
struct SetConfigBody {
    value: Value,
}

#[derive(serde::Deserialize)]
struct ProviderConfigureBody {
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    default_model: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    embedding_model: Option<String>,
    #[serde(default)]
    fallback_models: Vec<String>,
    #[serde(default)]
    route: Option<String>,
}

#[derive(serde::Deserialize)]
struct CreateMeshInviteBody {
    mesh_host: String,
    #[serde(default)]
    ttl_secs: Option<u64>,
}

async fn handle_mesh_invite_create(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<CreateMeshInviteBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    if body.mesh_host.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "mesh_host must not be empty"})),
        )
            .into_response();
    }
    match ipc_create_mesh_invite(
        &state.socket,
        state.hotel.as_str(),
        body.mesh_host.trim(),
        body.ttl_secs,
    )
    .await
    {
        Ok(data) => Json(json!({
            "ok": true,
            "hotel_name": state.hotel.as_str(),
            "file_name": format!("{}-mesh-invite.json", state.hotel.as_str()),
            "mesh_host": body.mesh_host.trim(),
            "invite": data,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
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
    if !is_mutable_config_key(&key) {
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

fn is_mutable_config_key(key: &str) -> bool {
    BASE_MUTABLE_CONFIG_KEYS.contains(&key) || provider_key_matches_mutable_config(key)
}

fn provider_key_matches_mutable_config(key: &str) -> bool {
    provider_key_specs().iter().any(|spec| {
        [
            Some(spec.api_key_ref_key),
            spec.default_model_key,
            spec.base_url_key,
            spec.embedding_model_key,
            spec.fallback_models_key,
            spec.route_key,
        ]
        .into_iter()
        .flatten()
        .any(|provider_key| provider_key == key)
    })
}

async fn handle_provider_configs(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    let mut providers = Vec::new();
    for spec in provider_key_specs() {
        match provider_config_view(&state, spec).await {
            Ok(view) => providers.push(view),
            Err(err) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": err.to_string()})),
                )
                    .into_response()
            }
        }
    }
    Json(json!({ "providers": providers })).into_response()
}

async fn handle_model_catalog(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    match model_catalog_view(&state.db_path) {
        Ok(catalog) => Json(json!({
            "catalog": catalog,
            "routing_effect": "none-read-only-projection",
        }))
        .into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

fn model_catalog_view(db_path: &PathBuf) -> Result<Vec<ModelCatalogProjection>> {
    let storage = SqliteGraphStorage::open(db_path)?;
    let graph = GraphDomain::new(Arc::new(storage.adapter()));
    let profiles = graph.list_model_profiles()?;
    Ok(project_model_catalog(&profiles))
}

async fn handle_provider_config_get(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(provider): Path<String>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    let Some(spec) = provider_key_spec(&provider) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown provider '{provider}'")})),
        )
            .into_response();
    };
    match provider_config_view(&state, spec).await {
        Ok(view) => Json(view).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": err.to_string()})),
        )
            .into_response(),
    }
}

async fn handle_provider_configure(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Json(body): Json<ProviderConfigureBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    let Some(spec) = provider_key_spec(&provider) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("unknown provider '{provider}'")})),
        )
            .into_response();
    };

    let mut changed_keys = Vec::new();
    let mut secret_ref = match ipc_get_config(&state.socket, spec.api_key_ref_key).await {
        Ok(Some(Value::String(value))) => Some(value),
        _ => None,
    };

    if let Some(api_key) = body
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        match ipc_add_vault_entry(
            &state.socket,
            spec.vault_name,
            api_key,
            spec.allowed_roles
                .iter()
                .map(|role| role.to_string())
                .collect(),
        )
        .await
        {
            Ok(new_secret_ref) => {
                secret_ref = Some(new_secret_ref.clone());
                if let Err(err) = ipc_set_config(
                    &state.socket,
                    spec.api_key_ref_key,
                    &serde_json::to_string(&new_secret_ref).unwrap_or_else(|_| "null".into()),
                )
                .await
                {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({"error": err.to_string()})),
                    )
                        .into_response();
                }
                changed_keys.push(spec.api_key_ref_key);
            }
            Err(err) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": err.to_string()})),
                )
                    .into_response()
            }
        }
    }

    let config_updates = [
        (
            spec.default_model_key,
            body.default_model.as_deref().map(str::trim),
        ),
        (spec.base_url_key, body.base_url.as_deref().map(str::trim)),
        (
            spec.embedding_model_key,
            body.embedding_model.as_deref().map(str::trim),
        ),
        (spec.route_key, body.route.as_deref().map(str::trim)),
    ];
    for (key, value) in config_updates {
        let Some(key) = key else { continue };
        let Some(value) = value.filter(|value| !value.is_empty()) else {
            continue;
        };
        let value_json = match serde_json::to_string(value) {
            Ok(value_json) => value_json,
            Err(err) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": err.to_string()})),
                )
                    .into_response()
            }
        };
        if let Err(err) = ipc_set_config(&state.socket, key, &value_json).await {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": err.to_string()})),
            )
                .into_response();
        }
        changed_keys.push(key);
    }

    if let Some(key) = spec.fallback_models_key {
        let fallback_models: Vec<String> = body
            .fallback_models
            .iter()
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty())
            .collect();
        if !fallback_models.is_empty() {
            let value_json = match serde_json::to_string(&fallback_models) {
                Ok(value_json) => value_json,
                Err(err) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": err.to_string()})),
                    )
                        .into_response()
                }
            };
            if let Err(err) = ipc_set_config(&state.socket, key, &value_json).await {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({"error": err.to_string()})),
                )
                    .into_response();
            }
            changed_keys.push(key);
        }
    }

    let event = json!({
        "type": "provider-config:updated",
        "payload": { "provider": spec.provider, "changed_keys": changed_keys }
    });
    let _ = state.tx.send(event.to_string());
    Json(json!({
        "ok": true,
        "provider": spec.provider,
        "secret_configured": secret_ref.is_some(),
        "secret_ref": secret_ref,
        "changed_keys": changed_keys,
    }))
    .into_response()
}

async fn provider_config_view(state: &AppState, spec: &ProviderKeySpec) -> anyhow::Result<Value> {
    let secret_ref = ipc_config_string(&state.socket, spec.api_key_ref_key).await?;
    let default_model = optional_config_string(&state.socket, spec.default_model_key).await?;
    let base_url = optional_config_string(&state.socket, spec.base_url_key).await?;
    let embedding_model = optional_config_string(&state.socket, spec.embedding_model_key).await?;
    let route = optional_config_string(&state.socket, spec.route_key).await?;
    let fallback_models = if let Some(key) = spec.fallback_models_key {
        ipc_get_config(&state.socket, key)
            .await?
            .and_then(|raw| {
                raw.as_array().map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect::<Vec<_>>()
                })
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    Ok(json!({
        "provider": spec.provider,
        "display_name": spec.display_name,
        "secret_configured": secret_ref.is_some(),
        "secret_ref": secret_ref,
        "api_key_ref_key": spec.api_key_ref_key,
        "default_model": default_model,
        "default_model_key": spec.default_model_key,
        "base_url": base_url,
        "base_url_key": spec.base_url_key,
        "embedding_model": embedding_model,
        "embedding_model_key": spec.embedding_model_key,
        "fallback_models": fallback_models,
        "fallback_models_key": spec.fallback_models_key,
        "route": route,
        "route_key": spec.route_key,
        "defaults": {
            "default_model": spec.default_model,
            "base_url": spec.default_base_url,
        },
        "allowed_roles": spec.allowed_roles,
    }))
}

async fn optional_config_string(socket: &str, key: Option<&str>) -> anyhow::Result<Option<String>> {
    let Some(key) = key else {
        return Ok(None);
    };
    ipc_config_string(socket, key).await
}

async fn ipc_config_string(socket: &str, key: &str) -> anyhow::Result<Option<String>> {
    Ok(ipc_get_config(socket, key)
        .await?
        .and_then(|raw| raw.as_str().map(str::to_string)))
}

// ── POST /api/secrets/rotate ──────────────────────────────────────────────────

async fn handle_secret_rotate(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<ConfirmSecretRefBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    if let Err(response) =
        require_exact_confirmation(&body.confirm_secret_ref, &body.secret_ref, "secret_ref")
    {
        return response;
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

#[derive(Clone, Debug, serde::Deserialize)]
struct PlacementQuery {
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    role_name: Option<String>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    required_markers: Vec<String>,
    #[serde(default)]
    prefer_locality: bool,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct SetRoleHomeBody {
    #[serde(default)]
    target_hotel: Option<String>,
    reason: String,
    confirm_role_binding: String,
}

async fn handle_vault_add(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<ConfirmVaultEntryBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    if let Err(response) =
        require_exact_confirmation(&body.confirm_vault_name, &body.vault_name, "vault_name")
    {
        return response;
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
    import_workspace: Option<String>,
    #[serde(default)]
    default_toolset: Option<Vec<String>>,
    #[serde(default)]
    default_skillset: Option<Vec<String>>,
    #[serde(default)]
    response_route_policy: Option<ResponseRoutePolicyBody>,
}

#[derive(serde::Deserialize)]
struct ResponseRoutePolicyBody {
    #[serde(default)]
    default_route: Option<String>,
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
        body.import_workspace,
        body.default_toolset,
        body.default_skillset,
        body.response_route_policy.and_then(|policy| {
            policy
                .default_route
                .map(|default_route| ResponseRoutePolicyView { default_route })
        }),
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

// ── GET /api/user-profile ─────────────────────────────────────────────────────

async fn handle_user_profile_get(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    let hotel_name = state.hotel.as_ref().clone();
    match ipc_get_user_profile(&state.socket, &hotel_name).await {
        Ok(profile) => Json(profile).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ── PATCH /api/user-profile ───────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct PatchUserProfileBody {
    #[serde(default)]
    timezone: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
}

async fn handle_user_profile_patch(
    headers: HeaderMap,
    State(state): State<AppState>,
    Json(body): Json<PatchUserProfileBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    let hotel_name = state.hotel.as_ref().clone();
    match ipc_patch_user_profile(&state.socket, &hotel_name, body.timezone, body.display_name).await
    {
        Ok(profile) => {
            let event = json!({ "type": "user_profile:updated" });
            let _ = state.tx.send(event.to_string());
            Json(profile).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn ipc_get_user_profile(socket: &str, hotel_name: &str) -> Result<Value> {
    let mut client = connect_management_client(socket, "philotic-web-user-profile").await?;
    match client
        .send_request(IpcRequest::GetUserProfile {
            hotel_name: hotel_name.to_string(),
        })
        .await?
    {
        IpcResponse::UserProfileData(p) => {
            Ok(json!({ "timezone": p.timezone, "display_name": p.display_name }))
        }
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected get_user_profile response: {other:?}")),
    }
}

async fn ipc_patch_user_profile(
    socket: &str,
    hotel_name: &str,
    timezone: Option<String>,
    display_name: Option<String>,
) -> Result<Value> {
    let mut client = connect_management_client(socket, "philotic-web-user-profile").await?;
    match client
        .send_request(IpcRequest::PatchUserProfile {
            hotel_name: hotel_name.to_string(),
            timezone,
            display_name,
        })
        .await?
    {
        IpcResponse::UserProfileData(p) => {
            Ok(json!({ "timezone": p.timezone, "display_name": p.display_name }))
        }
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected patch_user_profile response: {other:?}")),
    }
}

async fn load_operator_user_settings_view(
    state: &AppState,
    session: &OperatorSessionRecord,
) -> Result<OperatorUserSettingsView> {
    let user = resolve_operator_user(&state.db_path, state.hotel.as_ref(), &session.user_id)?
        .context("operator user record missing")?;
    let timezone = match ipc_get_user_profile(&state.socket, state.hotel.as_ref()).await {
        Ok(profile) => profile
            .get("timezone")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        Err(_) => None,
    };
    let external_identity_links =
        list_external_identity_links_for_user(&state.db_path, &session.user_id)?;
    let projected_identity = load_projected_user_identity_for_user(
        &state.db_path,
        state.hotel.as_ref(),
        &session.user_id,
    )?;
    Ok(OperatorUserSettingsView {
        user_id: user.user_id,
        mesh_principal_id: user.mesh_principal_id,
        display_name: user.display_name,
        preferred_name: user.preferred_name,
        primary_email: user.primary_email,
        timezone,
        home_hotel: user.home_hotel,
        status: user.status,
        onboarding_state: user.onboarding_state,
        external_identity_links,
        projected_identity,
    })
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
    import_workspace: Option<String>,
    default_toolset: Option<Vec<String>>,
    default_skillset: Option<Vec<String>>,
    response_route_policy: Option<ResponseRoutePolicyView>,
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
            import_workspace,
            default_toolset,
            default_skillset,
            response_route_policy,
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
            // This endpoint doesn't expose ladder editing yet; `None` now means
            // "preserve" server-side (see ConfigureRole fix), so the patch no
            // longer silently wipes a DB-edited fallback ladder.
            fallback_tiers: None,
            // Same preserve-on-None contract; this endpoint doesn't expose
            // per-agent model-binding editing yet either.
            model_bindings: None,
            content_policy: None,
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
    if let Err(response) = require_exact_confirmation(&body.confirm_guest_id, &guest_id, "guest_id")
    {
        return response;
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
    Json(body): Json<ConfirmGuestActionBody>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    if let Err(response) = require_exact_confirmation(&body.confirm_guest_id, &guest_id, "guest_id")
    {
        return response;
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

async fn ipc_list_operator_sessions(
    socket: &str,
    target_agent_id: Option<String>,
    limit: Option<u32>,
) -> Result<Vec<OperatorSessionView>> {
    let mut client = connect_management_client(socket, "philotic-web-edge-sessions").await?;
    match client
        .send_request(IpcRequest::ListOperatorSessions {
            target_agent_id,
            limit,
        })
        .await?
    {
        IpcResponse::OperatorSessionList { operator_sessions } => Ok(operator_sessions),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected operator session list response: {other:?}"
        )),
    }
}

async fn ipc_list_session_turns(
    socket: &str,
    session_id: String,
    limit: Option<u32>,
    before_turn_id: Option<String>,
) -> Result<Vec<SessionTurnView>> {
    let mut client = connect_management_client(socket, "philotic-web-edge-session-turns").await?;
    match client
        .send_request(IpcRequest::ListSessionTurns {
            session_id,
            limit,
            before_turn_id,
        })
        .await?
    {
        IpcResponse::SessionTurnList { session_turns, .. } => Ok(session_turns),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected session turn list response: {other:?}")),
    }
}

/// Round-trip a read-only life-graph datasource tool over hotel IPC: emit an
/// `execute_tool` task at the local `life-graph-runner` role and await its
/// `datasource_response` push. Mirrors the request/reply discipline of
/// `philotic-client`'s `life_graph_ipc_smoke_driver`: the `EmitTask` ack is
/// only a routing ack — the result arrives as a separate `InboundTask`,
/// correlated here by `turn_id`. Each call subscribes a unique reply role so
/// concurrent edge requests can never drain each other's replies.
async fn ipc_life_graph_datasource_call(
    socket: &str,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value> {
    let targets = ipc_desktop_membrane_targets(socket).await?;
    let local_node = targets
        .iter()
        .find(|t| t.is_local)
        .map(|t| t.target_node_id.clone())
        .ok_or_else(|| anyhow!("no local operator target for life-graph routing"))?;

    let reply_guest_id = new_operator_chat_id("edge-lifegraph");
    let reply_role = format!("management.life-graph.reply.{reply_guest_id}");
    let turn_id = new_operator_chat_id("lifegraph-turn");

    let mut client = connect_client_with_identity(
        socket,
        GuestIdentity {
            guest_id: reply_guest_id.clone(),
            role: reply_role.clone(),
            supported_tools: vec![],
        },
    )
    .await?;
    match client
        .send_request(IpcRequest::SubscribeInbox {
            role: reply_role.clone(),
        })
        .await?
    {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("life-graph reply subscription refused: {other:?}"),
    }

    let task_json = serde_json::json!({
        "action": "execute_tool",
        "tool_name": tool_name,
        "arguments": arguments,
        "session_id": format!("edge-lifegraph:{reply_guest_id}"),
        "turn_id": turn_id,
        "chat_id": "",
        "agent_id": reply_guest_id,
        "reply_to": local_node,
        "reply_role": reply_role,
    })
    .to_string();
    match client
        .send_request(IpcRequest::EmitTask {
            target_node: local_node.clone(),
            target_role: "life-graph-runner".into(),
            target_guest_id: None,
            task_json,
        })
        .await?
    {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("life-graph task emit refused: {other:?}"),
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            bail!("timed out awaiting {tool_name} datasource reply");
        }
        let pushed = tokio::time::timeout(remaining, client.recv_task())
            .await
            .map_err(|_| anyhow!("timed out awaiting {tool_name} datasource reply"))??;
        // Skip unrelated pushes (MuninnStatus, apartment updates, stale
        // replies) — only this call's datasource_response terminates the loop.
        let IpcResponse::InboundTask { task_json, .. } = pushed else {
            continue;
        };
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&task_json) else {
            continue;
        };
        if payload.get("action").and_then(serde_json::Value::as_str) != Some("datasource_response")
        {
            continue;
        }
        if payload.get("turn_id").and_then(serde_json::Value::as_str) != Some(turn_id.as_str()) {
            continue;
        }
        if let Some(error) = payload.get("error").filter(|e| !e.is_null()) {
            bail!("life-graph {tool_name} failed: {error}");
        }
        return Ok(payload
            .get("result")
            .and_then(|r| r.get("data"))
            .cloned()
            .unwrap_or(serde_json::Value::Null));
    }
}

/// Round-trip a tool-runner tool and return its `content` string.
///
/// Sibling of [`ipc_life_graph_datasource_call`], but the two cannot be
/// merged: the life-graph runner replies with `action: "datasource_response"`
/// carrying `result.data`, while tool-runner replies with
/// `action: "tool_result"` carrying a flat `content` string, and they are
/// addressed by different roles (`life-graph-runner` vs `tool.<tool_name>`).
///
/// Used by the edge memory read plane (seam: apple-entity-index-plane,
/// Muninn extension) to reach `memory.recall.structured`.
async fn ipc_tool_runner_call(
    socket: &str,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<String> {
    let targets = ipc_desktop_membrane_targets(socket).await?;
    let local_node = targets
        .iter()
        .find(|t| t.is_local)
        .map(|t| t.target_node_id.clone())
        .ok_or_else(|| anyhow!("no local operator target for tool-runner routing"))?;

    let reply_guest_id = new_operator_chat_id("edge-tool");
    let reply_role = format!("management.tool.reply.{reply_guest_id}");
    let turn_id = new_operator_chat_id("tool-turn");

    let mut client = connect_client_with_identity(
        socket,
        GuestIdentity {
            guest_id: reply_guest_id.clone(),
            role: reply_role.clone(),
            supported_tools: vec![],
        },
    )
    .await?;
    match client
        .send_request(IpcRequest::SubscribeInbox {
            role: reply_role.clone(),
        })
        .await?
    {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("tool-runner reply subscription refused: {other:?}"),
    }

    let task_json = serde_json::json!({
        "action": "execute_tool",
        "tool_name": tool_name,
        "arguments": arguments,
        "session_id": format!("edge-tool:{reply_guest_id}"),
        "turn_id": turn_id,
        "chat_id": "",
        "agent_id": reply_guest_id,
        "reply_to": local_node,
        "reply_role": reply_role,
    })
    .to_string();
    match client
        .send_request(IpcRequest::EmitTask {
            target_node: local_node.clone(),
            target_role: format!("tool.{tool_name}"),
            target_guest_id: None,
            task_json,
        })
        .await?
    {
        IpcResponse::Standard { ok: true, .. } => {}
        other => bail!("tool-runner task emit refused: {other:?}"),
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            bail!("timed out awaiting {tool_name} tool reply");
        }
        let pushed = tokio::time::timeout(remaining, client.recv_task())
            .await
            .map_err(|_| anyhow!("timed out awaiting {tool_name} tool reply"))??;
        let IpcResponse::InboundTask { task_json, .. } = pushed else {
            continue;
        };
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&task_json) else {
            continue;
        };
        if payload.get("action").and_then(serde_json::Value::as_str) != Some("tool_result") {
            continue;
        }
        if payload.get("turn_id").and_then(serde_json::Value::as_str) != Some(turn_id.as_str()) {
            continue;
        }
        return Ok(payload
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string());
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

async fn ipc_desktop_membrane_target_components(
    socket: &str,
    target_node_id: &str,
) -> Result<OperatorTargetComponentInventoryView> {
    let mut client =
        connect_management_client(socket, "philotic-web-mesh-target-components").await?;
    match client
        .send_request(IpcRequest::QueryOperatorTargetComponents {
            target_node_id: target_node_id.to_string(),
        })
        .await?
    {
        IpcResponse::OperatorTargetComponentsView {
            operator_target_components,
        } => Ok(operator_target_components),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected desktop membrane target components response: {other:?}"
        )),
    }
}

async fn ipc_desktop_membrane_target_config(
    socket: &str,
    target_node_id: &str,
) -> Result<OperatorTargetConfigView> {
    let mut client = connect_management_client(socket, "philotic-web-mesh-target-config").await?;
    match client
        .send_request(IpcRequest::QueryOperatorTargetConfig {
            target_node_id: target_node_id.to_string(),
        })
        .await?
    {
        IpcResponse::OperatorTargetConfigView {
            operator_target_config,
        } => Ok(operator_target_config),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected desktop membrane target config response: {other:?}"
        )),
    }
}

async fn ipc_desktop_membrane_target_secrets(
    socket: &str,
    target_node_id: &str,
) -> Result<OperatorTargetSecretInventoryView> {
    let mut client = connect_management_client(socket, "philotic-web-mesh-target-secrets").await?;
    match client
        .send_request(IpcRequest::QueryOperatorTargetSecrets {
            target_node_id: target_node_id.to_string(),
        })
        .await?
    {
        IpcResponse::OperatorTargetSecretsView {
            operator_target_secrets,
        } => Ok(operator_target_secrets),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected desktop membrane target secrets response: {other:?}"
        )),
    }
}

async fn ipc_desktop_membrane_target_placement(
    socket: &str,
    target_node_id: &str,
    agent_id: Option<String>,
    role_name: Option<String>,
    tool_name: Option<String>,
    required_markers: Vec<String>,
    prefer_locality: bool,
) -> Result<OperatorTargetPlacementView> {
    let mut client =
        connect_management_client(socket, "philotic-web-mesh-target-placement").await?;
    match client
        .send_request(IpcRequest::QueryOperatorTargetPlacement {
            target_node_id: target_node_id.to_string(),
            agent_id,
            role_name,
            tool_name,
            required_markers,
            prefer_locality,
        })
        .await?
    {
        IpcResponse::OperatorTargetPlacementView {
            operator_target_placement,
        } => Ok(operator_target_placement),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected desktop membrane target placement response: {other:?}"
        )),
    }
}

async fn ipc_desktop_membrane_target_component(
    socket: &str,
    target_node_id: &str,
    guest_id: &str,
) -> Result<ComponentInventoryEntryView> {
    let components = ipc_desktop_membrane_target_components(socket, target_node_id).await?;
    components
        .components
        .into_iter()
        .find(|component| component.guest_id == guest_id)
        .ok_or_else(|| anyhow!("component not found"))
}

async fn ipc_set_target_config(
    socket: &str,
    target_node_id: &str,
    key: &str,
    value_json: &str,
) -> Result<OperatorTargetConfigMutationAckView> {
    let mut client = connect_management_client(socket, "philotic-web-target-config").await?;
    match client
        .send_request(IpcRequest::SetOperatorTargetConfig {
            target_node_id: target_node_id.to_string(),
            key: key.to_string(),
            value_json: value_json.to_string(),
        })
        .await?
    {
        IpcResponse::OperatorTargetConfigMutationAckView {
            operator_target_config_mutation,
        } => Ok(operator_target_config_mutation),
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected target config mutation response: {other:?}"
        )),
    }
}

async fn ipc_rotate_target_secret(
    socket: &str,
    target_node_id: &str,
    secret_ref: &str,
    plaintext: &str,
) -> Result<OperatorTargetSecretMutationAckView> {
    let mut client = connect_management_client(socket, "philotic-web-target-secrets").await?;
    match client
        .send_request(IpcRequest::RotateOperatorTargetSecret {
            target_node_id: target_node_id.to_string(),
            secret_ref: secret_ref.to_string(),
            plaintext: plaintext.to_string(),
        })
        .await?
    {
        IpcResponse::OperatorTargetSecretMutationAckView {
            operator_target_secret_mutation,
        } => Ok(operator_target_secret_mutation),
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected target secret rotation response: {other:?}"
        )),
    }
}

async fn ipc_add_target_vault_entry(
    socket: &str,
    target_node_id: &str,
    vault_name: &str,
    plaintext: &str,
    allowed_roles: Vec<String>,
) -> Result<OperatorTargetSecretMutationAckView> {
    let mut client = connect_management_client(socket, "philotic-web-target-secrets").await?;
    match client
        .send_request(IpcRequest::AddOperatorTargetVaultEntry {
            target_node_id: target_node_id.to_string(),
            vault_name: vault_name.to_string(),
            plaintext: plaintext.to_string(),
            allowed_roles,
        })
        .await?
    {
        IpcResponse::OperatorTargetSecretMutationAckView {
            operator_target_secret_mutation,
        } => Ok(operator_target_secret_mutation),
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected target vault entry response: {other:?}")),
    }
}

async fn ipc_set_target_role_home(
    socket: &str,
    target_node_id: &str,
    agent_id: &str,
    role_name: &str,
    calling_role: &str,
    target_hotel: Option<String>,
) -> Result<OperatorTargetRoleHomeAckView> {
    let mut client = connect_management_client(socket, "philotic-web-target-role-home").await?;
    match client
        .send_request(IpcRequest::SetOperatorTargetRoleHome {
            target_node_id: target_node_id.to_string(),
            agent_id: agent_id.to_string(),
            role_name: role_name.to_string(),
            calling_role: calling_role.to_string(),
            target_hotel,
        })
        .await?
    {
        IpcResponse::OperatorTargetRoleHomeAckView {
            operator_target_role_home,
        } => Ok(operator_target_role_home),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected target role home response: {other:?}")),
    }
}

async fn ipc_register_target_component(
    socket: &str,
    target_node_id: &str,
    manifest: ComponentManifest,
) -> Result<ComponentInventoryEntryView> {
    let guest_id = manifest.guest_id.clone();
    let mut client = connect_management_client(socket, "philotic-web-target-components").await?;
    match client
        .send_request(IpcRequest::RegisterOperatorTargetComponent {
            target_node_id: target_node_id.to_string(),
            manifest,
        })
        .await?
    {
        IpcResponse::ComponentInventory { components } => components
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("component mutation reply missing refreshed component"))
            .and_then(|value| serde_json::from_value(value).map_err(Into::into)),
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected register_target_component response: {other:?}"
        )),
    }
    .and_then(|component: ComponentInventoryEntryView| {
        if component.guest_id == guest_id {
            Ok(component)
        } else {
            Err(anyhow!("refreshed component reply guest mismatch"))
        }
    })
}

async fn ipc_set_target_component_active(
    socket: &str,
    target_node_id: &str,
    guest_id: &str,
    active: bool,
) -> Result<OperatorTargetComponentMutationAckView> {
    let mut client = connect_management_client(socket, "philotic-web-target-components").await?;
    match client
        .send_request(IpcRequest::SetOperatorTargetComponentActive {
            target_node_id: target_node_id.to_string(),
            guest_id: guest_id.to_string(),
            active,
        })
        .await?
    {
        IpcResponse::OperatorTargetComponentMutationAckView {
            operator_target_component_mutation,
        } => Ok(operator_target_component_mutation),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected set_target_component_active response: {other:?}"
        )),
    }
}

async fn ipc_restart_target_component(
    socket: &str,
    target_node_id: &str,
    guest_id: &str,
) -> Result<OperatorTargetComponentMutationAckView> {
    let mut client = connect_management_client(socket, "philotic-web-target-components").await?;
    match client
        .send_request(IpcRequest::RestartOperatorTargetComponent {
            target_node_id: target_node_id.to_string(),
            guest_id: guest_id.to_string(),
        })
        .await?
    {
        IpcResponse::OperatorTargetComponentMutationAckView {
            operator_target_component_mutation,
        } => Ok(operator_target_component_mutation),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected restart_target_component response: {other:?}"
        )),
    }
}

async fn ipc_remove_target_component(
    socket: &str,
    target_node_id: &str,
    guest_id: &str,
) -> Result<OperatorTargetComponentMutationAckView> {
    let mut client = connect_management_client(socket, "philotic-web-target-components").await?;
    match client
        .send_request(IpcRequest::RemoveOperatorTargetComponent {
            target_node_id: target_node_id.to_string(),
            guest_id: guest_id.to_string(),
        })
        .await?
    {
        IpcResponse::OperatorTargetComponentMutationAckView {
            operator_target_component_mutation,
        } => Ok(operator_target_component_mutation),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!(
            "unexpected remove_target_component response: {other:?}"
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

async fn ipc_create_mesh_invite(
    socket: &str,
    hotel_name: &str,
    mesh_host: &str,
    ttl_secs: Option<u64>,
) -> Result<Value> {
    let mut client = connect_management_client(socket, "philotic-web-mesh-invite").await?;
    match client
        .send_request(IpcRequest::CreateMeshInvite {
            hotel_name: hotel_name.to_string(),
            mesh_host: mesh_host.to_string(),
            ttl_secs,
        })
        .await?
    {
        IpcResponse::Standard {
            ok: true,
            data: Some(data),
            ..
        } => Ok(data),
        IpcResponse::Standard { message, .. } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected mesh invite response: {other:?}")),
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
            // Provider/API keys keep the DEF-065 default: kind == vault_name.
            secret_kind: None,
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

async fn ipc_list_components(socket: &str) -> Result<Vec<ComponentInventoryEntryView>> {
    let mut client = connect_management_client(socket, "philotic-web-components").await?;
    match client.send_request(IpcRequest::ListComponents {}).await? {
        IpcResponse::ComponentInventory { components } => components
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<ComponentInventoryEntryView>, _>>()
            .map_err(Into::into),
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected list_components response: {other:?}")),
    }
}

async fn ipc_get_component(socket: &str, guest_id: &str) -> Result<ComponentInventoryEntryView> {
    let components = ipc_list_components(socket).await?;
    components
        .into_iter()
        .find(|component| component.guest_id == guest_id)
        .ok_or_else(|| anyhow!("component not found"))
}

async fn ipc_register_component(
    socket: &str,
    manifest: ComponentManifest,
) -> Result<ComponentInventoryEntryView> {
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
            // Operator-initiated restart from the desktop/web UI — deliberate, never
            // budget-limited.
            reason: philotic_client::RestartReason::Operator,
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
            id: "model-controller-openrouter".into(),
            label: "OpenRouter Model Controller".into(),
            description: "OpenRouter-backed model controller for hosted fallback routing. API keys and model routing belong in hotel provider config, not component manifests.".into(),
            command: "model-controller-openrouter".into(),
            role: "model.openrouter".into(),
            env_fields: vec![],
            component_config_fields: vec![],
            dependencies: vec![
                ComponentTemplateDependencyView {
                    key: "openrouter_api_key_ref".into(),
                    label: "OpenRouter API Key Ref".into(),
                    location: "hotel-config".into(),
                    required: true,
                    secret: true,
                    vault_only: true,
                    help: "Configure through phil keys configure openrouter or the desktop provider config API. The raw key is stored in the hotel vault.".into(),
                },
                ComponentTemplateDependencyView {
                    key: "openrouter_default_model".into(),
                    label: "Default Model".into(),
                    location: "hotel-config".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    help: "Primary OpenRouter model id used when a task does not override provider options.".into(),
                },
                ComponentTemplateDependencyView {
                    key: "openrouter_fallback_models".into(),
                    label: "Fallback Models".into(),
                    location: "hotel-config".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    help: "Ordered model list passed to OpenRouter as the fallback chain.".into(),
                },
                ComponentTemplateDependencyView {
                    key: "openrouter_route".into(),
                    label: "Routing Mode".into(),
                    location: "hotel-config".into(),
                    required: false,
                    secret: false,
                    vault_only: false,
                    help: "OpenRouter route value, usually fallback when a fallback model list is configured.".into(),
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
            label: "Agent Graph (agent-datasource)".into(),
            description: "Per-agent cognitive graph guest (agent-datasource binary). The agent id is required; the graph DB path is optional unless you want an explicit storage location.".into(),
            command: "agent-datasource".into(),
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
    ]
}

/// Correlation id: turn ids, reply guest ids, and similar. 64 bits of CSPRNG
/// output is ample for uniqueness — these are not credentials. Use
/// [`new_secret_token`] for anything an attacker would want to guess.
fn new_operator_chat_id(prefix: &str) -> String {
    let mut rng = rand::thread_rng();
    let suffix = format!("{:016x}", rng.r#gen::<u64>());
    format!("{prefix}-{suffix}")
}

/// Unguessable token for values that gate access: the operator session bearer
/// and the OIDC `state` nonce.
///
/// Both were previously minted by `new_operator_chat_id`, i.e. 64 bits. That is
/// unpredictable (it is a CSPRNG) but thin for a credential — the session token
/// in particular is a bearer secret with an 8-hour life that appears in a
/// cookie and an `Authorization` header. 256 bits removes the question, and
/// costs nothing.
fn new_secret_token(prefix: &str) -> String {
    let bytes: [u8; 32] = rand::thread_rng().r#gen();
    format!("{prefix}-{}", hex::encode(bytes))
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

// ── GET /health ───────────────────────────────────────────────────────────────

/// Unauthenticated lightweight probe used by edge clients for latency racing.
/// Returns node identity + version only — no sensitive data, allowed on any tier.
async fn handle_health(State(state): State<AppState>) -> Response {
    Json(json!({
        "node_id": state.hotel.as_str(),
        "version": env!("CARGO_PKG_VERSION"),
    }))
    .into_response()
}

// ── GET /api/mesh/roster ──────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct MeshRosterHotelView {
    node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    /// philotic-web base URLs for this hotel (e.g. "http://100.79.239.64:7700"),
    /// raced client-side for the lowest-latency reachable endpoint.
    #[serde(default)]
    endpoints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
struct MeshRosterView {
    hotels: Vec<MeshRosterHotelView>,
    /// Full-fidelity mesh roster entries from the aiua GetMeshRoster IPC
    /// (roles, exposure ceiling, typed listener endpoints). Present only when
    /// `source` is "hotel-ipc".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    nodes: Vec<MeshRosterEntryView>,
    /// Where the roster came from: "hotel-ipc", "env", "config", or "unconfigured"
    source: String,
}

async fn handle_mesh_roster(headers: HeaderMap, State(state): State<AppState>) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }
    Json(load_mesh_roster(&state).await).into_response()
}

/// Single seam for roster sourcing — `handle_mesh_roster` only ever calls this.
///
/// Primary source is the aiua `GetMeshRoster` IPC (live self + registry peers).
/// The mesh does not yet advertise philotic-web base URLs in its perimeter
/// listeners, so per-hotel `endpoints` (raced client-side) are still overlaid
/// from the configured roster (PHILOTIC_WEB_ROSTER env / config `web_roster`)
/// by node_id; the typed listener endpoints ride along in `nodes`.
/// When the hotel IPC is unreachable the configured roster is served as-is.
async fn load_mesh_roster(state: &AppState) -> MeshRosterView {
    let (configured, configured_source) = configured_roster_hotels(state);
    match fetch_mesh_roster_ipc(&state.socket).await {
        Ok(entries) => {
            let mut hotels: Vec<MeshRosterHotelView> = entries
                .iter()
                .map(|entry| MeshRosterHotelView {
                    node_id: entry.node_id.clone(),
                    display_name: entry.display_name.clone(),
                    endpoints: configured
                        .iter()
                        .find(|hotel| hotel.node_id == entry.node_id)
                        .map(|hotel| hotel.endpoints.clone())
                        .unwrap_or_default(),
                })
                .collect();
            // Configured hotels the live registry has not (yet) seen stay listed.
            for hotel in &configured {
                if !hotels.iter().any(|known| known.node_id == hotel.node_id) {
                    hotels.push(hotel.clone());
                }
            }
            MeshRosterView {
                hotels,
                nodes: entries,
                source: "hotel-ipc".into(),
            }
        }
        Err(err) => {
            eprintln!(
                "philotic-web: GetMeshRoster IPC unavailable ({err:#}); serving {configured_source} roster"
            );
            MeshRosterView {
                hotels: configured,
                nodes: vec![],
                source: configured_source.into(),
            }
        }
    }
}

/// Live roster over the hotel IPC socket (aiua `GetMeshRoster`).
async fn fetch_mesh_roster_ipc(socket: &str) -> Result<Vec<MeshRosterEntryView>> {
    let mut client =
        connect_management_client(socket, &new_operator_chat_id("philotic-web-roster")).await?;
    match client.send_request(IpcRequest::GetMeshRoster).await? {
        IpcResponse::MeshRosterView { mesh_roster } => Ok(mesh_roster),
        other => bail!("unexpected mesh roster response: {other:?}"),
    }
}

/// The env/config-sourced roster chain (fallback + endpoint overlay source).
fn configured_roster_hotels(state: &AppState) -> (Vec<MeshRosterHotelView>, &'static str) {
    if let Some(raw) = env_trimmed("PHILOTIC_WEB_ROSTER") {
        match parse_mesh_roster_json(&raw) {
            Ok(hotels) => return (hotels, "env"),
            Err(err) => eprintln!("philotic-web: ignoring invalid PHILOTIC_WEB_ROSTER: {err:#}"),
        }
    }
    if let Some(value) =
        read_config_json(&state.config_path).and_then(|v| v.get("web_roster").cloned())
    {
        match parse_mesh_roster_value(&value) {
            Ok(hotels) => return (hotels, "config"),
            Err(err) => eprintln!("philotic-web: ignoring invalid config web_roster: {err:#}"),
        }
    }
    (vec![], "unconfigured")
}

fn parse_mesh_roster_json(raw: &str) -> Result<Vec<MeshRosterHotelView>> {
    let value: Value = serde_json::from_str(raw).context("mesh roster is not valid JSON")?;
    parse_mesh_roster_value(&value)
}

/// Accepts either a bare array of entries or an object wrapping it:
/// `[{"node_id": "...", "endpoints": [...]}]` or `{"hotels": [...]}`.
fn parse_mesh_roster_value(value: &Value) -> Result<Vec<MeshRosterHotelView>> {
    let entries = value.get("hotels").unwrap_or(value);
    serde_json::from_value(entries.clone()).context(
        "mesh roster must be a JSON array of {node_id, display_name?, endpoints[]} entries \
         (optionally wrapped in {\"hotels\": [...]})",
    )
}

// ── WebSocket /ws ─────────────────────────────────────────────────────────────

/// Upper bound on a single inbound WebSocket message, for both the operator
/// bus and the edge mux.
///
/// tungstenite's defaults are 64 MiB per message and 16 MiB per frame — sized
/// for file transfer, not for a control-plane socket whose largest legitimate
/// inbound frame is a small JSON command. A message is buffered in full before
/// it reaches `serde_json`, so the default lets one connection reserve tens of
/// megabytes at will. Outbound server pushes are unaffected by these limits.
const WS_MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const WS_MAX_FRAME_BYTES: usize = 256 * 1024;

async fn handle_ws(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if !check_auth(&headers, &state) {
        return unauthorized();
    }

    ws.max_message_size(WS_MAX_MESSAGE_BYTES)
        .max_frame_size(WS_MAX_FRAME_BYTES)
        .on_upgrade(move |socket| ws_handler(socket, state))
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
    read_config_json(config_path)
        .and_then(|v| {
            v.get("hotels")
                .and_then(|h| h.as_object())
                .and_then(|m| m.keys().next().cloned())
        })
        .unwrap_or_else(|| "default".to_string())
}

fn read_config_json(config_path: &PathBuf) -> Option<Value> {
    std::fs::read_to_string(config_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
}

fn current_operator_session(
    headers: &HeaderMap,
    state: &AppState,
) -> Option<OperatorSessionRecord> {
    let token = header_bearer_token(headers).or_else(|| cookie_token(headers, AUTH_COOKIE_NAME))?;
    resolve_operator_session(&state.db_path, token)
        .ok()
        .flatten()
}

fn ensure_operator_auth_tables(db_path: &PathBuf, hotel: &str) -> Result<()> {
    let conn = Connection::open(db_path)?;
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS operator_users (
            user_id TEXT PRIMARY KEY,
            mesh_principal_id TEXT NOT NULL,
            display_name TEXT NOT NULL,
            home_hotel TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS root_user_key_refs (
            user_id TEXT NOT NULL,
            key_purpose TEXT NOT NULL,
            vault_ref TEXT,
            public_fingerprint TEXT,
            rotation_state TEXT NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY (user_id, key_purpose)
        );

        CREATE TABLE IF NOT EXISTS operator_sessions (
            session_id TEXT PRIMARY KEY,
            session_token TEXT NOT NULL UNIQUE,
            user_id TEXT NOT NULL,
            display_name TEXT NOT NULL,
            issuing_hotel TEXT NOT NULL,
            surface_kind TEXT NOT NULL,
            posture TEXT NOT NULL,
            issued_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            status TEXT NOT NULL,
            auth_method TEXT NOT NULL,
            bootstrap_id TEXT
        );

        CREATE INDEX IF NOT EXISTS idx_operator_sessions_token
        ON operator_sessions(session_token);

        CREATE TABLE IF NOT EXISTS operator_auth_challenges (
            challenge_id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            intended_surface TEXT NOT NULL,
            auth_path TEXT NOT NULL,
            verifier_kind TEXT NOT NULL,
            verifier_hint TEXT,
            bind_label TEXT,
            challenge_nonce TEXT NOT NULL UNIQUE,
            exchange_secret TEXT,
            issued_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            status TEXT NOT NULL,
            consumed_at INTEGER
        );

        CREATE INDEX IF NOT EXISTS idx_operator_auth_challenges_nonce
        ON operator_auth_challenges(challenge_nonce);

        CREATE TABLE IF NOT EXISTS external_identity_links (
            link_id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            provider TEXT NOT NULL,
            provider_subject TEXT NOT NULL,
            email TEXT,
            login TEXT,
            display_name TEXT,
            verified_at INTEGER NOT NULL,
            last_seen_at INTEGER NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(provider, provider_subject)
        );

        CREATE INDEX IF NOT EXISTS idx_external_identity_links_user
        ON external_identity_links(user_id);
        ",
    )?;
    ensure_operator_user_column(&conn, "mesh_principal_id", "TEXT NOT NULL DEFAULT ''")?;
    ensure_operator_user_column(&conn, "preferred_name", "TEXT")?;
    ensure_operator_user_column(&conn, "primary_email", "TEXT")?;
    ensure_operator_user_column(
        &conn,
        "onboarding_state",
        "TEXT NOT NULL DEFAULT 'bootstrap'",
    )?;

    let now = now_epoch_secs();
    conn.execute(
        "INSERT INTO operator_users (user_id, mesh_principal_id, display_name, preferred_name, primary_email, home_hotel, status, onboarding_state, created_at, updated_at)
         VALUES (?1, ?2, ?3, NULL, NULL, ?4, 'active', 'bootstrap', ?5, ?5)
         ON CONFLICT(user_id) DO UPDATE SET
             home_hotel=excluded.home_hotel,
             mesh_principal_id = CASE
                 WHEN operator_users.mesh_principal_id = '' THEN excluded.mesh_principal_id
                 ELSE operator_users.mesh_principal_id
             END,
             updated_at=excluded.updated_at",
        rusqlite::params![
            default_operator_user_id(hotel),
            default_operator_principal_id(hotel),
            default_operator_display_name(),
            hotel,
            now,
        ],
    )?;

    let root_ref = detect_root_user_key_ref(hotel);
    conn.execute(
        "INSERT INTO root_user_key_refs
         (user_id, key_purpose, vault_ref, public_fingerprint, rotation_state, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(user_id, key_purpose) DO UPDATE SET
            vault_ref=excluded.vault_ref,
            public_fingerprint=excluded.public_fingerprint,
            rotation_state=excluded.rotation_state,
            updated_at=excluded.updated_at",
        rusqlite::params![
            default_operator_user_id(hotel),
            root_ref.key_purpose,
            root_ref.vault_ref,
            root_ref.public_fingerprint,
            root_ref.rotation_state,
            now,
        ],
    )?;

    Ok(())
}

fn ensure_operator_user_column(conn: &Connection, column: &str, definition: &str) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(operator_users)")?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if !names.iter().any(|name| name == column) {
        conn.execute(
            &format!("ALTER TABLE operator_users ADD COLUMN {column} {definition}"),
            [],
        )?;
    }
    Ok(())
}

fn resolve_operator_user(
    db_path: &PathBuf,
    hotel: &str,
    user_id: &str,
) -> Result<Option<OperatorUserRecord>> {
    ensure_operator_auth_tables(db_path, hotel)?;
    let conn = Connection::open(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT user_id, mesh_principal_id, display_name, preferred_name, primary_email, home_hotel, status, onboarding_state, created_at, updated_at
         FROM operator_users
         WHERE user_id = ?1",
    )?;
    stmt.query_row([user_id], |row| {
        Ok(OperatorUserRecord {
            user_id: row.get(0)?,
            mesh_principal_id: row.get(1)?,
            display_name: row.get(2)?,
            preferred_name: row.get(3)?,
            primary_email: row.get(4)?,
            home_hotel: row.get(5)?,
            status: row.get(6)?,
            onboarding_state: row.get(7)?,
            created_at: row.get(8)?,
            updated_at: row.get(9)?,
        })
    })
    .optional()
    .map_err(Into::into)
}

fn list_root_user_key_refs(
    db_path: &PathBuf,
    hotel: &str,
) -> Result<Vec<RootUserKeyRefStatusView>> {
    ensure_operator_auth_tables(db_path, hotel)?;
    let conn = Connection::open(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT user_id, key_purpose, vault_ref, public_fingerprint, rotation_state
         FROM root_user_key_refs
         WHERE user_id = ?1
         ORDER BY key_purpose",
    )?;
    let rows = stmt.query_map([default_operator_user_id(hotel)], |row| {
        let vault_ref: Option<String> = row.get(2)?;
        Ok(RootUserKeyRefStatusView {
            user_id: row.get(0)?,
            key_purpose: row.get(1)?,
            source_kind: root_user_key_source_kind(&vault_ref),
            vault_ref,
            public_fingerprint: row.get(3)?,
            rotation_state: row.get(4)?,
        })
    })?;

    Ok(rows.filter_map(Result::ok).collect())
}

fn list_external_identity_links(
    db_path: &PathBuf,
    hotel: &str,
) -> Result<Vec<ExternalIdentityLinkStatusView>> {
    ensure_operator_auth_tables(db_path, hotel)?;
    list_external_identity_links_for_user(db_path, &default_operator_user_id(hotel))
}

fn list_external_identity_links_for_user(
    db_path: &PathBuf,
    user_id: &str,
) -> Result<Vec<ExternalIdentityLinkStatusView>> {
    let conn = Connection::open(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT user_id, provider, provider_subject, email, login, display_name, verified_at, last_seen_at
         FROM external_identity_links
         WHERE user_id = ?1
         ORDER BY provider, provider_subject",
    )?;
    let rows = stmt.query_map([user_id], |row| {
        Ok(ExternalIdentityLinkStatusView {
            user_id: row.get(0)?,
            provider: row.get(1)?,
            provider_subject: row.get(2)?,
            email: row.get(3)?,
            login: row.get(4)?,
            display_name: row.get(5)?,
            verified_at: row.get(6)?,
            last_seen_at: row.get(7)?,
        })
    })?;

    Ok(rows.filter_map(Result::ok).collect())
}

fn list_external_identity_link_records_for_user(
    db_path: &PathBuf,
    user_id: &str,
) -> Result<Vec<ExternalIdentityLinkRecord>> {
    let conn = Connection::open(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT link_id, user_id, provider, provider_subject, email, login, display_name,
                verified_at, last_seen_at, created_at, updated_at
         FROM external_identity_links
         WHERE user_id = ?1
         ORDER BY provider, provider_subject",
    )?;
    let rows = stmt.query_map([user_id], |row| {
        Ok(ExternalIdentityLinkRecord {
            link_id: row.get(0)?,
            user_id: row.get(1)?,
            provider: row.get(2)?,
            provider_subject: row.get(3)?,
            email: row.get(4)?,
            login: row.get(5)?,
            display_name: row.get(6)?,
            verified_at: row.get(7)?,
            last_seen_at: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
        })
    })?;

    Ok(rows.filter_map(Result::ok).collect())
}

fn patch_operator_user_record(
    db_path: &PathBuf,
    hotel: &str,
    user_id: &str,
    display_name: Option<String>,
    preferred_name: Option<String>,
    primary_email: Option<String>,
    onboarding_state: Option<String>,
) -> Result<OperatorUserRecord> {
    let existing =
        resolve_operator_user(db_path, hotel, user_id)?.context("operator user record missing")?;
    let conn = Connection::open(db_path)?;
    let now = now_epoch_secs();
    let normalized_display_name =
        normalize_non_empty_option(display_name).unwrap_or_else(|| existing.display_name.clone());
    let normalized_preferred_name =
        normalize_optional_text(preferred_name).or(existing.preferred_name.clone());
    let normalized_primary_email =
        normalize_optional_text(primary_email).or(existing.primary_email.clone());
    let normalized_onboarding_state = normalize_non_empty_option(onboarding_state)
        .unwrap_or_else(|| existing.onboarding_state.clone());

    conn.execute(
        "UPDATE operator_users
         SET display_name = ?2,
             preferred_name = ?3,
             primary_email = ?4,
             onboarding_state = ?5,
             updated_at = ?6
         WHERE user_id = ?1",
        rusqlite::params![
            user_id,
            &normalized_display_name,
            &normalized_preferred_name,
            &normalized_primary_email,
            &normalized_onboarding_state,
            now,
        ],
    )?;
    conn.execute(
        "UPDATE operator_sessions
         SET display_name = ?2
         WHERE user_id = ?1 AND status = 'active'",
        rusqlite::params![user_id, &normalized_display_name],
    )?;

    if existing.mesh_principal_id.starts_with("local-user:") {
        if let Some(email) = normalized_primary_email.as_deref() {
            let _ = maybe_adopt_projected_identity_from_email(db_path, hotel, user_id, email)?;
        }
    }

    let updated = resolve_operator_user(db_path, hotel, user_id)?
        .context("operator user record disappeared")?;
    sync_projected_user_identity(db_path, hotel, user_id)?;
    Ok(updated)
}

fn issue_operator_session(
    db_path: &PathBuf,
    hotel: &str,
    display_name: &str,
    auth_method: &str,
    bootstrap_id: Option<String>,
) -> Result<OperatorSessionRecord> {
    ensure_operator_auth_tables(db_path, hotel)?;
    let conn = Connection::open(db_path)?;
    let now = now_epoch_secs();
    let expires_at = now + AUTH_COOKIE_MAX_AGE_SECS as i64;
    let session = OperatorSessionRecord {
        session_id: new_operator_chat_id("operator-session"),
        session_token: new_secret_token("operator-token"),
        user_id: default_operator_user_id(hotel),
        display_name: display_name.to_string(),
        issuing_hotel: hotel.to_string(),
        surface_kind: "desktop_membrane".into(),
        posture: "admin".into(),
        issued_at: now,
        expires_at,
        status: "active".into(),
        auth_method: auth_method.into(),
        bootstrap_id,
    };
    conn.execute(
        "INSERT INTO operator_sessions
        (session_id, session_token, user_id, display_name, issuing_hotel, surface_kind, posture, issued_at, expires_at, status, auth_method, bootstrap_id)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        rusqlite::params![
            &session.session_id,
            &session.session_token,
            &session.user_id,
            &session.display_name,
            &session.issuing_hotel,
            &session.surface_kind,
            &session.posture,
            session.issued_at,
            session.expires_at,
            &session.status,
            &session.auth_method,
            &session.bootstrap_id,
        ],
    )?;

    sync_projected_user_identity(db_path, hotel, &session.user_id)?;
    Ok(session)
}

fn issue_operator_auth_challenge(
    db_path: &PathBuf,
    hotel: &str,
    intended_surface: &str,
    auth_path: &str,
    verifier_kind: &str,
    verifier_hint: Option<String>,
    bind_label: Option<String>,
    exchange_secret: Option<String>,
) -> Result<OperatorAuthChallengeRecord> {
    ensure_operator_auth_tables(db_path, hotel)?;
    let conn = Connection::open(db_path)?;
    let now = now_epoch_secs();
    let expires_at = now + 60 * 5;
    let challenge = OperatorAuthChallengeRecord {
        challenge_id: new_operator_chat_id("operator-auth-challenge"),
        user_id: default_operator_user_id(hotel),
        intended_surface: intended_surface.into(),
        auth_path: auth_path.into(),
        verifier_kind: verifier_kind.into(),
        verifier_hint,
        bind_label,
        challenge_nonce: new_secret_token("challenge-nonce"),
        exchange_secret,
        issued_at: now,
        expires_at,
        status: "pending".into(),
        consumed_at: None,
    };
    conn.execute(
        "INSERT INTO operator_auth_challenges
        (challenge_id, user_id, intended_surface, auth_path, verifier_kind, verifier_hint, bind_label, challenge_nonce, exchange_secret, issued_at, expires_at, status, consumed_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        rusqlite::params![
            &challenge.challenge_id,
            &challenge.user_id,
            &challenge.intended_surface,
            &challenge.auth_path,
            &challenge.verifier_kind,
            &challenge.verifier_hint,
            &challenge.bind_label,
            &challenge.challenge_nonce,
            &challenge.exchange_secret,
            challenge.issued_at,
            challenge.expires_at,
            &challenge.status,
            challenge.consumed_at,
        ],
    )?;
    Ok(challenge)
}

fn upsert_operator_external_identity_link(
    db_path: &PathBuf,
    hotel: &str,
    identity: &OidcIdentity,
) -> Result<ExternalIdentityLinkRecord> {
    ensure_operator_auth_tables(db_path, hotel)?;
    let conn = Connection::open(db_path)?;
    let now = now_epoch_secs();
    let user_id = default_operator_user_id(hotel);
    let display_name = identity.display_name.trim();
    if !display_name.is_empty() {
        conn.execute(
            "UPDATE operator_users
             SET display_name = ?2, updated_at = ?3
             WHERE user_id = ?1",
            rusqlite::params![&user_id, display_name, now],
        )?;
    }

    let link_id = format!(
        "external-identity:{}:{}",
        identity.provider, identity.provider_subject
    );
    let current_principal = resolve_operator_user(db_path, hotel, &user_id)?
        .map(|user| user.mesh_principal_id)
        .unwrap_or_else(|| default_operator_principal_id(hotel));
    let exact_projected_principal = projected_principal_id_for_identity(identity);
    let next_principal = if current_principal.starts_with("local-user:") {
        if let Some(projected) =
            find_projected_user_identity_by_principal(db_path, &exact_projected_principal)?
        {
            projected.principal_id
        } else if let Some(email) = identity.email.as_deref() {
            if let Some(projected) = find_unique_projected_user_identity_by_email(db_path, email)? {
                projected.principal_id
            } else {
                exact_projected_principal
            }
        } else {
            exact_projected_principal
        }
    } else {
        current_principal
    };
    conn.execute(
        "UPDATE operator_users
         SET mesh_principal_id = ?2,
             primary_email = COALESCE(primary_email, ?3),
             updated_at = ?4
         WHERE user_id = ?1",
        rusqlite::params![
            &user_id,
            &next_principal,
            normalize_optional_text(identity.email.clone()),
            now
        ],
    )?;
    conn.execute(
        "INSERT INTO external_identity_links
        (link_id, user_id, provider, provider_subject, email, login, display_name, verified_at, last_seen_at, created_at, updated_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?8, ?8)
        ON CONFLICT(provider, provider_subject) DO UPDATE SET
            user_id=excluded.user_id,
            email=excluded.email,
            login=excluded.login,
            display_name=excluded.display_name,
            verified_at=excluded.verified_at,
            last_seen_at=excluded.last_seen_at,
            updated_at=excluded.updated_at",
        rusqlite::params![
            &link_id,
            &user_id,
            &identity.provider,
            &identity.provider_subject,
            &identity.email,
            &identity.login,
            &identity.display_name,
            now,
        ],
    )?;

    let link =
        resolve_external_identity_link(db_path, &identity.provider, &identity.provider_subject)?
            .context("external identity link disappeared after upsert")?;
    sync_projected_user_identity(db_path, hotel, &user_id)?;
    Ok(link)
}

fn resolve_external_identity_link(
    db_path: &PathBuf,
    provider: &str,
    provider_subject: &str,
) -> Result<Option<ExternalIdentityLinkRecord>> {
    let conn = Connection::open(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT link_id, user_id, provider, provider_subject, email, login, display_name,
                verified_at, last_seen_at, created_at, updated_at
         FROM external_identity_links
         WHERE provider = ?1 AND provider_subject = ?2",
    )?;
    stmt.query_row(rusqlite::params![provider, provider_subject], |row| {
        Ok(ExternalIdentityLinkRecord {
            link_id: row.get(0)?,
            user_id: row.get(1)?,
            provider: row.get(2)?,
            provider_subject: row.get(3)?,
            email: row.get(4)?,
            login: row.get(5)?,
            display_name: row.get(6)?,
            verified_at: row.get(7)?,
            last_seen_at: row.get(8)?,
            created_at: row.get(9)?,
            updated_at: row.get(10)?,
        })
    })
    .optional()
    .map_err(Into::into)
}

fn projected_external_identity(
    link: &ExternalIdentityLinkRecord,
) -> ProjectedExternalIdentityRecord {
    ProjectedExternalIdentityRecord {
        provider: link.provider.clone(),
        provider_subject: link.provider_subject.clone(),
        email: link.email.clone(),
        login: link.login.clone(),
        display_name: link.display_name.clone(),
        verified_at: link.verified_at.max(0) as u64,
        last_seen_at: link.last_seen_at.max(0) as u64,
    }
}

fn normalize_email_key(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_ascii_lowercase())
}

fn find_projected_user_identity_by_principal(
    db_path: &PathBuf,
    principal_id: &str,
) -> Result<Option<ProjectedUserIdentityRecord>> {
    let storage = SqliteGraphStorage::open(db_path)?;
    let graph = GraphDomain::new(Arc::new(storage.adapter()));
    graph.get_projected_user_identity(principal_id)
}

fn find_unique_projected_user_identity_by_email(
    db_path: &PathBuf,
    email: &str,
) -> Result<Option<ProjectedUserIdentityRecord>> {
    let Some(target_email) = normalize_email_key(email) else {
        return Ok(None);
    };
    let storage = SqliteGraphStorage::open(db_path)?;
    let graph = GraphDomain::new(Arc::new(storage.adapter()));
    let mut matches = graph
        .list_projected_user_identities()?
        .into_iter()
        .filter(|identity| {
            identity
                .primary_email
                .as_deref()
                .and_then(normalize_email_key)
                .as_deref()
                == Some(target_email.as_str())
                || identity.linked_identities.iter().any(|link| {
                    link.email
                        .as_deref()
                        .and_then(normalize_email_key)
                        .as_deref()
                        == Some(target_email.as_str())
                })
        });
    let first = matches.next();
    if matches.next().is_some() {
        return Ok(None);
    }
    Ok(first)
}

fn merge_projected_external_identities(
    existing: &[ProjectedExternalIdentityRecord],
    local: &[ProjectedExternalIdentityRecord],
) -> Vec<ProjectedExternalIdentityRecord> {
    let mut merged: HashMap<(String, String), ProjectedExternalIdentityRecord> = HashMap::new();
    for identity in existing {
        merged.insert(
            (identity.provider.clone(), identity.provider_subject.clone()),
            identity.clone(),
        );
    }
    for identity in local {
        merged
            .entry((identity.provider.clone(), identity.provider_subject.clone()))
            .and_modify(|current| {
                if identity.last_seen_at >= current.last_seen_at {
                    *current = identity.clone();
                }
            })
            .or_insert_with(|| identity.clone());
    }
    let mut values: Vec<_> = merged.into_values().collect();
    values.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then_with(|| left.provider_subject.cmp(&right.provider_subject))
    });
    values
}

fn maybe_adopt_projected_identity_from_email(
    db_path: &PathBuf,
    _hotel: &str,
    user_id: &str,
    email: &str,
) -> Result<Option<ProjectedUserIdentityRecord>> {
    let Some(projected) = find_unique_projected_user_identity_by_email(db_path, email)? else {
        return Ok(None);
    };
    let conn = Connection::open(db_path)?;
    let now = now_epoch_secs();
    conn.execute(
        "UPDATE operator_users
         SET mesh_principal_id = ?2,
             display_name = CASE
                 WHEN trim(display_name) = '' OR display_name = ?3 THEN ?4
                 ELSE display_name
             END,
             preferred_name = COALESCE(preferred_name, ?5),
             primary_email = COALESCE(primary_email, ?6),
             updated_at = ?7
         WHERE user_id = ?1",
        rusqlite::params![
            user_id,
            &projected.principal_id,
            default_operator_display_name(),
            &projected.display_name,
            &projected.preferred_name,
            &projected.primary_email,
            now,
        ],
    )?;
    Ok(Some(projected))
}

fn emit_projected_user_identity_sync_event(
    db_path: &PathBuf,
    hotel: &str,
    graph: &GraphDomain,
    identity: &ProjectedUserIdentityRecord,
) -> Result<()> {
    let local_node_id = graph
        .get_hotel(hotel)?
        .map(|record| record.capabilities.node_id)
        .unwrap_or_else(|| default_operator_node_id(hotel));
    let payload = serde_json::to_string(identity)
        .context("emit_projected_user_identity_sync_event: serialize identity")?;
    let mut env = EventEnvelope {
        event_id: uuid::Uuid::new_v4(),
        seq: 0,
        source_node_id: local_node_id,
        target_node_id: None,
        source_agent_id: operator_surface_agent_id(hotel),
        target_agent_id: None,
        kind: EventKind::ProjectedUserIdentitySync,
        corr_id: format!("projected-user-sync:{}", identity.principal_id),
        attempt: 0,
        created_at: now_epoch_secs().max(0) as u64,
        expires_at: None,
        payload: EventPayload::Inline { data: payload },
        trace: vec![format!(
            "operator-auth:projected-user-sync:{}",
            identity.principal_id
        )],
    };
    SqliteEventStorage::open(db_path)?.append_event(&mut env)?;
    Ok(())
}

fn load_projected_user_identity_for_user(
    db_path: &PathBuf,
    hotel: &str,
    user_id: &str,
) -> Result<Option<ProjectedUserIdentityRecord>> {
    let Some(user) = resolve_operator_user(db_path, hotel, user_id)? else {
        return Ok(None);
    };
    let storage = SqliteGraphStorage::open(db_path)?;
    let graph = GraphDomain::new(Arc::new(storage.adapter()));
    graph.get_projected_user_identity(&user.mesh_principal_id)
}

fn sync_projected_user_identity(db_path: &PathBuf, hotel: &str, user_id: &str) -> Result<()> {
    let Some(user) = resolve_operator_user(db_path, hotel, user_id)? else {
        return Ok(());
    };
    let links = list_external_identity_link_records_for_user(db_path, user_id)?;
    let local_links: Vec<_> = links.iter().map(projected_external_identity).collect();
    let existing = find_projected_user_identity_by_principal(db_path, &user.mesh_principal_id)?
        .unwrap_or_default();
    let projected = ProjectedUserIdentityRecord {
        principal_id: user.mesh_principal_id.clone(),
        local_user_id: user.user_id.clone(),
        home_hotel: if existing.principal_id.is_empty() {
            user.home_hotel.clone()
        } else {
            existing.home_hotel
        },
        display_name: if user.display_name.trim().is_empty() {
            existing.display_name
        } else {
            user.display_name.clone()
        },
        preferred_name: user.preferred_name.clone().or(existing.preferred_name),
        primary_email: user.primary_email.clone().or(existing.primary_email),
        linked_identities: merge_projected_external_identities(
            &existing.linked_identities,
            &local_links,
        ),
        updated_at: now_epoch_secs().max(0) as u64,
    };
    let storage = SqliteGraphStorage::open(db_path)?;
    let graph = GraphDomain::new(Arc::new(storage.adapter()));
    graph.upsert_projected_user_identity(&projected)?;
    emit_projected_user_identity_sync_event(db_path, hotel, &graph, &projected)
}

#[allow(dead_code)]
fn resolve_operator_auth_challenge(
    db_path: &PathBuf,
    challenge_id: &str,
) -> Result<Option<OperatorAuthChallengeRecord>> {
    let conn = Connection::open(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT challenge_id, user_id, intended_surface, auth_path, verifier_kind, verifier_hint,
                bind_label, challenge_nonce, exchange_secret, issued_at, expires_at, status, consumed_at
         FROM operator_auth_challenges
         WHERE challenge_id = ?1",
    )?;
    let maybe = stmt
        .query_row([challenge_id], |row| {
            Ok(OperatorAuthChallengeRecord {
                challenge_id: row.get(0)?,
                user_id: row.get(1)?,
                intended_surface: row.get(2)?,
                auth_path: row.get(3)?,
                verifier_kind: row.get(4)?,
                verifier_hint: row.get(5)?,
                bind_label: row.get(6)?,
                challenge_nonce: row.get(7)?,
                exchange_secret: row.get(8)?,
                issued_at: row.get(9)?,
                expires_at: row.get(10)?,
                status: row.get(11)?,
                consumed_at: row.get(12)?,
            })
        })
        .optional()?;
    Ok(maybe)
}

#[allow(dead_code)]
fn resolve_operator_auth_challenge_by_nonce(
    db_path: &PathBuf,
    nonce: &str,
) -> Result<Option<OperatorAuthChallengeRecord>> {
    let conn = Connection::open(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT challenge_id, user_id, intended_surface, auth_path, verifier_kind, verifier_hint,
                bind_label, challenge_nonce, exchange_secret, issued_at, expires_at, status, consumed_at
         FROM operator_auth_challenges
         WHERE challenge_nonce = ?1",
    )?;
    let maybe = stmt
        .query_row([nonce], |row| {
            Ok(OperatorAuthChallengeRecord {
                challenge_id: row.get(0)?,
                user_id: row.get(1)?,
                intended_surface: row.get(2)?,
                auth_path: row.get(3)?,
                verifier_kind: row.get(4)?,
                verifier_hint: row.get(5)?,
                bind_label: row.get(6)?,
                challenge_nonce: row.get(7)?,
                exchange_secret: row.get(8)?,
                issued_at: row.get(9)?,
                expires_at: row.get(10)?,
                status: row.get(11)?,
                consumed_at: row.get(12)?,
            })
        })
        .optional()?;
    Ok(maybe)
}

#[allow(dead_code)]
fn consume_operator_auth_challenge(
    db_path: &PathBuf,
    challenge_id: &str,
) -> Result<Option<OperatorAuthChallengeRecord>> {
    let conn = Connection::open(db_path)?;
    let now = now_epoch_secs();
    let changed = conn.execute(
        "UPDATE operator_auth_challenges
         SET status = 'consumed', consumed_at = ?2
         WHERE challenge_id = ?1
           AND status = 'pending'
           AND expires_at > ?2",
        rusqlite::params![challenge_id, now],
    )?;
    if changed == 0 {
        return Ok(None);
    }
    resolve_operator_auth_challenge(db_path, challenge_id)
}

fn resolve_operator_session(
    db_path: &PathBuf,
    token: &str,
) -> Result<Option<OperatorSessionRecord>> {
    let conn = Connection::open(db_path)?;
    let mut stmt = conn.prepare(
        "SELECT session_id, session_token, user_id, display_name, issuing_hotel, surface_kind, posture,
                issued_at, expires_at, status, auth_method, bootstrap_id
         FROM operator_sessions
         WHERE session_token = ?1",
    )?;
    let maybe = stmt
        .query_row([token], |row| {
            Ok(OperatorSessionRecord {
                session_id: row.get(0)?,
                session_token: row.get(1)?,
                user_id: row.get(2)?,
                display_name: row.get(3)?,
                issuing_hotel: row.get(4)?,
                surface_kind: row.get(5)?,
                posture: row.get(6)?,
                issued_at: row.get(7)?,
                expires_at: row.get(8)?,
                status: row.get(9)?,
                auth_method: row.get(10)?,
                bootstrap_id: row.get(11)?,
            })
        })
        .optional()?;

    Ok(maybe.filter(|session| session.status == "active" && session.expires_at > now_epoch_secs()))
}

fn revoke_operator_session(db_path: &PathBuf, token: &str) -> Result<()> {
    let conn = Connection::open(db_path)?;
    conn.execute(
        "UPDATE operator_sessions SET status = 'revoked' WHERE session_token = ?1",
        [token],
    )?;
    Ok(())
}

fn default_operator_user_id(hotel: &str) -> String {
    format!("root-user:{hotel}")
}

fn sanitize_hotel_name(hotel_name: &str) -> String {
    hotel_name
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '-',
        })
        .collect()
}

fn default_operator_principal_id(hotel: &str) -> String {
    format!("local-user:{hotel}")
}

fn default_operator_node_id(hotel: &str) -> String {
    format!("{}-aiua-01", sanitize_hotel_name(hotel))
}

fn operator_surface_agent_id(hotel: &str) -> String {
    format!("desktop:{hotel}:operator-surface")
}

fn projected_principal_id_for_identity(identity: &OidcIdentity) -> String {
    format!(
        "user:{}:{}",
        identity.provider.trim().to_ascii_lowercase(),
        identity.provider_subject.trim()
    )
}

fn default_operator_display_name() -> String {
    std::env::var("USER")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "Operator".into())
}

/// Mirrors the hotel's deterministic root-key resolution order (env -> key file ->
/// macOS Keychain) so the reported source matches what `aiua` actually uses. Keep in
/// sync with `aiua::vault::load_or_create_root_key`.
fn detect_root_user_key_ref(hotel: &str) -> RootUserKeyRefStatusView {
    let account = vault_root_key_account();

    if let Ok(key_bytes) = load_root_key_from_env() {
        return RootUserKeyRefStatusView {
            user_id: default_operator_user_id(hotel),
            key_purpose: "vault-root-key".into(),
            vault_ref: Some(format!("env://PHILOTIC_VAULT_MASTER_KEY/{account}")),
            public_fingerprint: Some(root_key_fingerprint(&key_bytes)),
            rotation_state: "active".into(),
            source_kind: "env".into(),
        };
    }

    if let Ok(key_bytes) = load_root_key_from_file() {
        return RootUserKeyRefStatusView {
            user_id: default_operator_user_id(hotel),
            key_purpose: "vault-root-key".into(),
            vault_ref: Some(format!("file://~/.philotic/vault-master-key.env/{account}")),
            public_fingerprint: Some(root_key_fingerprint(&key_bytes)),
            rotation_state: "active".into(),
            source_kind: "key-file".into(),
        };
    }

    if cfg!(target_os = "macos") {
        let keychain_ref = format!("keychain://ai.philotic.hotel-vault/{account}");
        match load_root_key_from_keychain(&account) {
            Ok(Some(key_bytes)) => {
                return RootUserKeyRefStatusView {
                    user_id: default_operator_user_id(hotel),
                    key_purpose: "vault-root-key".into(),
                    vault_ref: Some(keychain_ref),
                    public_fingerprint: Some(root_key_fingerprint(&key_bytes)),
                    rotation_state: "active".into(),
                    source_kind: "keychain".into(),
                };
            }
            Ok(None) => {}
            Err(_) => {
                return RootUserKeyRefStatusView {
                    user_id: default_operator_user_id(hotel),
                    key_purpose: "vault-root-key".into(),
                    vault_ref: Some(keychain_ref),
                    public_fingerprint: None,
                    rotation_state: "unavailable".into(),
                    source_kind: "keychain-error".into(),
                };
            }
        }
    }

    RootUserKeyRefStatusView {
        user_id: default_operator_user_id(hotel),
        key_purpose: "vault-root-key".into(),
        vault_ref: None,
        public_fingerprint: None,
        rotation_state: "unavailable".into(),
        source_kind: "missing".into(),
    }
}

fn load_root_key_from_env() -> Result<Vec<u8>> {
    let raw = std::env::var("PHILOTIC_VAULT_MASTER_KEY")?;
    decode_root_key(raw.trim(), "PHILOTIC_VAULT_MASTER_KEY")
}

/// Reads the operator-provisioned root key file, mirroring
/// `aiua::vault::load_env_file_root_key`.
fn load_root_key_from_file() -> Result<Vec<u8>> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
    let path = std::path::PathBuf::from(home)
        .join(".philotic")
        .join("vault-master-key.env");
    let content = std::fs::read_to_string(&path)
        .map_err(|err| anyhow!("failed to read {}: {err}", path.display()))?;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = trimmed.split_once('=') {
            if key.trim() == "PHILOTIC_VAULT_MASTER_KEY" {
                return decode_root_key(value.trim(), &path.display().to_string());
            }
        }
    }
    anyhow::bail!(
        "{} did not contain PHILOTIC_VAULT_MASTER_KEY",
        path.display()
    )
}

fn load_root_key_from_keychain(account: &str) -> Result<Option<Vec<u8>>> {
    // Skipped entirely where there is no unlocked login keychain — `security`
    // blocks forever there rather than failing. See ansible_mesh_core::keychain.
    if !ansible_mesh_core::keychain::enabled() {
        return Ok(None);
    }

    let output = ansible_mesh_core::keychain::run_security(
        &[
            "find-generic-password",
            "-s",
            "ai.philotic.hotel-vault",
            "-a",
            account,
            "-w",
        ],
        "reading the Philotic vault root key",
    )?;

    if output.status.success() {
        let raw = String::from_utf8(output.stdout)?;
        return decode_root_key(raw.trim(), "macOS Keychain").map(Some);
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    if stderr.contains("could not be found")
        || stderr.contains("The specified item could not be found")
    {
        return Ok(None);
    }

    anyhow::bail!(
        "failed to read Philotic vault root key from macOS Keychain: {}",
        stderr.trim()
    )
}

fn decode_root_key(raw: &str, source: &str) -> Result<Vec<u8>> {
    let key_bytes = base64::engine::general_purpose::STANDARD
        .decode(raw)
        .map_err(|err| anyhow!("failed to decode {source} as base64: {err}"))?;
    if key_bytes.len() != 32 {
        anyhow::bail!("{source} must decode to exactly 32 bytes");
    }
    Ok(key_bytes)
}

fn vault_root_key_account() -> String {
    std::env::var("PHILOTIC_VAULT_KEY_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "default-root-key".to_string())
}

fn root_key_fingerprint(key_bytes: &[u8]) -> String {
    let digest = Sha256::digest(key_bytes);
    format!("sha256:{}", hex::encode(&digest[..8]))
}

fn root_user_key_source_kind(vault_ref: &Option<String>) -> String {
    match vault_ref.as_deref() {
        Some(value) if value.starts_with("keychain://") => "keychain".into(),
        Some(value) if value.starts_with("env://") => "env".into(),
        Some(value) if value.starts_with("file://") => "key-file".into(),
        Some(_) => "opaque".into(),
        None => "missing".into(),
    }
}

fn now_epoch_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

#[derive(Clone, Debug)]
struct OidcProviderConfig {
    id: String,
    label: String,
    configured: bool,
    client_id: Option<String>,
    auth_url: String,
    scopes: Vec<String>,
    extra_auth_params: Vec<(String, String)>,
    redirect_uri: String,
}

#[derive(Debug)]
struct OidcIdentity {
    provider: String,
    provider_subject: String,
    display_name: String,
    email: Option<String>,
    login: Option<String>,
}

/// Config key holding the set of external identities permitted to become the
/// hotel operator. Comma- or newline-separated.
const OIDC_SUBJECT_ALLOWLIST_KEY: &str = "oidc_subject_allowlist";

/// Decide whether an OIDC identity that completed the flow may be linked to the
/// operator user and issued a session.
///
/// **Authentication is not authorization.** Completing an OIDC flow proves only
/// that the caller controls *some* account at the provider — Google and GitHub
/// will happily authenticate the entire internet. Without this gate, any
/// stranger who reaches a publicly-resolvable callback becomes `root-user:` with
/// `posture: "admin"`, which on this control plane means arbitrary component
/// registration (i.e. code execution on the hotel host).
///
/// **This fails closed.** An absent or blank allowlist rejects every identity
/// rather than admitting all of them. That is deliberate: the failure mode of a
/// forgotten allowlist must be "nobody can log in", never "anybody can".
///
/// Accepted entry forms (case-insensitive, whitespace-trimmed):
/// - `google:1234567890`  — provider and subject (most precise; subjects are stable)
/// - `github:octocat`     — provider and login handle
/// - `you@example.com`    — exact email, any provider
/// - `@example.com`       — any email at that domain
///
/// Emails are only honoured when the provider asserts them; an identity with no
/// email cannot be matched by an email rule.
fn oidc_identity_allowed(
    identity: &OidcIdentity,
    allowlist_raw: Option<&str>,
) -> Result<(), String> {
    let entries: Vec<String> = allowlist_raw
        .unwrap_or("")
        .split([',', '\n', '\r'])
        .map(|entry| entry.trim().to_ascii_lowercase())
        .filter(|entry| !entry.is_empty() && !entry.starts_with('#'))
        .collect();

    if entries.is_empty() {
        return Err(format!(
            "no OIDC subject allowlist configured — set `{OIDC_SUBJECT_ALLOWLIST_KEY}` \
             (e.g. `phil config set {OIDC_SUBJECT_ALLOWLIST_KEY} \"google:<your-subject>\"`) \
             before enabling OIDC login. Refusing to admit an unverified subject."
        ));
    }

    let provider = identity.provider.trim().to_ascii_lowercase();
    let subject = identity.provider_subject.trim().to_ascii_lowercase();
    let login = identity
        .login
        .as_deref()
        .map(|value| value.trim().to_ascii_lowercase());
    let email = identity
        .email
        .as_deref()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty());

    let matched = entries.iter().any(|entry| {
        if let Some(domain) = entry.strip_prefix('@') {
            return email
                .as_deref()
                .and_then(|value| value.rsplit_once('@'))
                .is_some_and(|(_, host)| host == domain);
        }
        if let Some((entry_provider, rest)) = entry.split_once(':') {
            if entry_provider == provider
                && (rest == subject || login.as_deref().is_some_and(|value| value == rest))
            {
                return true;
            }
        }
        email.as_deref().is_some_and(|value| value == entry)
    });

    if matched {
        Ok(())
    } else {
        // Deliberately vague to the caller; the specifics go to the operator's log.
        Err("this identity is not authorized to administer this hotel".to_string())
    }
}

async fn list_oidc_provider_statuses(
    socket: &str,
    headers: Option<&HeaderMap>,
) -> Vec<OidcProviderStatusView> {
    let mut providers = Vec::new();
    for provider in ["google", "github"] {
        if let Ok(Some(provider)) = oidc_provider_config(socket, provider, headers).await {
            providers.push(OidcProviderStatusView {
                provider: provider.id,
                label: provider.label,
                configured: provider.configured,
                callback_url: provider.redirect_uri,
            });
        }
    }
    providers
}

async fn oidc_provider_config(
    socket: &str,
    provider: &str,
    headers: Option<&HeaderMap>,
) -> Result<Option<OidcProviderConfig>> {
    let resolved = load_oidc_resolved_config(socket, headers).await?;
    oidc_provider_config_from_resolved(provider, resolved)
}

fn oidc_provider_config_from_resolved(
    provider: &str,
    resolved: OidcResolvedConfig,
) -> Result<Option<OidcProviderConfig>> {
    match provider {
        "google" => Ok(Some(OidcProviderConfig {
            id: "google".into(),
            label: "Google".into(),
            configured: resolved.google.client_id.is_some()
                && resolved.google.client_secret_ref.is_some(),
            client_id: resolved.google.client_id,
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            scopes: vec!["openid".into(), "profile".into(), "email".into()],
            extra_auth_params: vec![
                ("access_type".into(), "offline".into()),
                ("prompt".into(), "consent".into()),
            ],
            redirect_uri: format!("{}/auth/oidc/google/callback", resolved.public_base_url),
        })),
        "github" => Ok(Some(OidcProviderConfig {
            id: "github".into(),
            label: "GitHub".into(),
            configured: resolved.github.client_id.is_some()
                && resolved.github.client_secret_ref.is_some(),
            client_id: resolved.github.client_id,
            auth_url: "https://github.com/login/oauth/authorize".into(),
            scopes: vec!["read:user".into(), "user:email".into()],
            extra_auth_params: Vec::new(),
            redirect_uri: format!("{}/auth/oidc/github/callback", resolved.public_base_url),
        })),
        _ => Ok(None),
    }
}

async fn load_oidc_resolved_config(
    socket: &str,
    headers: Option<&HeaderMap>,
) -> Result<OidcResolvedConfig> {
    let (public_base_url, public_base_source) =
        operator_auth_public_base_url(socket, headers).await?;
    let google_client_id = oidc_config_string(socket, "oidc_google_client_id")
        .await?
        .or_else(|| env_trimmed("PHILOTIC_OIDC_GOOGLE_CLIENT_ID"));
    let github_client_id = oidc_config_string(socket, "oidc_github_client_id")
        .await?
        .or_else(|| env_trimmed("PHILOTIC_OIDC_GITHUB_CLIENT_ID"));
    let google_client_secret_ref =
        oidc_config_string(socket, "oidc_google_client_secret_ref").await?;
    let github_client_secret_ref =
        oidc_config_string(socket, "oidc_github_client_secret_ref").await?;

    Ok(OidcResolvedConfig {
        public_base_url,
        public_base_source,
        google: OidcProviderSettings {
            client_id: google_client_id,
            client_secret_ref: google_client_secret_ref,
        },
        github: OidcProviderSettings {
            client_id: github_client_id,
            client_secret_ref: github_client_secret_ref,
        },
    })
}

impl OidcProviderConfig {
    fn authorization_url(&self, state: &str, code_challenge: &str) -> Result<String> {
        let client_id = self
            .client_id
            .as_deref()
            .context("OIDC client id is not configured")?;
        let mut url = Url::parse(&self.auth_url).context("failed to build OIDC auth URL")?;
        url.query_pairs_mut()
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", &self.redirect_uri)
            .append_pair("response_type", "code")
            .append_pair("scope", &self.scopes.join(" "))
            .append_pair("state", state)
            .append_pair("code_challenge", code_challenge)
            .append_pair("code_challenge_method", "S256");
        for (key, value) in &self.extra_auth_params {
            url.query_pairs_mut().append_pair(key, value);
        }
        Ok(url.into())
    }
}

async fn operator_auth_public_base_url(
    socket: &str,
    headers: Option<&HeaderMap>,
) -> Result<(String, String)> {
    if let Some(value) = oidc_config_string(socket, "oidc_public_base_url").await? {
        return Ok((
            value.trim_end_matches('/').to_string(),
            "hotel-config".into(),
        ));
    }
    if let Some(value) = env_trimmed("PHILOTIC_OIDC_PUBLIC_BASE_URL")
        .or_else(|| env_trimmed("PHILOTIC_AUTH_PUBLIC_BASE_URL"))
    {
        return Ok((value.trim_end_matches('/').to_string(), "env".into()));
    }

    let headers = headers.context("missing host headers and no OIDC public base URL configured")?;
    let scheme = header_string(headers, "x-forwarded-proto")
        .or_else(|| header_string(headers, "x-forwarded-scheme"))
        .unwrap_or_else(|| "http".into());
    let host = header_string(headers, "x-forwarded-host")
        .or_else(|| header_string(headers, "host"))
        .context("missing host header for auth public base URL derivation")?;
    Ok((
        format!("{}://{}", scheme, host)
            .trim_end_matches('/')
            .to_string(),
        "request-headers".into(),
    ))
}

async fn oidc_config_string(socket: &str, key: &str) -> Result<Option<String>> {
    Ok(ipc_get_config(socket, key)
        .await?
        .and_then(|value| value.as_str().map(|value| value.trim().to_string()))
        .filter(|value| !value.is_empty()))
}

fn oidc_value_source(hotel_config_present: bool, env_fallback_present: bool) -> String {
    if hotel_config_present {
        "hotel-config".into()
    } else if env_fallback_present {
        "env".into()
    } else {
        "unset".into()
    }
}

fn header_string(headers: &HeaderMap, key: &str) -> Option<String> {
    headers
        .get(key)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn env_trimmed(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn oidc_loopback_bootstrap_only(resolved: &OidcResolvedConfig) -> bool {
    if resolved.public_base_source != "request-headers" {
        return false;
    }
    let Ok(url) = Url::parse(&resolved.public_base_url) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

fn sanitize_return_path(input: Option<&str>) -> Option<String> {
    input
        .map(str::trim)
        .filter(|value| value.starts_with('/'))
        .filter(|value| !value.starts_with("//"))
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn normalize_non_empty_option(input: Option<String>) -> Option<String> {
    input
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn normalize_optional_text(input: Option<String>) -> Option<String> {
    input
        .map(|value| value.trim().to_string())
        .and_then(|value| if value.is_empty() { None } else { Some(value) })
}

fn oidc_code_verifier() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn pkce_code_challenge(code_verifier: &str) -> String {
    let digest = Sha256::digest(code_verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

async fn exchange_oidc_identity_via_hotel(
    socket: &str,
    provider: &OidcProviderConfig,
    code: &str,
    code_verifier: &str,
) -> Result<Value> {
    let mut client = connect_client_with_identity(
        socket,
        GuestIdentity {
            guest_id: "philotic-web-oidc".into(),
            role: "management".into(),
            supported_tools: vec![],
        },
    )
    .await?;
    let response = client
        .send_request_with_timeout(
            IpcRequest::ExchangeOperatorOidc {
                provider: provider.id.clone(),
                authorization_code: code.to_string(),
                code_verifier: code_verifier.to_string(),
                redirect_uri: provider.redirect_uri.clone(),
            },
            Duration::from_secs(45),
        )
        .await
        .context("hotel OIDC exchange request failed")?;

    match response {
        IpcResponse::Standard {
            ok: true,
            data: Some(data),
            ..
        } => {
            let exchanged: ansible_mesh_core::integration::OidcExchangeResponse =
                serde_json::from_value(data)
                    .context("hotel returned invalid OIDC exchange data")?;
            if exchanged.provider_id != provider.id {
                bail!(
                    "hotel returned OIDC provider [{}] for requested provider [{}]",
                    exchanged.provider_id,
                    provider.id
                );
            }
            Ok(exchanged.userinfo)
        }
        IpcResponse::Standard { message, .. } => {
            bail!("hotel rejected OIDC exchange: {message}")
        }
        other => bail!("unexpected hotel OIDC exchange response: {other:?}"),
    }
}

fn oidc_identity_from_userinfo(
    provider: &OidcProviderConfig,
    body: &Value,
) -> Result<OidcIdentity> {
    let identity = match provider.id.as_str() {
        "google" => OidcIdentity {
            provider: provider.id.clone(),
            provider_subject: body
                .get("sub")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .context("google userinfo response missing subject")?
                .to_string(),
            display_name: body
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| body.get("email").and_then(Value::as_str))
                .unwrap_or("Operator")
                .to_string(),
            email: body
                .get("email")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            login: None,
        },
        "github" => OidcIdentity {
            provider: provider.id.clone(),
            provider_subject: body
                .get("id")
                .and_then(Value::as_i64)
                .map(|value| value.to_string())
                .context("github userinfo response missing id")?,
            display_name: body
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| body.get("login").and_then(Value::as_str))
                .unwrap_or("Operator")
                .to_string(),
            email: body
                .get("email")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
            login: body
                .get("login")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        },
        other => bail!("unsupported OIDC provider [{other}]"),
    };

    Ok(identity)
}

fn oidc_callback_error_response(status: StatusCode, message: String) -> Response {
    let html = format!(
        "<!doctype html><html><body><h1>Philotic operator auth failed</h1><p>{}</p></body></html>",
        message
    );
    (
        status,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::storage::EventStorage;
    use std::fs;

    fn temp_db_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("philotic-web-{name}-{}.db", uuid::Uuid::new_v4()))
    }

    /// Credentials get 256 bits; correlation ids stay at 64.
    #[test]
    fn secret_tokens_are_long_and_unique() {
        let token = new_secret_token("operator-token");
        let hex_part = token.strip_prefix("operator-token-").expect("prefix");
        assert_eq!(hex_part.len(), 64, "expected 32 bytes of hex: {token}");
        assert!(hex_part.chars().all(|c| c.is_ascii_hexdigit()));

        // Distinctness across a batch — a constant would pass the length check.
        let batch: std::collections::HashSet<String> =
            (0..256).map(|_| new_secret_token("t")).collect();
        assert_eq!(batch.len(), 256, "tokens must not repeat");

        // Correlation ids are deliberately shorter and are not credentials.
        let chat_id = new_operator_chat_id("operator-chat-turn");
        assert_eq!(
            chat_id
                .strip_prefix("operator-chat-turn-")
                .expect("prefix")
                .len(),
            16
        );
    }

    /// The OIDC callback derives its `Location` from the stored `bind_label` on
    /// the same response that sets an admin session cookie, so an off-site
    /// value there is an open redirect out of a successful login.
    #[test]
    fn post_login_redirect_rejects_offsite_targets() {
        for hostile in [
            "https://evil.example/",
            "http://evil.example",
            "//evil.example",  // scheme-relative
            "///evil.example", // extra slashes
            "javascript:alert(1)",
            "evil.example/path", // no leading slash
            "",
            "   ",
        ] {
            assert_eq!(
                sanitize_return_path(Some(hostile)),
                None,
                "{hostile:?} must not survive sanitization"
            );
        }

        // Legitimate in-app destinations still work.
        assert_eq!(
            sanitize_return_path(Some("/desktop")),
            Some("/desktop".into())
        );
        assert_eq!(sanitize_return_path(Some("/")), Some("/".into()));
        assert_eq!(
            sanitize_return_path(Some("  /settings/agents  ")),
            Some("/settings/agents".into())
        );

        // And the callback's fallback is the app root, never the raw value.
        let redirect = sanitize_return_path(Some("https://evil.example")).unwrap_or("/".into());
        assert_eq!(redirect, "/");
    }

    /// Two callers asking for the *same* opaque conversation id must land in
    /// different hotel sessions, and no caller may name another's session.
    #[test]
    fn operator_session_ids_are_namespaced_per_caller() {
        let phone = "edge:edge-phone";
        let laptop = "edge:edge-laptop";
        let desktop = "desktop-membrane";

        // Same client-chosen id, three callers, three distinct sessions.
        let a = scoped_operator_session_id(phone, "conv-1");
        let b = scoped_operator_session_id(laptop, "conv-1");
        let c = scoped_operator_session_id(desktop, "conv-1");
        assert_eq!(a, "operator-chat:edge:edge-phone:conv-1");
        assert_eq!(b, "operator-chat:edge:edge-laptop:conv-1");
        assert_eq!(c, "operator-chat:desktop-membrane:conv-1");
        assert_ne!(a, b);
        assert_ne!(a, c);

        // The attack this closes: the phone submits using the laptop's session
        // id verbatim. It still resolves under the phone's own namespace, so
        // the laptop's history is untouched.
        let spoof = scoped_operator_session_id(phone, "operator-chat:edge:edge-laptop:conv-1");
        assert_ne!(
            spoof, b,
            "a device must not be able to name another's session"
        );
        assert!(spoof.starts_with("operator-chat:edge:edge-phone:"));

        // Default-minted ids already carry the caller's prefix and must not be
        // double-wrapped, so existing conversations keep their session id.
        let already = "operator-chat:edge:edge-phone:jane";
        assert_eq!(scoped_operator_session_id(phone, already), already);
    }

    fn oidc_identity(provider: &str, subject: &str) -> OidcIdentity {
        OidcIdentity {
            provider: provider.into(),
            provider_subject: subject.into(),
            display_name: "Test Operator".into(),
            email: None,
            login: None,
        }
    }

    #[test]
    fn oidc_allowlist_fails_closed_when_unset_or_blank() {
        let identity = oidc_identity("google", "1234567890");
        // The security property: a missing allowlist admits NOBODY. If this
        // ever flips to Ok, any Google/GitHub account on the internet that
        // reaches the callback becomes a posture:admin operator.
        for raw in [
            None,
            Some(""),
            Some("   "),
            Some(",,\n"),
            Some("# only a comment"),
        ] {
            let err = oidc_identity_allowed(&identity, raw)
                .expect_err("an empty allowlist must reject, never admit");
            assert!(
                err.contains(OIDC_SUBJECT_ALLOWLIST_KEY),
                "the refusal should name the key to set, got: {err}"
            );
        }
    }

    #[test]
    fn oidc_allowlist_matches_provider_subject() {
        let identity = oidc_identity("google", "1234567890");
        assert!(oidc_identity_allowed(&identity, Some("google:1234567890")).is_ok());
        // Case and surrounding whitespace are not security boundaries.
        assert!(oidc_identity_allowed(&identity, Some("  GOOGLE:1234567890  ")).is_ok());
        // Same subject at a different provider must not match.
        assert!(oidc_identity_allowed(&identity, Some("github:1234567890")).is_err());
        // A different subject at the right provider must not match.
        assert!(oidc_identity_allowed(&identity, Some("google:9999999999")).is_err());
    }

    #[test]
    fn oidc_allowlist_matches_login_and_email_forms() {
        let mut identity = oidc_identity("github", "42");
        identity.login = Some("octocat".into());
        identity.email = Some("Octocat@Example.com".into());

        assert!(oidc_identity_allowed(&identity, Some("github:octocat")).is_ok());
        assert!(oidc_identity_allowed(&identity, Some("octocat@example.com")).is_ok());
        assert!(oidc_identity_allowed(&identity, Some("@example.com")).is_ok());
        // A domain rule must match the full domain, not a suffix of it.
        assert!(oidc_identity_allowed(&identity, Some("@ample.com")).is_err());
        // A login rule from another provider must not cross over.
        assert!(oidc_identity_allowed(&identity, Some("google:octocat")).is_err());
    }

    #[test]
    fn oidc_allowlist_email_rules_need_an_asserted_email() {
        // No email claim → email and domain rules cannot match, even though the
        // operator "meant" this person.
        let identity = oidc_identity("google", "1234567890");
        assert!(oidc_identity_allowed(&identity, Some("someone@example.com")).is_err());
        assert!(oidc_identity_allowed(&identity, Some("@example.com")).is_err());
    }

    #[test]
    fn oidc_allowlist_accepts_multi_entry_lists() {
        let identity = oidc_identity("google", "1234567890");
        assert!(
            oidc_identity_allowed(&identity, Some("github:someone, google:1234567890")).is_ok()
        );
        assert!(oidc_identity_allowed(&identity, Some("github:someone\n@example.com\n")).is_err());
    }

    /// Sets an env var for a test and RESTORES the previous value on drop.
    ///
    /// Tests run as threads of a single process, so unconditionally calling
    /// `remove_var` when a test finishes deletes the variable out from under
    /// anything running concurrently — and deletes the ambient value CI
    /// provides. That made three unrelated aiua tests fail on the runner while
    /// passing locally. Mirrors `VaultKeyEnv` in aiua's ipc tests.
    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(previous) => std::env::set_var(self.key, previous),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    fn seed_projected_identity(context_path: &PathBuf, identity: &ProjectedUserIdentityRecord) {
        let storage = SqliteGraphStorage::open(context_path).unwrap();
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        graph.upsert_projected_user_identity(identity).unwrap();
    }

    #[test]
    fn summarize_router_event_includes_failure_code() {
        assert_eq!(
            summarize_router_event("text.generate", "gemini", "failure", Some("RATE_LIMIT")),
            "text.generate via gemini failed (RATE_LIMIT)"
        );
    }

    #[test]
    fn create_cron_body_reads_explicit_silent_ok_and_defaults_to_false() {
        let with_silent: CreateCronBody = serde_json::from_value(serde_json::json!({
            "schedule": "0 0 7 * * * *",
            "target_role": "orchestrator",
            "payload": "{}",
            "silent_ok": true
        }))
        .expect("body with silent_ok should deserialize");
        assert!(with_silent.silent_ok);

        let without_silent: CreateCronBody = serde_json::from_value(serde_json::json!({
            "schedule": "0 0 7 * * * *",
            "target_role": "orchestrator",
            "payload": "{}"
        }))
        .expect("body without silent_ok should deserialize");
        assert!(
            !without_silent.silent_ok,
            "omitted silent_ok must default to false, preserving today's always-delivered behavior"
        );
    }

    #[test]
    fn query_event_log_merges_router_and_mesh_entries_newest_first() {
        let context_path = temp_db_path("context");
        let router_path = context_path.parent().unwrap().join("router_traces.db");

        {
            let conn = Connection::open(&context_path).unwrap();
            conn.execute_batch(
                "
                CREATE TABLE mesh_events (
                    seq INTEGER PRIMARY KEY AUTOINCREMENT,
                    event_id TEXT NOT NULL UNIQUE,
                    source_node_id TEXT NOT NULL,
                    target_node_id TEXT,
                    source_agent_id TEXT NOT NULL,
                    target_agent_id TEXT,
                    kind TEXT NOT NULL,
                    corr_id TEXT NOT NULL,
                    attempt INTEGER DEFAULT 0,
                    created_at INTEGER NOT NULL,
                    expires_at INTEGER,
                    payload_type TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    trace_json TEXT NOT NULL
                );
                INSERT INTO mesh_events
                    (event_id, source_node_id, target_node_id, source_agent_id, target_agent_id, kind, corr_id, attempt, created_at, expires_at, payload_type, payload_json, trace_json)
                VALUES
                    ('mesh-1', 'mac-jane-aiua-01', 'mbp-jane-aiua-01', 'agent-jane-01', 'agent-aria-01', 'session.handoff', 'corr-1', 0, 100, NULL, 'json', '{\"state\":\"ok\"}', '{}');
                ",
            )
            .unwrap();
        }

        {
            let conn = Connection::open(&router_path).unwrap();
            conn.execute_batch(
                "
                CREATE TABLE router_traces (
                    trace_id     TEXT PRIMARY KEY,
                    agent_id     TEXT NOT NULL,
                    session_id   TEXT NOT NULL DEFAULT '',
                    turn_id      TEXT NOT NULL DEFAULT '',
                    provider_id  TEXT NOT NULL,
                    model_id     TEXT,
                    task_kind    TEXT NOT NULL,
                    outcome      TEXT NOT NULL,
                    failure_code TEXT,
                    latency_ms   INTEGER,
                    token_count  INTEGER,
                    timestamp    INTEGER NOT NULL
                );
                INSERT INTO router_traces
                    (trace_id, agent_id, session_id, turn_id, provider_id, model_id, task_kind, outcome, failure_code, latency_ms, token_count, timestamp)
                VALUES
                    ('trace-1', 'agent-jane-01', 'session-1', 'turn-1', 'gemini', 'gemini-flash', 'text.generate', 'success', NULL, 42, 77, 200);
                ",
            )
            .unwrap();
        }

        let entries = query_event_log(&context_path, 10, None).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "trace-1");
        assert_eq!(entries[0].source, "router");
        assert_eq!(entries[1].id, "mesh-1");
        assert_eq!(entries[1].source, "mesh");

        let _ = fs::remove_file(&context_path);
        let _ = fs::remove_file(&router_path);
    }

    #[test]
    fn issue_operator_session_persists_and_resolves_active_session() {
        let context_path = temp_db_path("operator-session");
        ensure_operator_auth_tables(&context_path, "mac-jane").unwrap();
        let session = issue_operator_session(
            &context_path,
            "mac-jane",
            "Jared",
            "bootstrap_token",
            Some("startup-bootstrap".into()),
        )
        .unwrap();

        let resolved = resolve_operator_session(&context_path, &session.session_token)
            .unwrap()
            .expect("session should resolve");
        assert_eq!(resolved.display_name, "Jared");
        assert_eq!(resolved.issuing_hotel, "mac-jane");
        assert_eq!(resolved.posture, "admin");

        let _ = fs::remove_file(&context_path);
    }

    #[test]
    fn ensure_operator_auth_tables_seeds_operator_user_with_bootstrap_onboarding_state() {
        let context_path = temp_db_path("operator-user-seed");
        ensure_operator_auth_tables(&context_path, "mac-jane").unwrap();

        let user = resolve_operator_user(&context_path, "mac-jane", "root-user:mac-jane")
            .unwrap()
            .expect("operator user should exist");
        assert_eq!(user.mesh_principal_id, "local-user:mac-jane");
        assert_eq!(user.display_name, default_operator_display_name());
        assert_eq!(user.home_hotel, "mac-jane");
        assert_eq!(user.onboarding_state, "bootstrap");
        assert!(user.preferred_name.is_none());
        assert!(user.primary_email.is_none());

        let _ = fs::remove_file(&context_path);
    }

    #[test]
    fn issue_operator_auth_challenge_persists_pending_record() {
        let context_path = temp_db_path("operator-auth-challenge");
        ensure_operator_auth_tables(&context_path, "mac-jane").unwrap();
        let challenge = issue_operator_auth_challenge(
            &context_path,
            "mac-jane",
            "desktop_membrane",
            "membrane_challenge",
            "telegram",
            Some("@jaredlikes".into()),
            Some("desktop.jaredlikes.com".into()),
            None,
        )
        .unwrap();

        let resolved = resolve_operator_auth_challenge(&context_path, &challenge.challenge_id)
            .unwrap()
            .expect("challenge should resolve");
        assert_eq!(resolved.status, "pending");
        assert_eq!(resolved.verifier_kind, "telegram");
        assert_eq!(resolved.auth_path, "membrane_challenge");
        assert_eq!(resolved.user_id, "root-user:mac-jane");

        let _ = fs::remove_file(&context_path);
    }

    #[test]
    fn upsert_operator_external_identity_link_persists_google_subject_and_email() {
        let context_path = temp_db_path("external-identity-link");
        ensure_operator_auth_tables(&context_path, "mac-jane").unwrap();

        let link = upsert_operator_external_identity_link(
            &context_path,
            "mac-jane",
            &OidcIdentity {
                provider: "google".into(),
                provider_subject: "google-subject-123".into(),
                display_name: "Jared Likes".into(),
                email: Some("jared@example.com".into()),
                login: None,
            },
        )
        .unwrap();

        assert_eq!(link.user_id, "root-user:mac-jane");
        assert_eq!(link.provider, "google");
        assert_eq!(link.provider_subject, "google-subject-123");
        assert_eq!(link.email.as_deref(), Some("jared@example.com"));

        let listed = list_external_identity_links(&context_path, "mac-jane").unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].provider, "google");
        assert_eq!(listed[0].provider_subject, "google-subject-123");
        assert_eq!(listed[0].display_name.as_deref(), Some("Jared Likes"));

        let user = resolve_operator_user(&context_path, "mac-jane", "root-user:mac-jane")
            .unwrap()
            .expect("operator user should exist");
        assert_eq!(user.mesh_principal_id, "user:google:google-subject-123");

        let projected =
            load_projected_user_identity_for_user(&context_path, "mac-jane", "root-user:mac-jane")
                .unwrap()
                .expect("projected identity should exist");
        assert_eq!(projected.principal_id, "user:google:google-subject-123");
        assert_eq!(projected.home_hotel, "mac-jane");
        assert_eq!(projected.linked_identities.len(), 1);
        assert_eq!(projected.linked_identities[0].provider, "google");

        let _ = fs::remove_file(&context_path);
    }

    #[test]
    fn projected_user_identity_sync_appends_broadcast_mesh_event() {
        let context_path = temp_db_path("projected-user-sync");
        ensure_operator_auth_tables(&context_path, "mac-jane").unwrap();

        let _link = upsert_operator_external_identity_link(
            &context_path,
            "mac-jane",
            &OidcIdentity {
                provider: "google".into(),
                provider_subject: "google-subject-123".into(),
                display_name: "Jared Likes".into(),
                email: Some("jared@example.com".into()),
                login: None,
            },
        )
        .unwrap();

        let ledger = SqliteEventStorage::open(&context_path).unwrap();
        let events = ledger
            .query_unacked_events("remote-aiua-01", 0, 20)
            .unwrap();
        let sync_event = events
            .iter()
            .find(|event| event.kind == EventKind::ProjectedUserIdentitySync)
            .expect("projected identity sync event should be present");
        assert!(sync_event.target_node_id.is_none());
        let EventPayload::Inline { data } = &sync_event.payload else {
            panic!("expected inline payload");
        };
        let payload: ProjectedUserIdentityRecord = serde_json::from_str(data).unwrap();
        assert_eq!(payload.principal_id, "user:google:google-subject-123");
        assert_eq!(payload.home_hotel, "mac-jane");
        assert_eq!(payload.linked_identities.len(), 1);

        let _ = fs::remove_file(&context_path);
    }

    #[test]
    fn upsert_operator_external_identity_link_adopts_existing_projected_identity_by_email() {
        let context_path = temp_db_path("external-identity-project-adopt");
        ensure_operator_auth_tables(&context_path, "mac-jane").unwrap();
        seed_projected_identity(
            &context_path,
            &ProjectedUserIdentityRecord {
                principal_id: "user:google:google-subject-123".into(),
                local_user_id: "root-user:vps-jane".into(),
                home_hotel: "vps-jane".into(),
                display_name: "Jared Likes".into(),
                preferred_name: Some("Jared".into()),
                primary_email: Some("jared@example.com".into()),
                linked_identities: vec![ProjectedExternalIdentityRecord {
                    provider: "google".into(),
                    provider_subject: "google-subject-123".into(),
                    email: Some("jared@example.com".into()),
                    login: None,
                    display_name: Some("Jared Likes".into()),
                    verified_at: 100,
                    last_seen_at: 100,
                }],
                updated_at: 100,
            },
        );

        let _link = upsert_operator_external_identity_link(
            &context_path,
            "mac-jane",
            &OidcIdentity {
                provider: "github".into(),
                provider_subject: "github-user-456".into(),
                display_name: "Jared Likes".into(),
                email: Some("jared@example.com".into()),
                login: Some("likesjx".into()),
            },
        )
        .unwrap();

        let user = resolve_operator_user(&context_path, "mac-jane", "root-user:mac-jane")
            .unwrap()
            .expect("operator user should exist");
        assert_eq!(user.mesh_principal_id, "user:google:google-subject-123");

        let projected =
            load_projected_user_identity_for_user(&context_path, "mac-jane", "root-user:mac-jane")
                .unwrap()
                .expect("projected identity should exist");
        assert_eq!(projected.principal_id, "user:google:google-subject-123");
        assert_eq!(projected.home_hotel, "vps-jane");
        assert_eq!(projected.linked_identities.len(), 2);
        assert!(projected
            .linked_identities
            .iter()
            .any(|link| link.provider == "google"));
        assert!(projected
            .linked_identities
            .iter()
            .any(|link| link.provider == "github"));

        let _ = fs::remove_file(&context_path);
    }

    #[test]
    fn patch_operator_user_record_adopts_existing_projected_identity_by_email() {
        let context_path = temp_db_path("operator-user-patch-project-adopt");
        ensure_operator_auth_tables(&context_path, "mac-jane").unwrap();
        seed_projected_identity(
            &context_path,
            &ProjectedUserIdentityRecord {
                principal_id: "user:google:google-subject-123".into(),
                local_user_id: "root-user:vps-jane".into(),
                home_hotel: "vps-jane".into(),
                display_name: "Jared Likes".into(),
                preferred_name: Some("Jared".into()),
                primary_email: Some("jared@example.com".into()),
                linked_identities: vec![ProjectedExternalIdentityRecord {
                    provider: "google".into(),
                    provider_subject: "google-subject-123".into(),
                    email: Some("jared@example.com".into()),
                    login: None,
                    display_name: Some("Jared Likes".into()),
                    verified_at: 100,
                    last_seen_at: 100,
                }],
                updated_at: 100,
            },
        );

        let updated = patch_operator_user_record(
            &context_path,
            "mac-jane",
            "root-user:mac-jane",
            Some("Jared Likes".into()),
            Some("Jared".into()),
            Some("jared@example.com".into()),
            Some("enriched".into()),
        )
        .unwrap();

        assert_eq!(updated.mesh_principal_id, "user:google:google-subject-123");
        let projected =
            load_projected_user_identity_for_user(&context_path, "mac-jane", "root-user:mac-jane")
                .unwrap()
                .expect("projected identity should exist");
        assert_eq!(projected.principal_id, "user:google:google-subject-123");
        assert_eq!(projected.home_hotel, "vps-jane");

        let _ = fs::remove_file(&context_path);
    }

    #[test]
    fn patch_operator_user_record_updates_profile_fields_and_active_session_name() {
        let context_path = temp_db_path("operator-user-patch");
        ensure_operator_auth_tables(&context_path, "mac-jane").unwrap();
        let session = issue_operator_session(
            &context_path,
            "mac-jane",
            "Operator",
            "bootstrap_token",
            Some("startup-bootstrap".into()),
        )
        .unwrap();

        let updated = patch_operator_user_record(
            &context_path,
            "mac-jane",
            "root-user:mac-jane",
            Some("Jared Likes".into()),
            Some("Jared".into()),
            Some("jared@example.com".into()),
            Some("enriched".into()),
        )
        .unwrap();

        assert_eq!(updated.display_name, "Jared Likes");
        assert_eq!(updated.preferred_name.as_deref(), Some("Jared"));
        assert_eq!(updated.primary_email.as_deref(), Some("jared@example.com"));
        assert_eq!(updated.onboarding_state, "enriched");

        let resolved_session = resolve_operator_session(&context_path, &session.session_token)
            .unwrap()
            .expect("session should still resolve");
        assert_eq!(resolved_session.display_name, "Jared Likes");

        let _ = fs::remove_file(&context_path);
    }

    #[test]
    fn consumed_operator_auth_challenge_cannot_be_consumed_twice() {
        let context_path = temp_db_path("operator-auth-challenge-consume");
        ensure_operator_auth_tables(&context_path, "mac-jane").unwrap();
        let challenge = issue_operator_auth_challenge(
            &context_path,
            "mac-jane",
            "desktop_membrane",
            "oidc",
            "google",
            None,
            Some("desktop.jaredlikes.com".into()),
            Some("pkce-verifier".into()),
        )
        .unwrap();

        let first = consume_operator_auth_challenge(&context_path, &challenge.challenge_id)
            .unwrap()
            .expect("first consume should succeed");
        assert_eq!(first.status, "consumed");
        assert!(first.consumed_at.is_some());

        let second =
            consume_operator_auth_challenge(&context_path, &challenge.challenge_id).unwrap();
        assert!(second.is_none());

        let _ = fs::remove_file(&context_path);
    }

    #[test]
    fn oidc_provider_status_reports_google_callback_from_resolved_config() {
        let provider = oidc_provider_config_from_resolved(
            "google",
            OidcResolvedConfig {
                public_base_url: "https://brain.jaredlikes.com".into(),
                public_base_source: "hotel-config".into(),
                google: OidcProviderSettings {
                    client_id: Some("google-client".into()),
                    client_secret_ref: Some("vault:google".into()),
                },
                github: OidcProviderSettings {
                    client_id: None,
                    client_secret_ref: None,
                },
            },
        )
        .unwrap()
        .expect("google provider");
        assert!(provider.configured);
        assert_eq!(
            provider.redirect_uri,
            "https://brain.jaredlikes.com/auth/oidc/google/callback"
        );
    }

    #[test]
    fn google_auth_url_contains_state_pkce_and_redirect_uri() {
        let provider = oidc_provider_config_from_resolved(
            "google",
            OidcResolvedConfig {
                public_base_url: "https://brain.jaredlikes.com".into(),
                public_base_source: "hotel-config".into(),
                google: OidcProviderSettings {
                    client_id: Some("google-client".into()),
                    client_secret_ref: Some("vault:google".into()),
                },
                github: OidcProviderSettings {
                    client_id: None,
                    client_secret_ref: None,
                },
            },
        )
        .unwrap()
        .expect("google provider");
        let url = provider
            .authorization_url("state-123", "challenge-456")
            .unwrap();
        assert!(url.contains("client_id=google-client"));
        assert!(url.contains("state=state-123"));
        assert!(url.contains("code_challenge=challenge-456"));
        assert!(url.contains(
            "redirect_uri=https%3A%2F%2Fbrain.jaredlikes.com%2Fauth%2Foidc%2Fgoogle%2Fcallback"
        ));
    }

    #[test]
    fn loopback_request_header_base_requires_bootstrap_instead_of_oidc() {
        assert!(oidc_loopback_bootstrap_only(&OidcResolvedConfig {
            public_base_url: "http://127.0.0.1:7700".into(),
            public_base_source: "request-headers".into(),
            google: OidcProviderSettings {
                client_id: Some("google-client".into()),
                client_secret_ref: Some("vault:google".into()),
            },
            github: OidcProviderSettings {
                client_id: Some("github-client".into()),
                client_secret_ref: Some("vault:github".into()),
            },
        }));
        assert!(!oidc_loopback_bootstrap_only(&OidcResolvedConfig {
            public_base_url: "https://brain.jaredlikes.com".into(),
            public_base_source: "hotel-config".into(),
            google: OidcProviderSettings {
                client_id: Some("google-client".into()),
                client_secret_ref: Some("vault:google".into()),
            },
            github: OidcProviderSettings {
                client_id: Some("github-client".into()),
                client_secret_ref: Some("vault:github".into()),
            },
        }));
    }

    #[test]
    fn oidc_identity_from_google_userinfo_uses_subject_and_email() {
        let provider = oidc_provider_config_from_resolved(
            "google",
            OidcResolvedConfig {
                public_base_url: "https://brain.jaredlikes.com".into(),
                public_base_source: "hotel-config".into(),
                google: OidcProviderSettings {
                    client_id: Some("google-client".into()),
                    client_secret_ref: Some("vault:google".into()),
                },
                github: OidcProviderSettings {
                    client_id: None,
                    client_secret_ref: None,
                },
            },
        )
        .unwrap()
        .expect("google provider");

        let identity = oidc_identity_from_userinfo(
            &provider,
            &json!({
                "sub": "google-subject-1",
                "name": "Jared Likes",
                "email": "jared@example.com"
            }),
        )
        .unwrap();

        assert_eq!(identity.provider, "google");
        assert_eq!(identity.provider_subject, "google-subject-1");
        assert_eq!(identity.display_name, "Jared Likes");
        assert_eq!(identity.email.as_deref(), Some("jared@example.com"));
        assert!(identity.login.is_none());
    }

    #[test]
    fn oidc_identity_from_github_userinfo_uses_numeric_id_and_login() {
        let provider = oidc_provider_config_from_resolved(
            "github",
            OidcResolvedConfig {
                public_base_url: "https://brain.jaredlikes.com".into(),
                public_base_source: "hotel-config".into(),
                google: OidcProviderSettings {
                    client_id: None,
                    client_secret_ref: None,
                },
                github: OidcProviderSettings {
                    client_id: Some("github-client".into()),
                    client_secret_ref: Some("vault:github".into()),
                },
            },
        )
        .unwrap()
        .expect("github provider");

        let identity = oidc_identity_from_userinfo(
            &provider,
            &json!({
                "id": 42,
                "login": "jaredlikes",
                "name": "Jared Likes",
                "email": "jared@users.noreply.github.com"
            }),
        )
        .unwrap();

        assert_eq!(identity.provider, "github");
        assert_eq!(identity.provider_subject, "42");
        assert_eq!(identity.display_name, "Jared Likes");
        assert_eq!(identity.login.as_deref(), Some("jaredlikes"));
        assert_eq!(
            identity.email.as_deref(),
            Some("jared@users.noreply.github.com")
        );
    }

    #[test]
    fn ensure_operator_auth_tables_seeds_root_user_key_ref_from_env() {
        let context_path = temp_db_path("root-user-key-ref");
        let key_id = format!("test-key-{}", uuid::Uuid::new_v4());
        let root_key = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);

        // Restores the previous values on drop rather than deleting them.
        // Rust runs tests as threads of a single process, so an unconditional
        // remove_var here deleted the vault key out from under whatever ran
        // concurrently — including the value CI provides.
        let _key_id_env = EnvVarGuard::set("PHILOTIC_VAULT_KEY_ID", &key_id);
        let _root_key_env = EnvVarGuard::set("PHILOTIC_VAULT_MASTER_KEY", &root_key);

        ensure_operator_auth_tables(&context_path, "mac-jane").unwrap();
        let refs = list_root_user_key_refs(&context_path, "mac-jane").unwrap();
        let root_ref = refs
            .iter()
            .find(|entry| entry.key_purpose == "vault-root-key")
            .expect("root key ref should exist");
        assert_eq!(root_ref.user_id, "root-user:mac-jane");
        assert_eq!(root_ref.rotation_state, "active");
        assert_eq!(root_ref.source_kind, "env");
        let expected_ref = format!("env://PHILOTIC_VAULT_MASTER_KEY/{key_id}");
        assert_eq!(root_ref.vault_ref.as_deref(), Some(expected_ref.as_str()));
        assert!(root_ref.public_fingerprint.is_some());

        let _ = fs::remove_file(&context_path);
    }

    #[test]
    fn revoked_operator_session_stops_resolving() {
        let context_path = temp_db_path("operator-session-revoked");
        ensure_operator_auth_tables(&context_path, "mac-jane").unwrap();
        let session = issue_operator_session(
            &context_path,
            "mac-jane",
            "Jared",
            "bootstrap_token",
            Some("startup-bootstrap".into()),
        )
        .unwrap();
        revoke_operator_session(&context_path, &session.session_token).unwrap();

        let resolved = resolve_operator_session(&context_path, &session.session_token).unwrap();
        assert!(resolved.is_none());

        let _ = fs::remove_file(&context_path);
    }

    // ── Bind resolution ─────────────────────────────────────────────────────

    #[test]
    fn resolve_web_bind_defaults_to_loopback() {
        let addr = resolve_web_bind(None, None).unwrap();
        assert_eq!(addr, IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    #[test]
    fn resolve_web_bind_env_wins_over_config() {
        let addr = resolve_web_bind(Some("100.79.239.64"), Some("192.168.1.10")).unwrap();
        assert_eq!(addr, "100.79.239.64".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn resolve_web_bind_falls_back_to_config() {
        let addr = resolve_web_bind(None, Some("192.168.1.10")).unwrap();
        assert_eq!(addr, "192.168.1.10".parse::<IpAddr>().unwrap());
    }

    #[test]
    fn resolve_web_bind_blank_env_falls_through() {
        let addr = resolve_web_bind(Some("   "), None).unwrap();
        assert_eq!(addr, IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    #[test]
    fn parse_bind_host_accepts_ipv6_and_localhost_and_trims() {
        assert_eq!(
            parse_bind_host(" ::1 ").unwrap(),
            "::1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            parse_bind_host("LOCALHOST").unwrap(),
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        );
        assert_eq!(
            parse_bind_host("0.0.0.0").unwrap(),
            "0.0.0.0".parse::<IpAddr>().unwrap()
        );
    }

    #[test]
    fn parse_bind_host_rejects_garbage() {
        assert!(parse_bind_host("not-an-ip").is_err());
        assert!(parse_bind_host("127.0.0.1:7700").is_err());
        assert!(parse_bind_host("").is_err());
    }

    #[test]
    fn display_host_for_unspecified_and_ipv6() {
        assert_eq!(display_host_for("0.0.0.0".parse().unwrap()), "127.0.0.1");
        assert_eq!(display_host_for("::1".parse().unwrap()), "[::1]");
        assert_eq!(
            display_host_for("100.79.239.64".parse().unwrap()),
            "100.79.239.64"
        );
    }

    // ── Edge bearer token ───────────────────────────────────────────────────

    #[test]
    fn constant_time_eq_semantics() {
        assert!(constant_time_eq(b"abc123", b"abc123"));
        assert!(!constant_time_eq(b"abc123", b"abc124"));
        assert!(!constant_time_eq(b"abc123", b"abc12"));
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn resolve_edge_token_precedence() {
        assert_eq!(
            resolve_edge_token(Some("env-tok".into()), Some("cfg-tok".into())).as_deref(),
            Some("env-tok")
        );
        assert_eq!(
            resolve_edge_token(None, Some("cfg-tok".into())).as_deref(),
            Some("cfg-tok")
        );
        assert_eq!(resolve_edge_token(Some("  ".into()), None), None);
        assert_eq!(resolve_edge_token(None, None), None);
    }

    fn test_state(edge_token: Option<&str>, tier: ExposureTier) -> AppState {
        let (tx, _) = broadcast::channel::<String>(4);
        AppState {
            bootstrap_token: Arc::new("philotic-test".into()),
            db_path: temp_db_path("fence"),
            config_path: Arc::new(PathBuf::from("/nonexistent/mesh-config.json")),
            hotel: Arc::new("mac-jane".into()),
            socket: Arc::new(format!("/tmp/philotic-test-{}.sock", uuid::Uuid::new_v4())),
            tx,
            edge_token: edge_token.map(|t| Arc::new(t.to_string())),
            exposure_tier: tier,
            edge: edge::EdgeState::load(
                std::env::temp_dir().join(format!(
                    "philotic-web-test-edge-devices-{}.json",
                    uuid::Uuid::new_v4()
                )),
                Some("INV-TEST".into()),
            ),
        }
    }

    /// `/api/auth/status` is unauthenticated by necessity — the login screen
    /// needs the provider list — so it must disclose nothing else. Seeds real
    /// rows first so this proves suppression, not merely an empty database.
    #[tokio::test]
    async fn auth_status_withholds_operator_data_until_authenticated() {
        let state = test_state(None, ExposureTier::Local);
        ensure_operator_auth_tables(&state.db_path, &state.hotel).unwrap();
        upsert_operator_external_identity_link(
            &state.db_path,
            &state.hotel,
            &OidcIdentity {
                provider: "google".into(),
                provider_subject: "1234567890".into(),
                display_name: "Test Operator".into(),
                email: Some("operator@example.com".into()),
                login: None,
            },
        )
        .unwrap();

        // Precondition: the data this route used to leak really is present.
        assert!(
            !list_root_user_key_refs(&state.db_path, &state.hotel)
                .unwrap()
                .is_empty(),
            "test needs a seeded key ref to prove suppression"
        );

        let response = handle_auth_status(HeaderMap::new(), State(state.clone())).await;
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["authenticated"], Value::Bool(false));
        assert_eq!(json["hotel"], Value::String("mac-jane".into()));
        assert!(
            json["root_user_key_refs"].as_array().unwrap().is_empty(),
            "vault key refs must not reach an unauthenticated caller: {json}"
        );
        assert!(
            json["external_identity_links"]
                .as_array()
                .unwrap()
                .is_empty(),
            "operator identity links must not reach an unauthenticated caller: {json}"
        );
        assert!(json.get("session").is_none() || json["session"].is_null());

        // Belt and braces: no vault-ref scheme or operator email anywhere in the
        // serialized payload, regardless of which field might carry it.
        let raw = String::from_utf8_lossy(&body);
        for needle in ["keychain://", "env://", "sha256:", "operator@example.com"] {
            assert!(
                !raw.contains(needle),
                "unauthenticated auth-status leaked {needle}: {raw}"
            );
        }

        let _ = fs::remove_file(&state.db_path);
    }

    fn bearer_headers(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        headers
    }

    #[test]
    fn edge_bearer_authorized_matches_configured_token() {
        let state = test_state(Some("edge-secret"), ExposureTier::Mesh);
        assert!(edge_bearer_authorized(
            &bearer_headers("edge-secret"),
            &state
        ));
        assert!(!edge_bearer_authorized(&bearer_headers("wrong"), &state));
        assert!(!edge_bearer_authorized(&HeaderMap::new(), &state));

        let no_token = test_state(None, ExposureTier::Mesh);
        assert!(!edge_bearer_authorized(
            &bearer_headers("edge-secret"),
            &no_token
        ));
    }

    #[test]
    fn edge_bearer_accepts_enrolled_device_tokens() {
        let state = test_state(Some("edge-secret"), ExposureTier::Mesh);
        let enrolled = state
            .edge
            .enroll(&philotic_edge_protocol::EnrollmentRequest {
                invite_code: "INV-TEST".into(),
                device_pubkey_b64: "cHVia2V5".into(),
                device_name: "Jared's iPhone".into(),
                platform: "ios".into(),
            })
            .unwrap();

        // Device token authorizes the bearer path and resolves to its node.
        assert!(edge_bearer_authorized(
            &bearer_headers(&enrolled.edge_token),
            &state
        ));
        assert_eq!(
            edge::edge_bearer_identity(&bearer_headers(&enrolled.edge_token), &state),
            Some(edge::EdgeBearerIdentity::Device(enrolled.node_id.clone()))
        );
        // The shared token resolves to the shared identity.
        assert_eq!(
            edge::edge_bearer_identity(&bearer_headers("edge-secret"), &state),
            Some(edge::EdgeBearerIdentity::Shared)
        );
        // Bad tokens still fail.
        assert_eq!(
            edge::edge_bearer_identity(&bearer_headers("edge-tok-forged"), &state),
            None
        );
    }

    #[test]
    fn fence_leaves_edge_enrollment_reachable_without_credentials() {
        let state = test_state(None, ExposureTier::Mesh);
        let peer: IpAddr = "100.64.1.9".parse().unwrap();
        assert!(edge_fence_allows(
            &state,
            &Method::POST,
            "/api/edge/enroll",
            &HeaderMap::new(),
            peer
        ));
        // The edge WS stays behind the fence.
        assert!(!edge_fence_allows(
            &state,
            &Method::GET,
            "/api/edge/ws",
            &HeaderMap::new(),
            peer
        ));
    }

    #[test]
    fn fence_blocks_edge_enrollment_on_internet_tier() {
        // A static invite code is not a strong enough credential for a
        // public-facing bind — enrollment must not be reachable there.
        let state = test_state(None, ExposureTier::Internet);
        let peer: IpAddr = "203.0.113.9".parse().unwrap();
        assert!(!edge_fence_allows(
            &state,
            &Method::POST,
            "/api/edge/enroll",
            &HeaderMap::new(),
            peer
        ));
    }

    // ── Perimeter fence ─────────────────────────────────────────────────────

    #[test]
    fn fence_local_tier_allows_everything() {
        let state = test_state(None, ExposureTier::Local);
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(edge_fence_allows(
            &state,
            &Method::GET,
            "/api/status",
            &HeaderMap::new(),
            peer
        ));
    }

    #[test]
    fn fence_mesh_tier_blocks_api_without_credentials() {
        let state = test_state(Some("edge-secret"), ExposureTier::Mesh);
        let peer: IpAddr = "100.64.1.9".parse().unwrap();
        assert!(!edge_fence_allows(
            &state,
            &Method::GET,
            "/api/status",
            &HeaderMap::new(),
            peer
        ));
        assert!(!edge_fence_allows(
            &state,
            &Method::GET,
            "/ws",
            &HeaderMap::new(),
            peer
        ));
        // wrong bearer stays blocked
        assert!(!edge_fence_allows(
            &state,
            &Method::GET,
            "/api/status",
            &bearer_headers("wrong"),
            peer
        ));
    }

    #[test]
    fn edge_bearer_is_scoped_to_the_edge_surface() {
        let state = test_state(Some("edge-secret"), ExposureTier::Mesh);
        let peer: IpAddr = "100.64.1.9".parse().unwrap();

        // The edge bearer opens the edge WS...
        assert!(edge_fence_allows(
            &state,
            &Method::GET,
            "/api/edge/ws",
            &bearer_headers("edge-secret"),
            peer
        ));

        // ...but never the operator API or the operator /ws firehose: a
        // device credential must not read secrets, mutate config, or watch
        // every session.
        for path in [
            "/api/mesh/roster",
            "/api/secrets",
            "/api/secrets/rotate",
            "/api/vault",
            "/api/config/some-key",
            "/api/event-log",
            "/ws",
        ] {
            assert!(
                !edge_fence_allows(
                    &state,
                    &Method::GET,
                    path,
                    &bearer_headers("edge-secret"),
                    peer
                ),
                "edge bearer must not open {path}"
            );
        }

        // An enrolled per-device token is equally scoped.
        let enrolled = state
            .edge
            .enroll(&philotic_edge_protocol::EnrollmentRequest {
                invite_code: "INV-TEST".into(),
                device_pubkey_b64: "cHVia2V5LWZlbmNl".into(),
                device_name: "Jared's iPhone".into(),
                platform: "ios".into(),
            })
            .unwrap();
        assert!(edge_fence_allows(
            &state,
            &Method::GET,
            "/api/edge/ws",
            &bearer_headers(&enrolled.edge_token),
            peer
        ));
        assert!(!edge_fence_allows(
            &state,
            &Method::GET,
            "/api/secrets",
            &bearer_headers(&enrolled.edge_token),
            peer
        ));

        // check_auth itself never honours an edge bearer.
        assert!(!check_auth(&bearer_headers("edge-secret"), &state));
        assert!(!check_auth(&bearer_headers(&enrolled.edge_token), &state));
    }

    #[test]
    fn fence_leaves_non_api_surfaces_and_preflight_open() {
        let state = test_state(None, ExposureTier::Mesh);
        let peer: IpAddr = "100.64.1.9".parse().unwrap();
        // /health, static UI, and OIDC callback are outside the gate
        for path in [
            "/health",
            "/",
            "/assets/app.js",
            "/auth/oidc/github/callback",
        ] {
            assert!(
                edge_fence_allows(&state, &Method::GET, path, &HeaderMap::new(), peer),
                "expected {path} to stay reachable"
            );
        }
        // CORS preflight carries no credentials
        assert!(edge_fence_allows(
            &state,
            &Method::OPTIONS,
            "/api/status",
            &HeaderMap::new(),
            peer
        ));
    }

    #[test]
    fn fence_loopback_bypass_up_to_mesh_but_not_internet() {
        let loopback: IpAddr = "127.0.0.1".parse().unwrap();
        let mesh_state = test_state(None, ExposureTier::Mesh);
        assert!(edge_fence_allows(
            &mesh_state,
            &Method::GET,
            "/api/status",
            &HeaderMap::new(),
            loopback
        ));

        let internet_state = test_state(None, ExposureTier::Internet);
        assert!(!edge_fence_allows(
            &internet_state,
            &Method::GET,
            "/api/status",
            &HeaderMap::new(),
            loopback
        ));
    }

    // ── Mesh roster ─────────────────────────────────────────────────────────

    #[test]
    fn parse_mesh_roster_accepts_bare_array_and_wrapped_object() {
        let raw = r#"[{"node_id":"mbp-jane","endpoints":["http://100.79.239.64:7700"]}]"#;
        let hotels = parse_mesh_roster_json(raw).unwrap();
        assert_eq!(hotels.len(), 1);
        assert_eq!(hotels[0].node_id, "mbp-jane");
        assert_eq!(hotels[0].endpoints, vec!["http://100.79.239.64:7700"]);
        assert_eq!(hotels[0].display_name, None);

        let wrapped =
            r#"{"hotels":[{"node_id":"vps-jane","display_name":"VPS Jane","endpoints":[]}]}"#;
        let hotels = parse_mesh_roster_json(wrapped).unwrap();
        assert_eq!(hotels.len(), 1);
        assert_eq!(hotels[0].node_id, "vps-jane");
        assert_eq!(hotels[0].display_name.as_deref(), Some("VPS Jane"));
        assert!(hotels[0].endpoints.is_empty());
    }

    #[test]
    fn parse_mesh_roster_endpoints_default_to_empty() {
        let raw = r#"[{"node_id":"mac-jane"}]"#;
        let hotels = parse_mesh_roster_json(raw).unwrap();
        assert!(hotels[0].endpoints.is_empty());
    }

    #[test]
    fn parse_mesh_roster_rejects_invalid_shapes() {
        assert!(parse_mesh_roster_json("not json").is_err());
        assert!(parse_mesh_roster_json(r#"{"node_id":"missing-array-wrapper"}"#).is_err());
        assert!(
            parse_mesh_roster_json(r#"[{"endpoints":[]}]"#).is_err(),
            "node_id is required"
        );
    }

    #[test]
    fn mesh_roster_view_serializes_for_clients() {
        let view = MeshRosterView {
            hotels: vec![MeshRosterHotelView {
                node_id: "mbp-jane".into(),
                display_name: None,
                endpoints: vec!["http://100.79.239.64:7700".into()],
            }],
            nodes: vec![],
            source: "env".into(),
        };
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(
            value,
            json!({
                "hotels": [{
                    "node_id": "mbp-jane",
                    "endpoints": ["http://100.79.239.64:7700"],
                }],
                "source": "env",
            })
        );
    }

    #[test]
    fn load_mesh_roster_reads_config_web_roster() {
        let config_path = std::env::temp_dir().join(format!(
            "philotic-web-roster-config-{}.json",
            uuid::Uuid::new_v4()
        ));
        fs::write(
            &config_path,
            r#"{"web_roster":[{"node_id":"vps-jane","endpoints":["http://100.64.212.8:7700"]}]}"#,
        )
        .unwrap();
        let mut state = test_state(None, ExposureTier::Local);
        state.config_path = Arc::new(config_path.clone());

        let view = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(load_mesh_roster(&state));
        assert_eq!(view.source, "config");
        assert_eq!(view.hotels.len(), 1);
        assert_eq!(view.hotels[0].node_id, "vps-jane");

        let _ = fs::remove_file(&config_path);
    }

    #[test]
    fn load_mesh_roster_unconfigured_is_empty() {
        let state = test_state(None, ExposureTier::Local);
        let view = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(load_mesh_roster(&state));
        assert_eq!(view.source, "unconfigured");
        assert!(view.hotels.is_empty());
    }
}
