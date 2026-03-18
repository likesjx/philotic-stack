use ansible_mesh_core::beacon::BeaconDaemon;
use ansible_mesh_core::graph::{AbstractSkillRecord, AbstractToolRecord, ToolsetProfileRecord};
use ansible_mesh_core::heartbeat::emit_heartbeat;
use ansible_mesh_core::registry::{CapabilityAdvertisement, ExecutionReachability};
use ansible_mesh_core::storage::{
    AgentIdentityRecord, CursorStorage, EventStorage, GraphStorage, GuestRecord, HotelRecord,
};
use ansible_mesh_core::{NodeCapabilities, NodeRole};
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
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

mod auth;
mod graph;
mod memory;
mod muninn_provision;
mod vault;

mod service;
use service::blob::BlobService;
use service::ipc::IpcServer;
use std::sync::Arc;

// ── Profile path resolution ────────────────────────────────────────────────

/// Returns `~/.philotic/<profile>/` when `PHILOTIC_PROFILE` is set, else `None`.
///
/// When `Some`, all runtime paths (DB, socket) are namespaced to that directory
/// so that two profiles never collide. When `None`, legacy path behavior applies.
fn profile_dir() -> Option<PathBuf> {
    let profile = std::env::var("PHILOTIC_PROFILE").ok().filter(|s| !s.is_empty())?;
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".philotic").join(profile))
}

use ansible_mesh_core::event::EventEnvelope;
use auth::AuthCommand;
use vault::{SecretInput, store_secret};

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

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,

    /// Name of the hotel to boot from the Context Graph
    #[arg(long)]
    hotel: Option<String>,

    /// Optional path to a JSON file containing configuration to load into the Context Graph
    #[arg(long)]
    load_config: Option<String>,

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
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum StartupTest {
    #[value(name = "text-roundtrip", alias = "text-round-trip")]
    TextRoundTrip,
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
                .unwrap_or(false),
            enable_rust_dispatcher: std::env::var("PHILOTIC_ENABLE_RUST_DISPATCHER")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
            enable_rust_task_lifecycle: std::env::var("PHILOTIC_ENABLE_RUST_TASK_LIFECYCLE")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
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
        mesh_port: base_port,
        blob_port: base_port + 1,
        execution_port: hotel_execution_port(&safe_name),
        ipc_socket_path: profile_dir()
            .map(|d| d.join("aiua.sock").to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("/tmp/philotic-{safe_name}.sock")),
        active_pid: None,
    }
}

fn mesh_targets_for_graph(
    graph: &dyn GraphStorage,
    local_node_id: &str,
) -> Result<Vec<(String, String)>> {
    Ok(graph
        .list_hotels()?
        .into_iter()
        .filter(|hotel| hotel.capabilities.node_id != local_node_id)
        .map(|hotel| {
            (
                hotel.capabilities.node_id,
                format!("127.0.0.1:{}", hotel.mesh_port),
            )
        })
        .collect())
}

fn mesh_target_addr_for_node(
    graph: &dyn GraphStorage,
    target_node_id: &str,
) -> Result<Option<String>> {
    Ok(graph
        .list_hotels()?
        .into_iter()
        .find(|hotel| hotel.capabilities.node_id == target_node_id)
        .map(|hotel| format!("127.0.0.1:{}", hotel.mesh_port)))
}

fn execution_reachability_for_hotel(
    graph: &dyn GraphStorage,
    hotel: &HotelRecord,
) -> ExecutionReachability {
    let host = graph
        .get_config_value("execution_host")
        .ok()
        .flatten()
        .and_then(|value| serde_json::from_str::<String>(&value).ok().or(Some(value)))
        .filter(|value| !value.trim().is_empty())
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

fn local_capability_advertisements(
    graph: &dyn GraphStorage,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentProfile {
    agent_key: String,
    agent_id: String,
    persona_name: String,
    import_workspace: Option<String>,
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

fn merged_agent_config(
    config_json: &serde_json::Value,
    hotel_name: &str,
) -> Option<(String, serde_json::Map<String, serde_json::Value>)> {
    let default_hotel = hotel_object(config_json, "default");
    let selected_hotel = hotel_object(config_json, hotel_name);
    let selected_key = selected_hotel
        .and_then(selected_agent_key_for_hotel)
        .or_else(|| default_hotel.and_then(selected_agent_key_for_hotel))?;
    let mut merged = serde_json::Map::new();

    for hotel in [default_hotel, selected_hotel] {
        let Some(hotel) = hotel else {
            continue;
        };
        let Some(agents) = hotel.get("agents").and_then(serde_json::Value::as_object) else {
            continue;
        };
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

    Some((selected_key, merged))
}

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

    Some(AgentProfile {
        agent_key,
        agent_id,
        persona_name,
        import_workspace,
    })
}

/// Returns profiles for ALL agents in the hotel config.
/// Falls back to a single default profile if the hotel has no agents section.
fn all_agent_profiles_from_config(
    config_json: &serde_json::Value,
    hotel_name: &str,
) -> Vec<AgentProfile> {
    let get_agents = |hotel: &serde_json::Map<String, serde_json::Value>| {
        hotel
            .get("agents")
            .and_then(serde_json::Value::as_object)
            .cloned()
            .unwrap_or_default()
    };

    let default_agents = hotel_object(config_json, "default")
        .map(get_agents)
        .unwrap_or_default();
    let hotel_agents = if hotel_name != "default" {
        hotel_object(config_json, hotel_name)
            .map(get_agents)
            .unwrap_or_default()
    } else {
        serde_json::Map::new()
    };

    // Merge: hotel-specific agents overlay the default agents by key
    let mut all_agents = default_agents;
    for (key, val) in hotel_agents {
        all_agents.insert(key, val);
    }

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
            Some(AgentProfile {
                agent_key,
                agent_id,
                persona_name,
                import_workspace,
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
    for name in ["default", hotel_name] {
        if let Some(hotel) = hotel_object(config_json, name) {
            if let Some(agents) = hotel.get("agents").and_then(serde_json::Value::as_object) {
                if let Some(agent) = agents.get(agent_key).and_then(serde_json::Value::as_object) {
                    merged.extend(agent.clone());
                }
            }
        }
    }
    if merged.is_empty() { None } else { Some(merged) }
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

/// Hotel-level shared guests: one membrane for all agents, plus model controllers.
fn hotel_shared_guests(hotel_name: &str, profiles: &[AgentProfile]) -> Vec<GuestRecord> {
    let hotel = default_hotel_record(hotel_name);
    let socket_path = hotel.ipc_socket_path;
    let blob_base_url = format!("http://127.0.0.1:{}", hotel.blob_port);
    let node_id = hotel.capabilities.node_id;

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
                "command": "membrane",
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
                    "PHILOTIC_NODE_ID": node_id.clone()
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
            guest_id: format!("{hotel_name}:tool-runner"),
            role: "tool".into(),
            config_json: serde_json::json!({
                "command": "tool-runner",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path,
                    "PHILOTIC_NODE_ID": node_id
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
        if let Some(default_hotel) = hotels.get("default").and_then(serde_json::Value::as_object) {
            merge_hotel_base_entries(&mut merged, default_hotel);
        }

        if let Some(hotel_name) = hotel_name {
            if let Some(hotel) = hotels
                .get(hotel_name)
                .and_then(serde_json::Value::as_object)
            {
                merge_hotel_base_entries(&mut merged, hotel);
            }
        }
    }

    if let Some(hotel_name) = hotel_name {
        // For multi-agent hotels, extract per-agent token keys for all agents.
        for hotel_key in ["default", hotel_name] {
            if let Some(hotel) = hotel_object(config_json, hotel_key) {
                if let Some(agents) = hotel.get("agents").and_then(serde_json::Value::as_object) {
                    for (agent_key, agent_val) in agents {
                        if let Some(agent) = agent_val.as_object() {
                            merge_agent_entries(&mut merged, agent, Some(agent_key.as_str()));
                        }
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

    // Promote voice_id from voice_response_policy to elevenlabs_voice_id as a named fallback
    // so model-router ProviderConfigs can pick it up without knowing about VoiceResponsePolicy.
    if let Some(voice_id) = agent
        .get("voice_response_policy")
        .and_then(|p| p.get("voice_id"))
        .filter(|v| v.is_string())
    {
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
            merged.insert(
                format!("telegram_bot_token_{key}"),
                bot_token.clone(),
            );
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
            for key in ["voice_response_policy", "media_routing_policy"] {
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

fn reconcile_hotel_record(graph: &dyn GraphStorage, hotel_name: &str) -> Result<HotelRecord> {
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
    graph: &dyn GraphStorage,
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
        legacy_guest_ids.insert(format!("{hotel_name}:hegemon-gateway-{}", profile.agent_key));
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
                        "agent"
                            | "hegemon"
                            | "membrane"
                            | "model"
                            | "model"
                            | "model.elevenlabs"
                            | "tool"
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
fn seed_abstract_tool_catalog(graph: &dyn GraphStorage) -> anyhow::Result<()> {
    let catalog = [
        AbstractToolRecord {
            tool_name: "session.status".into(),
            description: "Returns a summary of the current session state, including the active \
                          session ID, turn count, approval policy, and active tool runners."
                .into(),
            input_schema: serde_json::json!({ "type": "object", "properties": {} }),
            class: "session".into(),
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
fn seed_abstract_skill_catalog(graph: &dyn GraphStorage) -> anyhow::Result<()> {
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
            description: "Govern role definitions deliberately for the current agent identity, reasoning explicitly about purpose, capability posture, handoff behavior, and limits before proposing changes.".into(),
            implied_tools: vec!["session.status".into(), "agent.configure".into(), "role.configure".into()],
            ..Default::default()
        },
    ];

    for skill in &catalog {
        graph.upsert_abstract_skill(skill)?;
    }
    Ok(())
}

fn seed_toolset_profiles(graph: &dyn GraphStorage) -> anyhow::Result<()> {
    let profiles = [
        ToolsetProfileRecord {
            profile_name: "orchestrator".into(),
            allowed_tools: vec![
                "session.status".into(),
                "echo".into(),
                "agent.configure".into(),
                "skill.register".into(),
                "subagent.spawn".into(),
                "workspace.list".into(),
                "workspace.read".into(),
                "bash.exec".into(),
            ],
            allowed_classes: vec!["session".into(), "utility".into(), "config".into()],
            allowed_skills: vec![
                "handoff.to_role".into(),
                "handoff.back".into(),
                "role.governance".into(),
            ],
            description: Some("Default orchestrator role profile.".into()),
        },
        ToolsetProfileRecord {
            profile_name: "codex".into(),
            allowed_tools: vec![
                "session.status".into(),
                "echo".into(),
                "workspace.list".into(),
                "workspace.read".into(),
            ],
            allowed_classes: vec!["session".into(), "utility".into(), "workspace".into()],
            allowed_skills: vec!["handoff.back".into()],
            description: Some("Codex specialist role profile — workspace read access.".into()),
        },
        ToolsetProfileRecord {
            profile_name: "research".into(),
            allowed_tools: vec!["session.status".into(), "echo".into()],
            allowed_classes: vec!["session".into(), "utility".into()],
            allowed_skills: vec!["handoff.back".into()],
            description: Some("Research specialist role profile — minimal tool surface.".into()),
        },
        ToolsetProfileRecord {
            profile_name: "utility".into(),
            allowed_tools: vec!["session.status".into(), "echo".into()],
            allowed_classes: vec!["session".into(), "utility".into()],
            allowed_skills: Vec::new(),
            description: Some("Bare utility profile — session and echo only.".into()),
        },
        ToolsetProfileRecord {
            profile_name: "admin".into(),
            allowed_tools: vec![
                "session.status".into(),
                "echo".into(),
                "agent.configure".into(),
                "skill.register".into(),
                "skill.list".into(),
                "skill.assign".into(),
                "skill.revoke".into(),
                "subagent.spawn".into(),
                "role.configure".into(),
                "workspace.list".into(),
                "workspace.read".into(),
                "bash.exec".into(),
            ],
            allowed_classes: vec!["session".into(), "utility".into(), "config".into(), "shell".into()],
            allowed_skills: vec![
                "skill.crafting".into(),
                "handoff.to_role".into(),
                "handoff.back".into(),
                "role.governance".into(),
            ],
            description: Some("Admin role profile — full skill crafting and role governance authority.".into()),
        },
    ];

    for profile in &profiles {
        graph.upsert_toolset_profile(profile)?;
    }
    Ok(())
}

fn seed_skill_crafting(graph: &dyn GraphStorage) -> anyhow::Result<()> {
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
            "role.configure".into(),
        ],
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

Rules:
- Reason explicitly before creating a role: purpose, toolset, handoff posture, limits.
- Do not bypass the approval gate; if a tool requires operator approval, surface it clearly.
- Keep soul_text and core identity stable — those changes require operator approval.
- Use handoff.to_role for sustained specialist work; use subagent.spawn for parallel bounded tasks.

Approval posture:
- Governance tools (role.configure, skill.register, handoff.to_role, handoff.back) run without per-action approval.
- Self-configuration (agent.configure for approval_policy, profile, bindings) runs without approval.
- Shell execution (bash.exec) and core identity field changes require operator approval.";

/// Seeds an orchestrator RoleIncarnationRecord for each agent profile.
///
/// This ensures every agent has a fully populated toolset and manifest from the first session
/// turn, breaking the chicken-and-egg where role.configure requires tools that only appear
/// after a role exists.
fn seed_orchestrator_roles(
    graph: &dyn GraphStorage,
    profiles: &[AgentProfile],
) -> anyhow::Result<()> {
    for profile in profiles {
        let record = ansible_mesh_core::graph::RoleIncarnationRecord {
            agent_id: profile.agent_id.clone(),
            role_name: "orchestrator".into(),
            guest_id: format!("{}:orchestrator", profile.agent_id),
            toolset_profile: "orchestrator".into(),
            role_identity_addendum: None,
            role_manifest: Some(ORCHESTRATOR_MANIFEST.into()),
            is_admin: false,
            inactive_ttl_seconds: None,
            turn_loop_config: ansible_mesh_core::graph::TurnLoopConfig::default(),
        };
        // Always upsert — the hotel seed is the canonical source for the orchestrator manifest.
        // The manifest is institutional (same rules for all agents), not per-agent customizable.
        // To change the manifest, update this seed and restart the hotel.
        graph.upsert_role_incarnation(&record)?;
    }
    Ok(())
}

fn enable_guest_test_overrides(
    graph: &dyn GraphStorage,
    hotel_name: &str,
    test: StartupTest,
) -> Result<()> {
    let mut guests = graph.list_guests(hotel_name, false)?;
    if guests.is_empty() {
        return Ok(());
    }

    match test {
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
                        "command": "membrane",
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
    let graph = ansible_mesh_core::sqlite_storage::SqliteGraphStorage::open(startup_test_db_path())?;
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
    graph: &Arc<dyn ansible_mesh_core::storage::GraphStorage>,
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

    if let Some(Command::Auth { provider }) = args.command {
        return auth::run_auth_command(provider).await;
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
        info!("Profile: {}  (DB: {})", std::env::var("PHILOTIC_PROFILE").unwrap_or_default(), db_path_buf.display());
        &db_path_buf
    } else {
        Path::new("aiua_context.db")
    };
    let graph_storage = ansible_mesh_core::sqlite_storage::SqliteGraphStorage::open(db_path)?;

    let hotel_name = args.hotel.clone();

    // Handle Config Loading if requested.
    // When PHILOTIC_PROFILE is set and no --load-config is given, auto-load
    // ~/.philotic/<profile>/config.json if it exists.
    let effective_load_config = args.load_config.or_else(|| {
        let pdir = profile_dir()?;
        let auto = pdir.join("config.json");
        if auto.exists() {
            info!("Auto-loading profile config: {}", auto.display());
            Some(auto.to_string_lossy().into_owned())
        } else {
            None
        }
    });

    // Lifted out of the if-block so we can reference it during multi-agent profile collection.
    let mut loaded_config_json: Option<serde_json::Value> = None;
    if let Some(config_path) = effective_load_config {
        info!(
            "Loading configuration from '{}' into the Context Graph...",
            config_path
        );
        let config_data = fs::read_to_string(&config_path).context("Failed to read config file")?;
        let config_json: serde_json::Value =
            serde_json::from_str(&config_data).context("Invalid JSON config file")?;
        loaded_config_json = Some(config_json.clone());

        let entries = extract_context_graph_entries(&config_json, hotel_name.as_deref());

        if !entries.is_empty() {
            let mut count = 0;
            for (key, value) in entries {
                let val_str = if value.is_string() {
                    // Store strings as-is (with quotes, so they remain valid JSON strings in the db)
                    serde_json::to_string(&value)?
                } else {
                    value.to_string()
                };

                graph_storage.set_config_value(&key, &val_str)?;
                count += 1;
            }
            info!(
                "Successfully injected {} configuration keys into Context Graph.",
                count
            );
        } else {
            warn!("Config file must be a JSON object or contain a top-level context_graph object.");
        }

        // Provision MuninnDB vaults from `context_graph.muninn` section if present.
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

            graph_storage.set_muninn_endpoint(endpoint)?;

            let vault_names = muninn_provision::derive_vault_names(&config_json);
            if !vault_names.is_empty() {
                muninn_provision::provision_muninn_vaults(
                    &graph_storage,
                    endpoint,
                    username,
                    password,
                    vault_names,
                )
                .await?;
            }
        }
    }

    let hotel_name =
        hotel_name.context("--hotel is required unless using a subcommand such as `auth`")?;
    // Collect all agent profiles from the config. Falls back to a single default if none found.
    let all_profiles = loaded_config_json
        .as_ref()
        .map(|cfg| all_agent_profiles_from_config(cfg, &hotel_name))
        .unwrap_or_else(|| vec![default_agent_profile_for_hotel(&hotel_name)]);
    info!(
        "Hotel '{}' will materialize {} agent(s): {}",
        hotel_name,
        all_profiles.len(),
        all_profiles
            .iter()
            .map(|p| p.persona_name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Keep the single effective_agent_profile for legacy code paths that need exactly one.
    let _effective_agent_profile = all_profiles
        .first()
        .cloned()
        .unwrap_or_else(|| default_agent_profile_for_hotel(&hotel_name));

    let startup_test = args.test;
    if graph_storage.get_hotel(&hotel_name)?.is_none() {
        info!(
            "Hotel '{}' is missing from the Context Graph. Bootstrapping it now.",
            hotel_name
        );
    }

    // Seed guests: one philote per agent, plus one shared membrane + model controllers.
    let mut all_desired_guests: Vec<GuestRecord> = Vec::new();
    for profile in &all_profiles {
        all_desired_guests.push(agent_guests_for_profile(&hotel_name, profile));
    }
    all_desired_guests.extend(hotel_shared_guests(&hotel_name, &all_profiles));
    deactivate_legacy_managed_guests(
        &graph_storage,
        &hotel_name,
        &all_profiles,
        &all_desired_guests,
    )?;
    graph_storage.seed_guests(&hotel_name, &all_desired_guests)?;
    let mut hotel = reconcile_hotel_record(&graph_storage, &hotel_name)?;

    seed_abstract_tool_catalog(&graph_storage)?;
    seed_abstract_skill_catalog(&graph_storage)?;
    seed_toolset_profiles(&graph_storage)?;
    seed_skill_crafting(&graph_storage)?;
    seed_orchestrator_roles(&graph_storage, &all_profiles)?;

    if let Some(test) = startup_test {
        prepare_startup_test_binaries(test)?;
        enable_guest_test_overrides(&graph_storage, &hotel_name, test)?;
    }

    // Upsert identity records for all agents.
    for profile in &all_profiles {
        let agent_config = loaded_config_json
            .as_ref()
            .and_then(|cfg| raw_agent_config_for_key(cfg, &hotel_name, &profile.agent_key));
        let identity = agent_identity_record_for_profile(profile, &hotel_name, agent_config.as_ref());
        graph_storage
            .upsert_agent_identity(&identity)
            .with_context(|| {
                format!(
                    "Failed to seed agent identity bundle for {}",
                    identity.agent_id
                )
            })?;
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
        graph_storage.set_hotel_pid(&hotel_name, None)?;
        hotel.active_pid = None;
    }

    let current_pid = std::process::id().to_string();
    graph_storage.set_hotel_pid(&hotel_name, Some(&current_pid))?;
    hotel.active_pid = Some(current_pid.clone());
    let smoke_mode = smoke_mode_enabled();

    let caps = hotel.capabilities.clone();
    let mesh_port = hotel.mesh_port;
    let addr = format!("0.0.0.0:{}", mesh_port);
    info!(
        "Starting Philotic Ansible Daemon for hotel '{}' as node '{}' on {}",
        hotel_name, caps.node_id, addr
    );

    let graph_arc: Arc<dyn ansible_mesh_core::storage::GraphStorage> = Arc::new(graph_storage);

    // Boot-time MuninnDB config load (Slice D).
    // Returns None if no vault registry is configured; guests fall back to NullMemoryEngine.
    let muninn_config_arc: Option<Arc<memory_core::MuninnConfig>> =
        match memory::load_muninn_config(graph_arc.as_ref()) {
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
            graph_arc.clone(),
        )
        .with_memory_config(muninn_config_arc.clone());
        tokio::spawn(async move {
            if let Err(e) = ipc_server.run().await {
                error!("Hotel Front Desk (UDS) failed: {}", e);
            }
        });

        tokio::signal::ctrl_c().await?;
        let _ = graph_arc.set_hotel_pid(&hotel_name, None);
        info!("Ansible smoke-mode shutdown complete.");
        return Ok(());
    }

    // Channel for inbound mesh UDP payloads bubbled up by the BeaconDaemon
    let (inbox_tx, mut inbox_rx) = mpsc::channel::<ansible_mesh_core::BeaconMessage>(1024);

    // PORT-BP-006: Pre-Shared Key for mesh authentication
    let mesh_psk = std::env::var("PHILOTIC_MESH_PSK")
        .unwrap_or_else(|_| "INSECURE_DEV_DEFAULT_PSK".to_string());

    let daemon = match BeaconDaemon::bind(
        &addr,
        caps.clone(),
        inbox_tx,
        &mesh_psk,
        db_path.to_str().unwrap_or(""),
        flags.enable_rust_auth,
    )
    .await
    {
        Ok(daemon) => daemon,
        Err(e) => {
            let _ = graph_arc.set_hotel_pid(&hotel_name, None);
            return Err(e);
        }
    };

    // Channel for pushing generated SDP Answers back out to the mesh
    let (webrtc_signal_tx, mut webrtc_signal_rx) =
        mpsc::channel::<ansible_mesh_core::webrtc::WebRtcSignalMessage>(32);

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
        graph_arc.clone(),
        materializer,
    ));

    let ipc_server = IpcServer::new(
        socket_path,
        caps.node_id.clone(),
        dispatcher_tx.clone(),
        graph_arc.clone(),
    )
    .with_memory_config(muninn_config_arc)
    .with_materialization_requester(guest_manager.clone())
    .with_registry(daemon.registry());
    let ipc_inboxes = ipc_server.inboxes();

    tokio::spawn(async move {
        if let Err(e) = ipc_server.run().await {
            error!("Hotel Front Desk (UDS) failed: {}", e);
        }
    });

    let execution_inbox_tx = daemon.inbox_tx().clone();
    let execution_caps = caps.clone();
    let execution_db_path = db_path.to_string_lossy().to_string();
    let execution_psk = mesh_psk.clone();
    tokio::spawn(async move {
        if let Err(e) = crate::service::execution_transport::serve_execution_plane(
            &execution_addr,
            execution_caps,
            execution_inbox_tx,
            &execution_psk,
            &execution_db_path,
            execution_enable_rust_auth,
        )
        .await
        {
            error!("Hotel execution transport failed: {}", e);
        }
    });

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
        stabilize_startup_test_guests(&guest_manager, &graph_arc, &hotel_name, &shutdown_rx)
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
        let _ = graph_arc.set_hotel_pid(&hotel_name, None);
        return test_result;
    }

    // PORT-BP-003: Mesh Outbound Dispatcher (Periodic Queuing Loop)
    if flags.enable_rust_dispatcher {
        let dispatcher_ledger = ledger.clone();
        let dispatcher_tracker = tracker.clone();
        let dispatcher_socket = daemon.socket();
        let dispatcher_graph = graph_arc.clone();
        let dispatcher_registry = daemon.registry();

        let rx_dispatch = shutdown_rx.resubscribe();
        tokio::spawn(crate::service::mesh_dispatcher::outbound_dispatcher(
            dispatcher_ledger,
            dispatcher_tracker,
            dispatcher_socket,
            dispatcher_graph,
            dispatcher_registry,
            caps.node_id.clone(),
            rx_dispatch,
        ));
    }

    let heartbeat_socket = daemon.socket().clone();
    let heartbeat_graph = graph_arc.clone();
    let heartbeat_hotel = hotel.clone();
    let heartbeat_caps = caps.clone();
    let mut heartbeat_shutdown = shutdown_rx.resubscribe();
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
                    let advertisements = match local_capability_advertisements(heartbeat_graph.as_ref(), &heartbeat_hotel) {
                        Ok(advertisements) => advertisements,
                        Err(err) => {
                            warn!("Failed to build local capability advertisements: {}", err);
                            continue;
                        }
                    };
                    let execution_reachability =
                        execution_reachability_for_hotel(heartbeat_graph.as_ref(), &heartbeat_hotel);
                    for (_target_node_id, target_addr) in targets {
                        let Ok(target) = target_addr.parse::<SocketAddr>() else {
                            warn!("Skipping invalid heartbeat target address {}", target_addr);
                            continue;
                        };
                        if let Err(err) = emit_heartbeat(
                            &heartbeat_socket,
                            target,
                            &heartbeat_caps,
                            &advertisements,
                            Some(execution_reachability.clone()),
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

    // PORT-BP-004: Async Mesh Inbound Router
    // Receives BeaconMessages from the UDP socket and forwards them to the single DB writer thread
    let dispatcher_inbound_tx = dispatcher_tx.clone();
    let inbound_socket = daemon.socket().clone();
    let inbound_graph = graph_arc.clone();
    let inbound_inboxes = ipc_inboxes.clone();
    let inbound_local_node_id = caps.node_id.clone();
    let mesh_auth_inbound = ansible_mesh_core::authz::MeshAuth::new(&mesh_psk);
    tokio::spawn(async move {
        while let Some(msg) = inbox_rx.recv().await {
            match msg.msg_type {
                ansible_mesh_core::MsgType::MeshEventBatch => {
                    if let Ok(events) = serde_json::from_slice::<Vec<EventEnvelope>>(&msg.payload) {
                        if !events.is_empty() {
                            let max_seq = events.iter().map(|e| e.seq).max().unwrap_or(0);
                            for event in &events {
                                IpcServer::deliver_event_envelope(&inbound_inboxes, event).await;
                            }
                            let _ = dispatcher_inbound_tx
                                .send(LedgerCommand::CommitInboundBatch {
                                    events,
                                    source_node: msg.src_node.clone(),
                                })
                                .await; // The DB writer pushes this durably to the Inbox

                            let ack_payload =
                                serde_json::json!({ "acked_seq": max_seq }).to_string();
                            if let Some(target_addr) =
                                mesh_target_addr_for_node(inbound_graph.as_ref(), &msg.src_node)
                                    .ok()
                                    .flatten()
                            {
                                let msg_id = uuid::Uuid::new_v4();
                                let seq = 0;
                                let timestamp = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_secs();
                                let payload = ack_payload.into_bytes();
                                let hmac = mesh_auth_inbound
                                    .sign(&msg_id, seq as u64, &payload, timestamp);
                                let ack = ansible_mesh_core::BeaconMessage {
                                    version: 1,
                                    msg_id,
                                    src_node: inbound_local_node_id.clone(),
                                    dest_node: msg.src_node.clone(),
                                    msg_type: ansible_mesh_core::MsgType::MeshEventAck,
                                    seq,
                                    total: 1,
                                    payload,
                                    timestamp,
                                    hmac,
                                };
                                match serde_json::to_vec(&ack) {
                                    Ok(packet) => {
                                        if let Err(err) =
                                            inbound_socket.send_to(&packet, &target_addr).await
                                        {
                                            warn!(
                                                "Failed to return mesh ACK to {} at {}: {}",
                                                msg.src_node, target_addr, err
                                            );
                                        }
                                    }
                                    Err(err) => warn!(
                                        "Failed to serialize mesh ACK for {}: {}",
                                        msg.src_node, err
                                    ),
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
                ansible_mesh_core::MsgType::MeshEventAck => {
                    debug!("Received MeshEventAck from {}", msg.src_node);
                    // Dispatch to the single writer thread to handle cursor advancement
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
                ansible_mesh_core::MsgType::WebRtcSignal => {
                    info!("Received WebRTC Signaling Payload from {}", msg.src_node);
                    if let Ok(signal_msg) = serde_json::from_slice::<
                        ansible_mesh_core::webrtc::WebRtcSignalMessage,
                    >(&msg.payload)
                    {
                        let webrtc_signal_tx = webrtc_signal_tx.clone();
                        tokio::spawn(async move {
                            // In MVP 2 this channels to a long-running Guest Manager
                            // For MVP 1 we just spin off a detached Guest directly
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

    // PORT-BP-008: WebRTC SDP Signal Dispatcher Loop
    let local_node_id = caps.node_id.clone();

    let socket_webrtc = daemon.socket().clone();
    let mesh_auth_webrtc = ansible_mesh_core::authz::MeshAuth::new(&mesh_psk);
    let local_node_id_webrtc = local_node_id.clone();

    tokio::spawn(async move {
        while let Some(signal) = webrtc_signal_rx.recv().await {
            // trace!("Dispatching WebRTC Signal to Mesh: {:?}", signal.signal);
            if let Ok(payload_bytes) = serde_json::to_vec(&signal) {
                let msg_id = uuid::Uuid::new_v4();
                let seq = 0;
                let timestamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                let hmac = mesh_auth_webrtc.sign(&msg_id, seq as u64, &payload_bytes, timestamp);

                let msg = ansible_mesh_core::BeaconMessage {
                    version: 1,
                    msg_id,
                    src_node: local_node_id_webrtc.clone(),
                    dest_node: signal.target_guest_id.clone(), // In MVP, this relies on beacon broadcast or explicit target IP map
                    msg_type: ansible_mesh_core::MsgType::WebRtcSignal,
                    seq,
                    total: 1,
                    timestamp,
                    payload: payload_bytes,
                    hmac,
                };

                if let Ok(packet) = serde_json::to_vec(&msg) {
                    let target_addr = "127.0.0.1:8999"; // MVP strict routing
                    if let Err(e) = socket_webrtc.send_to(&packet, target_addr).await {
                        tracing::error!("UDP WebRTC Signal send failed: {}", e);
                    }
                }
            }
        }
    });

    tokio::select! {
        res = daemon.run_loop() => {
            if let Err(e) = res {
                error!("Beacon Daemon error: {}", e);
            }
        }
        _ = tokio::signal::ctrl_c() => {
            warn!("Ctrl-C received! Initiating shutdown of all Materialized Guests...");
            let _ = shutdown_tx.send(());
            // Give Guests a tiny breather to exit voluntarily
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            let _ = graph_arc.set_hotel_pid(&hotel_name, None);
            info!("Ansible Daemon shutdown complete.");
        }
    }

    let _ = graph_arc.set_hotel_pid(&hotel_name, None);

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
        hotel_base_port, local_capability_advertisements, startup_test_gemini_base_url,
    };
    use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
    use ansible_mesh_core::storage::{GraphStorage, GuestRecord};

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
    fn default_guest_seed_injects_hotel_socket_env() {
        let guests = default_guest_seed("beta-hotel");
        assert_eq!(guests.len(), 5);
        // Membrane is the first guest from hotel_shared_guests
        let membrane = guests.iter().find(|g| g.role == "membrane").expect("membrane");
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
        let roster_json = config["env"]["PHILOTIC_AGENT_ROSTER"].as_str().expect("roster");
        let roster: Vec<serde_json::Value> = serde_json::from_str(roster_json).expect("parse roster");
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
            },
        );
        let membrane_guest = guests.iter().find(|g| g.role == "membrane").expect("membrane");
        let agent_guest = guests.iter().find(|g| g.role == "agent").expect("agent");
        let membrane: serde_json::Value =
            serde_json::from_str(&membrane_guest.config_json).expect("membrane config");
        let agent: serde_json::Value =
            serde_json::from_str(&agent_guest.config_json).expect("agent config");

        // Single membrane uses PHILOTIC_AGENT_ROSTER; agent_id is embedded in the roster
        let roster_json = membrane["env"]["PHILOTIC_AGENT_ROSTER"].as_str().expect("roster");
        let roster: Vec<serde_json::Value> = serde_json::from_str(roster_json).expect("parse roster");
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
        let graph = SqliteGraphStorage::open(":memory:").expect("open sqlite graph");
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
        let graph = SqliteGraphStorage::open(":memory:").expect("open sqlite graph");
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
        assert!(entries.iter().any(|(k, v)| k == "telegram_bot_token_jane" && v.as_str() == Some("jane-token")));
        assert!(entries.iter().any(|(k, v)| k == "telegram_bot_token_aria" && v.as_str() == Some("aria-token")));
    }

    #[test]
    fn guest_seed_uses_per_agent_token_key() {
        let profile = AgentProfile {
            agent_key: "aria".into(),
            agent_id: "agent-aria".into(),
            persona_name: "Aria".into(),
            import_workspace: None,
        };
        let guests = guest_seed_for_profile("default", &profile);
        let membrane = guests.iter().find(|g| g.role == "membrane").expect("membrane guest");
        let config: serde_json::Value =
            serde_json::from_str(&membrane.config_json).expect("parse membrane config");
        // Token keys are now embedded in PHILOTIC_AGENT_ROSTER; membrane resolves them at runtime
        let roster_json = config["env"]["PHILOTIC_AGENT_ROSTER"].as_str().expect("roster");
        let roster: Vec<serde_json::Value> = serde_json::from_str(roster_json).expect("parse roster");
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
        assert_eq!(identity["identity_text"].as_str(), Some("Workspace identity."));
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
        let graph = SqliteGraphStorage::open(":memory:").expect("open sqlite");
        let hotel_name = "startup-test-hotel";
        let profile = default_agent_profile_for_hotel(hotel_name);
        let desired = guest_seed_for_profile(hotel_name, &profile);
        let legacy = vec![
            GuestRecord {
                hotel_name: hotel_name.into(),
                guest_id: format!("{hotel_name}:philote-jane"),
                role: "agent".into(),
                config_json: serde_json::json!({ "command": "target/debug/philote" })
                    .to_string(),
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
        let graph = SqliteGraphStorage::open(":memory:").expect("open sqlite");
        let guests = default_guest_seed("startup-test-hotel");
        graph
            .seed_guests("startup-test-hotel", &guests)
            .expect("seed guests");

        enable_guest_test_overrides(&graph, "startup-test-hotel", StartupTest::TextRoundTrip)
            .expect("apply startup overrides");

        let stored = graph
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
        let graph = SqliteGraphStorage::open(":memory:").expect("open sqlite");
        let guests = default_guest_seed("startup-test-hotel");
        graph
            .seed_guests("startup-test-hotel", &guests)
            .expect("seed guests");

        enable_guest_test_overrides(
            &graph,
            "startup-test-hotel",
            StartupTest::GeminiOAuthRoundTrip,
        )
        .expect("apply startup overrides");

        let stored = graph
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
