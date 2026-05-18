use ansible_mesh_core::beacon::BeaconDaemon;
use ansible_mesh_core::graph::{AbstractSkillRecord, AbstractToolRecord, ToolsetProfileRecord};
use ansible_mesh_core::heartbeat::{emit_capability_sync, emit_heartbeat};
use ansible_mesh_core::membership::{
    MeshMembershipAcceptPayload, derive_transport_session_key, fingerprint_from_base64url,
    now_epoch_secs, verify_join_request,
};
use ansible_mesh_core::registry::{CapabilityAdvertisement, ExecutionReachability, NodeRegistry};
use ansible_mesh_core::storage::{
    AgentIdentityRecord, CursorStorage, EventStorage, GuestRecord, HotelRecord,
};
use ansible_mesh_core::{NodeCapabilities, NodeHealthSnapshot, NodeRole};
use anyhow::{Context, Result};
use axum::body::{Body, to_bytes};
use axum::extract::{Path as AxumPath, Query, Request, State};
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use clap::{Parser, Subcommand, ValueEnum};
use philotic_client::{
    GuestIdentity, IpcRequest, IpcResponse, OPERATOR_SURFACE_QUERY_HANDOFF_KIND,
    OPERATOR_SURFACE_QUERY_ROLE, OperatorSurfaceQueryHandoff, OperatorTargetGuestInventoryView,
    OperatorTargetStatusView, PhiloticClient,
};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::net::{SocketAddr, TcpListener, UdpSocket as StdUdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::{Mutex, RwLock, broadcast, mpsc};
use tracing::{debug, error, info, warn};

mod auth;
mod dream;
mod graph;
mod memory;
mod muninn_provision;
mod vault;

mod service;
use service::blob::BlobService;
use service::cron_ticker::CronTicker;
use service::ipc::IpcServer;
use std::sync::Arc;

// ── Profile path resolution ────────────────────────────────────────────────

/// Returns `~/.philotic/<profile>/` when `PHILOTIC_PROFILE` is set, else `None`.
///
/// When `Some`, all runtime paths (DB, socket) are namespaced to that directory
/// so that two profiles never collide. When `None`, legacy path behavior applies.
fn profile_dir() -> Option<PathBuf> {
    let profile = std::env::var("PHILOTIC_PROFILE")
        .ok()
        .filter(|s| !s.is_empty())?;
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".philotic").join(profile))
}

use ansible_mesh_core::domain::GraphDomain;
use ansible_mesh_core::event::EventEnvelope;
use auth::AuthCommand;
use vault::{SecretAccess, SecretInput, resolve_secret, store_secret};

/// Instructions for the strictly-serialized DB writer thread
pub enum LedgerCommand {
    /// A new event spawned locally via IPC that needs to be durably outboxed
    AppendLocal(EventEnvelope),
    /// A batch of events received over the mesh that need to be durably inboxed
    CommitInboundBatch {
        events: Vec<EventEnvelope>,
        source_node: String,
    },
    /// An ACK from a remote node that advances our delivery cursor
    ProcessAck {
        consumer_node_id: String,
        acked_seq: u64,
    },
}

type BeaconInboxReceiver = Arc<Mutex<Option<mpsc::Receiver<ansible_mesh_core::BeaconMessage>>>>;
type WebRtcSignalReceiver =
    Arc<Mutex<Option<mpsc::Receiver<ansible_mesh_core::webrtc::WebRtcSignalMessage>>>>;

#[derive(Clone)]
struct MeshRuntimeContext {
    hotel_name: String,
    hotel: HotelRecord,
    caps: NodeCapabilities,
    mesh_addr: String,
    execution_addr: String,
    db_path: String,
    enable_rust_auth: bool,
    enable_rust_dispatcher: bool,
    graph_domain: Arc<GraphDomain>,
    registry: Arc<RwLock<NodeRegistry>>,
    ledger: Arc<dyn EventStorage>,
    tracker: Arc<dyn CursorStorage>,
    dispatcher_tx: mpsc::Sender<LedgerCommand>,
    ipc_inboxes: crate::service::ipc::InboxRegistry,
    ipc_parked_inbound: crate::service::ipc::ParkedInboundRegistry,
    ipc_materialization_requester:
        Option<std::sync::Arc<dyn crate::service::guest_manager::GuestMaterializationRequester>>,
    shutdown_tx: tokio::sync::broadcast::Sender<()>,
    inbox_tx: mpsc::Sender<ansible_mesh_core::BeaconMessage>,
    inbox_rx: BeaconInboxReceiver,
    webrtc_signal_tx: mpsc::Sender<ansible_mesh_core::webrtc::WebRtcSignalMessage>,
    webrtc_signal_rx: WebRtcSignalReceiver,
}

/// Operator surface query worker — receives tasks via in-process channel (no UDS
/// self-connection) and uses a single persistent `PhiloticClient` for outgoing
/// queries and reply emission. This eliminates the socket-leak crash loop.
async fn run_operator_surface_query_worker(
    mut rx: tokio::sync::mpsc::Receiver<String>,
    socket_path: String,
    local_node_id: String,
    mut shutdown_rx: broadcast::Receiver<()>,
) {
    let mut backoff_ms: u64 = 500;
    let mut client: Option<PhiloticClient> = None;

    loop {
        // Drain the channel; if shutdown arrives, exit.
        let task_json = tokio::select! {
            _ = shutdown_rx.recv() => return,
            msg = rx.recv() => match msg {
                Some(j) => j,
                None => return, // channel closed
            }
        };

        // Ensure we have a live query client (connect once; reconnect only on error).
        if client.is_none() {
            let connect_fut = PhiloticClient::connect_at(
                &socket_path,
                GuestIdentity {
                    guest_id: "aiua-operator-surface-query-worker".into(),
                    role: OPERATOR_SURFACE_QUERY_ROLE.into(),
                    supported_tools: Vec::new(),
                },
            );
            match connect_fut.await {
                Ok(c) => {
                    backoff_ms = 500;
                    client = Some(c);
                }
                Err(err) => {
                    warn!(
                        backoff_ms,
                        "Operator surface query worker: query client connect failed: {err}"
                    );
                    tokio::select! {
                        _ = shutdown_rx.recv() => return,
                        _ = tokio::time::sleep(tokio::time::Duration::from_millis(backoff_ms)) => {}
                    }
                    backoff_ms = (backoff_ms * 2).min(30_000);
                    continue;
                }
            }
        }

        let c = client.as_mut().unwrap();
        if let Err(err) = handle_operator_surface_query_task(c, &local_node_id, &task_json).await {
            warn!("Operator surface query worker task failed: {err}");
            // Drop the client so we reconnect fresh on the next task.
            client = None;
            backoff_ms = 500;
        }
    }
}

async fn handle_operator_surface_query_task(
    client: &mut PhiloticClient,
    local_node_id: &str,
    task_json: &str,
) -> Result<()> {
    let payload: OperatorSurfaceQueryHandoff = serde_json::from_str(task_json)
        .context("failed to decode operator surface query handoff")?;
    if payload.handoff_kind != OPERATOR_SURFACE_QUERY_HANDOFF_KIND {
        anyhow::bail!(
            "unexpected operator surface handoff kind: [{}]",
            payload.handoff_kind
        );
    }

    let hotel = match client
        .send_request(IpcRequest::GetDesktopMembraneStatus)
        .await?
    {
        IpcResponse::DesktopMembraneStatusView { membrane_status } => membrane_status,
        other => anyhow::bail!("unexpected desktop membrane status response: {other:?}"),
    };
    let reply_json = match payload.surface.as_str() {
        "operator.targets.guests" => {
            let guests = match client
                .send_request(IpcRequest::QueryOperatorTargetGuests {
                    target_node_id: local_node_id.to_string(),
                })
                .await?
            {
                IpcResponse::OperatorTargetGuestsView {
                    operator_target_guests,
                } => operator_target_guests.guests,
                other => anyhow::bail!("unexpected operator target guests response: {other:?}"),
            };
            serde_json::to_string(&OperatorTargetGuestInventoryView {
                target_node_id: local_node_id.to_string(),
                target_hotel: hotel.hotel.clone(),
                source_hotel: hotel.hotel,
                observation_kind: "remote-canonical".into(),
                available: true,
                pending_remote_query_state: "none".into(),
                guests,
                note: Some("derived from the target hotel's canonical guest table".into()),
            })?
        }
        "operator.targets.status" => serde_json::to_string(&OperatorTargetStatusView {
            target_node_id: local_node_id.to_string(),
            target_hotel: hotel.hotel.clone(),
            source_hotel: hotel.hotel,
            observation_kind: "remote-canonical".into(),
            daemon_status: hotel.daemon,
            freshness_state: "remote-query-now".into(),
            freshness_age_secs: 0,
            freshness_ttl_secs: 0,
            reachability: None,
            note: Some("derived from the target hotel's canonical status view".into()),
        })?,
        "operator.targets.agents" => serde_json::to_string(&match client
            .send_request(IpcRequest::QueryOperatorTargetAgents {
                target_node_id: local_node_id.to_string(),
            })
            .await?
        {
            IpcResponse::OperatorTargetAgentsView {
                operator_target_agents,
            } => operator_target_agents,
            other => anyhow::bail!("unexpected operator target agents response: {other:?}"),
        })?,
        _ => return Ok(()),
    };

    match client
        .send_request(IpcRequest::EmitTask {
            target_node: payload.reply_to_node,
            target_role: payload.reply_to_role,
            target_guest_id: payload.reply_to_guest_id,
            task_json: reply_json,
        })
        .await?
    {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        other => {
            anyhow::bail!("unexpected operator surface query reply emit response: {other:?}")
        }
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,

    /// Name of the hotel to boot from the Context Graph
    #[arg(long)]
    hotel: Option<String>,

    /// Optional startup validation to run after the hotel materializes its guests
    #[arg(long, value_enum)]
    test: Option<StartupTest>,

    /// Output path for startup test artifacts such as voice samples
    #[arg(long)]
    test_output: Option<String>,

    /// Text payload for startup tests that synthesize or generate content
    #[arg(long)]
    test_text: Option<String>,
}

#[derive(Subcommand, Debug)]
enum Command {
    Auth {
        #[command(subcommand)]
        provider: AuthCommand,
    },
    /// Apply a config file to the Context Graph DB.
    /// Run this once on first setup or whenever the config changes.
    /// Normal `aiua --hotel <name>` startup runs purely from the DB.
    Load {
        /// Path to the JSON config file to apply
        #[arg(long)]
        file: String,
        /// Hotel section to seed (default: "default")
        #[arg(long, default_value = "default")]
        hotel: String,
    },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum StartupTest {
    #[value(name = "text-roundtrip", alias = "text-round-trip")]
    TextRoundTrip,
    #[value(name = "graph-roundtrip", alias = "graph-round-trip")]
    GraphRoundTrip,
    #[value(name = "cognitive-roundtrip", alias = "cognitive-round-trip")]
    CognitiveRoundTrip,
    #[value(name = "gemini-oauth-roundtrip", alias = "gemini-oauth")]
    GeminiOAuthRoundTrip,
    VoiceSample,
    #[value(name = "telegram-roundtrip", alias = "telegram-round-trip")]
    TelegramRoundTrip,
    #[value(name = "telegram-poll-lease", alias = "telegram-poll-owner")]
    TelegramPollLease,
}

const STARTUP_TEST_TEXT_REPLY: &str = "startup text smoke ok";
const STARTUP_TEST_COGNITIVE_REPLY: &str = "startup cognitive smoke ok";
const STARTUP_TEST_GEMINI_OAUTH_REPLY: &str = "oauth-guest-ok";
const STARTUP_TEST_TELEGRAM_TOKEN: &str = "startup-test-telegram-token";
const STARTUP_TEST_GEMINI_API_KEY: &str = "startup-test-gemini-key";
const STARTUP_TEST_TELEGRAM_TEXT_REPLY: &str = "startup telegram text smoke ok";
const STARTUP_TEST_TELEGRAM_PHOTO_REPLY: &str = "startup telegram photo smoke ok";
const STARTUP_TEST_TELEGRAM_VOICE_REPLY: &str = "startup telegram voice smoke ok";

#[derive(Debug, Default)]
struct FakeTelegramState {
    updates: std::sync::Mutex<VecDeque<serde_json::Value>>,
    sent_messages: std::sync::Mutex<Vec<serde_json::Value>>,
    sent_media: std::sync::Mutex<Vec<serde_json::Value>>,
    registered_commands: std::sync::Mutex<Vec<serde_json::Value>>,
    deleted_command_syncs: std::sync::Mutex<u32>,
    get_updates_calls: AtomicUsize,
    files: std::sync::Mutex<HashMap<String, FakeTelegramFile>>,
}

#[derive(Debug, Clone)]
struct FakeTelegramFile {
    file_path: String,
    bytes: Vec<u8>,
}

#[derive(Debug, Default)]
struct FakeGeminiMediaState {
    requests: std::sync::Mutex<Vec<serde_json::Value>>,
}

#[derive(Debug, serde::Deserialize)]
struct TelegramGetUpdatesQuery {
    offset: Option<i64>,
}

#[derive(Debug, serde::Deserialize)]
struct TelegramSendMessageRequest {
    chat_id: serde_json::Value,
    text: String,
    #[allow(dead_code)]
    parse_mode: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct TelegramSetMyCommandsRequest {
    commands: Vec<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct AnsibleCutoverFlags {
    pub enable_rust_auth: bool,
    pub enable_rust_dispatcher: bool,
    pub enable_rust_task_lifecycle: bool,
}

impl AnsibleCutoverFlags {
    pub fn from_env() -> Self {
        Self {
            enable_rust_auth: std::env::var("PHILOTIC_ENABLE_RUST_AUTH")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(true),
            enable_rust_dispatcher: std::env::var("PHILOTIC_ENABLE_RUST_DISPATCHER")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(true),
            enable_rust_task_lifecycle: std::env::var("PHILOTIC_ENABLE_RUST_TASK_LIFECYCLE")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(true),
        }
    }
}

fn guest_supervision_enabled() -> bool {
    std::env::var("PHILOTIC_ENABLE_GUEST_SUPERVISOR")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false)
}

fn smoke_mode_enabled() -> bool {
    std::env::var("PHILOTIC_SMOKE_MODE")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false)
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

fn hotel_base_port(hotel_name: &str) -> u16 {
    let mut hash: u16 = 0;
    for byte in hotel_name.bytes() {
        hash = hash.wrapping_mul(31).wrapping_add(byte as u16);
    }
    10_000 + (hash % 20_000)
}

fn startup_test_telegram_port(hotel_name: &str) -> u16 {
    hotel_base_port(hotel_name) + 20
}

fn startup_test_telegram_api_base_url(hotel_name: &str) -> String {
    format!(
        "http://127.0.0.1:{}",
        startup_test_telegram_port(hotel_name)
    )
}

fn startup_test_gemini_port(hotel_name: &str) -> u16 {
    hotel_base_port(hotel_name) + 21
}

fn startup_test_gemini_api_base_url(hotel_name: &str) -> String {
    format!("http://127.0.0.1:{}", startup_test_gemini_port(hotel_name))
}

fn startup_test_blob_port(hotel_name: &str) -> u16 {
    hotel_base_port(hotel_name) + 22
}

fn startup_test_blob_base_url(hotel_name: &str) -> String {
    format!("http://127.0.0.1:{}", startup_test_blob_port(hotel_name))
}

fn hotel_execution_port(hotel_name: &str) -> u16 {
    hotel_base_port(hotel_name) + 2
}

/// Detect the current git worktree branch by matching the working directory against
/// `git worktree list --porcelain`. Returns `None` if not in a worktree, on the
/// main checkout, or if git is unavailable.
fn detect_git_worktree_branch() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let output = std::process::Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut current_path = String::new();
    let mut current_branch = String::new();
    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = path.to_string();
            current_branch.clear();
        } else if let Some(branch) = line.strip_prefix("branch refs/heads/") {
            current_branch = branch.to_string();
        } else if line.is_empty() && !current_path.is_empty() {
            if std::path::Path::new(&current_path) == cwd {
                // Only return if it's a codex/* branch — main checkout is not a workstream.
                return if current_branch.starts_with("codex/") {
                    Some(current_branch.clone())
                } else {
                    None
                };
            }
            current_path.clear();
            current_branch.clear();
        }
    }
    None
}

fn is_udp_port_available(port: u16) -> bool {
    StdUdpSocket::bind(("0.0.0.0", port)).is_ok()
}

fn is_tcp_port_available(port: u16) -> bool {
    TcpListener::bind(("0.0.0.0", port)).is_ok()
}

fn port_cluster_available(mesh_port: u16, blob_port: u16, execution_port: u16) -> bool {
    is_udp_port_available(mesh_port)
        && is_tcp_port_available(blob_port)
        && is_tcp_port_available(execution_port)
}

fn local_service_ports_available(blob_port: u16, execution_port: u16) -> bool {
    is_tcp_port_available(blob_port) && is_tcp_port_available(execution_port)
}

fn nearest_available_base_port<F>(desired: u16, mut cluster_ok: F) -> Option<u16>
where
    F: FnMut(u16) -> bool,
{
    if cluster_ok(desired) {
        return Some(desired);
    }

    let max_offset = u16::MAX.saturating_sub(desired).max(desired);
    for offset in 1..=max_offset {
        let up = desired.saturating_add(offset);
        if up >= desired && cluster_ok(up) {
            return Some(up);
        }

        let down = desired.saturating_sub(offset);
        if down < desired && cluster_ok(down) {
            return Some(down);
        }
    }

    None
}

fn resolve_runtime_ports(hotel: &HotelRecord, mesh_enabled: bool) -> Result<(u16, u16, u16)> {
    let desired_mesh = hotel.mesh_port;
    let desired_blob = hotel.blob_port;
    let desired_execution = hotel.execution_port;

    let desired_ports_available = if mesh_enabled {
        port_cluster_available(desired_mesh, desired_blob, desired_execution)
    } else {
        local_service_ports_available(desired_blob, desired_execution)
    };

    if desired_ports_available {
        return Ok((desired_mesh, desired_blob, desired_execution));
    }

    let Some(base) = nearest_available_base_port(desired_mesh, |base| {
        let Some(blob) = base.checked_add(1) else {
            return false;
        };
        let Some(execution) = base.checked_add(2) else {
            return false;
        };
        if mesh_enabled {
            port_cluster_available(base, blob, execution)
        } else {
            local_service_ports_available(blob, execution)
        }
    }) else {
        anyhow::bail!(
            "No available {} port cluster found near base port {}",
            if mesh_enabled {
                "mesh/blob/execution"
            } else {
                "blob/execution"
            },
            desired_mesh
        );
    };

    Ok((base, base + 1, base + 2))
}

fn hotel_ipc_socket_path(hotel_name: &str) -> String {
    let safe_name = sanitize_hotel_name(hotel_name);
    profile_dir()
        .map(|d| {
            d.join(format!("aiua-{safe_name}.sock"))
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_else(|| format!("/tmp/philotic-{safe_name}.sock"))
}

fn default_hotel_record(hotel_name: &str) -> HotelRecord {
    let safe_name = sanitize_hotel_name(hotel_name);
    let base_port = hotel_base_port(&safe_name);

    HotelRecord {
        hotel_name: hotel_name.to_string(),
        capabilities: NodeCapabilities {
            node_id: format!("{safe_name}-aiua-01"),
            roles: vec![NodeRole::AnsibleNode, NodeRole::Other("membrane".into())],
            models: vec![],
            tools: vec![],
            constraints: Default::default(),
        },
        mesh_host: None,
        mesh_port: base_port,
        blob_port: base_port + 1,
        execution_port: hotel_execution_port(&safe_name),
        ipc_socket_path: hotel_ipc_socket_path(hotel_name),
        active_pid: None,
    }
}

fn mesh_host_for_hotel(hotel: &HotelRecord) -> &str {
    hotel
        .mesh_host
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("127.0.0.1")
}

fn mesh_targets_for_graph(
    graph: &GraphDomain,
    local_node_id: &str,
) -> Result<Vec<(String, String)>> {
    Ok(graph
        .list_hotels()?
        .into_iter()
        .filter(|hotel| hotel.capabilities.node_id != local_node_id)
        .map(|hotel| {
            let node_id = hotel.capabilities.node_id.clone();
            let addr = format!("{}:{}", mesh_host_for_hotel(&hotel), hotel.mesh_port);
            (node_id, addr)
        })
        .collect())
}

/// Handle an inbound `CronFired` broadcast from a peer hotel.
///
/// Updates the local `CronJob` record so that this hotel's `CronTicker`
/// suppresses its staggered-offset fire for the same epoch.
fn handle_cron_fired_broadcast(graph: &GraphDomain, payload_json: &str) {
    #[derive(serde::Deserialize)]
    struct CronFiredPayload {
        job_id: String,
        fire_epoch: u64,
    }

    let parsed: CronFiredPayload = match serde_json::from_str(payload_json) {
        Ok(p) => p,
        Err(e) => {
            warn!("handle_cron_fired_broadcast: invalid payload: {e}");
            return;
        }
    };

    match graph.get_cron_job(&parsed.job_id) {
        Ok(Some(job)) => {
            // Only update if this epoch is still pending on this hotel.
            if job.last_fired_epoch == Some(parsed.fire_epoch) {
                return; // already up-to-date
            }
            if job.next_fire_at != parsed.fire_epoch {
                return; // epoch mismatch — stale or duplicate broadcast
            }
            let next =
                match ansible_mesh_core::cron::next_fire_after(&job.schedule, parsed.fire_epoch) {
                    Ok(n) => n,
                    Err(e) => {
                        warn!(
                            "handle_cron_fired_broadcast: no next fire for job {}: {e}",
                            parsed.job_id
                        );
                        return;
                    }
                };
            let mut updated = job;
            updated.last_fired_epoch = Some(parsed.fire_epoch);
            updated.next_fire_at = next;
            if let Err(e) = graph.upsert_cron_job(&updated) {
                warn!(
                    "handle_cron_fired_broadcast: failed to update job {}: {e}",
                    parsed.job_id
                );
            } else {
                info!(
                    "CronFired broadcast applied: job={} epoch={}",
                    parsed.job_id, parsed.fire_epoch
                );
            }
        }
        Ok(None) => {
            debug!(
                "handle_cron_fired_broadcast: job {} not found locally (ok if not registered here)",
                parsed.job_id
            );
        }
        Err(e) => {
            warn!(
                "handle_cron_fired_broadcast: graph lookup failed for job {}: {e}",
                parsed.job_id
            );
        }
    }
}

/// Handle an inbound `CronJobSync` broadcast from a peer hotel.
///
/// Replicates job definitions locally so this hotel can participate in
/// guaranteed firing without requiring a shared config file.
fn handle_cron_job_sync(graph: &GraphDomain, payload_json: &str) {
    #[derive(serde::Deserialize)]
    struct CronJobSyncPayload {
        op: String,
        job: Option<ansible_mesh_core::cron::CronJob>,
        job_id: Option<String>,
    }

    let parsed: CronJobSyncPayload = match serde_json::from_str(payload_json) {
        Ok(p) => p,
        Err(e) => {
            warn!("handle_cron_job_sync: invalid payload: {e}");
            return;
        }
    };

    match parsed.op.as_str() {
        "upsert" => {
            if let Some(job) = parsed.job {
                if let Err(e) = graph.upsert_cron_job(&job) {
                    warn!("handle_cron_job_sync: upsert failed for {}: {e}", job.id);
                } else {
                    info!("CronJobSync: replicated upsert for job {}", job.id);
                }
            } else {
                warn!("handle_cron_job_sync: upsert op missing job field");
            }
        }
        "remove" => {
            if let Some(job_id) = parsed.job_id {
                if let Err(e) = graph.remove_cron_job(&job_id) {
                    warn!("handle_cron_job_sync: remove failed for {job_id}: {e}");
                } else {
                    info!("CronJobSync: replicated remove for job {job_id}");
                }
            } else {
                warn!("handle_cron_job_sync: remove op missing job_id field");
            }
        }
        other => {
            warn!("handle_cron_job_sync: unknown op '{other}'");
        }
    }
}

/// Handle an inbound `ProjectedUserIdentitySync` broadcast from a peer hotel.
///
/// Replicates only the non-secret user ghost mirror so remote hotels can
/// recognize the same human without inheriting local sessions or credentials.
fn handle_projected_user_identity_sync(graph: &GraphDomain, payload_json: &str) {
    let identity: ansible_mesh_core::storage::ProjectedUserIdentityRecord =
        match serde_json::from_str(payload_json) {
            Ok(identity) => identity,
            Err(e) => {
                warn!("handle_projected_user_identity_sync: invalid payload: {e}");
                return;
            }
        };

    if let Err(e) = graph.upsert_projected_user_identity(&identity) {
        warn!(
            "handle_projected_user_identity_sync: upsert failed for {}: {e}",
            identity.principal_id
        );
    } else {
        info!(
            "ProjectedUserIdentitySync: replicated principal {} from {}",
            identity.principal_id, identity.home_hotel
        );
    }
}

fn mesh_target_addr_for_node(graph: &GraphDomain, target_node_id: &str) -> Result<Option<String>> {
    Ok(graph
        .list_hotels()?
        .into_iter()
        .find(|hotel| hotel.capabilities.node_id == target_node_id)
        .map(|hotel| format!("{}:{}", mesh_host_for_hotel(&hotel), hotel.execution_port)))
}

fn mesh_pending_invite_config_key(nonce: &str) -> String {
    format!("mesh_pending_invite:{nonce}")
}

fn mesh_member_public_key_config_key(hotel_name: &str) -> String {
    format!("mesh_member_public_key:{hotel_name}")
}

fn mesh_auth_key_config_key(node_id: &str) -> String {
    format!("mesh_auth_key:{node_id}")
}

fn mesh_transport_private_key_ref_config_key(hotel_name: &str) -> String {
    format!("mesh_transport_private_key_ref:{hotel_name}")
}

fn read_string_config(graph: &GraphDomain, key: &str) -> Result<Option<String>> {
    Ok(graph
        .get_config_value(key)?
        .and_then(|value| serde_json::from_str::<String>(&value).ok().or(Some(value)))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty()))
}

fn resolve_internal_secret(graph: &GraphDomain, secret_ref: &str) -> Result<String> {
    resolve_secret(
        graph,
        secret_ref,
        &SecretAccess {
            role: "hotel.internal".into(),
            guest_id: "aiua".into(),
        },
    )?
    .ok_or_else(|| anyhow::anyhow!("vault secret not found: {secret_ref}"))
}

fn mesh_auth_key_for_node(graph: &GraphDomain, node_id: &str) -> Result<Option<String>> {
    read_string_config(graph, &mesh_auth_key_config_key(node_id))
}

fn handle_mesh_membership_accept(graph: &GraphDomain, payload_json: &str) {
    let payload = match serde_json::from_str::<MeshMembershipAcceptPayload>(payload_json) {
        Ok(payload) => payload,
        Err(err) => {
            warn!(
                "Failed to parse mesh membership acceptance payload: {}",
                err
            );
            return;
        }
    };

    if let Err(err) = verify_join_request(&payload) {
        warn!(
            "Rejecting mesh membership acceptance with invalid signature: {}",
            err
        );
        return;
    }

    if payload.payload.hotel_name.trim().is_empty()
        || payload.payload.capabilities.node_id.trim().is_empty()
        || payload.payload.mesh_host.trim().is_empty()
    {
        warn!("Ignoring mesh membership acceptance with incomplete remote hotel identity");
        return;
    }

    let pending_key = mesh_pending_invite_config_key(&payload.payload.invite_nonce);
    let Some(pending_value) = (match graph.get_config_value(&pending_key) {
        Ok(value) => value,
        Err(err) => {
            warn!(
                "Rejecting mesh membership acceptance for nonce [{}]: failed to load pending invite: {}",
                payload.payload.invite_nonce, err
            );
            return;
        }
    }) else {
        warn!(
            "Rejecting mesh membership acceptance for nonce [{}]: no pending invite exists",
            payload.payload.invite_nonce
        );
        return;
    };

    let pending: serde_json::Value = match serde_json::from_str(&pending_value) {
        Ok(value) => value,
        Err(err) => {
            warn!(
                "Rejecting mesh membership acceptance for nonce [{}]: invalid pending invite record: {}",
                payload.payload.invite_nonce, err
            );
            return;
        }
    };

    let status = pending
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    if status != "pending" {
        warn!(
            "Rejecting mesh membership acceptance for nonce [{}]: invite status is [{}], not pending",
            payload.payload.invite_nonce, status
        );
        return;
    }

    let now = now_epoch_secs();
    let expires_at = pending
        .get("expires_at")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    if expires_at < now {
        warn!(
            "Rejecting mesh membership acceptance for nonce [{}]: invite expired at {}",
            payload.payload.invite_nonce, expires_at
        );
        let _ = graph.remove_config_value(&pending_key);
        return;
    }

    let expected_fingerprint = match fingerprint_from_base64url(&payload.payload.joiner_pubkey_b64)
    {
        Ok(value) => value,
        Err(err) => {
            warn!(
                "Rejecting mesh membership acceptance for nonce [{}]: invalid joiner public key: {}",
                payload.payload.invite_nonce, err
            );
            return;
        }
    };
    if expected_fingerprint != payload.payload.joiner_fingerprint {
        warn!(
            "Rejecting mesh membership acceptance for nonce [{}]: joiner fingerprint mismatch",
            payload.payload.invite_nonce
        );
        return;
    }

    let pubkey_config_key = mesh_member_public_key_config_key(&payload.payload.hotel_name);
    match read_string_config(graph, &pubkey_config_key) {
        Ok(Some(existing_key)) if existing_key != payload.payload.joiner_pubkey_b64 => {
            warn!(
                "Rejecting mesh membership acceptance for hotel [{}]: stored public key does not match join request",
                payload.payload.hotel_name
            );
            return;
        }
        Ok(_) => {}
        Err(err) => {
            warn!(
                "Rejecting mesh membership acceptance for hotel [{}]: failed to read stored member key: {}",
                payload.payload.hotel_name, err
            );
            return;
        }
    }

    let Some(local_hotel_name) = pending
        .get("hotel_name")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
    else {
        warn!(
            "Rejecting mesh membership acceptance for nonce [{}]: pending invite missing local hotel",
            payload.payload.invite_nonce
        );
        return;
    };

    let private_ref_key = mesh_transport_private_key_ref_config_key(local_hotel_name);
    let local_transport_private_key_hex = match read_string_config(graph, &private_ref_key)
        .and_then(|value| {
            value
                .map(|secret_ref| resolve_internal_secret(graph, &secret_ref))
                .transpose()
        }) {
        Ok(Some(value)) => value,
        Ok(None) => {
            warn!(
                "Rejecting mesh membership acceptance for nonce [{}]: inviter transport identity missing",
                payload.payload.invite_nonce
            );
            return;
        }
        Err(err) => {
            warn!(
                "Rejecting mesh membership acceptance for nonce [{}]: failed to load inviter transport identity: {}",
                payload.payload.invite_nonce, err
            );
            return;
        }
    };

    let session_key = match derive_transport_session_key(
        &payload.payload.invite_nonce,
        &local_transport_private_key_hex,
        &payload.payload.joiner_transport_pubkey_b64,
    ) {
        Ok(value) => value,
        Err(err) => {
            warn!(
                "Rejecting mesh membership acceptance for nonce [{}]: failed to derive per-peer auth key: {}",
                payload.payload.invite_nonce, err
            );
            return;
        }
    };

    let hotel = HotelRecord {
        hotel_name: payload.payload.hotel_name.clone(),
        capabilities: payload.payload.capabilities.clone(),
        mesh_host: Some(payload.payload.mesh_host.clone()),
        mesh_port: payload.payload.mesh_port,
        blob_port: payload.payload.blob_port,
        execution_port: payload.payload.execution_port,
        ipc_socket_path: String::new(),
        active_pid: None,
    };

    if let Err(err) = graph.set_config_value(
        &pubkey_config_key,
        &serde_json::to_string(&payload.payload.joiner_pubkey_b64)
            .unwrap_or_else(|_| "null".into()),
    ) {
        warn!(
            hotel = %hotel.hotel_name,
            "Failed to pin accepted mesh member public key: {}",
            err
        );
        return;
    }

    let auth_key_config_key = mesh_auth_key_config_key(&hotel.capabilities.node_id);
    if let Err(err) = graph.set_config_value(
        &auth_key_config_key,
        &serde_json::to_string(&session_key).unwrap_or_else(|_| "null".into()),
    ) {
        warn!(
            hotel = %hotel.hotel_name,
            "Failed to persist accepted mesh member auth key: {}",
            err
        );
        let _ = graph.remove_config_value(&pubkey_config_key);
        return;
    }

    if let Err(err) = graph.upsert_hotel(&hotel) {
        warn!(
            hotel = %hotel.hotel_name,
            node = %hotel.capabilities.node_id,
            "Failed to persist accepted mesh membership: {}",
            err
        );
        let _ = graph.remove_config_value(&pubkey_config_key);
        let _ = graph.remove_config_value(&auth_key_config_key);
        return;
    }

    if let Err(err) = graph.set_config_value(
        &pending_key,
        &serde_json::json!({
            "hotel_name": hotel.hotel_name,
            "created_at": pending.get("created_at").and_then(|value| value.as_u64()).unwrap_or(now),
            "expires_at": expires_at,
            "status": "accepted",
            "accepted_at": now,
            "member_hotel": hotel.hotel_name,
        })
        .to_string(),
    ) {
        warn!(
            hotel = %hotel.hotel_name,
            "Failed to mark mesh invite as accepted: {}",
            err
        );
        return;
    }
    info!(
        hotel = %hotel.hotel_name,
        node = %hotel.capabilities.node_id,
        host = %mesh_host_for_hotel(&hotel),
        mesh_port = hotel.mesh_port,
        "Accepted remote hotel into local mesh membership registry"
    );
}

fn execution_reachability_for_hotel(
    graph: &GraphDomain,
    hotel: &HotelRecord,
) -> ExecutionReachability {
    let host = graph
        .get_config_value("execution_host")
        .ok()
        .flatten()
        .and_then(|value| serde_json::from_str::<String>(&value).ok().or(Some(value)))
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            hotel
                .mesh_host
                .as_ref()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "127.0.0.1".into());
    let protocol = graph
        .get_config_value("execution_protocol")
        .ok()
        .flatten()
        .and_then(|value| serde_json::from_str::<String>(&value).ok().or(Some(value)))
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "tcp-framed-v1".into());

    ExecutionReachability {
        protocol,
        host,
        port: hotel.execution_port,
    }
}

/// Samples local environment vitals for inclusion in the outbound heartbeat.
/// All fields are best-effort; failures are silently swallowed so a bad sysfs
/// read never blocks the heartbeat loop.
fn sample_node_health(graph: &GraphDomain, hotel_name: &str) -> NodeHealthSnapshot {
    let guest_count = graph
        .list_guests(hotel_name, false)
        .ok()
        .map(|gs| gs.len() as u32);

    // Disk: percentage free on the filesystem that holds the DB file.
    let disk_free_pct = (|| -> Option<f32> {
        #[cfg(target_os = "macos")]
        {
            let out = std::process::Command::new("df")
                .args(["-k", "."])
                .output()
                .ok()?;
            let stdout = String::from_utf8_lossy(&out.stdout);
            let line = stdout.lines().nth(1)?;
            let cols: Vec<&str> = line.split_whitespace().collect();
            // df -k columns: Filesystem 512-blocks Used Available Capacity Mounted
            let avail: u64 = cols.get(3)?.parse().ok()?;
            let used: u64 = cols.get(2)?.parse().ok()?;
            let total = used + avail;
            if total == 0 {
                return None;
            }
            Some(avail as f32 / total as f32 * 100.0)
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    })();

    // Memory: percentage free from /proc/meminfo (Linux) or vm_stat (macOS).
    let mem_free_pct = (|| -> Option<f32> {
        #[cfg(target_os = "macos")]
        {
            let out = std::process::Command::new("vm_stat").output().ok()?;
            let stdout = String::from_utf8_lossy(&out.stdout);
            let page_size: u64 = {
                let line = stdout.lines().next()?;
                let s = line.split_whitespace().last()?.trim_end_matches('.');
                s.parse().ok()?
            };
            let mut free_pages: u64 = 0;
            let mut total_pages: u64 = 0;
            for line in stdout.lines() {
                let parts: Vec<&str> = line.splitn(2, ':').collect();
                if parts.len() != 2 {
                    continue;
                }
                let val: u64 = parts[1].trim().trim_end_matches('.').parse().unwrap_or(0);
                total_pages += val;
                if parts[0].contains("Pages free") || parts[0].contains("Pages speculative") {
                    free_pages += val;
                }
            }
            if total_pages == 0 {
                return None;
            }
            let _ = page_size;
            Some(free_pages as f32 / total_pages as f32 * 100.0)
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    })();

    // Load average from /proc/loadavg (Linux) or sysctl (macOS).
    let load_avg_1m = (|| -> Option<f32> {
        #[cfg(target_os = "macos")]
        {
            let out = std::process::Command::new("sysctl")
                .arg("vm.loadavg")
                .output()
                .ok()?;
            let stdout = String::from_utf8_lossy(&out.stdout);
            // vm.loadavg: { 0.52 0.61 0.63 }
            let inner = stdout.split('{').nth(1)?.split('}').next()?;
            inner.split_whitespace().next()?.parse().ok()
        }
        #[cfg(not(target_os = "macos"))]
        {
            let content = std::fs::read_to_string("/proc/loadavg").ok()?;
            content.split_whitespace().next()?.parse().ok()
        }
    })();

    NodeHealthSnapshot {
        guest_count,
        disk_free_pct,
        mem_free_pct,
        load_avg_1m,
    }
}

async fn activate_mesh_runtime(ctx: MeshRuntimeContext) -> Result<()> {
    let daemon = Arc::new(
        BeaconDaemon::bind_with_registry(
            &ctx.mesh_addr,
            ctx.caps.clone(),
            ctx.inbox_tx.clone(),
            ctx.graph_domain.clone(),
            &ctx.db_path,
            ctx.enable_rust_auth,
            ctx.registry.clone(),
        )
        .await?,
    );

    let mut inbox_rx = {
        let mut guard = ctx.inbox_rx.lock().await;
        guard.take()
    }
    .context("mesh runtime activation missing beacon inbox receiver")?;

    let mut webrtc_signal_rx = {
        let mut guard = ctx.webrtc_signal_rx.lock().await;
        guard.take()
    }
    .context("mesh runtime activation missing WebRTC signal receiver")?;

    info!(
        hotel = %ctx.hotel_name,
        "Mesh transport antenna extended"
    );

    if ctx.enable_rust_dispatcher {
        tokio::spawn(crate::service::mesh_dispatcher::outbound_dispatcher(
            ctx.ledger.clone(),
            ctx.tracker.clone(),
            daemon.socket(),
            ctx.graph_domain.clone(),
            ctx.registry.clone(),
            ctx.caps.node_id.clone(),
            ctx.shutdown_tx.subscribe(),
        ));
    }

    {
        let execution_inbox_tx = daemon.inbox_tx();
        let execution_caps = ctx.caps.clone();
        let execution_db_path = ctx.db_path.clone();
        let execution_graph = ctx.graph_domain.clone();
        let execution_addr = ctx.execution_addr.clone();
        let execution_enable_rust_auth = ctx.enable_rust_auth;
        tokio::spawn(async move {
            if let Err(e) = crate::service::execution_transport::serve_execution_plane(
                &execution_addr,
                execution_caps,
                execution_inbox_tx,
                execution_graph,
                &execution_db_path,
                execution_enable_rust_auth,
            )
            .await
            {
                error!("Hotel execution transport failed: {}", e);
            }
        });
    }

    {
        let heartbeat_socket = daemon.socket();
        let heartbeat_graph = ctx.graph_domain.clone();
        let heartbeat_hotel_name = ctx.hotel_name.clone();
        let heartbeat_caps = ctx.caps.clone();
        let mut heartbeat_shutdown = ctx.shutdown_tx.subscribe();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(3));
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let targets = match mesh_targets_for_graph(heartbeat_graph.as_ref(), &heartbeat_caps.node_id) {
                            Ok(targets) => targets,
                            Err(err) => {
                                warn!("Failed to resolve mesh heartbeat targets: {}", err);
                                continue;
                            }
                        };
                        if targets.is_empty() {
                            continue;
                        }
                        let heartbeat_hotel = match heartbeat_graph.get_hotel(&heartbeat_hotel_name) {
                            Ok(Some(hotel)) => hotel,
                            Ok(None) => {
                                warn!("Skipping heartbeat: hotel record [{}] missing", heartbeat_hotel_name);
                                continue;
                            }
                            Err(err) => {
                                warn!("Failed to load current hotel record [{}] for heartbeat: {}", heartbeat_hotel_name, err);
                                continue;
                            }
                        };
                        let execution_reachability =
                            execution_reachability_for_hotel(heartbeat_graph.as_ref(), &heartbeat_hotel);
                        let node_health =
                            sample_node_health(heartbeat_graph.as_ref(), &heartbeat_hotel.hotel_name);
                        for (_target_node_id, target_addr) in targets {
                            let Ok(target) = target_addr.parse::<SocketAddr>() else {
                                warn!("Skipping invalid heartbeat target address {}", target_addr);
                                continue;
                            };
                            let Some(auth_key) = mesh_auth_key_for_node(heartbeat_graph.as_ref(), &_target_node_id).ok().flatten() else {
                                debug!("Skipping heartbeat to {} until mesh auth key exists", _target_node_id);
                                continue;
                            };
                            if let Err(err) = emit_heartbeat(
                                &heartbeat_socket,
                                target,
                                &heartbeat_caps,
                                Some(execution_reachability.clone()),
                                &auth_key,
                                Some(node_health.clone()),
                            )
                            .await
                            {
                                warn!("Failed to emit heartbeat to {}: {}", target_addr, err);
                            }
                        }
                    }
                    _ = heartbeat_shutdown.recv() => break,
                }
            }
        });
    }

    {
        let capability_socket = daemon.socket();
        let capability_graph = ctx.graph_domain.clone();
        let capability_hotel_name = ctx.hotel_name.clone();
        let capability_caps = ctx.caps.clone();
        let mut capability_shutdown = ctx.shutdown_tx.subscribe();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
            let mut last_sync_fingerprint: Option<String> = None;
            let mut last_full_sync =
                std::time::Instant::now() - std::time::Duration::from_secs(3600);
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let targets = match mesh_targets_for_graph(capability_graph.as_ref(), &capability_caps.node_id) {
                            Ok(targets) => targets,
                            Err(err) => {
                                warn!("Failed to resolve mesh capability-sync targets: {}", err);
                                continue;
                            }
                        };
                        if targets.is_empty() {
                            continue;
                        }
                        let capability_hotel = match capability_graph.get_hotel(&capability_hotel_name) {
                            Ok(Some(hotel)) => hotel,
                            Ok(None) => {
                                warn!(
                                    "Skipping capability sync: hotel record [{}] missing",
                                    capability_hotel_name
                                );
                                continue;
                            }
                            Err(err) => {
                                warn!(
                                    "Failed to load current hotel record [{}] for capability sync: {}",
                                    capability_hotel_name,
                                    err
                                );
                                continue;
                            }
                        };

                        let advertisements = match local_capability_advertisements(capability_graph.as_ref(), &capability_hotel) {
                            Ok(advertisements) => advertisements,
                            Err(err) => {
                                warn!("Failed to build local capability advertisements: {}", err);
                                continue;
                            }
                        };
                        let execution_reachability =
                            execution_reachability_for_hotel(capability_graph.as_ref(), &capability_hotel);
                        let sync_fingerprint = capability_sync_fingerprint(&advertisements, &execution_reachability);
                        let should_sync =
                            last_sync_fingerprint.as_ref() != Some(&sync_fingerprint)
                            || last_full_sync.elapsed() >= std::time::Duration::from_secs(3600);
                        if !should_sync {
                            continue;
                        }

                        for (target_node_id, target_addr) in targets {
                            let Ok(target) = target_addr.parse::<SocketAddr>() else {
                                warn!("Skipping invalid capability-sync target address {}", target_addr);
                                continue;
                            };
                            let Some(auth_key) = mesh_auth_key_for_node(capability_graph.as_ref(), &target_node_id).ok().flatten() else {
                                debug!("Skipping capability sync to {} until mesh auth key exists", target_node_id);
                                continue;
                            };
                            if let Err(err) = emit_capability_sync(
                                &capability_socket,
                                target,
                                &capability_caps,
                                &advertisements,
                                Some(execution_reachability.clone()),
                                &auth_key,
                                None,
                            )
                            .await
                            {
                                warn!("Failed to emit capability sync to {}: {}", target_addr, err);
                            }
                        }

                        last_sync_fingerprint = Some(sync_fingerprint);
                        last_full_sync = std::time::Instant::now();
                    }
                    _ = capability_shutdown.recv() => break,
                }
            }
        });
    }

    {
        let dispatcher_inbound_tx = ctx.dispatcher_tx.clone();
        let inbound_graph = ctx.graph_domain.clone();
        let inbound_inboxes = ctx.ipc_inboxes.clone();
        let inbound_parked = ctx.ipc_parked_inbound.clone();
        let inbound_mat_req = ctx.ipc_materialization_requester.clone();
        let inbound_local_node_id = ctx.caps.node_id.clone();
        let webrtc_signal_tx_inbound = ctx.webrtc_signal_tx.clone();
        tokio::spawn(async move {
            while let Some(msg) = inbox_rx.recv().await {
                match msg.msg_type {
                    ansible_mesh_core::MsgType::MeshEventBatch
                    | ansible_mesh_core::MsgType::ExecutionEventBatch => {
                        if let Ok(events) =
                            serde_json::from_slice::<Vec<EventEnvelope>>(&msg.payload)
                        {
                            if !events.is_empty() {
                                let max_seq = events.iter().map(|e| e.seq).max().unwrap_or(0);
                                for event in &events {
                                    IpcServer::deliver_event_envelope(&inbound_inboxes, event)
                                        .await;
                                    // Cron control-plane broadcasts.
                                    match &event.kind {
                                        ansible_mesh_core::event::EventKind::CronFired => {
                                            if let ansible_mesh_core::event::EventPayload::Inline {
                                                data,
                                            } = &event.payload
                                            {
                                                handle_cron_fired_broadcast(
                                                    inbound_graph.as_ref(),
                                                    data,
                                                );
                                            }
                                        }
                                        ansible_mesh_core::event::EventKind::CronJobSync => {
                                            if let ansible_mesh_core::event::EventPayload::Inline {
                                                data,
                                            } = &event.payload
                                            {
                                                handle_cron_job_sync(
                                                    inbound_graph.as_ref(),
                                                    data,
                                                );
                                            }
                                        }
                                        ansible_mesh_core::event::EventKind::ProjectedUserIdentitySync => {
                                            if let ansible_mesh_core::event::EventPayload::Inline {
                                                data,
                                            } = &event.payload
                                            {
                                                handle_projected_user_identity_sync(
                                                    inbound_graph.as_ref(),
                                                    data,
                                                );
                                            }
                                        }
                                        ansible_mesh_core::event::EventKind::SessionControl => {
                                            if let ansible_mesh_core::event::EventPayload::Inline {
                                                data,
                                            } = &event.payload
                                            {
                                                if let Ok(v) = serde_json::from_str::<
                                                    serde_json::Value,
                                                >(data)
                                                {
                                                    if v.get("action")
                                                        .and_then(|a| a.as_str())
                                                        == Some("session.handoff")
                                                    {
                                                        let graph = inbound_graph.clone();
                                                        let inboxes = inbound_inboxes.clone();
                                                        let parked = inbound_parked.clone();
                                                        let mat_req = inbound_mat_req.clone();
                                                        let node_id =
                                                            inbound_local_node_id.clone();
                                                        let data = data.clone();
                                                        tokio::spawn(async move {
                                                            IpcServer::handle_remote_role_handoff(
                                                                graph.as_ref(),
                                                                &inboxes,
                                                                &parked,
                                                                mat_req,
                                                                &node_id,
                                                                &data,
                                                            )
                                                            .await;
                                                        });
                                                    }
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                let _ = dispatcher_inbound_tx
                                    .send(LedgerCommand::CommitInboundBatch {
                                        events,
                                        source_node: msg.src_node.clone(),
                                    })
                                    .await;

                                let ack_payload =
                                    serde_json::json!({ "acked_seq": max_seq }).to_string();
                                if let Some(target_addr) =
                                    mesh_target_addr_for_node(inbound_graph.as_ref(), &msg.src_node)
                                        .ok()
                                        .flatten()
                                {
                                    let Some(auth_key) = mesh_auth_key_for_node(
                                        inbound_graph.as_ref(),
                                        &msg.src_node,
                                    )
                                    .ok()
                                    .flatten() else {
                                        warn!(
                                            "No mesh auth key found for ACK destination {}",
                                            msg.src_node
                                        );
                                        continue;
                                    };
                                    let msg_id = uuid::Uuid::new_v4();
                                    let seq = 0;
                                    let timestamp = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs();
                                    let payload = ack_payload.into_bytes();
                                    let hmac = ansible_mesh_core::authz::MeshAuth::new(auth_key)
                                        .sign(&msg_id, seq as u64, &payload, timestamp);
                                    let ack = ansible_mesh_core::BeaconMessage {
                                        version: 1,
                                        msg_id,
                                        src_node: inbound_local_node_id.clone(),
                                        dest_node: msg.src_node.clone(),
                                        msg_type: ansible_mesh_core::MsgType::ExecutionEventAck,
                                        seq,
                                        total: 1,
                                        payload,
                                        timestamp,
                                        hmac,
                                    };
                                    if let Err(err) =
                                        crate::service::execution_transport::send_execution_message(
                                            &target_addr,
                                            &ack,
                                        )
                                        .await
                                    {
                                        warn!(
                                            "Failed to return execution ACK to {} at {}: {}",
                                            msg.src_node, target_addr, err
                                        );
                                    }
                                } else {
                                    warn!(
                                        "No mesh target address found for ACK destination {}",
                                        msg.src_node
                                    );
                                }
                            }
                        }
                    }
                    ansible_mesh_core::MsgType::MeshEventAck
                    | ansible_mesh_core::MsgType::ExecutionEventAck => {
                        debug!("Received MeshEventAck from {}", msg.src_node);
                        if let Ok(ack_payload) =
                            serde_json::from_slice::<serde_json::Value>(&msg.payload)
                        {
                            if let Some(acked_seq) =
                                ack_payload.get("acked_seq").and_then(|v| v.as_u64())
                            {
                                let _ = dispatcher_inbound_tx
                                    .send(LedgerCommand::ProcessAck {
                                        consumer_node_id: msg.src_node.clone(),
                                        acked_seq,
                                    })
                                    .await;
                            }
                        }
                    }
                    ansible_mesh_core::MsgType::MeshMembershipAccept => {
                        if let Ok(payload_json) = String::from_utf8(msg.payload.clone()) {
                            handle_mesh_membership_accept(inbound_graph.as_ref(), &payload_json);
                        } else {
                            warn!(
                                "Received mesh membership acceptance from {} with non-UTF8 payload",
                                msg.src_node
                            );
                        }
                    }
                    ansible_mesh_core::MsgType::WebRtcSignal => {
                        info!("Received WebRTC Signaling Payload from {}", msg.src_node);
                        if let Ok(signal_msg) = serde_json::from_slice::<
                            ansible_mesh_core::webrtc::WebRtcSignalMessage,
                        >(&msg.payload)
                        {
                            let webrtc_signal_tx = webrtc_signal_tx_inbound.clone();
                            tokio::spawn(async move {
                                if let ansible_mesh_core::webrtc::SignalPayload::Offer(sdp) =
                                    signal_msg.signal
                                {
                                    let guest = crate::service::webrtc_guest::WebRtcGuest::new(
                                        signal_msg.session_id,
                                        msg.src_node,
                                        webrtc_signal_tx,
                                    );
                                    if let Err(e) = guest.run_answering(sdp).await {
                                        error!("WebRTC Transceiver Guest failed: {}", e);
                                    }
                                }
                            });
                        }
                    }
                    _ => {}
                }
            }
        });
    }

    {
        let socket_webrtc = daemon.socket();
        let webrtc_graph = ctx.graph_domain.clone();
        let local_node_id_webrtc = ctx.caps.node_id.clone();
        tokio::spawn(async move {
            while let Some(signal) = webrtc_signal_rx.recv().await {
                if let Ok(payload_bytes) = serde_json::to_vec(&signal) {
                    let Some(auth_key) =
                        mesh_auth_key_for_node(webrtc_graph.as_ref(), &signal.target_guest_id)
                            .ok()
                            .flatten()
                    else {
                        debug!(
                            "Skipping WebRTC signal for {} until mesh auth key exists",
                            signal.target_guest_id
                        );
                        continue;
                    };
                    let msg_id = uuid::Uuid::new_v4();
                    let seq = 0;
                    let timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();

                    let hmac = ansible_mesh_core::authz::MeshAuth::new(auth_key).sign(
                        &msg_id,
                        seq as u64,
                        &payload_bytes,
                        timestamp,
                    );

                    let msg = ansible_mesh_core::BeaconMessage {
                        version: 1,
                        msg_id,
                        src_node: local_node_id_webrtc.clone(),
                        dest_node: signal.target_guest_id.clone(),
                        msg_type: ansible_mesh_core::MsgType::WebRtcSignal,
                        seq,
                        total: 1,
                        timestamp,
                        payload: payload_bytes,
                        hmac,
                    };

                    if let Ok(packet) = serde_json::to_vec(&msg) {
                        let target_addr = "127.0.0.1:8999";
                        if let Err(e) = socket_webrtc.send_to(&packet, target_addr).await {
                            tracing::error!("UDP WebRTC Signal send failed: {}", e);
                        }
                    }
                }
            }
        });
    }

    {
        let daemon = daemon.clone();
        tokio::spawn(async move {
            if let Err(e) = daemon.run_loop().await {
                error!("Beacon Daemon error: {}", e);
            }
        });
    }

    Ok(())
}

fn local_capability_advertisements(
    graph: &GraphDomain,
    hotel: &HotelRecord,
) -> Result<Vec<CapabilityAdvertisement>> {
    let tool_runner_registry = graph
        .get_config_value("tool_runner_registry")?
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default();
    let tool_capabilities = tool_runner_registry
        .into_iter()
        .filter_map(|entry| {
            Some((
                entry.get("guest_id")?.as_str()?.to_string(),
                entry
                    .get("supported_tools")?
                    .as_array()?
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>(),
            ))
        })
        .collect::<std::collections::HashMap<_, _>>();

    let mut advertisements = Vec::new();
    for guest in graph.list_guests(&hotel.hotel_name, true)? {
        let availability_state = if guest.active_pid.is_some() {
            "live"
        } else {
            "materialization_required"
        };
        let selection_hint = if guest.active_pid.is_some() {
            Some("local_live_preferred".into())
        } else {
            Some("local_materialization_required".into())
        };

        if guest.role == "tool" {
            if let Some(supported_tools) = tool_capabilities.get(&guest.guest_id) {
                for tool_name in supported_tools {
                    advertisements.push(CapabilityAdvertisement {
                        hotel_id: hotel.hotel_name.clone(),
                        node_id: hotel.capabilities.node_id.clone(),
                        incarnation_id: guest.guest_id.clone(),
                        target_role: format!("tool.{tool_name}"),
                        availability_state: availability_state.into(),
                        selection_hint: selection_hint.clone(),
                        latency_hint_ms: hotel.capabilities.constraints.latency_hint_ms,
                        max_concurrent_jobs: hotel.capabilities.constraints.max_concurrent_jobs,
                        active_jobs: 0,
                        queue_depth: 0,
                    });
                }
                continue;
            }
        }

        advertisements.push(CapabilityAdvertisement {
            hotel_id: hotel.hotel_name.clone(),
            node_id: hotel.capabilities.node_id.clone(),
            incarnation_id: guest.guest_id,
            target_role: guest.role,
            availability_state: availability_state.into(),
            selection_hint: selection_hint.clone(),
            latency_hint_ms: hotel.capabilities.constraints.latency_hint_ms,
            max_concurrent_jobs: hotel.capabilities.constraints.max_concurrent_jobs,
            active_jobs: 0,
            queue_depth: 0,
        });
    }
    Ok(advertisements)
}

fn capability_sync_fingerprint(
    advertisements: &[CapabilityAdvertisement],
    execution_reachability: &ExecutionReachability,
) -> String {
    let payload = serde_json::json!({
        "advertisements": advertisements,
        "execution_reachability": execution_reachability,
    });
    let encoded = serde_json::to_vec(&payload).unwrap_or_default();
    let digest = Sha256::digest(encoded);
    format!("{:x}", digest)
}

#[derive(Debug, Clone, PartialEq)]
struct AgentProfile {
    agent_key: String,
    agent_id: String,
    persona_name: String,
    import_workspace: Option<String>,
    /// When true, the seeded orchestrator role incarnation will have is_admin = true,
    /// granting this agent the ability to modify orchestrator and admin roles.
    is_admin: bool,
    /// Operator-supplied turn loop config for the orchestrator role.
    /// When present, overrides the default (empty) TurnLoopConfig on every seed.
    orchestrator_turn_loop_config: Option<ansible_mesh_core::graph::TurnLoopConfig>,
}

fn title_case_agent_name(agent_key: &str) -> String {
    agent_key
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn default_agent_key_for_hotel(hotel_name: &str) -> String {
    hotel_name
        .split('-')
        .find(|part| !part.is_empty() && *part != "hotel" && *part != "test")
        .unwrap_or(hotel_name)
        .to_ascii_lowercase()
}

fn default_agent_profile_for_hotel(hotel_name: &str) -> AgentProfile {
    let agent_key = default_agent_key_for_hotel(hotel_name);
    AgentProfile {
        agent_id: format!("agent-{}-01", agent_key),
        persona_name: title_case_agent_name(&agent_key),
        agent_key,
        import_workspace: None,
        is_admin: false,
        orchestrator_turn_loop_config: None,
    }
}

fn hotel_object<'a>(
    config_json: &'a serde_json::Value,
    hotel_name: &str,
) -> Option<&'a serde_json::Map<String, serde_json::Value>> {
    config_json
        .as_object()?
        .get("hotels")?
        .as_object()?
        .get(hotel_name)?
        .as_object()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfiguredPeerHotel {
    hotel_name: String,
    mesh_host: String,
    mesh_port: u16,
    blob_port: u16,
    execution_port: u16,
}

fn merge_configured_peer_hotels(
    merged: &mut std::collections::BTreeMap<String, ConfiguredPeerHotel>,
    hotel: &serde_json::Map<String, serde_json::Value>,
) {
    let Some(peers) = hotel
        .get("backbone_peers")
        .or_else(|| hotel.get("backbonePeers"))
        .or_else(|| hotel.get("peers"))
        .and_then(serde_json::Value::as_array)
    else {
        return;
    };

    for peer in peers {
        let Some(peer_obj) = peer.as_object() else {
            continue;
        };

        let Some(peer_name) = peer_obj
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };

        let Some(peer_host) = peer_obj
            .get("host")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };

        let mesh_port = peer_obj
            .get("beacon_port")
            .or_else(|| peer_obj.get("beaconPort"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(8999);
        let blob_port = peer_obj
            .get("blob_port")
            .or_else(|| peer_obj.get("blobPort"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(mesh_port.saturating_add(1));
        let execution_port = peer_obj
            .get("execution_port")
            .or_else(|| peer_obj.get("executionPort"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .unwrap_or(mesh_port.saturating_add(2));

        merged.insert(
            peer_name.to_string(),
            ConfiguredPeerHotel {
                hotel_name: peer_name.to_string(),
                mesh_host: peer_host.to_string(),
                mesh_port,
                blob_port,
                execution_port,
            },
        );
    }
}

fn configured_peer_hotels(
    config_json: &serde_json::Value,
    hotel_name: &str,
) -> Vec<ConfiguredPeerHotel> {
    let mut merged = std::collections::BTreeMap::new();

    if let Some(default_hotel) = hotel_object(config_json, "default") {
        merge_configured_peer_hotels(&mut merged, default_hotel);
    }

    if hotel_name != "default" {
        if let Some(hotel) = hotel_object(config_json, hotel_name) {
            merge_configured_peer_hotels(&mut merged, hotel);
        }
    }

    merged.remove(hotel_name);
    merged.into_values().collect()
}

fn seed_peer_hotels_from_config(
    graph: &GraphDomain,
    config_json: &serde_json::Value,
    local_hotel_name: &str,
) -> Result<usize> {
    let peers = configured_peer_hotels(config_json, local_hotel_name);
    let mut count = 0;

    for peer in peers {
        let mut hotel = graph
            .get_hotel(&peer.hotel_name)?
            .unwrap_or_else(|| default_hotel_record(&peer.hotel_name));
        hotel.mesh_host = Some(peer.mesh_host);
        hotel.mesh_port = peer.mesh_port;
        hotel.blob_port = peer.blob_port;
        hotel.execution_port = peer.execution_port;
        graph.upsert_hotel(&hotel)?;
        count += 1;
    }

    Ok(count)
}

#[cfg(test)]
fn selected_agent_key_for_hotel(
    hotel: &serde_json::Map<String, serde_json::Value>,
) -> Option<String> {
    let agents = hotel.get("agents")?.as_object()?;
    if let Some(selected) = hotel
        .get("selected_agent")
        .and_then(serde_json::Value::as_str)
    {
        if agents.contains_key(selected) {
            return Some(selected.to_string());
        }
    }
    if agents.len() == 1 {
        return agents.keys().next().cloned();
    }
    if agents.contains_key("default") {
        return Some("default".into());
    }
    None
}

#[cfg(test)]
fn merged_agent_config(
    config_json: &serde_json::Value,
    hotel_name: &str,
) -> Option<(String, serde_json::Map<String, serde_json::Value>)> {
    let selected_hotel = hotel_object(config_json, hotel_name);
    let selected_key = selected_hotel.and_then(selected_agent_key_for_hotel)?;
    let mut merged = serde_json::Map::new();

    if let Some(hotel) = selected_hotel {
        if let Some(agents) = hotel.get("agents").and_then(serde_json::Value::as_object) {
            if selected_key != "default" {
                if let Some(default_agent) =
                    agents.get("default").and_then(serde_json::Value::as_object)
                {
                    merged.extend(default_agent.clone());
                }
            }
            if let Some(agent) = agents
                .get(&selected_key)
                .and_then(serde_json::Value::as_object)
            {
                merged.extend(agent.clone());
            }
        }
    }

    Some((selected_key, merged))
}

#[cfg(test)]
fn agent_profile_from_config(
    config_json: &serde_json::Value,
    hotel_name: &str,
) -> Option<AgentProfile> {
    let (selected_key, agent) = merged_agent_config(config_json, hotel_name)?;
    let agent_key = if selected_key == "default" {
        agent
            .get("agent_key")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| default_agent_key_for_hotel(hotel_name))
    } else {
        selected_key
    };
    let agent_id = agent
        .get("agent_id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("agent-{}-01", agent_key));
    let persona_name = agent
        .get("persona_name")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| title_case_agent_name(&agent_key));
    let import_workspace = agent
        .get("import_workspace")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let is_admin = agent
        .get("is_admin")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    let orchestrator_turn_loop_config = agent
        .get("turn_loop_config")
        .and_then(|v| serde_json::from_value(v.clone()).ok());

    Some(AgentProfile {
        agent_key,
        agent_id,
        persona_name,
        import_workspace,
        is_admin,
        orchestrator_turn_loop_config,
    })
}

/// Returns profiles for ALL agents in the hotel config.
/// Falls back to a single default profile if the hotel has no agents section.
fn all_agent_profiles_from_config(
    config_json: &serde_json::Value,
    hotel_name: &str,
) -> Vec<AgentProfile> {
    let all_agents = hotel_object(config_json, hotel_name)
        .and_then(|hotel| hotel.get("agents"))
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();

    if all_agents.is_empty() {
        return vec![default_agent_profile_for_hotel(hotel_name)];
    }

    all_agents
        .into_iter()
        .filter_map(|(agent_key, agent_val)| {
            let agent = agent_val.as_object()?;
            let agent_id = agent
                .get("agent_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("agent-{}-01", agent_key));
            let persona_name = agent
                .get("persona_name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| title_case_agent_name(&agent_key));
            let import_workspace = agent
                .get("import_workspace")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string);
            let is_admin = agent
                .get("is_admin")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let orchestrator_turn_loop_config = agent
                .get("turn_loop_config")
                .and_then(|v| serde_json::from_value(v.clone()).ok());
            Some(AgentProfile {
                agent_key,
                agent_id,
                persona_name,
                import_workspace,
                is_admin,
                orchestrator_turn_loop_config,
            })
        })
        .collect()
}

/// Returns the raw agent config map for a specific agent key (not selection-based).
fn raw_agent_config_for_key(
    config_json: &serde_json::Value,
    hotel_name: &str,
    agent_key: &str,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    let mut merged = serde_json::Map::new();
    if let Some(hotel) = hotel_object(config_json, hotel_name) {
        if let Some(agents) = hotel.get("agents").and_then(serde_json::Value::as_object) {
            if let Some(agent) = agents.get(agent_key).and_then(serde_json::Value::as_object) {
                merged.extend(agent.clone());
            }
        }
    }
    if merged.is_empty() {
        None
    } else {
        Some(merged)
    }
}

/// Per-agent guests: one philote per agent profile.
fn agent_guests_for_profile(hotel_name: &str, profile: &AgentProfile) -> GuestRecord {
    let hotel = default_hotel_record(hotel_name);
    let socket_path = hotel.ipc_socket_path;
    let node_id = hotel.capabilities.node_id;
    GuestRecord {
        hotel_name: hotel_name.to_string(),
        guest_id: format!("{hotel_name}:philote-{}", profile.agent_key),
        role: "agent".into(),
        config_json: serde_json::json!({
            "command": "philote",
            "args": [],
            "env": {
                "PHILOTIC_HOTEL_NAME": hotel_name,
                "PHILOTIC_HOTEL_SOCKET": socket_path,
                "PHILOTIC_NODE_ID": node_id,
                "PHILOTIC_AGENT_ID": profile.agent_id
            }
        })
        .to_string(),
        is_active: true,
        active_pid: None,
        last_active_at: None,
    }
}

/// Companion agent-graph-runner guest for a philote agent.
/// One per agent; stores per-agent cognitive graph at ~/.philotic/agent-graph-{id}.db.
fn agent_graph_runner_guest(hotel_name: &str, profile: &AgentProfile) -> GuestRecord {
    let hotel = default_hotel_record(hotel_name);
    let socket_path = hotel.ipc_socket_path;
    let guest_id = format!("{hotel_name}:agent-graph-{}", profile.agent_id);
    GuestRecord {
        hotel_name: hotel_name.to_string(),
        guest_id: guest_id.clone(),
        role: "agent-graph".into(),
        config_json: serde_json::json!({
            "command": "agent-graph-runner",
            "args": [],
            "env": {
                "PHILOTIC_AGENT_ID": profile.agent_id,
                "PHILOTIC_GRAPH_RUNNER_ID": guest_id,
                "PHILOTIC_IPC_SOCKET": socket_path
            }
        })
        .to_string(),
        is_active: true,
        active_pid: None,
        last_active_at: None,
    }
}

/// Hotel-level shared guests: one membrane for all agents, plus model controllers.
fn hotel_shared_guests(hotel_name: &str, profiles: &[AgentProfile]) -> Vec<GuestRecord> {
    let hotel = default_hotel_record(hotel_name);
    let socket_path = hotel.ipc_socket_path;
    let blob_base_url = format!("http://127.0.0.1:{}", hotel.blob_port);
    let node_id = hotel.capabilities.node_id;
    let training_base = profile_dir().unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        std::path::PathBuf::from(home).join(".philotic")
    });
    let whisper_db = training_base
        .join("whisper_training.db")
        .to_string_lossy()
        .to_string();
    let training_audio_dir = training_base
        .join("training_audio")
        .to_string_lossy()
        .to_string();

    // Build the agent roster JSON for the single membrane
    let roster: Vec<serde_json::Value> = profiles
        .iter()
        .map(|p| {
            serde_json::json!({
                "agent_key": p.agent_key,
                "agent_id": p.agent_id
            })
        })
        .collect();
    let roster_json = serde_json::to_string(&roster).unwrap_or_else(|_| "[]".to_string());

    vec![
        GuestRecord {
            hotel_name: hotel_name.to_string(),
            guest_id: format!("{hotel_name}:membrane-gateway"),
            role: "membrane".into(),
            config_json: serde_json::json!({
                "command": "membrane-telegram",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path.clone(),
                    "PHILOTIC_NODE_ID": node_id.clone(),
                    "PHILOTIC_BLOB_BASE_URL": blob_base_url,
                    "PHILOTIC_GUEST_ID": format!("{hotel_name}:membrane-gateway"),
                    "PHILOTIC_AGENT_ROSTER": roster_json
                }
            })
            .to_string(),
            is_active: true,
            active_pid: None,
            last_active_at: None,
        },
        GuestRecord {
            hotel_name: hotel_name.to_string(),
            guest_id: format!("{hotel_name}:model-controller-gemini"),
            config_json: serde_json::json!({
                "command": "model-controller-gemini",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path.clone(),
                    "PHILOTIC_NODE_ID": node_id.clone(),
                    "PHILOTIC_ROUTER_CAPTURE_ENABLED": "true"
                }
            })
            .to_string(),
            role: "model".into(),
            is_active: true,
            active_pid: None,
            last_active_at: None,
        },
        GuestRecord {
            hotel_name: hotel_name.to_string(),
            guest_id: format!("{hotel_name}:model-controller-elevenlabs"),
            role: "model.elevenlabs".into(),
            config_json: serde_json::json!({
                "command": "model-controller-elevenlabs",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path.clone(),
                    "PHILOTIC_NODE_ID": node_id.clone()
                }
            })
            .to_string(),
            is_active: true,
            active_pid: None,
            last_active_at: None,
        },
        GuestRecord {
            hotel_name: hotel_name.to_string(),
            guest_id: format!("{hotel_name}:model-controller-onnx"),
            role: "model.local".into(),
            config_json: serde_json::json!({
                "command": "model-controller-onnx",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path.clone(),
                    "PHILOTIC_NODE_ID": node_id.clone(),
                    "PHILOTIC_ONNX_GUEST_ID": format!("{hotel_name}:model-controller-onnx"),
                    "PHILOTIC_ONNX_SIDECAR_ADDR": format!("127.0.0.1:{}", hotel.blob_port + 4)
                }
            })
            .to_string(),
            is_active: true,
            active_pid: None,
            last_active_at: None,
        },
        GuestRecord {
            hotel_name: hotel_name.to_string(),
            guest_id: format!("{hotel_name}:tool-runner"),
            role: "tool".into(),
            config_json: serde_json::json!({
                "command": "tool-runner",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path.clone(),
                    "PHILOTIC_NODE_ID": node_id.clone()
                }
            })
            .to_string(),
            is_active: true,
            active_pid: None,
            last_active_at: None,
        },
        GuestRecord {
            hotel_name: hotel_name.to_string(),
            guest_id: format!("{hotel_name}:graph-runner"),
            role: "tool.graph".into(),
            config_json: serde_json::json!({
                "command": "graph-runner",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path.clone(),
                    "PHILOTIC_NODE_ID": node_id.clone(),
                    "PHILOTIC_GRAPH_RUNNER_ID": format!("{hotel_name}:graph-runner")
                }
            })
            .to_string(),
            is_active: true,
            active_pid: None,
            last_active_at: None,
        },
        GuestRecord {
            hotel_name: hotel_name.to_string(),
            guest_id: format!("{hotel_name}:graph-datasource"),
            role: "graph-datasource".into(),
            config_json: serde_json::json!({
                "command": "graph-datasource",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path.clone(),
                    "PHILOTIC_NODE_ID": node_id.clone(),
                    "PHILOTIC_GRAPH_DATASOURCE_ID": format!("{hotel_name}:graph-datasource")
                }
            })
            .to_string(),
            is_active: true,
            active_pid: None,
            last_active_at: None,
        },
        GuestRecord {
            hotel_name: hotel_name.to_string(),
            guest_id: format!("{hotel_name}:table-datasource"),
            role: "table-datasource".into(),
            config_json: serde_json::json!({
                "command": "table-datasource",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path.clone(),
                    "PHILOTIC_NODE_ID": node_id.clone(),
                    "PHILOTIC_TABLE_DATASOURCE_ID": format!("{hotel_name}:table-datasource")
                }
            })
            .to_string(),
            is_active: true,
            active_pid: None,
            last_active_at: None,
        },
        GuestRecord {
            hotel_name: hotel_name.to_string(),
            guest_id: format!("{hotel_name}:router-listener"),
            role: "router-listener".into(),
            config_json: serde_json::json!({
                "command": "router-listener",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path.clone(),
                    "PHILOTIC_NODE_ID": node_id.clone(),
                    "PHILOTIC_TRAINING_DB": whisper_db,
                    "PHILOTIC_TRAINING_AUDIO_DIR": training_audio_dir,
                    "PHILOTIC_TRAINING_AUTO_ELIGIBLE": "false"
                }
            })
            .to_string(),
            is_active: true,
            active_pid: None,
            last_active_at: None,
        },
        GuestRecord {
            hotel_name: hotel_name.to_string(),
            guest_id: format!("{hotel_name}:membrane-mcp"),
            role: "mcp-membrane".into(),
            config_json: serde_json::json!({
                "command": "membrane-mcp",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path,
                    "PHILOTIC_NODE_ID": node_id,
                    "PHILOTIC_GUEST_ID": format!("{hotel_name}:membrane-mcp"),
                    "MCP_PORT": "9100"
                }
            })
            .to_string(),
            is_active: true,
            active_pid: None,
            last_active_at: None,
        },
    ]
}

/// Legacy single-profile seed — used in tests that expect the old per-profile layout.
#[cfg(test)]
fn guest_seed_for_profile(hotel_name: &str, profile: &AgentProfile) -> Vec<GuestRecord> {
    let mut guests = hotel_shared_guests(hotel_name, std::slice::from_ref(profile));
    guests.push(agent_guests_for_profile(hotel_name, profile));
    guests.push(agent_graph_runner_guest(hotel_name, profile));
    guests
}

#[cfg(test)]
fn default_guest_seed(hotel_name: &str) -> Vec<GuestRecord> {
    guest_seed_for_profile(hotel_name, &default_agent_profile_for_hotel(hotel_name))
}

fn identity_bundle_from_workspace(source_agent: &str, workspace: &Path) -> serde_json::Value {
    serde_json::json!({
        "source_kind": "openclaw_workspace",
        "source_agent": source_agent,
        "workspace_path": workspace,
        "soul_text": maybe_load_text(&workspace.join("SOUL.md")),
        "identity_text": maybe_load_text(&workspace.join("IDENTITY.md")),
        "user_context_text": maybe_load_text(&workspace.join("USER.md")),
        "agents_text": maybe_load_text(&workspace.join("AGENTS.md")),
        "memory_summary": maybe_load_text(&workspace.join("MEMORY.md")),
    })
}

fn maybe_load_text(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

/// If `workspace` does not exist, create it and seed any files from `existing_bundle`.
/// Files are only written when the workspace file is absent — existing files are never
/// overwritten, so manual edits are always preserved.
fn ensure_workspace_exists(workspace: &Path, existing_bundle: Option<&serde_json::Value>) {
    if workspace.exists() {
        return;
    }
    if let Err(e) = fs::create_dir_all(workspace) {
        warn!(
            "Could not create workspace dir {}: {}",
            workspace.display(),
            e
        );
        return;
    }
    info!("Created workspace directory: {}", workspace.display());

    let bundle = match existing_bundle {
        Some(b) => b,
        None => return,
    };

    let file_map: &[(&str, &str)] = &[
        ("soul_text", "SOUL.md"),
        ("identity_text", "IDENTITY.md"),
        ("user_context_text", "USER.md"),
        ("agents_text", "AGENTS.md"),
        ("memory_summary", "MEMORY.md"),
    ];
    for (field, filename) in file_map {
        if let Some(text) = bundle.get(field).and_then(|v| v.as_str()) {
            if !text.trim().is_empty() {
                let path = workspace.join(filename);
                if let Err(e) = fs::write(&path, text) {
                    warn!("Could not write {}: {}", path.display(), e);
                } else {
                    info!("Seeded {} from graph bundle.", filename);
                }
            }
        }
    }
}

fn extract_context_graph_entries(
    config_json: &serde_json::Value,
    hotel_name: Option<&str>,
) -> Vec<(String, serde_json::Value)> {
    let Some(obj) = config_json.as_object() else {
        return Vec::new();
    };

    let mut merged = serde_json::Map::new();

    if let Some(context_graph) = obj
        .get("context_graph")
        .and_then(serde_json::Value::as_object)
    {
        merged.extend(context_graph.clone());
    }

    if let Some(hotels) = obj.get("hotels").and_then(serde_json::Value::as_object) {
        if let Some(hotel_name) = hotel_name {
            // Prefer the named hotel; fall back to "default" for shared/overlay config.
            let hotel_obj = hotels
                .get(hotel_name)
                .or_else(|| hotels.get("default"))
                .and_then(serde_json::Value::as_object);
            if let Some(hotel) = hotel_obj {
                merge_hotel_base_entries(&mut merged, hotel);
            }
        }
    }

    if let Some(hotel_name) = hotel_name {
        if let Some(hotel) = hotel_object(config_json, hotel_name) {
            if let Some(agents) = hotel.get("agents").and_then(serde_json::Value::as_object) {
                for (agent_key, agent_val) in agents {
                    if let Some(agent) = agent_val.as_object() {
                        merge_agent_entries(&mut merged, agent, Some(agent_key.as_str()));
                    }
                }
            }
        }
    }

    if !merged.is_empty() {
        return merged.into_iter().collect();
    }

    obj.iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

fn merge_hotel_base_entries(
    merged: &mut serde_json::Map<String, serde_json::Value>,
    hotel: &serde_json::Map<String, serde_json::Value>,
) {
    if let Some(context_graph) = hotel
        .get("context_graph")
        .and_then(serde_json::Value::as_object)
    {
        merged.extend(context_graph.clone());
    }

    if let Some(telegram) = hotel.get("telegram").and_then(serde_json::Value::as_object) {
        merge_telegram_entries(merged, telegram, None);
    }
}

fn merge_agent_entries(
    merged: &mut serde_json::Map<String, serde_json::Value>,
    agent: &serde_json::Map<String, serde_json::Value>,
    agent_key: Option<&str>,
) {
    if let Some(context_graph) = agent
        .get("context_graph")
        .and_then(serde_json::Value::as_object)
    {
        merged.extend(context_graph.clone());
    }

    if let Some(telegram) = agent.get("telegram").and_then(serde_json::Value::as_object) {
        merge_telegram_entries(merged, telegram, agent_key);
    }

    if let Some(model) = agent.get("model").and_then(serde_json::Value::as_object) {
        if let Some(default_model) = model.get("default_model") {
            merged.insert("default_model".into(), default_model.clone());
        }
    }

    // Store agent-level policy objects wholesale so any guest can read them via GetConfig.
    for key in ["voice_response_policy", "media_routing_policy"] {
        if let Some(policy) = agent.get(key) {
            merged.insert(key.into(), policy.clone());
        }
    }

    // Promote the ElevenLabs-specific voice_id to elevenlabs_voice_id so model-router
    // ProviderConfigs can pick it up without knowing about VoiceResponsePolicy.
    // Prefer voice_ids.elevenlabs over the top-level voice_id (which belongs to onnx).
    let elevenlabs_id = agent.get("voice_response_policy").and_then(|p| {
        p.get("voice_ids")
            .and_then(|m| m.get("elevenlabs"))
            .filter(|v| v.is_string())
            .or_else(|| p.get("voice_id").filter(|v| v.is_string()))
    });
    if let Some(voice_id) = elevenlabs_id {
        merged
            .entry("elevenlabs_voice_id".to_string())
            .or_insert_with(|| voice_id.clone());
    }
}

fn merge_telegram_entries(
    merged: &mut serde_json::Map<String, serde_json::Value>,
    telegram: &serde_json::Map<String, serde_json::Value>,
    agent_key: Option<&str>,
) {
    if let Some(bot_token) = telegram.get("bot_token") {
        // Always store the per-agent key so membrane can retrieve it by agent_key.
        if let Some(key) = agent_key {
            merged.insert(format!("telegram_bot_token_{key}"), bot_token.clone());
        }
        // Also store the global fallback key for single-agent / legacy configs.
        merged.insert("telegram_bot_token".into(), bot_token.clone());
    }
    if let Some(allowed_users) = telegram.get("allowed_users") {
        if let Some(key) = agent_key {
            merged.insert(
                format!("telegram_allowed_users_{key}"),
                allowed_users.clone(),
            );
        }
        merged.insert("telegram_allowed_users".into(), allowed_users.clone());
    }
}

#[cfg(test)]
fn configured_agent_identity_from_config(
    config_json: &serde_json::Value,
    hotel_name: &str,
) -> Option<AgentIdentityRecord> {
    let profile = agent_profile_from_config(config_json, hotel_name)?;
    let import_workspace = profile.import_workspace.as_deref()?;
    if import_workspace.is_empty() {
        return None;
    }

    let agent_config = merged_agent_config(config_json, hotel_name).map(|(_, agent)| agent);
    Some(agent_identity_record_for_profile(
        &profile,
        hotel_name,
        agent_config.as_ref(),
    ))
}

fn agent_identity_record_for_profile(
    profile: &AgentProfile,
    authority_hotel: &str,
    agent_config: Option<&serde_json::Map<String, serde_json::Value>>,
) -> AgentIdentityRecord {
    let mut bundle_json = profile
        .import_workspace
        .as_deref()
        .map(|workspace| identity_bundle_from_workspace(&profile.agent_key, Path::new(workspace)))
        .unwrap_or_else(|| serde_json::json!({}));

    // Merge policy and identity fields from config into bundle.
    if let Some(bundle_obj) = bundle_json.as_object_mut() {
        if let Some(config) = agent_config {
            for key in [
                "voice_response_policy",
                "media_routing_policy",
                "default_toolset",
            ] {
                if let Some(value) = config.get(key) {
                    bundle_obj.insert(key.to_string(), value.clone());
                }
            }
            // If the workspace didn't supply an identity_text (no IDENTITY.md), fall back to
            // system_prompt from the agent config. This lets operators define agent personas
            // directly in mesh-config.json without needing workspace files on disk.
            let has_identity = bundle_obj
                .get("identity_text")
                .map(|v| !v.is_null() && v.as_str().is_some_and(|s| !s.is_empty()))
                .unwrap_or(false);
            if !has_identity {
                if let Some(sp) = config.get("system_prompt").and_then(|v| v.as_str()) {
                    if !sp.is_empty() {
                        bundle_obj.insert("identity_text".to_string(), sp.into());
                    }
                }
            }
        }
    }

    AgentIdentityRecord {
        agent_id: profile.agent_id.clone(),
        persona_name: profile.persona_name.clone(),
        authority_hotel: authority_hotel.to_string(),
        bundle_json,
    }
}

fn reconcile_hotel_record(graph: &GraphDomain, hotel_name: &str) -> Result<HotelRecord> {
    let Some(mut hotel) = graph.get_hotel(hotel_name)? else {
        let hotel = default_hotel_record(hotel_name);
        graph.upsert_hotel(&hotel)?;
        return Ok(hotel);
    };

    let desired = default_hotel_record(hotel_name);
    let mut changed = false;

    if hotel.execution_port == 0 {
        hotel.execution_port = desired.execution_port;
        changed = true;
    }
    // When a profile is active, always use the profile-derived socket path.
    // The stored path may be from a non-profile run and must not win.
    if hotel.ipc_socket_path.trim().is_empty() || profile_dir().is_some() {
        hotel.ipc_socket_path = desired.ipc_socket_path;
        changed = true;
    }

    if changed {
        graph.upsert_hotel(&hotel)?;
    }

    Ok(hotel)
}

fn deactivate_legacy_managed_guests(
    graph: &GraphDomain,
    hotel_name: &str,
    profiles: &[AgentProfile],
    desired_guests: &[GuestRecord],
) -> Result<()> {
    // Transitional cleanup: the Telegram gateway used to be named "hegemon".
    // Keep deactivating those legacy rows until old startup databases stop carrying them.
    let desired_ids = desired_guests
        .iter()
        .map(|guest| guest.guest_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut legacy_guest_ids = std::collections::HashSet::new();
    legacy_guest_ids.insert("hegemon-gateway".to_string());
    legacy_guest_ids.insert("model-router-gemini".to_string());
    for profile in profiles {
        legacy_guest_ids.insert(format!("philote-{}", profile.agent_key));
        legacy_guest_ids.insert(format!("{hotel_name}:philote-{}", profile.agent_key));
        legacy_guest_ids.insert(format!("hegemon-gateway-{}", profile.agent_key));
        legacy_guest_ids.insert(format!(
            "{hotel_name}:hegemon-gateway-{}",
            profile.agent_key
        ));
    }

    let stale = graph
        .list_guests(hotel_name, false)?
        .into_iter()
        .filter(|guest| {
            if desired_ids.contains(guest.guest_id.as_str()) {
                return false;
            }

            let hotel_prefixed_legacy_guest = guest
                .guest_id
                .strip_prefix(&format!("{hotel_name}:"))
                .is_some_and(|suffix| {
                    suffix.starts_with("philote-")
                        || suffix.starts_with("hegemon-gateway")
                        || suffix.starts_with("model-router-")
                        // Old per-agent membranes: membrane-gateway-{agent_key}
                        // New: single membrane-gateway (no agent_key suffix)
                        || (suffix.starts_with("membrane-gateway-"))
                });

            legacy_guest_ids.contains(&guest.guest_id)
                || hotel_prefixed_legacy_guest
                || (!guest.guest_id.starts_with(&format!("{hotel_name}:"))
                    && matches!(
                        guest.role.as_str(),
                        "agent" | "hegemon" | "membrane" | "model" | "model.elevenlabs" | "tool"
                    ))
        })
        .map(|mut guest| {
            guest.is_active = false;
            guest.active_pid = None;
            guest
        })
        .collect::<Vec<_>>();

    if !stale.is_empty() {
        graph.seed_guests(hotel_name, &stale)?;
    }

    Ok(())
}

/// Seed the built-in abstract tool catalog into the context graph.
///
/// Uses upsert semantics — safe to call on every startup. Tools not already
/// present are inserted; existing entries are updated to the current definition.
/// Operator-added or tool-runner-provided tools with distinct names are unaffected.
fn seed_abstract_tool_catalog(graph: &GraphDomain) -> anyhow::Result<()> {
    let catalog = [
        AbstractToolRecord {
            tool_name: "session.status".into(),
            description: "Returns a summary of the current session state, including the active \
                          session ID, turn count, approval policy, and active tool runners."
                .into(),
            input_schema: serde_json::json!({ "type": "object", "properties": {} }),
            class: "session".into(),
            tool_markers: Vec::new(),
        },
        AbstractToolRecord {
            tool_name: "echo".into(),
            description: "Echoes a string back unchanged. Use for testing tool routing and \
                          round-trip connectivity."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string", "description": "The text to echo back." }
                },
                "required": ["text"]
            }),
            class: "utility".into(),
            tool_markers: Vec::new(),
        },
        AbstractToolRecord {
            tool_name: "workspace.list".into(),
            description: "Lists files and directories at the given path within the workspace. \
                          Defaults to the workspace root if no path is provided."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path within the workspace to list."
                    }
                }
            }),
            class: "workspace".into(),
            tool_markers: Vec::new(),
        },
        AbstractToolRecord {
            tool_name: "workspace.read".into(),
            description: "Reads the contents of a file in the workspace. Supports optional \
                          byte-range limiting via offset and limit."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path to the file within the workspace."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Byte offset to start reading from."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of bytes to read."
                    }
                },
                "required": ["path"]
            }),
            class: "workspace".into(),
            tool_markers: Vec::new(),
        },
        AbstractToolRecord {
            tool_name: "agent.configure".into(),
            description: "Update an agent configuration field. Supports approval_policy, \
                          profile, and bindings sections. Requires operator approval unless \
                          preapproved."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "config_path": { "type": "string" },
                    "value": {},
                    "operation": {
                        "type": "string",
                        "enum": ["set", "append", "remove"]
                    }
                },
                "required": ["config_path", "value"]
            }),
            class: "config".into(),
            tool_markers: Vec::new(),
        },
        AbstractToolRecord {
            tool_name: "hotel.status".into(),
            description: "Returns the current hotel status: active guests, registered roles, \
                          materialized processes, and system health. Use this before asking the \
                          operator for information about running agents or system state."
                .into(),
            input_schema: serde_json::json!({ "type": "object", "properties": {} }),
            class: "session".into(),
            tool_markers: Vec::new(),
        },
        AbstractToolRecord {
            tool_name: "hotel.logs".into(),
            description: "Returns recent hotel log lines. Use this to inspect system events, \
                          errors, and agent activity before reaching for bash.exec. Defaults to \
                          50 lines; request up to 500."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "lines": {
                        "type": "integer",
                        "description": "Number of recent log lines to return (max 500, default 50)."
                    }
                }
            }),
            class: "session".into(),
            tool_markers: Vec::new(),
        },
        AbstractToolRecord {
            tool_name: "role.list".into(),
            description: "Lists all role incarnations configured for this agent, with their \
                          toolset profile, readiness state, and home hotel. Call this before \
                          role.configure or role.set_home to confirm the current roster."
                .into(),
            input_schema: serde_json::json!({ "type": "object", "properties": {} }),
            class: "session".into(),
            tool_markers: Vec::new(),
        },
        AbstractToolRecord {
            tool_name: "role.set_home".into(),
            description: "Pin a role's execution to a specific hotel (or clear the pin to use \
                          the authority hotel). After pinning, handoff.to_role routes the role \
                          there automatically. Requires role_name, reason, and optionally \
                          target_hotel (omit or null to clear)."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "role_name": {
                        "type": "string",
                        "description": "The role to re-home."
                    },
                    "target_hotel": {
                        "type": "string",
                        "description": "Hotel name to pin to. Omit or set null to clear the pin."
                    },
                    "reason": {
                        "type": "string",
                        "description": "Why this role belongs on this hotel."
                    }
                },
                "required": ["role_name", "reason"]
            }),
            class: "config".into(),
            tool_markers: Vec::new(),
        },
        AbstractToolRecord {
            tool_name: "bash.exec".into(),
            description: "Last-resort shell execution. Runs a shell command and returns stdout, \
                          stderr, and exit code. Use ONLY when no Philotic-native tool can \
                          accomplish the task. Requires explicit operator approval before execution."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute (passed to `sh -c`)."
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Optional absolute path to use as the working directory."
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "description": "Maximum seconds to wait before killing the process. Defaults to 30."
                    }
                },
                "required": ["command"]
            }),
            class: "shell".into(),
            tool_markers: vec!["high_agency".into()],
        },
        AbstractToolRecord {
            tool_name: "desktop.observe".into(),
            description: "Returns low-agency metadata about the bound desktop automation runner \
                          and desktop session. This observe-only CUA scaffold does not take a \
                          screenshot and cannot click, type, press keys, or scroll."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "detail": {
                        "type": "string",
                        "enum": ["summary"],
                        "description": "Observation detail level. Only summary metadata is supported in the first scaffold."
                    }
                }
            }),
            class: "desktop".into(),
            tool_markers: vec!["desktop_bound".into(), "local_only".into(), "low_agency".into()],
        },
        // ── Training data admin tools ─────────────────────────────────────
        AbstractToolRecord {
            tool_name: "training.list".into(),
            description: "List captured voice training samples. Filter by state: all (default), \
                          uncorrected, eligible, or exported. Optionally narrow by agent_id."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "limit": {
                        "type": "integer",
                        "description": "Number of samples to return (default 20, max 200)."
                    },
                    "filter": {
                        "type": "string",
                        "enum": ["all", "uncorrected", "eligible", "exported"],
                        "description": "Filter samples by correction/export state."
                    },
                    "agent_id": {
                        "type": "string",
                        "description": "Restrict to samples from a specific agent."
                    }
                }
            }),
            class: "training".into(),
            tool_markers: Vec::new(),
        },
        AbstractToolRecord {
            tool_name: "training.correct".into(),
            description: "Apply an operator correction to a captured voice training sample. \
                          Sets the ground-truth transcript and marks the sample training_eligible."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "turn_id": {
                        "type": "string",
                        "description": "The turn to correct."
                    },
                    "corrected_transcript": {
                        "type": "string",
                        "description": "The ground-truth transcript."
                    }
                },
                "required": ["turn_id", "corrected_transcript"]
            }),
            class: "training".into(),
            tool_markers: vec!["high_agency".into()],
        },
        AbstractToolRecord {
            tool_name: "training.export".into(),
            description: "Export training-eligible samples to a file for the fine-tuning pipeline. \
                          Supports huggingface (JSON array) and nemo (one-JSON-per-line manifest) formats. \
                          Marks exported samples so they are not re-exported."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "format": {
                        "type": "string",
                        "enum": ["huggingface", "nemo"],
                        "description": "Output format."
                    },
                    "output_path": {
                        "type": "string",
                        "description": "Absolute path for the export file."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max samples to export (default: all eligible)."
                    }
                },
                "required": ["format", "output_path"]
            }),
            class: "training".into(),
            tool_markers: vec!["high_agency".into()],
        },
        AbstractToolRecord {
            tool_name: "training.status".into(),
            description: "Return a summary of voice training sample counts: total captured, \
                          uncorrected, eligible for export, and already exported. \
                          Optionally filtered by agent_id."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "Restrict counts to a specific agent."
                    }
                }
            }),
            class: "training".into(),
            tool_markers: Vec::new(),
        },
        AbstractToolRecord {
            tool_name: "asr.setup".into(),
            description: "Set up the Parakeet ASR provider on this node: verifies Python + nemo-toolkit, \
                          optionally installs nemo-toolkit[asr] via pip, writes the component config, \
                          and registers the model-controller-parakeet guest for automatic materialization. \
                          python_path defaults to 'python3'; auto_install defaults to true."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "python_path": {
                        "type": "string",
                        "description": "Python interpreter path (must have or will get nemo-toolkit)."
                    },
                    "model_name": {
                        "type": "string",
                        "description": "NeMo model name (default: nvidia/parakeet-tdt-0.6b-v2)."
                    },
                    "auto_install": {
                        "type": "boolean",
                        "description": "Attempt pip install if nemo import fails (default: true)."
                    }
                }
            }),
            class: "asr".into(),
            tool_markers: vec!["high_agency".into()],
        },
        AbstractToolRecord {
            tool_name: "asr.status".into(),
            description: "Return the current status of the Parakeet ASR provider: whether the guest \
                          is registered and active, its PID if running, and whether nemo-toolkit is \
                          importable on this node."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
            class: "asr".into(),
            tool_markers: Vec::new(),
        },
        // ── Cron scheduler tools ──────────────────────────────────────────────
        AbstractToolRecord {
            tool_name: "cron.register".into(),
            description: "Register or update a cron job on the hotel. The job fires on a 7-field \
                          cron schedule and delivers a JSON payload to a target role's inbox. \
                          Use cron.list first to avoid duplicates. Responds with the assigned job_id."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "schedule": {
                        "type": "string",
                        "description": "7-field cron expression: <sec> <min> <hour> <dom> <month> <dow> <year>. Example: \"0 */5 * * * * *\" for every 5 minutes."
                    },
                    "target_role": {
                        "type": "string",
                        "description": "Role name whose inbox receives the trigger payload."
                    },
                    "payload": {
                        "type": "string",
                        "description": "JSON payload string delivered to the role. Supports {timestamp}, {iso_timestamp}, {job_id}, {node_id}, {target_role} interpolation."
                    },
                    "guaranteed": {
                        "type": "boolean",
                        "description": "Mesh-coordinated delivery (future feature, currently ignored). Default false."
                    }
                },
                "required": ["schedule", "target_role", "payload"]
            }),
            class: "cron".into(),
            tool_markers: vec!["high_agency".into()],
        },
        AbstractToolRecord {
            tool_name: "cron.list".into(),
            description: "List all cron jobs registered on this hotel, including their schedule, \
                          target role, enabled state, and next fire time."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
            class: "cron".into(),
            tool_markers: Vec::new(),
        },
        AbstractToolRecord {
            tool_name: "cron.enable".into(),
            description: "Re-enable a previously disabled cron job. The job resumes firing on its \
                          schedule from the next occurrence after now."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "job_id": {
                        "type": "string",
                        "description": "The cron job UUID to enable."
                    }
                },
                "required": ["job_id"]
            }),
            class: "cron".into(),
            tool_markers: Vec::new(),
        },
        AbstractToolRecord {
            tool_name: "cron.disable".into(),
            description: "Disable a cron job without removing it. The job record is preserved and \
                          can be re-enabled with cron.enable."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "job_id": {
                        "type": "string",
                        "description": "The cron job UUID to disable."
                    }
                },
                "required": ["job_id"]
            }),
            class: "cron".into(),
            tool_markers: Vec::new(),
        },
        AbstractToolRecord {
            tool_name: "cron.remove".into(),
            description: "Permanently remove a cron job from the hotel. This cannot be undone. \
                          Use cron.disable instead if you may want to resume the job later."
                .into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "job_id": {
                        "type": "string",
                        "description": "The cron job UUID to remove."
                    }
                },
                "required": ["job_id"]
            }),
            class: "cron".into(),
            tool_markers: vec!["high_agency".into()],
        },
    ];

    for tool in &catalog {
        graph.upsert_abstract_tool(tool)?;
    }
    Ok(())
}

/// Seed the built-in abstract skill catalog into the context graph.
///
/// These skills are prompt-facing posture records first; their implied tool
/// grants stay intentionally narrow until the governed handoff and role
/// provisioning layers are fully wired.
fn seed_abstract_skill_catalog(graph: &GraphDomain) -> anyhow::Result<()> {
    let catalog = [
        AbstractSkillRecord {
            skill_name: "handoff.to_role".into(),
            description: "Handoff to a specialist role cleanly, explicitly transferring context, goals, and known constraints so the target can start work immediately without thrashing.".into(),
            implied_tools: vec!["session.status".into()],
            ..Default::default()
        },
        AbstractSkillRecord {
            skill_name: "handoff.back".into(),
            description: "Return a session from a specialist role back to the orchestrator with a concise summary of completed work, open questions, and the next recommended action.".into(),
            implied_tools: vec!["session.status".into()],
            ..Default::default()
        },
        AbstractSkillRecord {
            skill_name: "role.governance".into(),
            description: "Govern role definitions and placement for the current agent identity. \
                          Call role.list first to see the current roster, then use role.configure \
                          to update a role or role.set_home to pin a role to a specific hotel. \
                          Reason explicitly about purpose, capability posture, handoff behavior, \
                          and limits before proposing any change.".into(),
            implied_tools: vec![
                "session.status".into(),
                "role.list".into(),
                "role.configure".into(),
                "role.set_home".into(),
                "agent.configure".into(),
            ],
            ..Default::default()
        },
        AbstractSkillRecord {
            skill_name: "role.authoring".into(),
            description: "Author or revise a role using role.configure. Call role.list first to \
                          confirm the role exists. Gather missing inputs (role_name, toolset_profile, \
                          reasoning.purpose, reasoning.toolset_rationale, \
                          reasoning.handoff_posture_and_limits), construct the full payload, call \
                          role.configure, and optionally hand off into the updated role when the \
                          operator wants immediate use. Admin roles may use role.create_or_update \
                          to create brand-new roles not already in the roster.".into(),
            implied_tools: vec![
                "session.status".into(),
                "role.list".into(),
                "role.configure".into(),
                "handoff.to_role".into(),
            ],
            validation_state: ansible_mesh_core::graph::SkillValidationState::Validated,
            field_sources: serde_json::json!({
                "required_fields": [
                    "role_name",
                    "toolset_profile",
                    "reasoning.purpose",
                    "reasoning.toolset_rationale",
                    "reasoning.handoff_posture_and_limits"
                ],
                "optional_fields": [
                    "role_identity_addendum",
                    "role_manifest",
                    "inactive_ttl_seconds",
                    "iteration_cap",
                    "approval_policy",
                    "model_profile",
                    "context_window_policy",
                    "is_admin"
                ],
                "repo_skill_path": "skills/role-authoring/SKILL.md",
                "workflow_handoff": "role.configure",
                "format_example": {
                    "role_name": "researcher",
                    "toolset_profile": "research",
                    "reasoning": {
                        "purpose": "Bounded investigation role for a specific research task.",
                        "toolset_rationale": "The research profile keeps the tool surface narrow while preserving session continuity.",
                        "handoff_posture_and_limits": "Return concise findings to orchestrator custody when investigation completes."
                    }
                }
            }),
            ..Default::default()
        },
        AbstractSkillRecord {
            skill_name: "capability.request".into(),
            description: "Request a new capability from your orchestrator when your current role \
                          lacks a tool or skill needed to complete your work. Use skill.list first \
                          to confirm the capability does not already exist. Then hand back to the \
                          orchestrator with a structured capability request in the summary: what you \
                          need, why you need it, and what the subagent goal should look like. The \
                          orchestrator will register and assign the skill, then return you to this role.".into(),
            implied_tools: vec![
                "skill.list".into(),
                "handoff.back".into(),
            ],
            validation_state: ansible_mesh_core::graph::SkillValidationState::Validated,
            field_sources: serde_json::json!({
                "request_format": {
                    "summary": "CAPABILITY REQUEST: I need [capability name] to [reason]. Suggested skill: { skill_name, description, subagent_kind, goal, allowed_tools }. Please register and assign to my role '[role_name]', then return me to this session."
                },
                "repo_skill_path": "skills/capability-request/SKILL.md",
                "workflow": "skill.list → handoff.back with structured request → orchestrator registers + assigns → handoff.to_role returns"
            }),
            ..Default::default()
        },
        AbstractSkillRecord {
            skill_name: "skill.authoring".into(),
            description: "Author a new delegation skill for yourself. Identify a recurring pattern \
                          in your work, give it a name, write a goal template, declare what tools the \
                          subagent needs, then register it with skill.register and assign it to your \
                          current role with skill.assign. Registered skills persist across sessions \
                          and accumulate as part of your learned delegation vocabulary.".into(),
            implied_tools: vec![
                "skill.list".into(),
                "skill.register".into(),
                "skill.assign".into(),
            ],
            validation_state: ansible_mesh_core::graph::SkillValidationState::Validated,
            field_sources: serde_json::json!({
                "required_fields": [
                    "skill_name",
                    "description",
                    "subagent_kind",
                    "goal"
                ],
                "optional_fields": [
                    "allowed_tools",
                    "allowed_classes"
                ],
                "repo_skill_path": "skills/skill-authoring/SKILL.md",
                "workflow": "skill.list → skill.register → skill.assign",
                "format_example": {
                    "skill_name": "deep-search",
                    "description": "Delegate a multi-source research task to a focused subagent.",
                    "subagent_kind": "philote-worker",
                    "goal": "Research {{topic}} thoroughly using workspace and web tools. Return a structured summary with sources.",
                    "allowed_tools": ["workspace.read", "workspace.list"],
                    "allowed_classes": ["workspace", "utility"]
                }
            }),
            ..Default::default()
        },
        AbstractSkillRecord {
            skill_name: "delegate.to_peer".into(),
            description: "Cross-agent delegation: hand off a bounded task to another trusted peer agent on the mesh instead of changing roles. Best for leveraging a different identity, rather than shifting internal capabilities.".into(),
            implied_tools: vec!["delegate.to_peer".into()],
            ..Default::default()
        },
        AbstractSkillRecord {
            skill_name: "delegate.to_external_cognitive_peer".into(),
            description: "External delegation: hand off a bounded task to an unmanaged external system like Claude Code or Codex. Best when crossing deep security or execution boundaries where managed Philotic actors cannot natively reach.".into(),
            implied_tools: vec!["delegate.to_external_cognitive_peer".into()],
            ..Default::default()
        },
        AbstractSkillRecord {
            skill_name: "memory.fix".into(),
            description: "Diagnose and recover Muninn memory connectivity. Use when memory recall \
                          fails, vault registration is missing, or the Muninn endpoint is \
                          unreachable. Checks the configured vault endpoint, reports status, and \
                          guides the operator through recovery steps. Does NOT modify hotel config \
                          directly — surfaces the problem and a repair command for the operator."
                .into(),
            implied_tools: vec!["session.status".into(), "hotel.status".into(), "hotel.logs".into()],
            ..Default::default()
        },
        AbstractSkillRecord {
            skill_name: "training.admin".into(),
            description: "Run a voice training data review session: list uncorrected samples, \
                          apply corrections, check eligibility count, and export when ready. \
                          Guides an admin philote through the full capture-correct-export loop."
                .into(),
            implied_tools: vec![
                "training.list".into(),
                "training.correct".into(),
                "training.export".into(),
                "training.status".into(),
            ],
            ..Default::default()
        },
        AbstractSkillRecord {
            skill_name: "inference.scripting".into(),
            description: "Write, test, and iterate on Python inference scripts for local ML model \
                          runners. Understands the script contract: receives audio/image path and \
                          arguments via CLI, returns structured JSON on stdout. Uses bash.exec to \
                          validate scripts against real data before committing. Writes finalized \
                          scripts to the hotel's profile script directory so the Rust runner picks \
                          them up without a recompile. Pairs with asr.admin and vision.admin for \
                          full model provisioning."
                .into(),
            implied_tools: vec!["bash.exec".into()],
            field_sources: serde_json::json!({
                "contract": {
                    "stdout": "JSON object with at minimum a 'text' or 'result' key",
                    "stderr": "human-readable error messages only",
                    "exit_code": "0 on success, non-zero on failure"
                },
                "script_dir": "~/.philotic/<profile>/scripts/",
                "naming": "<model_slug>_infer.py  (e.g. parakeet_infer.py, falcon_ground_infer.py)",
                "workflow": "draft → bash.exec test → iterate → write to script_dir → verify via status tool"
            }),
            validation_state: ansible_mesh_core::graph::SkillValidationState::Validated,
            ..Default::default()
        },
        AbstractSkillRecord {
            skill_name: "asr.admin".into(),
            description: "Provision and maintain ASR model runners on this node. Covers the full \
                          lifecycle: check Python environment, write or refine the inference script \
                          (pairs with inference.scripting), run asr.setup to register the guest, \
                          verify with asr.status, and manage the training data pipeline \
                          (capture → correct → export). Use when onboarding a new ASR model or \
                          when transcription quality needs improvement."
                .into(),
            implied_tools: vec![
                "asr.setup".into(),
                "asr.status".into(),
                "training.list".into(),
                "training.correct".into(),
                "training.export".into(),
                "training.status".into(),
            ],
            ..Default::default()
        },
        AbstractSkillRecord {
            skill_name: "vision.admin".into(),
            description: "Provision and maintain vision model runners on this node. Covers: write \
                          or refine the inference script for a grounding or OCR model \
                          (pairs with inference.scripting), run vision.setup to register the guest, \
                          verify with vision.status, and invoke image.ground or image.ocr to \
                          validate output quality. Use when onboarding Falcon Perception, Falcon OCR, \
                          or any script-hosted vision model."
                .into(),
            implied_tools: vec![
                "vision.setup".into(),
                "vision.status".into(),
                "image.ground".into(),
                "image.ocr".into(),
            ],
            ..Default::default()
        },
        AbstractSkillRecord {
            skill_name: "observability.pipeline".into(),
            description: "Set up a structured event capture pipeline: create a local table, \
                          register a router-listener handler that writes matching inbound events \
                          into it, store a TableConfig node in your agent graph so the table appears \
                          in your cognitive envelope, and apply retention rules. Use when you want \
                          to capture recurring event streams (routing signals, transcriptions, \
                          sensor data) in a queryable flat store that persists across sessions."
                .into(),
            implied_tools: vec![
                "table.configure".into(),
                "table.add_listener".into(),
                "table.stats".into(),
                "table.rolloff".into(),
                "table.schema".into(),
                "graph.query".into(),
            ],
            ..Default::default()
        },
        AbstractSkillRecord {
            skill_name: "session.recover".into(),
            description: "Diagnose and recover from a stuck, failed, or confused session state. \
                          Classify the failure class (transient IPC, tool not found, context drift, \
                          loop detected, approval blocked, model confusion), choose the minimal \
                          recovery path, and resume work or escalate to the operator with a \
                          structured report. Never retry a failed tool more than once without \
                          reclassifying."
                .into(),
            implied_tools: vec![
                "session.status".into(),
                "hotel.status".into(),
                "hotel.logs".into(),
                "role.list".into(),
            ],
            validation_state: ansible_mesh_core::graph::SkillValidationState::Validated,
            field_sources: serde_json::json!({
                "failure_classes": [
                    "tool_not_found",
                    "transient_ipc",
                    "context_drift",
                    "loop_detected",
                    "approval_blocked",
                    "model_confusion"
                ],
                "repo_skill_path": "skills/session-recover/SKILL.md",
                "workflow": "session.status → classify → recover or escalate"
            }),
            ..Default::default()
        },
        AbstractSkillRecord {
            skill_name: "agent.initiate".into(),
            description: "Send a proactive, unsolicited message to a user or peer agent when \
                          a concrete system event authorizes it (cron job fired, threshold crossed, \
                          task completed, delegation received). Never initiate without a traceable \
                          trigger. Route via delegate.whisper for operator-facing outreach, or \
                          delegate.to_peer for inter-agent coordination."
                .into(),
            implied_tools: vec![
                "session.status".into(),
                "hotel.status".into(),
                "delegate.whisper".into(),
            ],
            validation_state: ansible_mesh_core::graph::SkillValidationState::Validated,
            field_sources: serde_json::json!({
                "required_fields": ["trigger_reason", "recipient", "message_intent"],
                "repo_skill_path": "skills/agent-initiate/SKILL.md",
                "workflow": "verify trigger → compose message → route"
            }),
            ..Default::default()
        },
        AbstractSkillRecord {
            skill_name: "cron.manage".into(),
            description: "Create, inspect, enable, disable, and remove scheduled cron jobs on \
                          the hotel. Use cron.list before registering to prevent duplicates. \
                          Choose schedules in 7-field cron format. Write payloads that the target \
                          role can act on without additional context injection. Use cron.disable \
                          instead of cron.remove when you may want to resume the job later."
                .into(),
            implied_tools: vec![
                "cron.register".into(),
                "cron.list".into(),
                "cron.enable".into(),
                "cron.disable".into(),
                "cron.remove".into(),
                "session.status".into(),
            ],
            validation_state: ansible_mesh_core::graph::SkillValidationState::Validated,
            field_sources: serde_json::json!({
                "required_fields": ["schedule", "target_role", "payload"],
                "repo_skill_path": "skills/cron-manage/SKILL.md",
                "workflow": "cron.list → compose job → cron.register → verify with cron.list"
            }),
            ..Default::default()
        },
        AbstractSkillRecord {
            skill_name: "context.synthesize".into(),
            description: "Restore session continuity at the start of a new conversation or after \
                          context compaction. Pull current state from hotel (session.status, \
                          hotel.status, role.list, skill.list) and form a working mental model \
                          before acting. Do not start substantive work until you have a current \
                          picture of the live system state. Complements Muninn recall: verify \
                          recalled facts against live hotel state before acting on them."
                .into(),
            implied_tools: vec![
                "session.status".into(),
                "hotel.status".into(),
                "role.list".into(),
                "skill.list".into(),
                "graph.query".into(),
            ],
            validation_state: ansible_mesh_core::graph::SkillValidationState::Validated,
            field_sources: serde_json::json!({
                "repo_skill_path": "skills/context-synthesize/SKILL.md",
                "workflow": "session.status → hotel.status → role.list → skill.list → synthesize"
            }),
            ..Default::default()
        },
        AbstractSkillRecord {
            skill_name: "profile.manage".into(),
            description: "Understand, audit, and grow your own capability profile. \
                          Call role.list to see toolset profiles and skill.list to see assigned \
                          skills. When you've identified a recurring delegation pattern (3+ times), \
                          register a new skill with skill.register and assign it with skill.assign. \
                          When you need a tool not in your allowed_tools, use capability.request \
                          to ask the orchestrator to expand your profile — do not attempt to modify \
                          allowed_tools directly. Update role_identity_addendum via role.configure \
                          when your responsibilities have materially changed."
                .into(),
            implied_tools: vec![
                "role.list".into(),
                "skill.list".into(),
                "skill.register".into(),
                "skill.assign".into(),
                "role.configure".into(),
                "session.status".into(),
            ],
            validation_state: ansible_mesh_core::graph::SkillValidationState::Validated,
            field_sources: serde_json::json!({
                "repo_skill_path": "skills/profile-manage/SKILL.md",
                "workflow": "role.list → skill.list → identify gap → register or request → assign"
            }),
            ..Default::default()
        },
        AbstractSkillRecord {
            skill_name: "graph.knowledge".into(),
            description: "Create and manage a personal knowledge graph in the agent graph store. \
                          Use graph.create to provision a named partition (or your default one keyed \
                          to your agent_id), graph.query to build and traverse a typed node/edge schema, \
                          graph.list to audit partitions, graph.drop to remove one, and graph.grant_access \
                          to share a partition with a peer. Treat your graph as a persistent structured \
                          memory: store entities, decisions, tasks, relationships, and any recurring \
                          domain knowledge that would otherwise be lost across sessions."
                .into(),
            implied_tools: vec![
                "graph.create".into(),
                "graph.query".into(),
                "graph.list".into(),
                "graph.drop".into(),
                "graph.grant_access".into(),
            ],
            validation_state: ansible_mesh_core::graph::SkillValidationState::Validated,
            field_sources: serde_json::json!({
                "workflow": "graph.list → graph.create (if needed) → graph.query CREATE nodes → graph.query MATCH to verify → maintain",
                "cypher_patterns": {
                    "create_partition": "graph.create { graph_id: 'my-workspace' }",
                    "create_node": "CREATE (n:Task {id: 'task-1', name: 'Do the thing', status: 'open'})",
                    "create_edge": "CREATE (a:Task {id: 'task-1'})-[:BLOCKS]->(b:Task {id: 'task-2'})",
                    "read_all": "MATCH (n) RETURN n",
                    "read_by_label": "MATCH (n:Task) RETURN n",
                    "read_by_id": "MATCH (n:Task {id: 'task-1'}) RETURN n",
                    "delete_node": "MATCH (n {id: 'task-1'}) DELETE n",
                    "delete_node_with_label": "MATCH (n:Task {id: 'task-1'}) DETACH DELETE n",
                    "delete_edge": "MATCH ()-[r {id: 'edge-id'}]-() DELETE r"
                },
                "conventions": {
                    "id_field": "Always set an explicit 'id' property on nodes and edges — it is the primary key.",
                    "partition_default": "Omit graph_id to use your own partition (keyed to your agent_id).",
                    "labels": "Use PascalCase labels (Task, Decision, Person, Concept).",
                    "edge_labels": "Use SCREAMING_SNAKE_CASE edge labels (BLOCKS, DEPENDS_ON, AUTHORED_BY)."
                }
            }),
            ..Default::default()
        },
    ];

    for skill in &catalog {
        graph.upsert_abstract_skill(skill)?;
    }
    Ok(())
}

fn seed_toolset_profiles(graph: &GraphDomain) -> anyhow::Result<()> {
    let profiles = [
        ToolsetProfileRecord {
            profile_name: "orchestrator".into(),
            allowed_tools: vec![
                "session.status".into(),
                "hotel.status".into(),
                "hotel.logs".into(),
                "echo".into(),
                "agent.configure".into(),
                "role.configure".into(),
                "role.list".into(),
                "role.set_home".into(),
                "rule.propose".into(),
                "routing.policy.propose".into(),
                "mcp.provision".into(),
                "mcp.revoke".into(),
                "desktop.observe".into(),
                "skill.register".into(),
                "skill.list".into(),
                "skill.assign".into(),
                "subagent.spawn".into(),
                "workspace.list".into(),
                "workspace.read".into(),
                "bash.exec".into(),
                "delegate.whisper".into(),
                "memory.recall".into(),
                "memory.remember".into(),
                "agent.graph.read".into(),
                "agent.graph.write".into(),
                "agent.graph.recall".into(),
                "graph.query".into(),
                "graph.create".into(),
                "graph.list".into(),
                "graph.drop".into(),
                "graph.grant_access".into(),
                "table.configure".into(),
                "table.query".into(),
                "table.insert".into(),
                "table.rolloff".into(),
                "table.stats".into(),
                "table.schema".into(),
                "table.add_listener".into(),
                "cron.register".into(),
                "cron.list".into(),
                "cron.enable".into(),
                "cron.disable".into(),
                "cron.remove".into(),
            ],
            allowed_classes: vec!["session".into(), "utility".into(), "config".into(), "memory".into(), "graph".into(), "agent_graph".into(), "table".into(), "cron".into(), "mcp".into(), "desktop".into()],
            allowed_skills: vec![
                "handoff.to_role".into(),
                "handoff.back".into(),
                "role.governance".into(),
                "role.authoring".into(),
                "skill.authoring".into(),
                "memory.fix".into(),
                "delegate.to_peer".into(),
                "delegate.to_external_cognitive_peer".into(),
                "observability.pipeline".into(),
                "session.recover".into(),
                "agent.initiate".into(),
                "cron.manage".into(),
                "context.synthesize".into(),
                "profile.manage".into(),
                "graph.knowledge".into(),
            ],
            description: Some("Default orchestrator role profile.".into()),
        },
        ToolsetProfileRecord {
            profile_name: "codex".into(),
            allowed_tools: vec![
                "session.status".into(),
                "echo".into(),
                "skill.list".into(),
                "role.list".into(),
                "workspace.list".into(),
                "workspace.read".into(),
                "graph.query".into(),
                "graph.create".into(),
                "graph.list".into(),
            ],
            allowed_classes: vec!["session".into(), "utility".into(), "workspace".into()],
            allowed_skills: vec![
                "handoff.back".into(),
                "capability.request".into(),
                "context.synthesize".into(),
                "session.recover".into(),
                "graph.knowledge".into(),
            ],
            description: Some("Codex specialist role profile — workspace read access.".into()),
        },
        ToolsetProfileRecord {
            profile_name: "research".into(),
            allowed_tools: vec![
                "session.status".into(),
                "echo".into(),
                "skill.list".into(),
                "role.list".into(),
                "graph.query".into(),
                "graph.create".into(),
                "graph.list".into(),
            ],
            allowed_classes: vec!["session".into(), "utility".into()],
            allowed_skills: vec![
                "handoff.back".into(),
                "capability.request".into(),
                "context.synthesize".into(),
                "session.recover".into(),
                "graph.knowledge".into(),
            ],
            description: Some("Research specialist role profile — minimal tool surface.".into()),
        },
        ToolsetProfileRecord {
            profile_name: "utility".into(),
            allowed_tools: vec![
                "session.status".into(),
                "echo".into(),
                "skill.list".into(),
                "role.list".into(),
                "graph.query".into(),
                "graph.create".into(),
                "graph.list".into(),
            ],
            allowed_classes: vec!["session".into(), "utility".into()],
            allowed_skills: vec![
                "capability.request".into(),
                "context.synthesize".into(),
                "session.recover".into(),
            ],
            description: Some("Bare utility profile — session and echo only.".into()),
        },
        ToolsetProfileRecord {
            profile_name: "admin".into(),
            allowed_tools: vec![
                "session.status".into(),
                "hotel.status".into(),
                "hotel.logs".into(),
                "echo".into(),
                "agent.configure".into(),
                "skill.register".into(),
                "skill.list".into(),
                "skill.assign".into(),
                "skill.revoke".into(),
                "subagent.spawn".into(),
                "role.configure".into(),
                "role.create_or_update".into(),
                "role.list".into(),
                "role.set_home".into(),
                "rule.propose".into(),
                "routing.policy.propose".into(),
                "mcp.provision".into(),
                "mcp.revoke".into(),
                "desktop.observe".into(),
                "workspace.list".into(),
                "workspace.read".into(),
                "bash.exec".into(),
                "delegate.whisper".into(),
                "memory.recall".into(),
                "memory.remember".into(),
                "agent.graph.read".into(),
                "agent.graph.write".into(),
                "training.list".into(),
                "training.correct".into(),
                "training.export".into(),
                "training.status".into(),
                "asr.setup".into(),
                "asr.status".into(),
                "graph.query".into(),
                "graph.create".into(),
                "graph.list".into(),
                "graph.drop".into(),
                "graph.grant_access".into(),
                "table.configure".into(),
                "table.query".into(),
                "table.insert".into(),
                "table.rolloff".into(),
                "table.stats".into(),
                "table.schema".into(),
                "table.add_listener".into(),
                "cron.register".into(),
                "cron.list".into(),
                "cron.enable".into(),
                "cron.disable".into(),
                "cron.remove".into(),
            ],
            allowed_classes: vec![
                "session".into(),
                "utility".into(),
                "config".into(),
                "memory".into(),
                "shell".into(),
                "training".into(),
                "asr".into(),
                "graph".into(),
                "agent_graph".into(),
                "table".into(),
                "cron".into(),
                "mcp".into(),
                "desktop".into(),
            ],
            allowed_skills: vec![
                "skill.crafting".into(),
                "handoff.to_role".into(),
                "handoff.back".into(),
                "role.governance".into(),
                "role.authoring".into(),
                "delegate.to_peer".into(),
                "delegate.to_external_cognitive_peer".into(),
                "training.admin".into(),
                "inference.scripting".into(),
                "asr.admin".into(),
                "vision.admin".into(),
                "session.recover".into(),
                "agent.initiate".into(),
                "cron.manage".into(),
                "context.synthesize".into(),
                "profile.manage".into(),
            ],
            description: Some(
                "Admin role profile — full skill crafting, role governance, training data authority, ASR provisioning, vision model provisioning, and cron scheduling.".into(),
            ),
        },
        ToolsetProfileRecord {
            profile_name: "architect".into(),
            allowed_tools: vec![
                "session.status".into(),
                "hotel.status".into(),
                "hotel.logs".into(),
                "echo".into(),
                "skill.list".into(),
                "role.list".into(),
                "workspace.list".into(),
                "workspace.read".into(),
                "bash.exec".into(),
                "memory.recall".into(),
                "memory.remember".into(),
                "agent.graph.read".into(),
                "graph.query".into(),
                "graph.create".into(),
                "graph.list".into(),
            ],
            allowed_classes: vec!["session".into(), "utility".into(), "workspace".into(), "memory".into(), "graph".into(), "agent_graph".into()],
            allowed_skills: vec![
                "handoff.back".into(),
                "capability.request".into(),
                "memory.fix".into(),
                "context.synthesize".into(),
                "session.recover".into(),
            ],
            description: Some(
                "Architect specialist role profile — systems, infrastructure, debugging. \
                 bash.exec requires operator approval."
                    .into(),
            ),
        },
        ToolsetProfileRecord {
            profile_name: "virtuoso".into(),
            allowed_tools: vec![
                "session.status".into(),
                "echo".into(),
                "skill.list".into(),
                "role.list".into(),
                "graph.query".into(),
                "graph.create".into(),
                "graph.list".into(),
            ],
            allowed_classes: vec!["session".into(), "utility".into()],
            allowed_skills: vec![
                "handoff.back".into(),
                "context.synthesize".into(),
                "session.recover".into(),
            ],
            description: Some(
                "Virtuoso specialist role profile — creative and expressive. \
                 Minimal tools, focused on reflection and lyrical output."
                    .into(),
            ),
        },
    ];

    for profile in &profiles {
        graph.upsert_toolset_profile(profile)?;
    }
    Ok(())
}

fn seed_skill_crafting(graph: &GraphDomain) -> anyhow::Result<()> {
    use ansible_mesh_core::graph::{AbstractSkillRecord, SkillValidationState};
    let skill = AbstractSkillRecord {
        skill_name: "skill.crafting".into(),
        description: "Grants access to skill management tools — register, list, assign, and revoke skills across roles. Intended for admin role use.".into(),
        implied_tools: vec![
            "skill.register".into(),
            "skill.list".into(),
            "skill.assign".into(),
            "skill.revoke".into(),
            "subagent.spawn".into(),
            "role.create_or_update".into(),
        ],
        skill_markers: Vec::new(),
        validation_state: SkillValidationState::Validated,
        source_snapshot: None,
        field_sources: serde_json::json!({}),
    };
    graph.upsert_abstract_skill(&skill)?;
    Ok(())
}

/// Governance document seeded for every agent's orchestrator role.
/// This is the agent's self-description — it tells the agent what it IS, what it can do,
/// what requires approval, and how to delegate. Agents can update this via role.configure.
const ORCHESTRATOR_MANIFEST: &str = "\
You are in orchestrator posture — the sovereign identity layer of your agent.

Responsibilities:
- Govern your own role definitions and delegation vocabulary.
- Configure your approval policy and operational bindings.
- Delegate sustained specialist work to configured roles via handoff.to_role.
- Oversee subagents spawned for parallel, bounded tasks.
- Return delegated work to orchestrator custody when roles complete.
- Author new delegation skills when you identify recurring patterns in your work.
- Assign registered skills to your roles to expand your capabilities over time.

## Role management

Your roles are pre-configured and persist in the hotel database. They survive restarts.
- Use role.list to see your full role roster, their toolset profiles, and readiness state.
- Use role.configure to update an existing role's manifest, toolset profile, or loop config.
- Use role.set_home to pin a role to a specific hotel (or clear the pin to run on this hotel).
- Do NOT create new roles speculatively — the roster is set by the operator. If you need a new role, surface the request explicitly and wait for operator approval.
- After any role update, hand off only when the operator asked to use it immediately.

## role.create_or_update — hard constraints

This tool writes a role DEFINITION. It is NOT an activation step.
- NEVER call role.create_or_update before handoff.to_role or delegate.whisper.
- NEVER call role.create_or_update because a user asked to switch roles or talk to a role.
- If a role already exists, use handoff.to_role directly — no prior create_or_update needed.
- Only call role.create_or_update when the operator explicitly asks you to create or change a role's definition (manifest, toolset, identity). A voice note asking to switch roles or talk to a role is NOT such a request.

Rules:
- Do not bypass the approval gate; if a tool requires operator approval, surface it clearly.
- Keep soul_text and core identity stable — those changes require operator approval.
- Use handoff.to_role for sustained specialist work; use subagent.spawn for parallel bounded tasks.
- When you notice a pattern you have delegated 3 or more times, consider registering it as a named skill.
- skill.assign only works on your own roles — you cannot grant skills to other agents.
- When a sub-role hands back with a CAPABILITY REQUEST in the summary: register the skill with skill.register, assign it to the requesting role with skill.assign, then use handoff.to_role to return them to that role so they can continue with the new capability.
- Never ignore a capability request from a sub-role — either grant it or explain why not before returning them.

Tool preference:
- Always prefer Philotic-native tools (hotel.status, hotel.logs, role.list, workspace.read, session.status) over shell commands.
- Use role.list before any role governance action to confirm the current roster.
- Use hotel.status to inspect running guests and agent identities before asking the operator for that information.
- Use hotel.logs to tail the aiua log for recent events before reaching for bash.exec.
- Use bash.exec only when no native tool can accomplish the task, and only after stating explicitly why bash is necessary.
- Never call a tool speculatively or for diagnostic purposes unless the operator has asked you to.
- If no tool is needed to answer a question, respond directly — do not call a tool just because one is available.

Approval posture:
- Governance tools (role.configure, role.list, skill.register, skill.assign, skill.list, handoff.to_role, handoff.back) run without per-action approval.
- Self-configuration (agent.configure for approval_policy, profile, bindings) runs without approval.
- Shell execution (bash.exec) and core identity field changes require operator approval.";

/// Seeds an orchestrator RoleIncarnationRecord for each agent profile.
///
/// This ensures every agent has a fully populated toolset and manifest from the first session
/// turn, breaking the chicken-and-egg where role.configure requires tools that only appear
/// after a role exists.
fn seed_orchestrator_roles(graph: &GraphDomain, profiles: &[AgentProfile]) -> anyhow::Result<()> {
    for profile in profiles {
        // Preserve operator-customized fields (turn_loop_config, role_identity_addendum)
        // from the existing record so that `aiua load` doesn't wipe them.
        let existing = graph
            .get_role_incarnation(&profile.agent_id, "orchestrator")
            .ok()
            .flatten();
        let turn_loop_config = profile
            .orchestrator_turn_loop_config
            .clone()
            .or_else(|| existing.as_ref().map(|r| r.turn_loop_config.clone()))
            .unwrap_or_default();
        let role_identity_addendum = existing.and_then(|r| r.role_identity_addendum);

        let record = ansible_mesh_core::graph::RoleIncarnationRecord {
            agent_id: profile.agent_id.clone(),
            role_name: "orchestrator".into(),
            guest_id: format!("{}:orchestrator", profile.agent_id),
            toolset_profile: "orchestrator".into(),
            role_identity_addendum,
            role_manifest: Some(ORCHESTRATOR_MANIFEST.into()),
            is_admin: profile.is_admin,
            readiness_state: ansible_mesh_core::graph::RoleReadinessState::Configured,
            inactive_ttl_seconds: None,
            turn_loop_config,
            home_node: None,
        };
        // Always upsert — the hotel seed is the canonical source for the orchestrator manifest.
        // The manifest is institutional (same rules for all agents), not per-agent customizable.
        // To change the manifest, update this seed and restart the hotel.
        graph.upsert_role_incarnation(&record)?;
    }
    Ok(())
}

fn enable_guest_test_overrides(
    graph: &GraphDomain,
    hotel_name: &str,
    test: StartupTest,
) -> Result<()> {
    let mut guests = graph.list_guests(hotel_name, false)?;
    if guests.is_empty() {
        return Ok(());
    }

    match test {
        StartupTest::GraphRoundTrip => {}
        StartupTest::TextRoundTrip => {
            for guest in &mut guests {
                if guest.role != "model" {
                    continue;
                }

                let mut config: serde_json::Value =
                    serde_json::from_str(&guest.config_json).unwrap_or_default();
                let env = config
                    .as_object_mut()
                    .and_then(|obj| obj.get_mut("env"))
                    .and_then(serde_json::Value::as_object_mut)
                    .context("guest config missing env object")?;
                env.insert(
                    "PHILOTIC_MODEL_ROUTER_STUB_RESPONSE".into(),
                    serde_json::Value::String(STARTUP_TEST_TEXT_REPLY.into()),
                );
                guest.config_json = config.to_string();
            }
        }
        StartupTest::CognitiveRoundTrip => {
            graph.set_config_value(
                "gemini_api_key",
                &serde_json::Value::String(STARTUP_TEST_GEMINI_API_KEY.into()).to_string(),
            )?;

            // Clear vault registry so the startup test uses NullMemoryEngine.
            // This prevents philote from trying to activate against a real Muninn
            // instance when the hotel was seeded from a production mesh-config.json.
            graph.set_config_value("vault_registry", &serde_json::json!([]).to_string())?;

            for guest in &mut guests {
                if guest.role != "model" {
                    continue;
                }

                let mut config: serde_json::Value =
                    serde_json::from_str(&guest.config_json).unwrap_or_default();
                let env = config
                    .as_object_mut()
                    .and_then(|obj| obj.get_mut("env"))
                    .and_then(serde_json::Value::as_object_mut)
                    .context("guest config missing env object")?;
                env.remove("PHILOTIC_MODEL_ROUTER_STUB_RESPONSE");
                env.insert(
                    "PHILOTIC_GEMINI_BASE_URL".into(),
                    serde_json::Value::String(startup_test_gemini_base_url(hotel_name)),
                );
                guest.config_json = config.to_string();
            }
        }
        StartupTest::GeminiOAuthRoundTrip => {
            let startup_secret_ref = store_secret(
                graph,
                SecretInput {
                    secret_kind: "gemini-startup-oauth-token".into(),
                    scope: "startup-test".into(),
                    allowed_roles: vec!["model".into()],
                    allowed_guests: Vec::new(),
                    plaintext: "startup-test-oauth-bearer".into(),
                },
            )?;

            for guest in &mut guests {
                if guest.role != "model" {
                    continue;
                }

                let mut config: serde_json::Value =
                    serde_json::from_str(&guest.config_json).unwrap_or_default();
                let env = config
                    .as_object_mut()
                    .and_then(|obj| obj.get_mut("env"))
                    .and_then(serde_json::Value::as_object_mut)
                    .context("guest config missing env object")?;
                env.insert(
                    "PHILOTIC_GEMINI_BASE_URL".into(),
                    serde_json::Value::String(startup_test_gemini_base_url(hotel_name)),
                );
                env.insert(
                    "PHILOTIC_GEMINI_OAUTH_ACCESS_TOKEN_REF".into(),
                    serde_json::Value::String(startup_secret_ref.clone()),
                );
                env.insert(
                    "PHILOTIC_GEMINI_OAUTH_PROJECT_ID".into(),
                    serde_json::Value::String("startup-test-project".into()),
                );
                guest.config_json = config.to_string();
            }
        }
        StartupTest::VoiceSample => {
            for guest in &mut guests {
                if guest.role != "model.elevenlabs" {
                    continue;
                }

                let mut config: serde_json::Value =
                    serde_json::from_str(&guest.config_json).unwrap_or_default();
                let env = config
                    .as_object_mut()
                    .and_then(|obj| obj.get_mut("env"))
                    .and_then(serde_json::Value::as_object_mut)
                    .context("guest config missing env object")?;
                env.insert(
                    "PHILOTIC_MODEL_CONTROLLER_INLINE_AUDIO".into(),
                    serde_json::Value::String("1".into()),
                );
                guest.config_json = config.to_string();
            }
        }
        StartupTest::TelegramRoundTrip | StartupTest::TelegramPollLease => {
            graph.set_config_value(
                "telegram_bot_token",
                &serde_json::Value::String(STARTUP_TEST_TELEGRAM_TOKEN.into()).to_string(),
            )?;
            if matches!(test, StartupTest::TelegramRoundTrip) {
                graph.set_config_value(
                    "gemini_api_key",
                    &serde_json::Value::String(STARTUP_TEST_GEMINI_API_KEY.into()).to_string(),
                )?;
            }

            let telegram_api_base_url = startup_test_telegram_api_base_url(hotel_name);
            let gemini_api_base_url = startup_test_gemini_api_base_url(hotel_name);
            let blob_base_url = startup_test_blob_base_url(hotel_name);

            for guest in &mut guests {
                let mut config: serde_json::Value =
                    serde_json::from_str(&guest.config_json).unwrap_or_default();
                let env = config
                    .as_object_mut()
                    .and_then(|obj| obj.get_mut("env"))
                    .and_then(serde_json::Value::as_object_mut)
                    .context("guest config missing env object")?;

                if guest.role == "model" {
                    env.remove("PHILOTIC_MODEL_ROUTER_STUB_RESPONSE");
                    env.insert(
                        "PHILOTIC_GEMINI_BASE_URL".into(),
                        serde_json::Value::String(gemini_api_base_url.clone()),
                    );
                }

                if guest.role == "membrane" {
                    env.insert(
                        "PHILOTIC_TELEGRAM_API_BASE_URL".into(),
                        serde_json::Value::String(telegram_api_base_url.clone()),
                    );
                    env.insert(
                        "PHILOTIC_TELEGRAM_FILE_API_BASE_URL".into(),
                        serde_json::Value::String(telegram_api_base_url.clone()),
                    );
                    env.insert(
                        "PHILOTIC_BLOB_BASE_URL".into(),
                        serde_json::Value::String(blob_base_url.clone()),
                    );
                }

                guest.config_json = config.to_string();
            }

            if matches!(test, StartupTest::TelegramPollLease) {
                let hotel = default_hotel_record(hotel_name);
                let primary_membrane = guests
                    .iter()
                    .find(|guest| guest.role == "membrane")
                    .cloned()
                    .context("telegram poll-lease startup test missing primary membrane guest")?;
                let primary_config: serde_json::Value =
                    serde_json::from_str(&primary_membrane.config_json).unwrap_or_default();
                let primary_env = primary_config
                    .get("env")
                    .and_then(serde_json::Value::as_object)
                    .context("primary membrane config missing env object")?;
                let target_agent_id = primary_env
                    .get("PHILOTIC_TARGET_AGENT_ID")
                    .and_then(serde_json::Value::as_str)
                    .context("primary membrane config missing PHILOTIC_TARGET_AGENT_ID")?;
                let standby_guest_id = format!("{}-standby", primary_membrane.guest_id);
                guests.push(GuestRecord {
                    hotel_name: hotel_name.to_string(),
                    guest_id: standby_guest_id.clone(),
                    role: "membrane".into(),
                    config_json: serde_json::json!({
                        "command": "membrane-telegram",
                        "args": [],
                        "env": {
                            "PHILOTIC_HOTEL_SOCKET": hotel.ipc_socket_path.clone(),
                            "PHILOTIC_NODE_ID": hotel.capabilities.node_id.clone(),
                            "PHILOTIC_BLOB_BASE_URL": blob_base_url,
                            "PHILOTIC_GUEST_ID": standby_guest_id,
                            "PHILOTIC_TARGET_AGENT_ID": target_agent_id,
                            "PHILOTIC_TELEGRAM_BOT_TOKEN_KEY": "telegram_bot_token",
                            "PHILOTIC_TELEGRAM_API_BASE_URL": telegram_api_base_url.clone(),
                            "PHILOTIC_TELEGRAM_FILE_API_BASE_URL": telegram_api_base_url.clone()
                        }
                    })
                    .to_string(),
                    is_active: true,
                    active_pid: None,
                    last_active_at: None,
                });
            }
        }
    }

    graph.seed_guests(hotel_name, &guests)?;
    Ok(())
}

fn startup_test_gemini_base_url(hotel_name: &str) -> String {
    startup_test_gemini_api_base_url(hotel_name)
}

#[derive(Clone)]
struct FakeGeminiOAuthState {
    expected_reply: String,
    required_prompt_substrings: Vec<String>,
    require_bearer_auth: bool,
}

fn spawn_fake_gemini_server(
    hotel_name: &str,
    expected_reply: String,
    required_prompt_substrings: Vec<String>,
    require_bearer_auth: bool,
) -> tokio::task::JoinHandle<()> {
    let bind_addr: SocketAddr = format!("127.0.0.1:{}", startup_test_gemini_port(hotel_name))
        .parse()
        .expect("startup fake Gemini socket address should parse");

    let app = Router::new()
        .fallback(any(fake_gemini_handler))
        .with_state(FakeGeminiOAuthState {
            expected_reply,
            required_prompt_substrings,
            require_bearer_auth,
        });

    tokio::spawn(async move {
        let listener = match tokio::net::TcpListener::bind(bind_addr).await {
            Ok(listener) => listener,
            Err(err) => {
                warn!(
                    "Failed to bind fake Gemini startup server at {}: {}",
                    bind_addr, err
                );
                return;
            }
        };

        if let Err(err) = axum::serve(listener, app).await {
            warn!("Fake Gemini startup server exited with error: {}", err);
        }
    })
}

async fn fake_gemini_handler(
    State(state): State<FakeGeminiOAuthState>,
    request: Request<Body>,
) -> Response {
    if request.method() != Method::POST {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            Json(serde_json::json!({
                "error": { "message": "expected POST" }
            })),
        )
            .into_response();
    }

    if !request.uri().path().contains(":generateContent") {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": { "message": "unexpected path" }
            })),
        )
            .into_response();
    }

    let uses_api_key_query = request
        .uri()
        .query()
        .map(|query| query.contains("key="))
        .unwrap_or(false);

    if state.require_bearer_auth {
        if uses_api_key_query {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": { "message": "startup oauth smoke requires bearer auth, not api-key query auth" }
                })),
            )
                .into_response();
        }

        let headers = request.headers();
        let auth_header = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !auth_header.starts_with("Bearer ") || auth_header.trim() == "Bearer" {
            return (
                StatusCode::UNAUTHORIZED,
                Json(serde_json::json!({
                    "error": { "message": "missing bearer auth" }
                })),
            )
                .into_response();
        }

        let project_header = headers
            .get("x-goog-user-project")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if project_header.trim().is_empty() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": { "message": "missing x-goog-user-project header" }
                })),
            )
                .into_response();
        }
    } else if !uses_api_key_query {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": { "message": "startup cognitive smoke expected api-key query auth" }
            })),
        )
            .into_response();
    }

    let body = match to_bytes(request.into_body(), usize::MAX).await {
        Ok(body) => body,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": { "message": format!("failed to read request body: {}", err) }
                })),
            )
                .into_response();
        }
    };
    let payload: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
    let prompt = payload
        .get("contents")
        .and_then(serde_json::Value::as_array)
        .and_then(|contents| contents.first())
        .and_then(|content| content.get("parts"))
        .and_then(serde_json::Value::as_array)
        .and_then(|parts| parts.first())
        .and_then(|part| part.get("text"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if !prompt.contains(&state.expected_reply) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": { "message": "prompt did not contain expected startup reply token" }
            })),
        )
            .into_response();
    }

    if let Some(missing) = state
        .required_prompt_substrings
        .iter()
        .find(|needle| !prompt.contains(needle.as_str()))
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": { "message": format!("prompt missing required startup marker {:?}", missing) }
            })),
        )
            .into_response();
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "candidates": [{
                "content": {
                    "parts": [{ "text": state.expected_reply }]
                }
            }]
        })),
    )
        .into_response()
}

fn fake_telegram_router(state: Arc<FakeTelegramState>) -> Router {
    Router::new()
        .route("/bot:token/getUpdates", get(fake_telegram_get_updates))
        .route("/bot:token/getFile", get(fake_telegram_get_file))
        .route("/file/*rest", get(fake_telegram_download_file))
        .route("/bot:token/sendMessage", post(fake_telegram_send_message))
        .route("/bot:token/sendVoice", post(fake_telegram_send_media))
        .route("/bot:token/sendAudio", post(fake_telegram_send_media))
        .route(
            "/bot:token/deleteMyCommands",
            post(fake_telegram_delete_my_commands),
        )
        .route(
            "/bot:token/setMyCommands",
            post(fake_telegram_set_my_commands),
        )
        .with_state(state)
}

async fn fake_telegram_get_updates(
    AxumPath(_token): AxumPath<String>,
    Query(query): Query<TelegramGetUpdatesQuery>,
    State(state): State<Arc<FakeTelegramState>>,
) -> impl IntoResponse {
    state.get_updates_calls.fetch_add(1, Ordering::Relaxed);
    let offset = query.offset.unwrap_or(0);
    let mut updates = state.updates.lock().expect("updates lock");
    let mut result = Vec::new();
    while let Some(update) = updates.front() {
        let update_id = update
            .get("update_id")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or_default();
        if update_id < offset {
            updates.pop_front();
            continue;
        }
        result.push(update.clone());
        updates.pop_front();
    }

    Json(serde_json::json!({
        "ok": true,
        "result": result
    }))
}

async fn fake_telegram_get_file(
    AxumPath(_token): AxumPath<String>,
    Query(query): Query<HashMap<String, String>>,
    State(state): State<Arc<FakeTelegramState>>,
) -> impl IntoResponse {
    let Some(file_id) = query.get("file_id") else {
        return Json(serde_json::json!({
            "ok": false,
            "description": "file_id is required"
        }));
    };

    let files = state.files.lock().expect("files lock");
    let Some(file) = files.get(file_id) else {
        return Json(serde_json::json!({
            "ok": false,
            "description": "file not found"
        }));
    };

    Json(serde_json::json!({
        "ok": true,
        "result": {
            "file_id": file_id,
            "file_path": file.file_path,
            "file_size": file.bytes.len()
        }
    }))
}

async fn fake_telegram_download_file(
    AxumPath(rest): AxumPath<String>,
    State(state): State<Arc<FakeTelegramState>>,
) -> impl IntoResponse {
    let Some((_, file_path)) = rest.split_once('/') else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    let files = state.files.lock().expect("files lock");
    let Some(file) = files.values().find(|file| file.file_path == file_path) else {
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };

    (
        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
        file.bytes.clone(),
    )
        .into_response()
}

async fn fake_telegram_send_message(
    AxumPath(_token): AxumPath<String>,
    State(state): State<Arc<FakeTelegramState>>,
    Json(payload): Json<TelegramSendMessageRequest>,
) -> impl IntoResponse {
    state
        .sent_messages
        .lock()
        .expect("sent messages lock")
        .push(serde_json::json!({
            "chat_id": payload.chat_id,
            "text": payload.text,
            "parse_mode": payload.parse_mode,
        }));

    Json(serde_json::json!({
        "ok": true,
        "result": {
            "message_id": 1
        }
    }))
}

async fn fake_telegram_send_media(
    AxumPath(_token): AxumPath<String>,
    State(state): State<Arc<FakeTelegramState>>,
    request: axum::extract::Request,
) -> impl IntoResponse {
    let headers = request.headers().clone();
    let bytes = axum::body::to_bytes(request.into_body(), usize::MAX)
        .await
        .unwrap_or_default();

    state
        .sent_media
        .lock()
        .expect("sent media lock")
        .push(serde_json::json!({
            "content_type": headers
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default(),
            "bytes_len": bytes.len(),
        }));

    Json(serde_json::json!({
        "ok": true,
        "result": {
            "message_id": 1
        }
    }))
}

async fn fake_telegram_set_my_commands(
    AxumPath(_token): AxumPath<String>,
    State(state): State<Arc<FakeTelegramState>>,
    Json(payload): Json<TelegramSetMyCommandsRequest>,
) -> impl IntoResponse {
    let commands = serde_json::to_value(payload.commands).unwrap_or(serde_json::Value::Null);
    state
        .registered_commands
        .lock()
        .expect("registered commands lock")
        .push(commands);

    Json(serde_json::json!({
        "ok": true,
        "result": true
    }))
}

async fn fake_telegram_delete_my_commands(
    AxumPath(_token): AxumPath<String>,
    State(state): State<Arc<FakeTelegramState>>,
) -> impl IntoResponse {
    let mut deleted = state
        .deleted_command_syncs
        .lock()
        .expect("deleted command syncs lock");
    *deleted += 1;

    Json(serde_json::json!({
        "ok": true,
        "result": true
    }))
}

fn fake_gemini_router(state: Arc<FakeGeminiMediaState>) -> Router {
    Router::new()
        .route("/v1beta/models/*rest", post(fake_gemini_generate_content))
        .with_state(state)
}

async fn fake_gemini_generate_content(
    AxumPath(_rest): AxumPath<String>,
    State(state): State<Arc<FakeGeminiMediaState>>,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    state
        .requests
        .lock()
        .expect("fake gemini requests lock")
        .push(payload.clone());

    let reply_text = fake_gemini_reply_text(&payload);
    Json(serde_json::json!({
        "candidates": [{
            "content": {
                "parts": [{
                    "text": reply_text
                }]
            }
        }]
    }))
}

fn fake_gemini_reply_text(payload: &serde_json::Value) -> &'static str {
    let parts = payload
        .get("contents")
        .and_then(serde_json::Value::as_array)
        .and_then(|contents| contents.first())
        .and_then(|content| content.get("parts"))
        .and_then(serde_json::Value::as_array);

    let Some(parts) = parts else {
        return STARTUP_TEST_TELEGRAM_TEXT_REPLY;
    };

    for part in parts {
        let inline_data = part
            .get("inline_data")
            .and_then(serde_json::Value::as_object);
        let Some(inline_data) = inline_data else {
            continue;
        };
        let mime_type = inline_data
            .get("mime_type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if mime_type.starts_with("image/") {
            return STARTUP_TEST_TELEGRAM_PHOTO_REPLY;
        }
        if mime_type.starts_with("audio/") {
            return STARTUP_TEST_TELEGRAM_VOICE_REPLY;
        }
    }

    STARTUP_TEST_TELEGRAM_TEXT_REPLY
}

fn startup_test_telegram_poll_lease_key(token_key: &str, token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let digest = hasher.finalize();
    format!("{token_key}:{}", hex::encode(&digest[..8]))
}

fn startup_test_db_path() -> PathBuf {
    profile_dir()
        .map(|d| d.join("context.db"))
        .unwrap_or_else(|| PathBuf::from("aiua_context.db"))
}

fn startup_test_membrane_guests(hotel_name: &str) -> Result<Vec<GuestRecord>> {
    let storage =
        ansible_mesh_core::sqlite_storage::SqliteGraphStorage::open(startup_test_db_path())?;
    let graph = ansible_mesh_core::domain::GraphDomain::new(std::sync::Arc::new(storage.adapter()));
    let mut membranes = graph
        .list_guests(hotel_name, false)?
        .into_iter()
        .filter(|guest| guest.role == "membrane")
        .collect::<Vec<_>>();
    membranes.sort_by(|left, right| left.guest_id.cmp(&right.guest_id));
    Ok(membranes)
}

fn startup_test_set_guest_active(
    hotel_name: &str,
    guest_id: &str,
    is_active: bool,
    active_pid: Option<&str>,
) -> Result<()> {
    let conn = rusqlite::Connection::open(startup_test_db_path())
        .context("failed to open context.db for startup guest update")?;
    conn.execute(
        "UPDATE materialized_guests SET is_active = ?1, active_pid = ?2 WHERE hotel_name = ?3 AND guest_id = ?4",
        rusqlite::params![is_active, active_pid, hotel_name, guest_id],
    )
    .with_context(|| format!("failed to update startup guest row for {}", guest_id))?;
    Ok(())
}

fn startup_test_clear_guest_pid(hotel_name: &str, guest_id: &str) -> Result<()> {
    let conn = rusqlite::Connection::open(startup_test_db_path())
        .context("failed to open context.db for startup guest pid clear")?;
    conn.execute(
        "UPDATE materialized_guests SET active_pid = NULL WHERE hotel_name = ?1 AND guest_id = ?2",
        rusqlite::params![hotel_name, guest_id],
    )
    .with_context(|| format!("failed to clear startup guest pid for {}", guest_id))?;
    Ok(())
}

fn startup_test_force_kill_pid(pid: u32) -> Result<()> {
    let status = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .with_context(|| format!("failed to send SIGKILL to pid {}", pid))?;
    if !status.success() {
        anyhow::bail!("kill -9 {} exited with status {}", pid, status);
    }
    Ok(())
}

fn prepare_startup_test_binaries(_test: StartupTest) -> Result<()> {
    let existing_bins = [
        "target/debug/membrane",
        "target/debug/philote",
        "target/debug/tool-runner",
        "target/debug/graph-runner",
        "target/debug/model-controller-gemini",
        "target/debug/model-controller-elevenlabs",
    ];
    if existing_bins
        .iter()
        .all(|path| std::path::Path::new(path).exists())
    {
        return Ok(());
    }

    let status = std::process::Command::new("cargo")
        .args([
            "build",
            "-p",
            "membrane",
            "-p",
            "philote",
            "-p",
            "tool-runner",
            "-p",
            "graph-runner",
            "-p",
            "model-router",
            "--bins",
        ])
        .status()
        .context("failed to launch cargo build for startup test binaries")?;

    if !status.success() {
        anyhow::bail!("startup test binary build failed with status {}", status);
    }

    Ok(())
}

async fn run_startup_test(
    test: StartupTest,
    hotel_name: &str,
    socket_path: &str,
    output: Option<&str>,
    text: Option<&str>,
) -> Result<()> {
    let local_node_id = default_hotel_record(hotel_name).capabilities.node_id;
    match test {
        StartupTest::GraphRoundTrip => {
            let graph_name = format!("startup-graph-{}-{}", hotel_name, std::process::id());

            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            let mut client = PhiloticClient::connect_at(
                socket_path,
                GuestIdentity {
                    guest_id: "aiua-startup-graph-client".into(),
                    role: "aiua-startup-graph".into(),
                    supported_tools: Vec::new(),
                },
            )
            .await?;

            let subscribe = client
                .send_request(IpcRequest::SubscribeInbox {
                    role: "aiua-startup-graph".into(),
                })
                .await?;
            match subscribe {
                IpcResponse::Standard { ok: true, .. } => {}
                other => anyhow::bail!("unexpected graph startup subscribe response: {other:?}"),
            }

            let graph_id = startup_test_emit_graph_tool(
                &mut client,
                &local_node_id,
                "graph.create",
                serde_json::json!({
                    "name": graph_name,
                    "default_visibility": "public",
                    "caller_id": "aiua-startup-graph-client"
                }),
            )
            .await?;
            let graph_id = graph_id
                .get("graph_id")
                .and_then(serde_json::Value::as_str)
                .context("graph.create startup reply missing graph_id")?
                .to_string();

            let node_result = startup_test_emit_graph_tool(
                &mut client,
                &local_node_id,
                "graph.node.upsert",
                serde_json::json!({
                    "graph_id": graph_id,
                    "node_type": "Smoke",
                    "label": "startup smoke node",
                    "visibility": ["public"],
                    "caller_id": "aiua-startup-graph-client"
                }),
            )
            .await?;
            let node_id = node_result
                .get("node_id")
                .and_then(serde_json::Value::as_str)
                .context("graph.node.upsert startup reply missing node_id")?
                .to_string();

            let get_result = startup_test_emit_graph_tool(
                &mut client,
                &local_node_id,
                "graph.node.get",
                serde_json::json!({
                    "graph_id": graph_id,
                    "node_id": node_id,
                    "caller_id": "aiua-startup-graph-client"
                }),
            )
            .await?;
            let label = get_result
                .get("node")
                .and_then(|node| node.get("label"))
                .and_then(serde_json::Value::as_str)
                .context("graph.node.get startup reply missing node.label")?;
            if label != "startup smoke node" {
                anyhow::bail!(
                    "unexpected graph.node.get startup label: expected {:?}, got {:?}",
                    "startup smoke node",
                    label
                );
            }

            let export_result = startup_test_emit_graph_tool(
                &mut client,
                &local_node_id,
                "graph.export",
                serde_json::json!({
                    "graph_id": graph_id,
                    "caller_id": "aiua-startup-graph-client"
                }),
            )
            .await?;
            let node_count = export_result
                .get("node_count")
                .and_then(serde_json::Value::as_u64)
                .context("graph.export startup reply missing node_count")?;
            if node_count < 1 {
                anyhow::bail!("expected graph export node_count >= 1, got {}", node_count);
            }

            info!(
                graph_id = %graph_id,
                node_id = %node_id,
                node_count,
                "Startup graph round-trip succeeded through materialized graph-runner"
            );
            Ok(())
        }
        StartupTest::TextRoundTrip => {
            let text = text
                .unwrap_or("hello from the Philotic startup text test")
                .to_string();

            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            let mut client = PhiloticClient::connect_at(
                socket_path,
                GuestIdentity {
                    guest_id: "aiua-startup-test-client".into(),
                    role: "aiua-startup-test".into(),
                    supported_tools: Vec::new(),
                },
            )
            .await?;

            let mut last_error = None;
            for attempt in 1..=5 {
                let response = client
                    .send_request(IpcRequest::EmitTask {
                        target_node: local_node_id.clone(),
                        target_role: "agent".into(),
                        target_guest_id: None,
                        task_json: serde_json::json!({
                            "source": "startup-test",
                            "session_id": "startup-test:text-roundtrip",
                            "turn_id": format!("startup-test-turn-{attempt}"),
                            "chat_id": "startup-test-chat",
                            "content": text,
                            "final_reply_to": local_node_id,
                            "final_reply_role": "aiua-startup-test",
                            "final_reply_guest_id": "aiua-startup-test-client"
                        })
                        .to_string(),
                    })
                    .await?;

                match response {
                    IpcResponse::Standard { ok: true, .. } => {}
                    other => anyhow::bail!("unexpected startup test emit response: {other:?}"),
                }

                match tokio::time::timeout(tokio::time::Duration::from_secs(10), client.recv_task())
                    .await
                {
                    Ok(Ok(IpcResponse::InboundTask { task_json, .. })) => {
                        let payload: serde_json::Value = serde_json::from_str(&task_json)
                            .context("failed to decode startup text reply")?;
                        let action = payload
                            .get("action")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        let content = payload
                            .get("content")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();

                        if action != "send_reply" {
                            anyhow::bail!("unexpected startup text action: {action}");
                        }
                        if content != STARTUP_TEST_TEXT_REPLY {
                            anyhow::bail!(
                                "unexpected startup text reply: expected {:?}, got {:?}",
                                STARTUP_TEST_TEXT_REPLY,
                                content
                            );
                        }

                        info!(
                            "Startup text round-trip received {:?} on attempt {}",
                            content, attempt
                        );
                        return Ok(());
                    }
                    Ok(Ok(other)) => anyhow::bail!("unexpected startup text envelope: {other:?}"),
                    Ok(Err(err)) => {
                        return Err(err.context("failed waiting for startup text reply"));
                    }
                    Err(err) => {
                        warn!(
                            "Startup text round-trip attempt {} timed out waiting for reply; retrying.",
                            attempt
                        );
                        last_error = Some(err);
                        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    }
                }
            }

            let err = last_error.context("startup text retry loop did not record a timeout")?;
            anyhow::bail!(
                "timed out waiting for startup text reply after retries: {}",
                err
            );
        }
        StartupTest::CognitiveRoundTrip => {
            let expected_reply = text
                .unwrap_or(STARTUP_TEST_COGNITIVE_REPLY)
                .trim()
                .to_string();
            let user_content = format!("Reply with exactly: {}", expected_reply);

            let fake_gemini = spawn_fake_gemini_server(
                hotel_name,
                expected_reply.clone(),
                vec![
                    "[Identity]".into(),
                    "[Instructions]".into(),
                    "[Memory]".into(),
                    "[Active turn]".into(),
                    user_content.clone(),
                ],
                false,
            );
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            let mut client = PhiloticClient::connect_at(
                socket_path,
                GuestIdentity {
                    guest_id: "aiua-startup-test-client".into(),
                    role: "aiua-startup-test".into(),
                    supported_tools: Vec::new(),
                },
            )
            .await?;

            let response = client
                .send_request(IpcRequest::EmitTask {
                    target_node: local_node_id.clone(),
                    target_role: "agent".into(),
                    target_guest_id: None,
                    task_json: serde_json::json!({
                        "source": "startup-test",
                        "session_id": "startup-test:cognitive-roundtrip",
                        "turn_id": "startup-cognitive-turn-1",
                        "chat_id": "startup-cognitive-chat",
                        "content": user_content,
                        "final_reply_to": local_node_id,
                        "final_reply_role": "aiua-startup-test",
                        "final_reply_guest_id": "aiua-startup-test-client"
                    })
                    .to_string(),
                })
                .await?;

            match response {
                IpcResponse::Standard { ok: true, .. } => {}
                other => anyhow::bail!("unexpected startup test emit response: {other:?}"),
            }

            let reply =
                tokio::time::timeout(tokio::time::Duration::from_secs(30), client.recv_task())
                    .await
                    .context("timed out waiting for cognitive startup reply")??;
            fake_gemini.abort();

            let IpcResponse::InboundTask { task_json, .. } = reply else {
                anyhow::bail!("unexpected startup test envelope: {reply:?}");
            };

            let payload: serde_json::Value = serde_json::from_str(&task_json)
                .context("failed to decode cognitive startup reply")?;
            if let Some(message) = payload
                .get("agent_action")
                .and_then(|value| value.get("message"))
                .and_then(serde_json::Value::as_str)
            {
                anyhow::bail!("startup cognitive round-trip failed: {message}");
            }

            let content = payload
                .get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if content != expected_reply {
                anyhow::bail!(
                    "unexpected cognitive startup reply: expected {:?}, got {:?}",
                    expected_reply,
                    content
                );
            }

            info!(
                "Startup cognitive round-trip received {:?} through fake Gemini-backed structured context",
                content
            );
            Ok(())
        }
        StartupTest::GeminiOAuthRoundTrip => {
            let expected_reply = text
                .unwrap_or(STARTUP_TEST_GEMINI_OAUTH_REPLY)
                .trim()
                .to_string();
            let prompt = format!("Reply with exactly: {}", expected_reply);

            let fake_gemini =
                spawn_fake_gemini_server(hotel_name, expected_reply.clone(), Vec::new(), true);
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            let mut client = PhiloticClient::connect_at(
                socket_path,
                GuestIdentity {
                    guest_id: "aiua-startup-test-client".into(),
                    role: "aiua-startup-test".into(),
                    supported_tools: Vec::new(),
                },
            )
            .await?;

            let response = client
                .send_request(IpcRequest::EmitTask {
                    target_node: local_node_id.clone(),
                    target_role: "model".into(),
                    target_guest_id: None,
                    task_json: serde_json::json!({
                        "kind": "text.generate",
                        "provider": "gemini",
                        "model": "gemini-2.5-flash",
                        "prompt": prompt,
                        "response_contract": {
                            "channels": ["display_text"]
                        },
                        "session_id": "startup-test:gemini-oauth-roundtrip",
                        "turn_id": "startup-test-turn-1",
                        "chat_id": "startup-test-chat",
                        "reply_to": local_node_id.clone(),
                        "reply_role": "aiua-startup-test",
                        "final_reply_to": local_node_id.clone(),
                        "final_reply_role": "aiua-startup-test",
                        "final_reply_guest_id": "aiua-startup-test-client"
                    })
                    .to_string(),
                })
                .await?;

            match response {
                IpcResponse::Standard { ok: true, .. } => {}
                other => anyhow::bail!("unexpected startup test emit response: {other:?}"),
            }

            let reply =
                tokio::time::timeout(tokio::time::Duration::from_secs(30), client.recv_task())
                    .await
                    .context("timed out waiting for Gemini OAuth startup reply")??;
            fake_gemini.abort();

            let IpcResponse::InboundTask { task_json, .. } = reply else {
                anyhow::bail!("unexpected startup test envelope: {reply:?}");
            };

            let payload: serde_json::Value =
                serde_json::from_str(&task_json).context("failed to decode startup test reply")?;
            if let Some(message) = payload
                .get("agent_action")
                .and_then(|value| value.get("message"))
                .and_then(serde_json::Value::as_str)
            {
                anyhow::bail!("startup gemini oauth round-trip failed: {message}");
            }

            let content = payload
                .get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if content != expected_reply {
                anyhow::bail!(
                    "unexpected Gemini OAuth startup reply: expected {:?}, got {:?}",
                    expected_reply,
                    content
                );
            }

            let trace_provider = payload
                .get("agent_action")
                .and_then(|value| value.get("model_result"))
                .and_then(|value| value.get("trace"))
                .and_then(|value| value.get("provider"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            if trace_provider != "gemini" {
                anyhow::bail!(
                    "unexpected Gemini OAuth startup trace provider: {:?}",
                    trace_provider
                );
            }

            info!(
                "Startup Gemini OAuth round-trip received {:?} through provider {:?}",
                content, trace_provider
            );
            Ok(())
        }
        StartupTest::VoiceSample => {
            let output_path = output
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("tmp/voice-samples/aiua-startup-sample.mp3"));
            let text = text
                .unwrap_or("Hello from Philotic. This is an ansible startup voice test.")
                .to_string();

            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            let mut client = PhiloticClient::connect_at(
                socket_path,
                GuestIdentity {
                    guest_id: "aiua-startup-test-client".into(),
                    role: "aiua-startup-test".into(),
                    supported_tools: Vec::new(),
                },
            )
            .await?;

            let response = client
                .send_request(IpcRequest::EmitTask {
                    target_node: local_node_id.clone(),
                    target_role: "model.elevenlabs".into(),
                    target_guest_id: None,
                    task_json: serde_json::json!({
                        "kind": "voice.synthesize",
                        "session_id": "startup-test:voice-sample",
                        "turn_id": "startup-test-turn-1",
                        "chat_id": "startup-test-chat",
                        "text": text,
                        "reply_to": local_node_id.clone(),
                        "reply_role": "aiua-startup-test",
                        "final_reply_to": local_node_id.clone(),
                        "final_reply_role": "aiua-startup-test"
                    })
                    .to_string(),
                })
                .await?;

            match response {
                IpcResponse::Standard { ok: true, .. } => {}
                other => anyhow::bail!("unexpected startup test emit response: {other:?}"),
            }

            let reply =
                tokio::time::timeout(tokio::time::Duration::from_secs(30), client.recv_task())
                    .await
                    .context("timed out waiting for startup test reply")??;
            let IpcResponse::InboundTask { task_json, .. } = reply else {
                anyhow::bail!("unexpected startup test envelope: {reply:?}");
            };

            let payload: serde_json::Value =
                serde_json::from_str(&task_json).context("failed to decode startup test reply")?;
            if let Some(message) = payload
                .get("agent_action")
                .and_then(|value| value.get("message"))
                .and_then(serde_json::Value::as_str)
            {
                anyhow::bail!("startup voice sample failed: {message}");
            }

            let content = payload
                .get("content")
                .and_then(serde_json::Value::as_str)
                .context("startup test reply missing content")?;
            let artifact: serde_json::Value = serde_json::from_str(content)
                .context("startup test content was not audio artifact json")?;
            let audio_base64 = artifact
                .get("audio_base64")
                .and_then(serde_json::Value::as_str)
                .context("audio artifact missing audio_base64")?;
            let audio_bytes = BASE64_STANDARD
                .decode(audio_base64)
                .context("failed to decode startup sample base64")?;

            if let Some(parent) = output_path.parent() {
                fs::create_dir_all(parent).with_context(|| {
                    format!(
                        "failed to create startup test output dir {}",
                        parent.display()
                    )
                })?;
            }
            fs::write(&output_path, audio_bytes).with_context(|| {
                format!(
                    "failed to write startup voice sample to {}",
                    output_path.display()
                )
            })?;

            info!(
                "Startup voice sample wrote {} using voice {}",
                output_path.display(),
                artifact
                    .get("voice_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
            );
            Ok(())
        }
        StartupTest::TelegramPollLease => {
            let telegram_api_base_url = startup_test_telegram_api_base_url(hotel_name);
            let telegram_addr = format!("127.0.0.1:{}", startup_test_telegram_port(hotel_name));
            let telegram_state = Arc::new(FakeTelegramState::default());
            let poll_lease_key = startup_test_telegram_poll_lease_key(
                "telegram_bot_token",
                STARTUP_TEST_TELEGRAM_TOKEN,
            );

            let telegram_listener = tokio::net::TcpListener::bind(&telegram_addr)
                .await
                .with_context(|| format!("failed to bind fake telegram api on {telegram_addr}"))?;
            let telegram_server = tokio::spawn({
                let state = Arc::clone(&telegram_state);
                async move {
                    axum::serve(telegram_listener, fake_telegram_router(state))
                        .await
                        .expect("fake telegram api should serve");
                }
            });

            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            let mut leader_guest_id = None;
            let mut leader_pid = None;
            for attempt in 1..=30 {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                let membranes = startup_test_membrane_guests(hotel_name)?;
                for guest in &membranes {
                    let Some(pid_text) = guest.active_pid.as_deref() else {
                        continue;
                    };
                    let Ok(pid) = pid_text.parse::<u32>() else {
                        continue;
                    };
                    if !pid_exists(pid) {
                        startup_test_clear_guest_pid(hotel_name, &guest.guest_id)?;
                    }
                }
                let membranes = startup_test_membrane_guests(hotel_name)?;
                let live = membranes
                    .iter()
                    .filter_map(|guest| {
                        guest.active_pid.as_deref().and_then(|pid_text| {
                            pid_text
                                .parse::<u32>()
                                .ok()
                                .filter(|pid| pid_exists(*pid))
                                .map(|pid| (guest.guest_id.clone(), pid))
                        })
                    })
                    .collect::<Vec<_>>();
                if let [single] = live.as_slice() {
                    let (guest_id, pid) = single;
                    leader_guest_id = Some(guest_id.clone());
                    leader_pid = Some(*pid);
                    info!(
                        "Startup telegram poll-lease smoke observed active poller [{}] at pid {} on attempt {}",
                        guest_id, pid, attempt
                    );
                    break;
                }
            }

            let leader_guest_id =
                leader_guest_id.context("timed out waiting for a single live membrane poller")?;
            let leader_pid =
                leader_pid.context("timed out waiting for active membrane pid for poller smoke")?;

            {
                let mut updates = telegram_state.updates.lock().expect("updates lock");
                updates.push_back(serde_json::json!({
                    "update_id": 1,
                    "message": {
                        "message_id": 1,
                        "text": "/ping",
                        "chat": { "id": 777100 },
                        "from": { "id": 42, "username": "startup_test" }
                    }
                }));
            }

            for attempt in 1..=20 {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                let sent_messages = telegram_state
                    .sent_messages
                    .lock()
                    .expect("sent messages lock")
                    .clone();
                let deleted_command_syncs = *telegram_state
                    .deleted_command_syncs
                    .lock()
                    .expect("deleted command syncs lock");
                let registered_commands_len = telegram_state
                    .registered_commands
                    .lock()
                    .expect("registered commands lock")
                    .len();
                if sent_messages.len() >= 1 {
                    let text = sent_messages[0]
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    if text != "pong" {
                        telegram_server.abort();
                        let _ = telegram_server.await;
                        anyhow::bail!(
                            "unexpected first telegram poll-lease reply on attempt {}: expected {:?}, got {:?}",
                            attempt,
                            "pong",
                            text
                        );
                    }
                    if deleted_command_syncs != 1 || registered_commands_len != 1 {
                        telegram_server.abort();
                        let _ = telegram_server.await;
                        anyhow::bail!(
                            "expected exactly one command sync before takeover, got delete={} set={}",
                            deleted_command_syncs,
                            registered_commands_len
                        );
                    }
                    break;
                }
                if attempt == 20 {
                    telegram_server.abort();
                    let _ = telegram_server.await;
                    anyhow::bail!("timed out waiting for first poll-lease /ping reply");
                }
            }

            startup_test_set_guest_active(hotel_name, &leader_guest_id, false, None)?;
            let mut leader_stopped = false;
            for _ in 1..=8 {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                let membranes = startup_test_membrane_guests(hotel_name)?;
                let Some(leader) = membranes
                    .iter()
                    .find(|guest| guest.guest_id == leader_guest_id)
                else {
                    leader_stopped = true;
                    break;
                };
                let Some(pid_text) = leader.active_pid.as_deref() else {
                    leader_stopped = true;
                    break;
                };
                let Ok(pid) = pid_text.parse::<u32>() else {
                    leader_stopped = true;
                    break;
                };
                if !pid_exists(pid) {
                    leader_stopped = true;
                    break;
                }
            }
            if !leader_stopped {
                startup_test_force_kill_pid(leader_pid)?;
            }

            let mut standby_guest_id = None;
            for _attempt in 1..=30 {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                let membranes = startup_test_membrane_guests(hotel_name)?;
                for guest in &membranes {
                    let Some(pid_text) = guest.active_pid.as_deref() else {
                        continue;
                    };
                    let Ok(pid) = pid_text.parse::<u32>() else {
                        continue;
                    };
                    if !pid_exists(pid) {
                        startup_test_clear_guest_pid(hotel_name, &guest.guest_id)?;
                    }
                }
                let membranes = startup_test_membrane_guests(hotel_name)?;
                if let Some(leader) = membranes
                    .iter()
                    .find(|guest| guest.guest_id == leader_guest_id)
                    .cloned()
                {
                    startup_test_set_guest_active(hotel_name, &leader.guest_id, false, None)?;
                    if let Some(pid_text) = leader.active_pid.as_deref() {
                        if let Ok(pid) = pid_text.parse::<u32>() {
                            if pid_exists(pid) {
                                startup_test_force_kill_pid(pid)?;
                                continue;
                            }
                        }
                    }
                }
                let live = membranes
                    .iter()
                    .filter_map(|guest| {
                        guest.active_pid.as_deref().and_then(|pid_text| {
                            pid_text
                                .parse::<u32>()
                                .ok()
                                .filter(|pid| pid_exists(*pid))
                                .map(|_pid| guest.guest_id.clone())
                        })
                    })
                    .collect::<Vec<_>>();
                if live.len() != 1 || live[0] == leader_guest_id {
                    continue;
                }
                let deleted_command_syncs = *telegram_state
                    .deleted_command_syncs
                    .lock()
                    .expect("deleted command syncs lock");
                let registered_commands_len = telegram_state
                    .registered_commands
                    .lock()
                    .expect("registered commands lock")
                    .len();
                if deleted_command_syncs >= 2 && registered_commands_len >= 2 {
                    standby_guest_id = Some(live[0].clone());
                    break;
                }
            }
            let standby_guest_id =
                standby_guest_id.context("timed out waiting for standby membrane takeover")?;

            {
                let mut updates = telegram_state.updates.lock().expect("updates lock");
                updates.push_back(serde_json::json!({
                    "update_id": 2,
                    "message": {
                        "message_id": 2,
                        "text": "/ping",
                        "chat": { "id": 777101 },
                        "from": { "id": 42, "username": "startup_test" }
                    }
                }));
            }

            for attempt in 1..=20 {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                let sent_messages = telegram_state
                    .sent_messages
                    .lock()
                    .expect("sent messages lock")
                    .clone();
                if sent_messages.len() >= 2 {
                    let text = sent_messages[1]
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    if text != "pong" {
                        telegram_server.abort();
                        let _ = telegram_server.await;
                        anyhow::bail!(
                            "unexpected takeover telegram poll-lease reply on attempt {}: expected {:?}, got {:?}",
                            attempt,
                            "pong",
                            text
                        );
                    }
                    let get_updates_calls =
                        telegram_state.get_updates_calls.load(Ordering::Relaxed);
                    if get_updates_calls < 2 {
                        telegram_server.abort();
                        let _ = telegram_server.await;
                        anyhow::bail!(
                            "expected multiple Telegram getUpdates calls across takeover, got {}",
                            get_updates_calls
                        );
                    }
                    info!(
                        "Startup telegram poll-lease smoke proved single-owner polling and standby takeover for lease [{}] from [{}] to [{}] via {}",
                        poll_lease_key, leader_guest_id, standby_guest_id, telegram_api_base_url
                    );
                    telegram_server.abort();
                    let _ = telegram_server.await;
                    return Ok(());
                }
                if attempt == 20 {
                    telegram_server.abort();
                    let _ = telegram_server.await;
                    anyhow::bail!("timed out waiting for standby takeover /ping reply");
                }
            }

            unreachable!("telegram poll-lease smoke should return or bail inside the loop");
        }
        StartupTest::TelegramRoundTrip => {
            let telegram_api_base_url = startup_test_telegram_api_base_url(hotel_name);
            let telegram_addr = format!("127.0.0.1:{}", startup_test_telegram_port(hotel_name));
            let gemini_api_base_url = startup_test_gemini_api_base_url(hotel_name);
            let gemini_addr = format!("127.0.0.1:{}", startup_test_gemini_port(hotel_name));
            let blob_port = startup_test_blob_port(hotel_name);
            let blob_addr = format!("127.0.0.1:{}", blob_port);
            let telegram_state = Arc::new(FakeTelegramState::default());
            let gemini_state = Arc::new(FakeGeminiMediaState::default());
            let startup_text = text
                .unwrap_or("hello from telegram startup test")
                .to_string();
            let blob_dir = std::env::temp_dir().join(format!(
                "philotic-startup-test-blobs-{}",
                uuid::Uuid::new_v4().simple()
            ));
            let blob_service = BlobService::new(blob_dir);
            {
                let mut files = telegram_state.files.lock().expect("files lock");
                files.insert(
                    "photo-large".into(),
                    FakeTelegramFile {
                        file_path: "photos/photo-large.jpg".into(),
                        bytes: b"startup-photo-bytes".to_vec(),
                    },
                );
                files.insert(
                    "voice-1".into(),
                    FakeTelegramFile {
                        file_path: "voice/voice-1.ogg".into(),
                        bytes: b"startup-voice-bytes".to_vec(),
                    },
                );
            }

            let telegram_listener = tokio::net::TcpListener::bind(&telegram_addr)
                .await
                .with_context(|| format!("failed to bind fake telegram api on {telegram_addr}"))?;
            let telegram_server = tokio::spawn({
                let state = Arc::clone(&telegram_state);
                async move {
                    axum::serve(telegram_listener, fake_telegram_router(state))
                        .await
                        .expect("fake telegram api should serve");
                }
            });
            let gemini_listener = tokio::net::TcpListener::bind(&gemini_addr)
                .await
                .with_context(|| format!("failed to bind fake gemini api on {gemini_addr}"))?;
            let gemini_server = tokio::spawn({
                let state = Arc::clone(&gemini_state);
                async move {
                    axum::serve(gemini_listener, fake_gemini_router(state))
                        .await
                        .expect("fake gemini api should serve");
                }
            });
            let blob_server = tokio::spawn(async move {
                blob_service
                    .serve(&blob_addr)
                    .await
                    .expect("fake blob service should serve");
            });

            {
                let telegram_state = Arc::clone(&telegram_state);
                tokio::spawn(async move {
                    let mut blob_ready = false;
                    for _ in 0..60 {
                        if tokio::net::TcpStream::connect(("127.0.0.1", blob_port))
                            .await
                            .is_ok()
                        {
                            blob_ready = true;
                            break;
                        }
                        tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
                    }
                    if !blob_ready {
                        return;
                    }
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    let mut updates = telegram_state.updates.lock().expect("updates lock");
                    updates.push_back(serde_json::json!({
                        "update_id": 1,
                        "message": {
                            "message_id": 1,
                            "text": startup_text,
                            "chat": { "id": 777000 },
                            "from": { "id": 42, "username": "startup_test" }
                        }
                    }));
                    updates.push_back(serde_json::json!({
                        "update_id": 2,
                        "message": {
                            "message_id": 2,
                            "caption": "what is in this image?",
                            "chat": { "id": 777001 },
                            "from": { "id": 42, "username": "startup_test" },
                            "photo": [
                                { "file_id": "photo-small" },
                                { "file_id": "photo-large" }
                            ]
                        }
                    }));
                    updates.push_back(serde_json::json!({
                        "update_id": 3,
                        "message": {
                            "message_id": 3,
                            "chat": { "id": 777002 },
                            "from": { "id": 42, "username": "startup_test" },
                            "voice": {
                                "file_id": "voice-1",
                                "mime_type": "audio/ogg"
                            }
                        }
                    }));
                });
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            let expected_text_replies = [
                STARTUP_TEST_TELEGRAM_TEXT_REPLY,
                STARTUP_TEST_TELEGRAM_PHOTO_REPLY,
            ];
            for attempt in 1..=30 {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                let registered_commands = telegram_state
                    .registered_commands
                    .lock()
                    .expect("registered commands lock")
                    .clone();
                let deleted_command_syncs = *telegram_state
                    .deleted_command_syncs
                    .lock()
                    .expect("deleted command syncs lock");
                let sent_messages = telegram_state
                    .sent_messages
                    .lock()
                    .expect("sent messages lock")
                    .clone();
                let sent_media = telegram_state
                    .sent_media
                    .lock()
                    .expect("sent media lock")
                    .clone();
                if sent_messages.len() >= expected_text_replies.len() && !sent_media.is_empty() {
                    if deleted_command_syncs == 0 {
                        telegram_server.abort();
                        gemini_server.abort();
                        blob_server.abort();
                        let _ = telegram_server.await;
                        let _ = gemini_server.await;
                        let _ = blob_server.await;
                        anyhow::bail!("expected deleteMyCommands to run before setMyCommands");
                    }

                    let latest_registered_commands = registered_commands
                        .last()
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    let command_names = latest_registered_commands
                        .as_array()
                        .map(|commands| {
                            commands
                                .iter()
                                .filter_map(|command| {
                                    command.get("command").and_then(serde_json::Value::as_str)
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    if !command_names.contains(&"help") || !command_names.contains(&"commands") {
                        telegram_server.abort();
                        gemini_server.abort();
                        blob_server.abort();
                        let _ = telegram_server.await;
                        let _ = gemini_server.await;
                        let _ = blob_server.await;
                        anyhow::bail!(
                            "expected setMyCommands to register help/commands, got {:?}",
                            command_names
                        );
                    }

                    for (index, expected_text) in expected_text_replies.iter().enumerate() {
                        let message = &sent_messages[index];
                        let text = message
                            .get("text")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        let parse_mode = message
                            .get("parse_mode")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default();
                        if text != *expected_text {
                            telegram_server.abort();
                            gemini_server.abort();
                            blob_server.abort();
                            let _ = telegram_server.await;
                            let _ = gemini_server.await;
                            let _ = blob_server.await;
                            anyhow::bail!(
                                "unexpected telegram startup reply {} on attempt {}: expected {:?}, got {:?}",
                                index + 1,
                                attempt,
                                expected_text,
                                text
                            );
                        }
                        if parse_mode != "HTML" {
                            telegram_server.abort();
                            gemini_server.abort();
                            blob_server.abort();
                            let _ = telegram_server.await;
                            let _ = gemini_server.await;
                            let _ = blob_server.await;
                            anyhow::bail!(
                                "unexpected telegram parse_mode for reply {} on attempt {}: expected HTML, got {:?}",
                                index + 1,
                                attempt,
                                parse_mode
                            );
                        }
                    }

                    let voice_delivery = sent_media.last().cloned().unwrap_or_default();
                    let content_type = voice_delivery
                        .get("content_type")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    let bytes_len = voice_delivery
                        .get("bytes_len")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or_default();
                    if !content_type.contains("multipart/form-data") || bytes_len == 0 {
                        telegram_server.abort();
                        gemini_server.abort();
                        blob_server.abort();
                        let _ = telegram_server.await;
                        let _ = gemini_server.await;
                        let _ = blob_server.await;
                        anyhow::bail!(
                            "expected Telegram voice delivery for {:?}, got content_type={:?} bytes_len={}",
                            STARTUP_TEST_TELEGRAM_VOICE_REPLY,
                            content_type,
                            bytes_len
                        );
                    }

                    let gemini_requests = gemini_state
                        .requests
                        .lock()
                        .expect("fake gemini requests lock")
                        .clone();
                    if gemini_requests.len() < 3 {
                        telegram_server.abort();
                        gemini_server.abort();
                        blob_server.abort();
                        let _ = telegram_server.await;
                        let _ = gemini_server.await;
                        let _ = blob_server.await;
                        anyhow::bail!(
                            "expected {} fake Gemini requests, got {}",
                            3,
                            gemini_requests.len()
                        );
                    }
                    assert_fake_gemini_media_request(&gemini_requests[1], "image/jpeg")?;
                    assert_fake_gemini_media_request(&gemini_requests[2], "audio/ogg")?;

                    info!(
                        "Startup telegram round-trip delivered {:?} plus voice media through fake Telegram API and fake Gemini API on attempt {} via {} and {}",
                        expected_text_replies, attempt, telegram_api_base_url, gemini_api_base_url
                    );
                    telegram_server.abort();
                    gemini_server.abort();
                    blob_server.abort();
                    let _ = telegram_server.await;
                    let _ = gemini_server.await;
                    let _ = blob_server.await;
                    return Ok(());
                }
            }

            telegram_server.abort();
            gemini_server.abort();
            blob_server.abort();
            let _ = telegram_server.await;
            let _ = gemini_server.await;
            let _ = blob_server.await;
            anyhow::bail!(
                "timed out waiting for fake Telegram media smoke replies at {} via {}",
                telegram_api_base_url,
                gemini_api_base_url
            );
        }
    }
}

async fn startup_test_emit_graph_tool(
    client: &mut PhiloticClient,
    local_node_id: &str,
    tool_name: &str,
    arguments: serde_json::Value,
) -> Result<serde_json::Value> {
    let session_id = format!("startup-test:graph:{tool_name}");
    let turn_id = format!("startup-graph-turn-{tool_name}");

    let response = client
        .send_request(IpcRequest::EmitTask {
            target_node: local_node_id.to_string(),
            target_role: format!("tool.{tool_name}"),
            target_guest_id: None,
            task_json: serde_json::json!({
                "action": "execute_tool",
                "tool_name": tool_name,
                "arguments": arguments,
                "session_id": session_id,
                "turn_id": turn_id,
                "chat_id": "startup-test-chat",
                "agent_id": "aiua-startup-graph-client",
                "caller_roles": ["aiua-startup-graph"],
                "reply_to": local_node_id,
                "reply_role": "aiua-startup-graph",
                "final_reply_to": local_node_id,
                "final_reply_role": "aiua-startup-graph",
                "final_reply_guest_id": "aiua-startup-graph-client"
            })
            .to_string(),
        })
        .await?;

    match response {
        IpcResponse::Standard { ok: true, .. } => {}
        other => anyhow::bail!("{tool_name}: unexpected startup graph emit response: {other:?}"),
    }

    let reply = tokio::time::timeout(tokio::time::Duration::from_secs(15), client.recv_task())
        .await
        .with_context(|| {
            format!("{tool_name}: timed out waiting for startup graph tool_result")
        })??;

    let IpcResponse::InboundTask { task_json, .. } = reply else {
        anyhow::bail!("{tool_name}: unexpected startup graph reply envelope: {reply:?}");
    };

    let payload: serde_json::Value = serde_json::from_str(&task_json)
        .with_context(|| format!("{tool_name}: failed to decode startup graph tool_result"))?;
    let content_str = payload
        .get("content")
        .and_then(serde_json::Value::as_str)
        .with_context(|| format!("{tool_name}: startup graph tool_result missing content"))?;
    let content: serde_json::Value = serde_json::from_str(content_str).with_context(|| {
        format!("{tool_name}: startup graph content was not valid JSON: {content_str}")
    })?;
    if content.get("ok").and_then(serde_json::Value::as_bool) != Some(true) {
        anyhow::bail!(
            "{tool_name}: startup graph tool returned ok=false: {}",
            content
                .get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(content_str)
        );
    }

    Ok(content)
}

fn assert_fake_gemini_media_request(
    payload: &serde_json::Value,
    expected_mime_type: &str,
) -> Result<()> {
    let parts = payload
        .get("contents")
        .and_then(serde_json::Value::as_array)
        .and_then(|contents| contents.first())
        .and_then(|content| content.get("parts"))
        .and_then(serde_json::Value::as_array)
        .context("fake Gemini request missing contents.parts")?;
    let inline_data = parts
        .iter()
        .find_map(|part| part.get("inline_data"))
        .context("fake Gemini request missing inline_data")?;
    let mime_type = inline_data
        .get("mime_type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if mime_type != expected_mime_type {
        anyhow::bail!(
            "expected fake Gemini inline_data mime type {:?}, got {:?}",
            expected_mime_type,
            mime_type
        );
    }
    let data = inline_data
        .get("data")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if data.is_empty() {
        anyhow::bail!("fake Gemini inline_data payload was empty");
    }
    let decoded = BASE64_STANDARD
        .decode(data)
        .context("fake Gemini inline_data was not valid base64")?;
    if decoded.is_empty() {
        anyhow::bail!("fake Gemini inline_data decoded to empty bytes");
    }
    Ok(())
}

fn pid_exists(pid: u32) -> bool {
    std::process::Command::new("ps")
        .arg("-p")
        .arg(pid.to_string())
        .arg("-o")
        .arg("stat=")
        .output()
        .map(|output| {
            if !output.status.success() {
                return false;
            }
            let stat = String::from_utf8_lossy(&output.stdout).trim().to_string();
            !stat.is_empty() && !stat.starts_with('Z')
        })
        .unwrap_or(false)
}

async fn stabilize_startup_test_guests(
    guest_manager: &Arc<crate::service::guest_manager::GuestManager>,
    graph: &Arc<GraphDomain>,
    hotel_name: &str,
    shutdown_rx: &tokio::sync::broadcast::Receiver<()>,
) -> Result<()> {
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    let guests = graph.list_guests(hotel_name, false)?;
    let mut cleared_dead_pids = 0usize;
    for guest in guests {
        let Some(pid_text) = guest.active_pid.as_deref() else {
            continue;
        };
        let Ok(pid) = pid_text.parse::<u32>() else {
            continue;
        };
        if pid_exists(pid) {
            continue;
        }
        graph.set_guest_pid(hotel_name, &guest.guest_id, None)?;
        cleared_dead_pids += 1;
    }

    if cleared_dead_pids == 0 {
        return Ok(());
    }

    warn!(
        "Startup test stabilization cleared {} dead guest PIDs before rerunning materialization.",
        cleared_dead_pids
    );

    guest_manager
        .materialize_all(shutdown_rx.resubscribe())
        .await?;
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    Ok(())
}

/// Apply a config file to the Context Graph DB and exit.
/// Idempotent — safe to run repeatedly; uses upsert semantics throughout.
async fn run_load_command(file: &str, hotel_name: &str) -> Result<()> {
    info!("Loading config '{}' into hotel '{}'...", file, hotel_name);

    // Open the same DB that a normal startup would use.
    let db_path_buf;
    let db_path: &Path = if let Some(ref pdir) = profile_dir() {
        fs::create_dir_all(pdir)
            .with_context(|| format!("create profile dir {}", pdir.display()))?;
        db_path_buf = pdir.join("context.db");
        &db_path_buf
    } else {
        Path::new("aiua_context.db")
    };
    let graph_storage = ansible_mesh_core::sqlite_storage::SqliteGraphStorage::open(db_path)?;
    let graph_domain = GraphDomain::new(Arc::new(graph_storage.adapter()));
    let hotel = reconcile_hotel_record(&graph_domain, hotel_name)?;
    graph_domain.upsert_hotel(&hotel)?;

    let config_data = fs::read_to_string(file).context("Failed to read config file")?;
    let config_json: serde_json::Value =
        serde_json::from_str(&config_data).context("Invalid JSON in config file")?;

    // Inject raw context_graph KV entries.
    let entries = extract_context_graph_entries(&config_json, Some(hotel_name));
    if !entries.is_empty() {
        let mut count = 0;
        for (key, value) in entries {
            let val_str = if value.is_string() {
                serde_json::to_string(&value)?
            } else {
                value.to_string()
            };
            graph_domain.set_config_value(&key, &val_str)?;
            count += 1;
        }
        info!("Injected {} config key(s) into Context Graph.", count);
    } else {
        warn!("Config file has no context_graph entries.");
    }

    let seeded_peer_hotels = seed_peer_hotels_from_config(&graph_domain, &config_json, hotel_name)?;
    if seeded_peer_hotels > 0 {
        info!(
            "Seeded {} peer hotel record(s) from config.",
            seeded_peer_hotels
        );
    }

    // Provision MuninnDB vaults if configured.
    if let Some(muninn) = config_json
        .get("context_graph")
        .and_then(|cg| cg.get("muninn"))
        .and_then(|v| v.as_object())
    {
        let endpoint = muninn
            .get("endpoint")
            .and_then(|v| v.as_str())
            .unwrap_or("http://127.0.0.1:8475");
        let username = muninn
            .get("admin_username")
            .and_then(|v| v.as_str())
            .unwrap_or("root");
        let password = muninn
            .get("admin_password")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        graph_domain.set_muninn_endpoint(endpoint)?;
        let vault_names = muninn_provision::derive_vault_names(&config_json);
        if !vault_names.is_empty() {
            muninn_provision::provision_muninn_vaults(
                &graph_domain,
                endpoint,
                username,
                password,
                vault_names,
            )
            .await?;
        }
    }

    // Derive agent profiles and seed guests + identities.
    let all_profiles = all_agent_profiles_from_config(&config_json, hotel_name);
    info!(
        "Seeding {} agent(s): {}",
        all_profiles.len(),
        all_profiles
            .iter()
            .map(|p| p.persona_name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let mut all_desired_guests: Vec<GuestRecord> = Vec::new();
    for profile in &all_profiles {
        all_desired_guests.push(agent_guests_for_profile(hotel_name, profile));
        all_desired_guests.push(agent_graph_runner_guest(hotel_name, profile));
    }
    all_desired_guests.extend(hotel_shared_guests(hotel_name, &all_profiles));

    deactivate_legacy_managed_guests(
        &graph_domain,
        hotel_name,
        &all_profiles,
        &all_desired_guests,
    )?;
    graph_domain.seed_guests(hotel_name, &all_desired_guests)?;
    info!("Seeded {} guest record(s).", all_desired_guests.len());

    seed_orchestrator_roles(&graph_domain, &all_profiles)?;
    seed_abstract_tool_catalog(&graph_domain)?;
    seed_abstract_skill_catalog(&graph_domain)?;
    seed_toolset_profiles(&graph_domain)?;
    seed_skill_crafting(&graph_domain)?;

    for profile in &all_profiles {
        let agent_config = raw_agent_config_for_key(&config_json, hotel_name, &profile.agent_key);
        let mut identity =
            agent_identity_record_for_profile(profile, hotel_name, agent_config.as_ref());
        // Preserve any cognitive envelope fields already written to the graph (e.g. via
        // agent.configure or direct graph writes). The workspace import only supplies values
        // at init time — subsequent phil load runs must not clobber live graph content.
        let existing = graph_domain
            .get_agent_identity(&identity.agent_id)
            .ok()
            .flatten();
        if let Some(ref existing_rec) = existing {
            if let (Some(existing_obj), Some(new_obj)) = (
                existing_rec.bundle_json.as_object(),
                identity.bundle_json.as_object_mut(),
            ) {
                for key in [
                    "soul_text",
                    "identity_text",
                    "user_context_text",
                    "agents_text",
                    "memory_summary",
                ] {
                    if new_obj.get(key).map(|v| v.is_null()).unwrap_or(true) {
                        if let Some(existing_val) = existing_obj.get(key) {
                            if !existing_val.is_null() {
                                new_obj.insert(key.to_string(), existing_val.clone());
                            }
                        }
                    }
                }
            }
        }
        // If the configured workspace path doesn't exist, create it and seed files from
        // whatever cognitive envelope content we have (graph-native or freshly loaded).
        if let Some(workspace_path) = profile
            .import_workspace
            .as_deref()
            .filter(|p| !p.is_empty())
        {
            let workspace = Path::new(workspace_path);
            let bundle_for_seed = existing
                .as_ref()
                .map(|e| &e.bundle_json)
                .or(Some(&identity.bundle_json));
            ensure_workspace_exists(workspace, bundle_for_seed);
        }
        graph_domain
            .upsert_agent_identity(&identity)
            .with_context(|| format!("Failed to upsert identity for {}", identity.agent_id))?;
        info!("Identity seeded for agent '{}'.", identity.agent_id);
    }

    println!(
        "✓ Config loaded into DB ({}).\n  Hotel '{}' is ready — start with: aiua --hotel {}",
        db_path.display(),
        hotel_name,
        hotel_name
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    if std::env::var_os("PHILOTIC_BIN_DIR").is_none() {
        if let Ok(current_exe) = std::env::current_exe() {
            if let Some(bin_dir) = current_exe.parent() {
                unsafe {
                    std::env::set_var("PHILOTIC_BIN_DIR", bin_dir);
                }
            }
        }
    }

    // Detect which git worktree the hotel is running from and expose it to all
    // spawned guests via PHILOTIC_WORKTREE. Guests (philote, membrane, etc.) can
    // use this to tag sessions and let the intel-graph link live sessions to
    // their workstream branch. No-op if not inside a git worktree or if already set.
    if std::env::var_os("PHILOTIC_WORKTREE").is_none() {
        if let Some(worktree_branch) = detect_git_worktree_branch() {
            unsafe {
                std::env::set_var("PHILOTIC_WORKTREE", &worktree_branch);
            }
            info!("Detected git worktree branch: {}", worktree_branch);
        }
    }

    if let Some(Command::Auth { provider }) = args.command {
        return auth::run_auth_command(provider).await;
    }

    if let Some(Command::Load { file, hotel }) = args.command {
        return run_load_command(&file, &hotel).await;
    }

    info!("Starting Philotic Ansible Daemon Boot Sequence...");

    let flags = AnsibleCutoverFlags::from_env();
    info!("--- CUTOVER FLAGS ---");
    info!(
        "Rust Auth Validation: {}",
        if flags.enable_rust_auth {
            "ENABLED"
        } else {
            "PASSTHROUGH"
        }
    );
    info!(
        "Rust Outbound Dispatcher: {}",
        if flags.enable_rust_dispatcher {
            "ENABLED"
        } else {
            "DISABLED"
        }
    );
    info!(
        "Rust Task Lifecycle Ledger: {}",
        if flags.enable_rust_task_lifecycle {
            "ENABLED"
        } else {
            "DISABLED"
        }
    );
    info!("---------------------");

    // Initialize the always-on Context Graph DB via the abstract storage trait
    // When PHILOTIC_PROFILE is set, namespace the DB into ~/.philotic/<profile>/.
    // Otherwise fall back to the legacy relative path for backward compatibility.
    let db_path_buf;
    let db_path: &Path = if let Some(ref pdir) = profile_dir() {
        fs::create_dir_all(pdir)
            .with_context(|| format!("create profile dir {}", pdir.display()))?;
        db_path_buf = pdir.join("context.db");
        info!(
            "Profile: {}  (DB: {})",
            std::env::var("PHILOTIC_PROFILE").unwrap_or_default(),
            db_path_buf.display()
        );
        &db_path_buf
    } else {
        Path::new("aiua_context.db")
    };
    let _graph_storage = ansible_mesh_core::sqlite_storage::SqliteGraphStorage::open(db_path)?;
    let graph_domain_arc = Arc::new(GraphDomain::new(Arc::new(_graph_storage.adapter())));

    // Open (or create) the training DB alongside the context DB.
    let training_db_path_buf;
    let training_db_path: &Path = if let Some(ref pdir) = profile_dir() {
        training_db_path_buf = pdir.join("training.db");
        &training_db_path_buf
    } else {
        Path::new("whisper_training.db")
    };
    let training_storage: Arc<dyn ansible_mesh_core::whisper_training::WhisperTrainingStorage> =
        Arc::new(
            ansible_mesh_core::whisper_training::SqliteWhisperTrainingStorage::open(
                training_db_path,
            )?,
        );

    let hotel_name = args
        .hotel
        .context("--hotel is required unless using a subcommand such as `aiua load`")?;

    let seeded_guests = graph_domain_arc.list_guests(&hotel_name, true)?;
    if seeded_guests.is_empty() {
        warn!(
            "Hotel '{}' has no seeded guests. Run `aiua load --file <config.json> --hotel {}` to provision.",
            hotel_name, hotel_name
        );
    } else {
        info!(
            "Hotel '{}' booting with {} seeded guest(s): {}",
            hotel_name,
            seeded_guests.len(),
            seeded_guests
                .iter()
                .map(|g| g.guest_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let startup_test = args.test;
    if graph_domain_arc.get_hotel(&hotel_name)?.is_none() {
        info!(
            "Hotel '{}' is missing from the Context Graph. Run `aiua load` to provision.",
            hotel_name
        );
    }

    let mut hotel = reconcile_hotel_record(&graph_domain_arc, &hotel_name)?;

    seed_abstract_tool_catalog(&graph_domain_arc)?;
    seed_abstract_skill_catalog(&graph_domain_arc)?;
    seed_toolset_profiles(&graph_domain_arc)?;
    seed_skill_crafting(&graph_domain_arc)?;

    if let Some(test) = startup_test {
        prepare_startup_test_binaries(test)?;
        enable_guest_test_overrides(&graph_domain_arc, &hotel_name, test)?;
    }

    if let Some(active_pid) = hotel.active_pid.as_deref() {
        if let Ok(pid) = active_pid.parse::<u32>() {
            if pid_exists(pid) {
                anyhow::bail!(
                    "Hotel '{}' is already running with PID {}. Stop that instance before starting another.",
                    hotel_name,
                    pid
                );
            }
        }
        graph_domain_arc.set_hotel_pid(&hotel_name, None)?;
        hotel.active_pid = None;
    }

    let smoke_mode = smoke_mode_enabled();
    let mesh_enabled = !smoke_mode;

    if !smoke_mode {
        let (resolved_mesh_port, resolved_blob_port, resolved_execution_port) =
            resolve_runtime_ports(&hotel, mesh_enabled)?;
        if resolved_mesh_port != hotel.mesh_port
            || resolved_blob_port != hotel.blob_port
            || resolved_execution_port != hotel.execution_port
        {
            warn!(
                hotel = %hotel_name,
                mesh_enabled,
                desired_mesh_port = hotel.mesh_port,
                desired_blob_port = hotel.blob_port,
                desired_execution_port = hotel.execution_port,
                resolved_mesh_port,
                resolved_blob_port,
                resolved_execution_port,
                "Preferred hotel runtime ports unavailable; using nearest available cluster"
            );
            hotel.mesh_port = resolved_mesh_port;
            hotel.blob_port = resolved_blob_port;
            hotel.execution_port = resolved_execution_port;
            graph_domain_arc.upsert_hotel(&hotel)?;

            // Patch PHILOTIC_BLOB_BASE_URL in all membrane guest configs so they
            // use the newly resolved blob port instead of the stale stored URL.
            let new_blob_base_url = format!("http://127.0.0.1:{}", resolved_blob_port);
            if let Ok(guests) = graph_domain_arc.list_guests(&hotel_name, false) {
                for mut guest in guests {
                    if guest.role != "membrane" {
                        continue;
                    }
                    if let Ok(mut cfg) =
                        serde_json::from_str::<serde_json::Value>(&guest.config_json)
                    {
                        if let Some(env) = cfg
                            .as_object_mut()
                            .and_then(|o| o.get_mut("env"))
                            .and_then(serde_json::Value::as_object_mut)
                        {
                            env.insert(
                                "PHILOTIC_BLOB_BASE_URL".into(),
                                serde_json::Value::String(new_blob_base_url.clone()),
                            );
                            guest.config_json = cfg.to_string();
                            if let Err(e) = graph_domain_arc.seed_guests(&hotel_name, &[guest]) {
                                warn!("Failed to patch membrane blob URL: {e}");
                            } else {
                                info!(
                                    blob_url = %new_blob_base_url,
                                    "Patched membrane guest blob URL after port shift."
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    let current_pid = std::process::id().to_string();
    graph_domain_arc.set_hotel_pid(&hotel_name, Some(&current_pid))?;
    hotel.active_pid = Some(current_pid.clone());

    let caps = hotel.capabilities.clone();
    let mesh_port = hotel.mesh_port;
    let addr = format!("0.0.0.0:{}", mesh_port);
    info!(
        "Starting Philotic Ansible Daemon for hotel '{}' as node '{}' on {}",
        hotel_name, caps.node_id, addr
    );

    // Boot-time MuninnDB config load (Slice D).
    // Returns None if no vault registry is configured; guests fall back to NullMemoryEngine.
    let muninn_config_arc: Option<Arc<memory_core::MuninnConfig>> = match memory::load_muninn_config(
        &graph_domain_arc,
    ) {
        Ok(Some(cfg)) => {
            info!(endpoint = %cfg.base_url, vaults = cfg.vault_tokens.len(), "MuninnDB configured");
            Some(Arc::new(cfg))
        }
        Ok(None) => {
            info!("MuninnDB not configured — guests will use NullMemoryEngine");
            None
        }
        Err(e) => {
            warn!(error = %e, "Failed to load MuninnDB config — continuing without memory");
            None
        }
    };

    if smoke_mode {
        warn!(
            "PHILOTIC_SMOKE_MODE enabled: starting local-only IPC runtime without mesh or guest materialization."
        );

        let (dispatcher_tx, mut dispatcher_rx) = mpsc::channel::<LedgerCommand>(1024);
        std::thread::spawn(move || while let Some(_) = dispatcher_rx.blocking_recv() {});

        let ipc_server = IpcServer::new(
            hotel.ipc_socket_path.clone(),
            caps.node_id.clone(),
            dispatcher_tx,
            graph_domain_arc.clone(),
        )
        .with_memory_config(muninn_config_arc.clone())
        .with_training_storage(training_storage.clone());
        tokio::spawn(async move {
            if let Err(e) = ipc_server.run().await {
                error!("Hotel Front Desk (UDS) failed: {}", e);
            }
        });

        tokio::signal::ctrl_c().await?;
        let _ = graph_domain_arc.set_hotel_pid(&hotel_name, None);
        info!("Ansible smoke-mode shutdown complete.");
        return Ok(());
    }

    // Channel for inbound mesh UDP payloads bubbled up by the BeaconDaemon
    let (inbox_tx, inbox_rx) = mpsc::channel::<ansible_mesh_core::BeaconMessage>(1024);
    let inbox_rx = Arc::new(Mutex::new(Some(inbox_rx)));

    // Channel for pushing generated SDP Answers back out to the mesh
    let (webrtc_signal_tx, webrtc_signal_rx) =
        mpsc::channel::<ansible_mesh_core::webrtc::WebRtcSignalMessage>(32);
    let webrtc_signal_rx = Arc::new(Mutex::new(Some(webrtc_signal_rx)));

    // Broadcast channel to tell tasks to kill their child process on shutdown
    let (shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel::<()>(16);

    // Spawning the "Hotel Front Desk" local IPC listener for Materialized Guests
    let socket_path = hotel.ipc_socket_path.clone();
    let execution_addr = format!("0.0.0.0:{}", hotel.execution_port);
    let execution_enable_rust_auth = flags.enable_rust_auth;

    // Create the memory channel dispatcher for PORT-BP-003 to pick up
    // In PORT-BP-003, this receiver will hand off to the persistent mesh_events ledger
    let (dispatcher_tx, mut dispatcher_rx) = mpsc::channel::<LedgerCommand>(1024);

    // PORT-BP-004: Strictly Serialized Single Writer Thread for Durable Event Ledger
    let db_path_writer = db_path.to_owned();

    // Initialize Mutable State Components First
    let ledger: Arc<dyn EventStorage> = Arc::new(
        match ansible_mesh_core::sqlite_storage::SqliteEventStorage::open(&db_path_writer) {
            Ok(l) => l,
            Err(e) => {
                error!("Failed to open Event Ledger: {}", e);
                std::process::exit(1);
            }
        },
    );

    let tracker: Arc<dyn CursorStorage> = Arc::new(
        match ansible_mesh_core::sqlite_storage::SqliteCursorStorage::open(&db_path_writer) {
            Ok(t) => t,
            Err(e) => {
                error!("Failed to open Cursor Tracker: {}", e);
                std::process::exit(1);
            }
        },
    );

    // Extract writer thread clones
    let ledger_writer = ledger.clone();
    let tracker_writer = tracker.clone();

    if flags.enable_rust_task_lifecycle {
        std::thread::spawn(move || {
            info!("Durable Event Ledger Writer Thread spanning up...");
            while let Some(cmd) = dispatcher_rx.blocking_recv() {
                match cmd {
                    LedgerCommand::AppendLocal(mut evt) => {
                        if let Err(e) = ledger_writer.append_event(&mut evt) {
                            error!(
                                "Failed to durably commit local event {}: {}",
                                evt.event_id, e
                            );
                        }
                    }
                    LedgerCommand::CommitInboundBatch {
                        events,
                        source_node: _,
                    } => {
                        let mut max_seq = 0;
                        for mut evt in events {
                            if evt.seq > max_seq {
                                max_seq = evt.seq;
                            }
                            if let Err(e) = ledger_writer.append_event(&mut evt) {
                                error!(
                                    "Failed to durably commit inbound event {}: {}",
                                    evt.event_id, e
                                );
                            }
                        }
                        // Typically we would now trigger an ACK back to source_node with max_seq
                        // For MVP, that logic is built into the mesh receiver hook.
                    }
                    LedgerCommand::ProcessAck {
                        consumer_node_id,
                        acked_seq,
                    } => {
                        let ts = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_millis() as u64;
                        if let Err(e) =
                            tracker_writer.advance_cursor(&consumer_node_id, acked_seq, ts)
                        {
                            error!(
                                "Failed to advance cursor for node {}: {}",
                                consumer_node_id, e
                            );
                        } else {
                            info!(
                                "Cursor for node {} advanced to seq {}",
                                consumer_node_id, acked_seq
                            );
                        }
                    }
                }
            }
        });
    } else {
        std::thread::spawn(move || {
            // Drain queue silently to prevent backpressure in passthrough mode
            while let Some(_) = dispatcher_rx.blocking_recv() {}
        });
    }

    // Abstracted Universal Materializer with trait-object storage
    let materializer = Box::new(crate::service::guest_manager::LocalProcessMaterializer::new());
    let guest_manager = Arc::new(crate::service::guest_manager::GuestManager::new(
        hotel_name.clone(),
        graph_domain_arc.clone(),
        materializer,
    ));

    let registry = Arc::new(RwLock::new(NodeRegistry::new()));

    // In-process channel: operator surface query tasks are delivered here
    // instead of through UDS self-connection, eliminating the socket leak.
    let (operator_surface_tx, operator_surface_rx) = tokio::sync::mpsc::channel::<String>(128);

    let ipc_server = IpcServer::new(
        socket_path.clone(),
        caps.node_id.clone(),
        dispatcher_tx.clone(),
        graph_domain_arc.clone(),
    )
    .with_memory_config(muninn_config_arc.clone())
    .with_training_storage(training_storage.clone())
    .with_materialization_requester(guest_manager.clone())
    .with_registry(registry.clone())
    .with_operator_surface_channel(operator_surface_tx);
    let ipc_inboxes = ipc_server.inboxes();
    let ipc_parked_inbound = ipc_server.parked_inbound();
    let ipc_materialization_requester = ipc_server.materialization_requester_arc();
    let network_broadcast_tx = ipc_server.network_broadcast_tx();

    tokio::spawn(async move {
        if let Err(e) = ipc_server.run().await {
            error!("Hotel Front Desk (UDS) failed: {}", e);
        }
    });

    // Network reachability monitor: TCP-probe 1.1.1.1:53 and broadcast NetworkState
    // to all connected guests when the online/offline state changes.
    tokio::spawn(async move {
        use philotic_client::IpcResponse;
        use std::time::Duration;
        let mut last_online: Option<bool> = None;
        loop {
            let online = tokio::time::timeout(
                Duration::from_secs(3),
                tokio::net::TcpStream::connect("1.1.1.1:53"),
            )
            .await
            .map(|r| r.is_ok())
            .unwrap_or(false);

            if last_online != Some(online) {
                if last_online.is_some() {
                    info!(
                        online,
                        "Network reachability changed — broadcasting NetworkState to guests."
                    );
                }
                last_online = Some(online);
                // Ignore send errors: no subscribers yet is fine.
                let _ = network_broadcast_tx.send(IpcResponse::NetworkState { online });
            }

            // Poll every 30s when online, every 5s when offline (faster recovery).
            let interval = if online { 30 } else { 5 };
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    });

    {
        let cron_offset_ms = std::env::var("PHILOTIC_CRON_OFFSET_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            * 1000;
        let cron_ticker = CronTicker::new(
            graph_domain_arc.clone(),
            dispatcher_tx.clone(),
            ipc_inboxes.clone(),
            caps.node_id.clone(),
            cron_offset_ms,
        );
        tokio::spawn(async move {
            cron_ticker.run().await;
        });
    }

    tokio::spawn(run_operator_surface_query_worker(
        operator_surface_rx,
        socket_path.clone(),
        caps.node_id.clone(),
        shutdown_rx.resubscribe(),
    ));

    let mesh_runtime = MeshRuntimeContext {
        hotel_name: hotel_name.clone(),
        hotel: hotel.clone(),
        caps: caps.clone(),
        mesh_addr: addr.clone(),
        execution_addr: execution_addr.clone(),
        db_path: db_path.to_string_lossy().to_string(),
        enable_rust_auth: execution_enable_rust_auth,
        enable_rust_dispatcher: flags.enable_rust_dispatcher,
        graph_domain: graph_domain_arc.clone(),
        registry: registry.clone(),
        ledger: ledger.clone(),
        tracker: tracker.clone(),
        dispatcher_tx: dispatcher_tx.clone(),
        ipc_inboxes: ipc_inboxes.clone(),
        ipc_parked_inbound: ipc_parked_inbound.clone(),
        ipc_materialization_requester: ipc_materialization_requester.clone(),
        shutdown_tx: shutdown_tx.clone(),
        inbox_tx: inbox_tx.clone(),
        inbox_rx: inbox_rx.clone(),
        webrtc_signal_tx: webrtc_signal_tx.clone(),
        webrtc_signal_rx: webrtc_signal_rx.clone(),
    };

    if let Err(e) = activate_mesh_runtime(mesh_runtime.clone()).await {
        let _ = graph_domain_arc.set_hotel_pid(&hotel_name, None);
        return Err(e);
    }

    // RESOURCE BROKER BOOT RECONCILIATION (transitional — Seam 2 / demand-derived-materialization)
    // Reads agents from the context graph, replays their static_resource_declarations through the
    // resource registry, and logs the demand-derived guest set. Does not yet replace the
    // materialize_all path below; that replacement lands when the registry is proven stable.
    {
        use crate::service::resource_registry::{ResourceRegistry, boot_reconcile};
        let mut resource_registry = ResourceRegistry::new();
        match graph_domain_arc.list_agent_identities() {
            Ok(agents) => {
                let results = boot_reconcile(&mut resource_registry, &agents);
                info!(
                    agents_with_declarations = results.len(),
                    resource_instances = resource_registry.instance_count(),
                    "demand-derived-materialization: reconciliation complete (transitional)"
                );
            }
            Err(e) => {
                warn!("demand-derived-materialization: could not load agent identities: {e}");
            }
        }
    }

    // MATERIALIZATION LOOP: Spin up all guests defined in the DB as child processes
    info!("--- BEGIN UNIVERSAL MATERIALIZATION ---");

    // Give the front desk a moment to bind the UDS path before guests attempt to register.
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

    if let Err(e) = guest_manager
        .materialize_all(shutdown_rx.resubscribe())
        .await
    {
        error!("Universal Materialization failed: {}", e);
    }

    if startup_test.is_some() {
        stabilize_startup_test_guests(&guest_manager, &graph_domain_arc, &hotel_name, &shutdown_rx)
            .await?;
    }

    let supervision_enabled = guest_supervision_enabled() || startup_test.is_some();
    if supervision_enabled {
        let gm_clone = Arc::clone(&guest_manager);
        let rx_supervise = shutdown_rx.resubscribe();
        tokio::spawn(async move {
            gm_clone.supervise_guests(rx_supervise).await;
        });
    } else {
        warn!(
            "Guest supervisor loop is disabled by default until guest heartbeats are implemented."
        );
    }

    if let Some(test) = startup_test {
        let test_result = run_startup_test(
            test,
            &hotel_name,
            &hotel.ipc_socket_path,
            args.test_output.as_deref(),
            args.test_text.as_deref(),
        )
        .await;
        let _ = shutdown_tx.send(());
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        let _ = graph_domain_arc.set_hotel_pid(&hotel_name, None);
        return test_result;
    }

    // PORT-BP-005: Large Payload Transport via Dedicated HTTP Server
    let blob_port = hotel.blob_port;
    let blob_addr = format!("0.0.0.0:{}", blob_port);
    let blob_dir = std::path::Path::new(db_path)
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("blobs");
    let blob_service = BlobService::new(blob_dir);
    tokio::spawn(async move {
        if let Err(e) = blob_service.serve(&blob_addr).await {
            error!("Blob HTTP Server failed: {}", e);
        }
    });
    // Wait for either Ctrl-C or SIGTERM.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut sigterm = signal(SignalKind::terminate())?;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                warn!("Ctrl-C received — initiating graceful drain.");
            }
            _ = sigterm.recv() => {
                warn!("SIGTERM received — initiating graceful drain.");
            }
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;

    // Phase 1: signal every registered guest to drain in-flight work and exit.
    const DRAIN_TIMEOUT_SECS: u64 = 30;
    {
        let guard = ipc_inboxes.lock().await;
        let mut count = 0usize;
        for subscribers in guard.values() {
            for sub in subscribers {
                let _ = sub.tx.send(philotic_client::IpcResponse::GracefulShutdown {
                    drain_timeout_secs: DRAIN_TIMEOUT_SECS,
                });
                count += 1;
            }
        }
        info!(
            "Graceful drain signal sent to {} guest subscriber(s).",
            count
        );
    }

    // Phase 2: wait for guest PIDs to exit (poll every 500ms, up to DRAIN_TIMEOUT_SECS).
    let drain_deadline =
        tokio::time::Instant::now() + tokio::time::Duration::from_secs(DRAIN_TIMEOUT_SECS);
    loop {
        let all_gone = {
            let guard = ipc_inboxes.lock().await;
            guard.values().all(|subs| subs.is_empty())
        };
        if all_gone || tokio::time::Instant::now() >= drain_deadline {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    }
    info!("All guest subscribers drained (or drain window elapsed). Shutting down hotel.");

    // DreamsPhase: semantic consolidation + Hebbian sweep across all agent vaults.
    // Runs after guests drain, before the internal shutdown broadcast.
    // Uses direct HTTP to ONNX sidecar (:11435) and Ollama (:11434) — no IPC needed.
    if let Some(ref cfg) = muninn_config_arc {
        dream::dream_sweep(cfg, &graph_domain_arc, &hotel_name).await;
    }

    let _ = shutdown_tx.send(());
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
    let _ = graph_domain_arc.set_hotel_pid(&hotel_name, None);
    info!("Ansible Daemon shutdown complete.");

    let _ = graph_domain_arc.set_hotel_pid(&hotel_name, None);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        AgentProfile, StartupTest, agent_identity_record_for_profile, agent_profile_from_config,
        all_agent_profiles_from_config, deactivate_legacy_managed_guests,
        default_agent_profile_for_hotel, default_guest_seed, default_hotel_record,
        enable_guest_test_overrides, execution_reachability_for_hotel,
        extract_context_graph_entries, guest_seed_for_profile, guest_supervision_enabled,
        hotel_base_port, hotel_ipc_socket_path, local_capability_advertisements,
        nearest_available_base_port, resolve_runtime_ports, startup_test_gemini_base_url,
    };
    use ansible_mesh_core::domain::GraphDomain;
    use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
    use ansible_mesh_core::storage::{GuestRecord, HotelRecord};
    use std::sync::Arc;

    #[test]
    fn guest_supervision_defaults_disabled() {
        unsafe {
            std::env::remove_var("PHILOTIC_ENABLE_GUEST_SUPERVISOR");
        }
        assert!(!guest_supervision_enabled());
    }

    #[test]
    fn default_hotel_record_is_deterministic_and_namespaced() {
        let hotel = default_hotel_record("alpha-hotel");
        assert_eq!(hotel.hotel_name, "alpha-hotel");
        assert_eq!(hotel.capabilities.node_id, "alpha-hotel-aiua-01");
        assert_eq!(hotel.ipc_socket_path, "/tmp/philotic-alpha-hotel.sock");
        assert_eq!(hotel.mesh_port, hotel_base_port("alpha-hotel"));
        assert_eq!(hotel.blob_port, hotel.mesh_port + 1);
        assert_eq!(hotel.execution_port, hotel.mesh_port + 2);
    }

    #[test]
    fn profile_socket_path_is_hotel_scoped() {
        unsafe {
            std::env::set_var("HOME", "/tmp/codex-home");
            std::env::set_var("PHILOTIC_PROFILE", "jane");
        }

        assert_eq!(
            hotel_ipc_socket_path("default"),
            "/tmp/codex-home/.philotic/jane/aiua-default.sock"
        );
        assert_eq!(
            hotel_ipc_socket_path("second-hotel"),
            "/tmp/codex-home/.philotic/jane/aiua-second-hotel.sock"
        );

        unsafe {
            std::env::remove_var("PHILOTIC_PROFILE");
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn nearest_available_base_port_prefers_closest_upward_match() {
        let resolved = nearest_available_base_port(100, |base| base == 101 || base == 103);
        assert_eq!(resolved, Some(101));
    }

    #[test]
    fn resolve_runtime_ports_ignores_mesh_port_when_mesh_is_dormant() {
        let hotel = HotelRecord {
            mesh_port: 30_000,
            blob_port: 30_001,
            execution_port: 30_002,
            ..default_hotel_record("default")
        };

        let resolved = resolve_runtime_ports(&hotel, false).expect("ports should resolve");

        assert_eq!(resolved, (30_000, 30_001, 30_002));
    }

    #[test]
    fn default_guest_seed_injects_hotel_socket_env() {
        let guests = default_guest_seed("beta-hotel");
        assert_eq!(guests.len(), 12); // shared: membrane, model-gemini, model-elevenlabs, model-onnx, tool-runner, graph-runner, graph-datasource, table-datasource, router-listener, mcp-membrane; profile: agent, agent-graph-runner
        // Membrane is the first guest from hotel_shared_guests
        let membrane = guests
            .iter()
            .find(|g| g.role == "membrane")
            .expect("membrane");
        let config: serde_json::Value = serde_json::from_str(&membrane.config_json).unwrap();
        assert_eq!(
            config["env"]["PHILOTIC_HOTEL_SOCKET"].as_str(),
            Some("/tmp/philotic-beta-hotel.sock")
        );
        assert!(guests.iter().all(|guest| guest.hotel_name == "beta-hotel"));
        assert!(guests.iter().any(|guest| guest.role == "model"));
        assert!(guests.iter().any(|guest| guest.role == "model.elevenlabs"));
        assert!(guests.iter().any(|guest| guest.role == "tool"));
        // Single membrane uses PHILOTIC_AGENT_ROSTER (not per-agent token key)
        let roster_json = config["env"]["PHILOTIC_AGENT_ROSTER"]
            .as_str()
            .expect("roster");
        let roster: Vec<serde_json::Value> =
            serde_json::from_str(roster_json).expect("parse roster");
        assert!(!roster.is_empty());
        assert_eq!(roster[0]["agent_key"].as_str(), Some("beta"));
        assert_eq!(roster[0]["agent_id"].as_str(), Some("agent-beta-01"));
    }

    #[test]
    fn guest_seed_for_profile_targets_profile_agent_and_token_key() {
        let guests = guest_seed_for_profile(
            "beacon-test-hotel",
            &AgentProfile {
                agent_key: "beacon".into(),
                agent_id: "agent-beacon-01".into(),
                persona_name: "Beacon".into(),
                import_workspace: None,
                is_admin: false,
                orchestrator_turn_loop_config: None,
            },
        );
        let membrane_guest = guests
            .iter()
            .find(|g| g.role == "membrane")
            .expect("membrane");
        let agent_guest = guests.iter().find(|g| g.role == "agent").expect("agent");
        let membrane: serde_json::Value =
            serde_json::from_str(&membrane_guest.config_json).expect("membrane config");
        let agent: serde_json::Value =
            serde_json::from_str(&agent_guest.config_json).expect("agent config");

        // Single membrane uses PHILOTIC_AGENT_ROSTER; agent_id is embedded in the roster
        let roster_json = membrane["env"]["PHILOTIC_AGENT_ROSTER"]
            .as_str()
            .expect("roster");
        let roster: Vec<serde_json::Value> =
            serde_json::from_str(roster_json).expect("parse roster");
        assert_eq!(roster[0]["agent_key"].as_str(), Some("beacon"));
        assert_eq!(roster[0]["agent_id"].as_str(), Some("agent-beacon-01"));

        assert_eq!(
            agent["env"]["PHILOTIC_AGENT_ID"].as_str(),
            Some("agent-beacon-01")
        );
        assert!(agent_guest.guest_id.contains("beacon"));
    }

    #[test]
    fn local_capability_advertisements_include_hotel_scoped_incarnations() {
        let storage = SqliteGraphStorage::open(":memory:").expect("open sqlite graph");
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        let hotel = default_hotel_record("aria-architect-hotel");
        let guests = default_guest_seed("aria-architect-hotel");
        graph
            .seed_guests("aria-architect-hotel", &guests)
            .expect("seed guests");

        let ads = local_capability_advertisements(&graph, &hotel).expect("ads should build");
        // Only active guests generate advertisements; tool-runner is seeded with is_active=false.
        let active_guest_count = guests.iter().filter(|g| g.is_active).count();
        assert_eq!(ads.len(), active_guest_count);
        assert!(ads.iter().all(|ad| ad.hotel_id == "aria-architect-hotel"));
        assert!(
            ads.iter()
                .all(|ad| ad.incarnation_id.starts_with("aria-architect-hotel:"))
        );
        assert!(
            ads.iter()
                .all(|ad| ad.selection_hint.as_deref() == Some("local_materialization_required"))
        );
    }

    #[test]
    fn execution_reachability_prefers_configured_host() {
        let storage = SqliteGraphStorage::open(":memory:").expect("open sqlite graph");
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        graph
            .set_config_value("execution_host", &serde_json::json!("jane-vps").to_string())
            .expect("set execution host");
        let hotel = default_hotel_record("default");

        let reachability = execution_reachability_for_hotel(&graph, &hotel);

        assert_eq!(reachability.protocol, "tcp-framed-v1");
        assert_eq!(reachability.host, "jane-vps");
        assert_eq!(reachability.port, hotel.execution_port);
    }

    #[test]
    fn execution_reachability_falls_back_to_hotel_mesh_host() {
        let storage = SqliteGraphStorage::open(":memory:").expect("open sqlite graph");
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        let mut hotel = default_hotel_record("default");
        hotel.mesh_host = Some("100.64.230.106".into());

        let reachability = execution_reachability_for_hotel(&graph, &hotel);

        assert_eq!(reachability.protocol, "tcp-framed-v1");
        assert_eq!(reachability.host, "100.64.230.106");
        assert_eq!(reachability.port, hotel.execution_port);
    }

    #[test]
    fn context_graph_entries_support_nested_section() {
        let entries = extract_context_graph_entries(
            &serde_json::json!({
                "context_graph": {
                    "telegram_bot_token": "token",
                    "elevenlabs_api_key": "key"
                },
                "ignored": {
                    "not": "imported"
                }
            }),
            None,
        );

        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|(key, _)| key == "telegram_bot_token"));
        assert!(entries.iter().any(|(key, _)| key == "elevenlabs_api_key"));
    }

    #[test]
    fn context_graph_entries_merge_default_and_hotel_specific_sections() {
        let entries = extract_context_graph_entries(
            &serde_json::json!({
                "context_graph": {
                    "gemini_api_key": "shared"
                },
                "hotels": {
                    "default": {
                        "agents": {
                            "jane": {
                                "telegram": {
                                    "bot_token": "jane-token",
                                    "allowed_users": ["JaneHegemonBot"]
                                },
                                "model": {
                                    "default_model": "gemini-pro"
                                }
                            }
                        }
                    },
                    "aria-architect-hotel": {
                        "agents": {
                            "aria": {
                                "telegram": {
                                    "bot_token": "aria-token",
                                    "allowed_users": ["AriaArchitectBot"]
                                },
                                "model": {
                                    "default_model": "gemini-2.5-pro"
                                }
                            }
                        }
                    }
                }
            }),
            Some("aria-architect-hotel"),
        );

        assert!(
            entries.iter().any(|(key, value)| {
                key == "gemini_api_key" && value.as_str() == Some("shared")
            })
        );
        assert!(entries.iter().any(|(key, value)| {
            key == "telegram_bot_token" && value.as_str() == Some("aria-token")
        }));
        assert!(entries.iter().any(|(key, value)| {
            key == "telegram_allowed_users"
                && value
                    .as_array()
                    .is_some_and(|arr| arr.len() == 1 && arr[0] == "AriaArchitectBot")
        }));
        assert!(entries.iter().any(|(key, value)| {
            key == "default_model" && value.as_str() == Some("gemini-2.5-pro")
        }));
    }

    #[test]
    fn all_agent_profiles_returns_all_agents_in_hotel() {
        let config = serde_json::json!({
            "hotels": {
                "default": {
                    "agents": {
                        "jane": { "agent_id": "agent-jane", "persona_name": "Jane" },
                        "aria": { "agent_id": "agent-aria", "persona_name": "Aria" },
                        "beacon": { "agent_id": "agent-beacon", "persona_name": "Beacon" }
                    }
                }
            }
        });
        let profiles = all_agent_profiles_from_config(&config, "default");
        assert_eq!(profiles.len(), 3);
        let names: std::collections::HashSet<&str> =
            profiles.iter().map(|p| p.persona_name.as_str()).collect();
        assert!(names.contains("Jane"));
        assert!(names.contains("Aria"));
        assert!(names.contains("Beacon"));
    }

    #[test]
    fn non_default_hotel_does_not_inherit_default_agents() {
        let config = serde_json::json!({
            "hotels": {
                "default": {
                    "agents": {
                        "bjork": { "agent_id": "agent-bjork-01", "persona_name": "Bjork" }
                    }
                },
                "second-hotel": {
                    "agents": {
                        "aria": { "agent_id": "agent-coach-01", "persona_name": "Coach" }
                    }
                }
            }
        });

        let profiles = all_agent_profiles_from_config(&config, "second-hotel");
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].agent_key, "aria");
        assert_eq!(profiles[0].persona_name, "Coach");
    }

    #[test]
    fn multi_agent_context_graph_stores_per_agent_token_keys() {
        let config = serde_json::json!({
            "hotels": {
                "default": {
                    "agents": {
                        "jane": {
                            "telegram": { "bot_token": "jane-token", "allowed_users": ["jared"] }
                        },
                        "aria": {
                            "telegram": { "bot_token": "aria-token", "allowed_users": ["jared"] }
                        }
                    }
                }
            }
        });
        let entries = extract_context_graph_entries(&config, Some("default"));
        // Per-agent keys must be present
        assert!(
            entries
                .iter()
                .any(|(k, v)| k == "telegram_bot_token_jane" && v.as_str() == Some("jane-token"))
        );
        assert!(
            entries
                .iter()
                .any(|(k, v)| k == "telegram_bot_token_aria" && v.as_str() == Some("aria-token"))
        );
    }

    #[test]
    fn non_default_hotel_context_entries_do_not_import_default_agent_tokens() {
        let config = serde_json::json!({
            "hotels": {
                "default": {
                    "agents": {
                        "bjork": {
                            "telegram": { "bot_token": "bjork-token", "allowed_users": ["likesjx"] }
                        }
                    }
                },
                "second-hotel": {
                    "agents": {
                        "aria": {
                            "telegram": { "bot_token": "coach-token", "allowed_users": ["likesjx"] }
                        }
                    }
                }
            }
        });

        let entries = extract_context_graph_entries(&config, Some("second-hotel"));
        assert!(
            entries
                .iter()
                .any(|(k, v)| k == "telegram_bot_token_aria" && v.as_str() == Some("coach-token"))
        );
        assert!(!entries.iter().any(|(k, _)| k == "telegram_bot_token_bjork"));
    }

    #[test]
    fn guest_seed_uses_per_agent_token_key() {
        let profile = AgentProfile {
            agent_key: "aria".into(),
            agent_id: "agent-aria".into(),
            persona_name: "Aria".into(),
            import_workspace: None,
            is_admin: false,
            orchestrator_turn_loop_config: None,
        };
        let guests = guest_seed_for_profile("default", &profile);
        let membrane = guests
            .iter()
            .find(|g| g.role == "membrane")
            .expect("membrane guest");
        let config: serde_json::Value =
            serde_json::from_str(&membrane.config_json).expect("parse membrane config");
        // Token keys are now embedded in PHILOTIC_AGENT_ROSTER; membrane resolves them at runtime
        let roster_json = config["env"]["PHILOTIC_AGENT_ROSTER"]
            .as_str()
            .expect("roster");
        let roster: Vec<serde_json::Value> =
            serde_json::from_str(roster_json).expect("parse roster");
        assert_eq!(roster[0]["agent_key"].as_str(), Some("aria"));
        assert_eq!(roster[0]["agent_id"].as_str(), Some("agent-aria"));
    }

    #[test]
    fn system_prompt_used_as_identity_text_when_no_workspace() {
        let profile = AgentProfile {
            agent_key: "beacon".into(),
            agent_id: "agent-beacon".into(),
            persona_name: "Beacon".into(),
            import_workspace: None,
            is_admin: false,
            orchestrator_turn_loop_config: None,
        };
        let mut agent_config = serde_json::Map::new();
        agent_config.insert(
            "system_prompt".into(),
            "You are Beacon, Chief of Staff.".into(),
        );
        let identity = agent_identity_record_for_profile(&profile, "default", Some(&agent_config));
        let identity_text = identity.bundle_json["identity_text"].as_str().unwrap_or("");
        assert_eq!(identity_text, "You are Beacon, Chief of Staff.");
    }

    #[test]
    fn workspace_identity_text_takes_precedence_over_system_prompt() {
        // If the workspace supplied identity_text, system_prompt must not overwrite it.
        let profile = AgentProfile {
            agent_key: "beacon".into(),
            agent_id: "agent-beacon".into(),
            persona_name: "Beacon".into(),
            // Workspace path that doesn't exist — bundle will be empty
            import_workspace: None,
            is_admin: false,
            orchestrator_turn_loop_config: None,
        };
        let mut agent_config = serde_json::Map::new();
        agent_config.insert("system_prompt".into(), "Fallback prompt.".into());
        // Manually inject identity_text as if a workspace had provided it
        let identity = {
            let mut bundle = serde_json::json!({ "identity_text": "Workspace identity." });
            let bundle_obj = bundle.as_object_mut().unwrap();
            bundle_obj.insert("system_prompt".into(), "Fallback prompt.".into());
            // system_prompt should NOT overwrite the existing identity_text
            let has_identity = bundle_obj
                .get("identity_text")
                .map(|v| !v.is_null() && v.as_str().is_some_and(|s| !s.is_empty()))
                .unwrap_or(false);
            if !has_identity {
                if let Some(sp) = agent_config.get("system_prompt").and_then(|v| v.as_str()) {
                    bundle_obj.insert("identity_text".to_string(), sp.into());
                }
            }
            bundle
        };
        assert_eq!(
            identity["identity_text"].as_str(),
            Some("Workspace identity.")
        );
    }

    #[test]
    fn context_graph_entries_support_hotel_level_telegram_overlay() {
        let entries = extract_context_graph_entries(
            &serde_json::json!({
                "hotels": {
                    "default": {
                        "telegram": {
                            "bot_token": "fallback-token",
                            "allowed_users": ["shared-bot"]
                        }
                    }
                }
            }),
            Some("beta-hotel"),
        );

        assert!(entries.iter().any(|(key, value)| {
            key == "telegram_bot_token" && value.as_str() == Some("fallback-token")
        }));
        assert!(entries.iter().any(|(key, value)| {
            key == "telegram_allowed_users"
                && value
                    .as_array()
                    .is_some_and(|arr| arr.len() == 1 && arr[0] == "shared-bot")
        }));
    }

    #[test]
    fn configured_agent_identity_reads_import_workspace_for_selected_hotel() {
        let config = serde_json::json!({
            "hotels": {
                "beacon-test-hotel": {
                    "agents": {
                        "beacon": {
                            "agent_id": "agent-beacon-01",
                            "persona_name": "Beacon",
                            "import_workspace": "/tmp/aria-workspace"
                        }
                    }
                }
            }
        });

        let identity = super::configured_agent_identity_from_config(&config, "beacon-test-hotel")
            .expect("beacon import workspace should be detected");
        assert_eq!(identity.agent_id, "agent-beacon-01");
        assert_eq!(identity.persona_name, "Beacon");
        assert_eq!(
            identity.bundle_json["workspace_path"].as_str(),
            Some("/tmp/aria-workspace")
        );
    }

    #[test]
    fn configured_peer_hotels_reads_backbone_peers_from_selected_hotel() {
        let config = serde_json::json!({
            "hotels": {
                "beacon-test-hotel": {
                    "backbone_peers": [
                        {
                            "name": "mbp-jane",
                            "host": "100.79.239.64",
                            "beacon_port": 8999
                        },
                        {
                            "name": "default",
                            "host": "100.64.230.106",
                            "beacon_port": 9100,
                            "blob_port": 9101,
                            "execution_port": 9102
                        }
                    ]
                }
            }
        });

        let peers = super::configured_peer_hotels(&config, "beacon-test-hotel");
        assert_eq!(peers.len(), 2);
        assert!(peers.iter().any(|peer| {
            peer.hotel_name == "mbp-jane"
                && peer.mesh_host == "100.79.239.64"
                && peer.mesh_port == 8999
                && peer.blob_port == 9000
                && peer.execution_port == 9001
        }));
        assert!(peers.iter().any(|peer| {
            peer.hotel_name == "default"
                && peer.mesh_host == "100.64.230.106"
                && peer.mesh_port == 9100
                && peer.blob_port == 9101
                && peer.execution_port == 9102
        }));
    }

    #[test]
    fn configured_peer_hotels_overlay_default_entries() {
        let config = serde_json::json!({
            "hotels": {
                "default": {
                    "backbone_peers": [
                        {
                            "name": "shared-peer",
                            "host": "100.64.0.1",
                            "beacon_port": 8999
                        }
                    ]
                },
                "mbp-jane": {
                    "backbone_peers": [
                        {
                            "name": "shared-peer",
                            "host": "100.64.0.2",
                            "beacon_port": 9100
                        },
                        {
                            "name": "beacon-test-hotel",
                            "host": "100.64.212.8",
                            "beacon_port": 8999
                        }
                    ]
                }
            }
        });

        let peers = super::configured_peer_hotels(&config, "mbp-jane");
        assert_eq!(peers.len(), 2);
        assert!(peers.iter().any(|peer| {
            peer.hotel_name == "shared-peer"
                && peer.mesh_host == "100.64.0.2"
                && peer.mesh_port == 9100
        }));
        assert!(peers.iter().any(|peer| {
            peer.hotel_name == "beacon-test-hotel"
                && peer.mesh_host == "100.64.212.8"
                && peer.mesh_port == 8999
        }));
    }

    #[test]
    fn agent_profile_from_config_prefers_hotel_agent_over_builtins() {
        let config = serde_json::json!({
            "hotels": {
                "beacon-test-hotel": {
                    "agents": {
                        "beacon": {
                            "agent_id": "agent-beacon-01",
                            "persona_name": "Beacon"
                        }
                    }
                }
            }
        });

        let profile =
            agent_profile_from_config(&config, "beacon-test-hotel").expect("profile should exist");
        assert_eq!(profile.agent_key, "beacon");
        assert_eq!(profile.agent_id, "agent-beacon-01");
        assert_eq!(profile.persona_name, "Beacon");
    }

    #[test]
    fn default_agent_profile_is_generic_from_hotel_name() {
        let profile = default_agent_profile_for_hotel("beacon-test-hotel");
        assert_eq!(profile.agent_key, "beacon");
        assert_eq!(profile.agent_id, "agent-beacon-01");
        assert_eq!(profile.persona_name, "Beacon");
    }

    #[test]
    fn deactivate_legacy_managed_guests_disables_hotel_prefixed_hegemon_ids() {
        let storage = SqliteGraphStorage::open(":memory:").expect("open sqlite");
        let graph = GraphDomain::new(Arc::new(storage.adapter()));
        let hotel_name = "startup-test-hotel";
        let profile = default_agent_profile_for_hotel(hotel_name);
        let desired = guest_seed_for_profile(hotel_name, &profile);
        let legacy = vec![
            GuestRecord {
                hotel_name: hotel_name.into(),
                guest_id: format!("{hotel_name}:philote-jane"),
                role: "agent".into(),
                config_json: serde_json::json!({ "command": "target/debug/philote" }).to_string(),
                is_active: true,
                active_pid: None,
                last_active_at: None,
            },
            GuestRecord {
                hotel_name: hotel_name.into(),
                guest_id: format!("{hotel_name}:hegemon-gateway-jane"),
                role: "hegemon".into(),
                config_json: serde_json::json!({ "command": "target/debug/hegemon" }).to_string(),
                is_active: true,
                active_pid: None,
                last_active_at: None,
            },
        ];

        graph
            .seed_guests(hotel_name, &legacy)
            .expect("seed legacy predecessor guests");
        graph
            .seed_guests(hotel_name, &desired)
            .expect("seed desired guests");

        deactivate_legacy_managed_guests(&graph, hotel_name, &[profile], &desired)
            .expect("deactivate legacy predecessor guests");

        let stored = graph
            .list_guests(hotel_name, false)
            .expect("list guests after cleanup");

        let legacy_agent = stored
            .iter()
            .find(|guest| guest.guest_id == format!("{hotel_name}:philote-jane"))
            .expect("legacy agent guest should remain in graph");
        assert!(!legacy_agent.is_active);

        let legacy_hegemon = stored
            .iter()
            .find(|guest| guest.guest_id == format!("{hotel_name}:hegemon-gateway-jane"))
            .expect("legacy hegemon predecessor guest should remain in graph");
        assert!(!legacy_hegemon.is_active);
    }

    #[test]
    fn text_startup_test_injects_stub_response_into_model_guest() {
        let storage = SqliteGraphStorage::open(":memory:").expect("open sqlite");
        let graph_domain = GraphDomain::new(Arc::new(storage.adapter()));
        let guests = default_guest_seed("startup-test-hotel");
        graph_domain
            .seed_guests("startup-test-hotel", &guests)
            .expect("seed guests");

        enable_guest_test_overrides(
            &graph_domain,
            "startup-test-hotel",
            StartupTest::TextRoundTrip,
        )
        .expect("apply startup overrides");

        let stored = graph_domain
            .list_guests("startup-test-hotel", false)
            .expect("list guests");
        let model = stored
            .into_iter()
            .find(|guest| guest.role == "model")
            .expect("model guest should exist");
        let config: serde_json::Value =
            serde_json::from_str(&model.config_json).expect("config should decode");

        assert_eq!(
            config["env"]["PHILOTIC_MODEL_ROUTER_STUB_RESPONSE"].as_str(),
            Some("startup text smoke ok")
        );
    }

    #[test]
    fn gemini_oauth_startup_test_injects_fake_base_url_into_model_guest() {
        let storage = SqliteGraphStorage::open(":memory:").expect("open sqlite");
        let graph_domain = GraphDomain::new(Arc::new(storage.adapter()));
        let guests = default_guest_seed("startup-test-hotel");
        graph_domain
            .seed_guests("startup-test-hotel", &guests)
            .expect("seed guests");

        enable_guest_test_overrides(
            &graph_domain,
            "startup-test-hotel",
            StartupTest::GeminiOAuthRoundTrip,
        )
        .expect("apply startup overrides");

        let stored = graph_domain
            .list_guests("startup-test-hotel", false)
            .expect("list guests");
        let model = stored
            .into_iter()
            .find(|guest| guest.role == "model")
            .expect("model guest should exist");
        let config: serde_json::Value =
            serde_json::from_str(&model.config_json).expect("config should decode");
        let expected_base_url = startup_test_gemini_base_url("startup-test-hotel");

        assert_eq!(
            config["env"]["PHILOTIC_GEMINI_BASE_URL"].as_str(),
            Some(expected_base_url.as_str())
        );
    }
}
