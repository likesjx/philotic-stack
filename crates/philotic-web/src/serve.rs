//! `philotic-web serve` — local management HTTP + WebSocket server.
//!
//! Generates a hotel-issued bootstrap token on startup and requires an
//! explicit bootstrap/login ceremony before issuing a bounded same-origin
//! operator session cookie for the embedded UI.
//!
//! REST endpoints:
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
//!   POST /api/mesh/targets/:target_node_id/agents/:agent_id/chat
//!   GET  /api/event-log
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

use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
use ansible_mesh_core::storage::{ProjectedExternalIdentityRecord, ProjectedUserIdentityRecord};
use anyhow::{anyhow, bail, Context, Result};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Json, Response},
    routing::{delete, get, post, put},
    Router,
};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::Rng;
use reqwest::Url;
use rusqlite::{Connection, OptionalExtension};
use rust_embed::RustEmbed;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::{broadcast, watch, Mutex};
use tower_http::cors::{AllowOrigin, CorsLayer};

use philotic_client::{
    ComponentManifest, CronJob, CronJobSource, DesktopMembraneAgentView, DesktopMembraneGuestView,
    DesktopMembraneStatusView, GuestIdentity, IpcRequest, IpcResponse, LeaseEnvelope,
    OperatorTargetAgentInventoryView, OperatorTargetGuestInventoryView, OperatorTargetStatusView,
    OperatorTargetView, PhiloticClient, ResponseRoutePolicyView, OPERATOR_CHAT_REPLY_ROLE,
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
    bootstrap_token: Arc<String>,
    db_path: PathBuf,
    hotel: Arc<String>,
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
    callback_url: String,
}

#[derive(Debug, Clone)]
struct OidcProviderSettings {
    client_id: Option<String>,
    client_secret: Option<String>,
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
    let socket = crate::start::socket_path(&hotel);
    let lease_key = desktop_membrane_lease_key(&hotel);
    let lease_handle = acquire_desktop_membrane_lease(&socket, &lease_key, port).await?;

    // Generate bootstrap token for the first login ceremony.
    let bootstrap_token: String = {
        let bytes: [u8; 24] = rand::thread_rng().gen();
        format!("philotic-{}", hex::encode(bytes))
    };

    // Broadcast channel for WebSocket events (capacity 256)
    let (tx, _) = broadcast::channel::<String>(256);

    let state = AppState {
        bootstrap_token: Arc::new(bootstrap_token.clone()),
        db_path,
        hotel: Arc::new(hotel),
        socket: Arc::new(socket),
        tx,
    };

    ensure_operator_auth_tables(&state.db_path, &state.hotel)?;

    // CORS — localhost only; UI is embedded and served from the same origin
    let cors = build_cors(allow_origins.as_deref());

    let app = Router::new()
        // API routes
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
        .route("/setup-guide", get(handle_setup_guide))
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
    println!("  Bootstrap token: {bootstrap_token}");
    println!();
    println!("  Press Ctrl-C to stop.");

    let open_path = normalized_open_path(open_path.as_deref());
    let browser_url = format!("http://127.0.0.1:{port}{open_path}");

    // Auto-open the embedded desktop in the default browser
    let _ = tokio::process::Command::new("open")
        .arg(&browser_url)
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
        <p>The management UI is where model setup should become legible instead of hidden in config folklore.</p>
        <ul>
          <li>Remote Gemini path: confirm the configured default model and the Gemini secret/config entries.</li>
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

async fn handle_auth_status(headers: HeaderMap, State(state): State<AppState>) -> Response {
    let session = current_operator_session(&headers, &state);
    let root_user_key_refs =
        list_root_user_key_refs(&state.db_path, &state.hotel).unwrap_or_default();
    let external_identity_links =
        list_external_identity_links(&state.db_path, &state.hotel).unwrap_or_default();
    let oidc_providers = list_oidc_provider_statuses(&state.socket, Some(&headers)).await;
    Json(AuthStatusView {
        authenticated: session.is_some(),
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
    if body.bootstrap_token != *state.bootstrap_token {
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
    let bind_label = body
        .bind_label
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

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

    let token = match exchange_oidc_code(&provider, code, code_verifier).await {
        Ok(token) => token,
        Err(err) => {
            return oidc_callback_error_response(
                StatusCode::BAD_GATEWAY,
                format!("OIDC token exchange failed: {err}"),
            );
        }
    };

    let identity = match fetch_oidc_identity(&provider, &token.access_token).await {
        Ok(identity) => identity,
        Err(err) => {
            return oidc_callback_error_response(
                StatusCode::BAD_GATEWAY,
                format!("OIDC identity fetch failed: {err}"),
            );
        }
    };

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

    let redirect_path = challenge.bind_label.unwrap_or_else(|| "/".into());
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

fn check_auth(headers: &HeaderMap, state: &AppState) -> bool {
    current_operator_session(headers, state).is_some()
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
                    env_trimmed("PHILOTIC_OIDC_GOOGLE_CLIENT_SECRET").is_some(),
                ),
                secret_configured: resolved.google.client_secret.is_some(),
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
                    env_trimmed("PHILOTIC_OIDC_GITHUB_CLIENT_SECRET").is_some(),
                ),
                secret_configured: resolved.github.client_secret.is_some(),
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
        ("oidc_google_client_secret", "oidc_google_client_secret_ref"),
        ("oidc_github_client_secret", "oidc_github_client_secret_ref"),
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
const MUTABLE_CONFIG_KEYS: &[&str] = &[
    "telegram_bot_token",
    "execution_host",
    "vault_registry",
    "oidc_public_base_url",
    "oidc_google_client_id",
    "oidc_google_client_secret_ref",
    "oidc_github_client_id",
    "oidc_github_client_secret_ref",
];

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

async fn ipc_get_secret(socket: &str, secret_ref: &str) -> Result<Option<Value>> {
    let mut client = connect_management_client(socket, "philotic-web-secret-read").await?;
    match client
        .send_request(IpcRequest::GetSecret {
            secret_ref: secret_ref.to_string(),
        })
        .await?
    {
        IpcResponse::SecretData {
            value_json: Some(raw),
            ..
        } => {
            let parsed: Value = serde_json::from_str(&raw).unwrap_or_else(|_| Value::String(raw));
            Ok(Some(parsed))
        }
        IpcResponse::SecretData {
            value_json: None, ..
        } => Ok(None),
        IpcResponse::Standard {
            ok: false, message, ..
        } => Err(anyhow!(message)),
        IpcResponse::Error(message) => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected get_secret response: {other:?}")),
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
    if current_operator_session(&headers, &state).is_none() {
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
        session_token: new_operator_chat_id("operator-token"),
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
        challenge_nonce: new_operator_chat_id("challenge-nonce"),
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
    let next_principal = if current_principal.starts_with("local-user:") {
        projected_principal_id_for_identity(identity)
    } else {
        current_principal
    };
    conn.execute(
        "UPDATE operator_users
         SET mesh_principal_id = ?2, updated_at = ?3
         WHERE user_id = ?1",
        rusqlite::params![&user_id, &next_principal, now],
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
    let projected = ProjectedUserIdentityRecord {
        principal_id: user.mesh_principal_id.clone(),
        local_user_id: user.user_id.clone(),
        home_hotel: user.home_hotel.clone(),
        display_name: user.display_name.clone(),
        preferred_name: user.preferred_name.clone(),
        primary_email: user.primary_email.clone(),
        linked_identities: links.iter().map(projected_external_identity).collect(),
        updated_at: now_epoch_secs().max(0) as u64,
    };
    let storage = SqliteGraphStorage::open(db_path)?;
    let graph = GraphDomain::new(Arc::new(storage.adapter()));
    graph.upsert_projected_user_identity(&projected)
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

fn default_operator_principal_id(hotel: &str) -> String {
    format!("local-user:{hotel}")
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

fn detect_root_user_key_ref(hotel: &str) -> RootUserKeyRefStatusView {
    let account = vault_root_key_account();
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

    match load_root_key_from_env() {
        Ok(key_bytes) => RootUserKeyRefStatusView {
            user_id: default_operator_user_id(hotel),
            key_purpose: "vault-root-key".into(),
            vault_ref: Some(format!(
                "env://PHILOTIC_VAULT_MASTER_KEY/{}",
                vault_root_key_account()
            )),
            public_fingerprint: Some(root_key_fingerprint(&key_bytes)),
            rotation_state: "active".into(),
            source_kind: "env".into(),
        },
        Err(_) => RootUserKeyRefStatusView {
            user_id: default_operator_user_id(hotel),
            key_purpose: "vault-root-key".into(),
            vault_ref: None,
            public_fingerprint: None,
            rotation_state: "unavailable".into(),
            source_kind: "missing".into(),
        },
    }
}

fn load_root_key_from_env() -> Result<Vec<u8>> {
    let raw = std::env::var("PHILOTIC_VAULT_MASTER_KEY")?;
    decode_root_key(raw.trim(), "PHILOTIC_VAULT_MASTER_KEY")
}

fn load_root_key_from_keychain(account: &str) -> Result<Option<Vec<u8>>> {
    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "ai.philotic.hotel-vault",
            "-a",
            account,
            "-w",
        ])
        .output()?;

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
    client_secret: Option<String>,
    auth_url: String,
    token_url: String,
    userinfo_url: String,
    scopes: Vec<String>,
    extra_auth_params: Vec<(String, String)>,
    redirect_uri: String,
}

#[derive(Debug, serde::Deserialize)]
struct OidcTokenResponse {
    access_token: String,
}

#[derive(Debug)]
struct OidcIdentity {
    provider: String,
    provider_subject: String,
    display_name: String,
    email: Option<String>,
    login: Option<String>,
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
                && resolved.google.client_secret.is_some(),
            client_id: resolved.google.client_id,
            client_secret: resolved.google.client_secret,
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            userinfo_url: "https://openidconnect.googleapis.com/v1/userinfo".into(),
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
                && resolved.github.client_secret.is_some(),
            client_id: resolved.github.client_id,
            client_secret: resolved.github.client_secret,
            auth_url: "https://github.com/login/oauth/authorize".into(),
            token_url: "https://github.com/login/oauth/access_token".into(),
            userinfo_url: "https://api.github.com/user".into(),
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
    let (google_client_secret_ref, google_client_secret) = oidc_secret_from_ref_or_env(
        socket,
        "oidc_google_client_secret_ref",
        "PHILOTIC_OIDC_GOOGLE_CLIENT_SECRET",
    )
    .await?;
    let (github_client_secret_ref, github_client_secret) = oidc_secret_from_ref_or_env(
        socket,
        "oidc_github_client_secret_ref",
        "PHILOTIC_OIDC_GITHUB_CLIENT_SECRET",
    )
    .await?;

    Ok(OidcResolvedConfig {
        public_base_url,
        public_base_source,
        google: OidcProviderSettings {
            client_id: google_client_id,
            client_secret: google_client_secret,
            client_secret_ref: google_client_secret_ref,
        },
        github: OidcProviderSettings {
            client_id: github_client_id,
            client_secret: github_client_secret,
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

async fn oidc_secret_from_ref_or_env(
    socket: &str,
    ref_key: &str,
    env_key: &str,
) -> Result<(Option<String>, Option<String>)> {
    if let Some(secret_ref) = oidc_config_string(socket, ref_key).await? {
        let secret = ipc_get_secret(socket, &secret_ref)
            .await?
            .and_then(|value| value.as_str().map(|value| value.to_string()));
        return Ok((Some(secret_ref), secret));
    }
    Ok((None, env_trimmed(env_key)))
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

async fn exchange_oidc_code(
    provider: &OidcProviderConfig,
    code: &str,
    code_verifier: &str,
) -> Result<OidcTokenResponse> {
    let client_id = provider
        .client_id
        .as_deref()
        .context("OIDC client id is not configured")?;
    let mut form = vec![
        ("client_id", client_id.to_string()),
        ("code", code.to_string()),
        ("code_verifier", code_verifier.to_string()),
        ("grant_type", "authorization_code".into()),
        ("redirect_uri", provider.redirect_uri.clone()),
    ];
    if let Some(client_secret) = provider.client_secret.as_deref() {
        form.push(("client_secret", client_secret.to_string()));
    }

    let response = reqwest::Client::new()
        .post(&provider.token_url)
        .header("accept", "application/json")
        .header("user-agent", "philotic-web/0.1")
        .form(&form)
        .send()
        .await
        .context("failed to exchange OIDC authorization code")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!(
            "token exchange failed ({}): {}",
            status.as_u16(),
            body.trim()
        );
    }

    response
        .json::<OidcTokenResponse>()
        .await
        .context("failed to decode OIDC token response")
}

async fn fetch_oidc_identity(
    provider: &OidcProviderConfig,
    access_token: &str,
) -> Result<OidcIdentity> {
    let response = reqwest::Client::new()
        .get(&provider.userinfo_url)
        .header("accept", "application/json")
        .header("user-agent", "philotic-web/0.1")
        .bearer_auth(access_token)
        .send()
        .await
        .context("failed to fetch OIDC userinfo")?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        bail!(
            "userinfo fetch failed ({}): {}",
            status.as_u16(),
            body.trim()
        );
    }

    let body: Value = response
        .json()
        .await
        .context("failed to decode OIDC userinfo response")?;
    oidc_identity_from_userinfo(provider, &body)
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
    use std::fs;

    fn temp_db_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("philotic-web-{name}-{}.db", uuid::Uuid::new_v4()))
    }

    #[test]
    fn summarize_router_event_includes_failure_code() {
        assert_eq!(
            summarize_router_event("text.generate", "gemini", "failure", Some("RATE_LIMIT")),
            "text.generate via gemini failed (RATE_LIMIT)"
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
                    client_secret: Some("google-secret".into()),
                    client_secret_ref: Some("vault:google".into()),
                },
                github: OidcProviderSettings {
                    client_id: None,
                    client_secret: None,
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
                    client_secret: Some("google-secret".into()),
                    client_secret_ref: Some("vault:google".into()),
                },
                github: OidcProviderSettings {
                    client_id: None,
                    client_secret: None,
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
                client_secret: Some("google-secret".into()),
                client_secret_ref: Some("vault:google".into()),
            },
            github: OidcProviderSettings {
                client_id: Some("github-client".into()),
                client_secret: Some("github-secret".into()),
                client_secret_ref: Some("vault:github".into()),
            },
        }));
        assert!(!oidc_loopback_bootstrap_only(&OidcResolvedConfig {
            public_base_url: "https://brain.jaredlikes.com".into(),
            public_base_source: "hotel-config".into(),
            google: OidcProviderSettings {
                client_id: Some("google-client".into()),
                client_secret: Some("google-secret".into()),
                client_secret_ref: Some("vault:google".into()),
            },
            github: OidcProviderSettings {
                client_id: Some("github-client".into()),
                client_secret: Some("github-secret".into()),
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
                    client_secret: Some("google-secret".into()),
                    client_secret_ref: Some("vault:google".into()),
                },
                github: OidcProviderSettings {
                    client_id: None,
                    client_secret: None,
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
                    client_secret: None,
                    client_secret_ref: None,
                },
                github: OidcProviderSettings {
                    client_id: Some("github-client".into()),
                    client_secret: Some("github-secret".into()),
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

        unsafe {
            std::env::set_var("PHILOTIC_VAULT_KEY_ID", &key_id);
            std::env::set_var("PHILOTIC_VAULT_MASTER_KEY", &root_key);
        }

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

        unsafe {
            std::env::remove_var("PHILOTIC_VAULT_KEY_ID");
            std::env::remove_var("PHILOTIC_VAULT_MASTER_KEY");
        }
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
}
