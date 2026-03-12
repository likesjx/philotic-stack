use ansible_mesh_core::beacon::BeaconDaemon;
use ansible_mesh_core::graph::AbstractToolRecord;
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
use std::collections::{HashMap, VecDeque};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

mod auth;
mod graph;
mod vault;

mod service;
use service::blob::BlobService;
use service::ipc::IpcServer;
use std::sync::Arc;

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
    #[value(name = "gemini-oauth-roundtrip", alias = "gemini-oauth")]
    GeminiOAuthRoundTrip,
    VoiceSample,
    #[value(name = "telegram-roundtrip", alias = "telegram-round-trip")]
    TelegramRoundTrip,
}

const STARTUP_TEST_TEXT_REPLY: &str = "startup text smoke ok";
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
            node_id: format!("{safe_name}-ansible-01"),
            roles: vec![NodeRole::AnsibleNode, NodeRole::Other("membrane".into())],
            models: vec![],
            tools: vec![],
            constraints: Default::default(),
        },
        mesh_port: base_port,
        blob_port: base_port + 1,
        execution_port: hotel_execution_port(&safe_name),
        ipc_socket_path: format!("/tmp/philotic-{safe_name}.sock"),
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

fn guest_seed_for_profile(hotel_name: &str, profile: &AgentProfile) -> Vec<GuestRecord> {
    let hotel = default_hotel_record(hotel_name);
    let socket_path = hotel.ipc_socket_path;
    let blob_base_url = format!("http://127.0.0.1:{}", hotel.blob_port);
    let node_id = hotel.capabilities.node_id;
    vec![
        GuestRecord {
            hotel_name: hotel_name.to_string(),
            guest_id: format!("{hotel_name}:membrane-gateway-{}", profile.agent_key),
            role: "membrane".into(),
            config_json: serde_json::json!({
                "command": "membrane",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path.clone(),
                    "PHILOTIC_NODE_ID": node_id.clone(),
                    "PHILOTIC_BLOB_BASE_URL": blob_base_url,
                    "PHILOTIC_TARGET_AGENT_ID": profile.agent_id,
                    "PHILOTIC_TELEGRAM_BOT_TOKEN_KEY": "telegram_bot_token"
                }
            })
            .to_string(),
            is_active: true,
            active_pid: None,
        },
        GuestRecord {
            hotel_name: hotel_name.to_string(),
            guest_id: format!("{hotel_name}:agent-core-{}", profile.agent_key),
            role: "agent".into(),
            config_json: serde_json::json!({
                "command": "agent-core",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path.clone(),
                    "PHILOTIC_NODE_ID": node_id.clone(),
                    "PHILOTIC_AGENT_ID": profile.agent_id
                }
            })
            .to_string(),
            is_active: true,
            active_pid: None,
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
            role: "model.gemini".into(),
            is_active: true,
            active_pid: None,
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
            // Not yet implemented — marked inactive so the hotel skips spawn
            // without a hard failure. Activate when tool-runner crate exists.
            is_active: false,
            active_pid: None,
        },
    ]
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
        if let Some((_, agent)) = merged_agent_config(config_json, hotel_name) {
            merge_agent_entries(&mut merged, &agent);
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
        merge_telegram_entries(merged, telegram);
    }
}

fn merge_agent_entries(
    merged: &mut serde_json::Map<String, serde_json::Value>,
    agent: &serde_json::Map<String, serde_json::Value>,
) {
    if let Some(context_graph) = agent
        .get("context_graph")
        .and_then(serde_json::Value::as_object)
    {
        merged.extend(context_graph.clone());
    }

    if let Some(telegram) = agent.get("telegram").and_then(serde_json::Value::as_object) {
        merge_telegram_entries(merged, telegram);
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
) {
    if let Some(bot_token) = telegram.get("bot_token") {
        merged.insert("telegram_bot_token".into(), bot_token.clone());
    }
    if let Some(allowed_users) = telegram.get("allowed_users") {
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
        agent_config.as_ref(),
    ))
}

fn agent_identity_record_for_profile(
    profile: &AgentProfile,
    agent_config: Option<&serde_json::Map<String, serde_json::Value>>,
) -> AgentIdentityRecord {
    let mut bundle_json = profile
        .import_workspace
        .as_deref()
        .map(|workspace| identity_bundle_from_workspace(&profile.agent_key, Path::new(workspace)))
        .unwrap_or_else(|| serde_json::json!({}));

    // Merge policy fields from config into bundle so agent-core's AgentProfile picks them up
    // via serde deserialization of the session snapshot's agent_profile field.
    if let Some(bundle_obj) = bundle_json.as_object_mut() {
        if let Some(config) = agent_config {
            for key in ["voice_response_policy", "media_routing_policy"] {
                if let Some(value) = config.get(key) {
                    bundle_obj.insert(key.to_string(), value.clone());
                }
            }
        }
    }

    AgentIdentityRecord {
        agent_id: profile.agent_id.clone(),
        persona_name: profile.persona_name.clone(),
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
    if hotel.ipc_socket_path.trim().is_empty() {
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
    profile: &AgentProfile,
    desired_guests: &[GuestRecord],
) -> Result<()> {
    let desired_ids = desired_guests
        .iter()
        .map(|guest| guest.guest_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let legacy_guest_ids = [
        format!("agent-core-{}", profile.agent_key),
        format!("hegemon-gateway-{}", profile.agent_key),
        "hegemon-gateway".to_string(),
        "model-router-gemini".to_string(),
    ]
    .into_iter()
    .collect::<std::collections::HashSet<_>>();

    let stale = graph
        .list_guests(hotel_name, false)?
        .into_iter()
        .filter(|guest| {
            if desired_ids.contains(guest.guest_id.as_str()) {
                return false;
            }

            legacy_guest_ids.contains(&guest.guest_id)
                || guest.guest_id == format!("{hotel_name}:hegemon-gateway-{}", profile.agent_key)
                || (!guest.guest_id.starts_with(&format!("{hotel_name}:"))
                    && matches!(
                        guest.role.as_str(),
                        "agent"
                            | "hegemon"
                            | "membrane"
                            | "model"
                            | "model.gemini"
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
                if guest.role != "model.gemini" {
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
        StartupTest::GeminiOAuthRoundTrip => {
            let startup_secret_ref = store_secret(
                graph,
                SecretInput {
                    secret_kind: "gemini-startup-oauth-token".into(),
                    scope: "startup-test".into(),
                    allowed_roles: vec!["model.gemini".into()],
                    allowed_guests: Vec::new(),
                    plaintext: "startup-test-oauth-bearer".into(),
                },
            )?;

            for guest in &mut guests {
                if guest.role != "model.gemini" {
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
        StartupTest::TelegramRoundTrip => {
            graph.set_config_value(
                "telegram_bot_token",
                &serde_json::Value::String(STARTUP_TEST_TELEGRAM_TOKEN.into()).to_string(),
            )?;
            graph.set_config_value(
                "gemini_api_key",
                &serde_json::Value::String(STARTUP_TEST_GEMINI_API_KEY.into()).to_string(),
            )?;

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

                if guest.role == "model.gemini" {
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
}

fn spawn_fake_gemini_server(
    hotel_name: &str,
    expected_reply: String,
) -> tokio::task::JoinHandle<()> {
    let bind_addr: SocketAddr = format!("127.0.0.1:{}", startup_test_gemini_port(hotel_name))
        .parse()
        .expect("startup fake Gemini socket address should parse");

    let app = Router::new()
        .fallback(any(fake_gemini_handler))
        .with_state(FakeGeminiOAuthState { expected_reply });

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

    if request
        .uri()
        .query()
        .map(|query| query.contains("key="))
        .unwrap_or(false)
    {
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
        .with_state(state)
}

async fn fake_telegram_get_updates(
    AxumPath(_token): AxumPath<String>,
    Query(query): Query<TelegramGetUpdatesQuery>,
    State(state): State<Arc<FakeTelegramState>>,
) -> impl IntoResponse {
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

fn prepare_startup_test_binaries(_test: StartupTest) -> Result<()> {
    let status = std::process::Command::new("cargo")
        .args([
            "build",
            "-p",
            "membrane",
            "-p",
            "agent-core",
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
                    guest_id: "ansible-startup-test-client".into(),
                    role: "ansible-startup-test".into(),
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
                            "final_reply_role": "ansible-startup-test",
                            "final_reply_guest_id": "ansible-startup-test-client"
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
        StartupTest::GeminiOAuthRoundTrip => {
            let expected_reply = text
                .unwrap_or(STARTUP_TEST_GEMINI_OAUTH_REPLY)
                .trim()
                .to_string();
            let prompt = format!("Reply with exactly: {}", expected_reply);

            let fake_gemini = spawn_fake_gemini_server(hotel_name, expected_reply.clone());
            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            let mut client = PhiloticClient::connect_at(
                socket_path,
                GuestIdentity {
                    guest_id: "ansible-startup-test-client".into(),
                    role: "ansible-startup-test".into(),
                    supported_tools: Vec::new(),
                },
            )
            .await?;

            let response = client
                .send_request(IpcRequest::EmitTask {
                    target_node: local_node_id.clone(),
                    target_role: "model.gemini".into(),
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
                        "reply_role": "ansible-startup-test",
                        "final_reply_to": local_node_id.clone(),
                        "final_reply_role": "ansible-startup-test",
                        "final_reply_guest_id": "ansible-startup-test-client"
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
                .unwrap_or_else(|| PathBuf::from("tmp/voice-samples/ansible-startup-sample.mp3"));
            let text = text
                .unwrap_or("Hello from Philotic. This is an ansible startup voice test.")
                .to_string();

            tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

            let mut client = PhiloticClient::connect_at(
                socket_path,
                GuestIdentity {
                    guest_id: "ansible-startup-test-client".into(),
                    role: "ansible-startup-test".into(),
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
                        "reply_role": "ansible-startup-test",
                        "final_reply_to": local_node_id.clone(),
                        "final_reply_role": "ansible-startup-test"
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

            let expected_replies = [
                STARTUP_TEST_TELEGRAM_TEXT_REPLY,
                STARTUP_TEST_TELEGRAM_PHOTO_REPLY,
                STARTUP_TEST_TELEGRAM_VOICE_REPLY,
            ];
            for attempt in 1..=30 {
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                let sent_messages = telegram_state
                    .sent_messages
                    .lock()
                    .expect("sent messages lock")
                    .clone();
                if sent_messages.len() >= expected_replies.len() {
                    for (index, expected_text) in expected_replies.iter().enumerate() {
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

                    let gemini_requests = gemini_state
                        .requests
                        .lock()
                        .expect("fake gemini requests lock")
                        .clone();
                    if gemini_requests.len() < expected_replies.len() {
                        telegram_server.abort();
                        gemini_server.abort();
                        blob_server.abort();
                        let _ = telegram_server.await;
                        let _ = gemini_server.await;
                        let _ = blob_server.await;
                        anyhow::bail!(
                            "expected {} fake Gemini requests, got {}",
                            expected_replies.len(),
                            gemini_requests.len()
                        );
                    }
                    assert_fake_gemini_media_request(&gemini_requests[1], "image/jpeg")?;
                    assert_fake_gemini_media_request(&gemini_requests[2], "audio/ogg")?;

                    info!(
                        "Startup telegram round-trip delivered {:?} through fake Telegram API and fake Gemini API on attempt {} via {} and {}",
                        expected_replies, attempt, telegram_api_base_url, gemini_api_base_url
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
        .arg("pid=")
        .output()
        .map(|output| {
            output.status.success() && !String::from_utf8_lossy(&output.stdout).trim().is_empty()
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
    let db_path = Path::new("ansible_context.db");
    let graph_storage = ansible_mesh_core::sqlite_storage::SqliteGraphStorage::open(db_path)?;

    let hotel_name = args.hotel.clone();

    // Handle Config Loading if requested
    let mut effective_agent_profile = None;
    let mut effective_agent_config: Option<serde_json::Map<String, serde_json::Value>> = None;
    if let Some(config_path) = args.load_config {
        info!(
            "Loading configuration from '{}' into the Context Graph...",
            config_path
        );
        let config_data = fs::read_to_string(&config_path).context("Failed to read config file")?;
        let config_json: serde_json::Value =
            serde_json::from_str(&config_data).context("Invalid JSON config file")?;
        if let Some(hotel_name) = hotel_name.as_deref() {
            effective_agent_profile = agent_profile_from_config(&config_json, hotel_name);
            // Stash the raw merged agent config so policy fields can be seeded into bundle_json.
            effective_agent_config =
                merged_agent_config(&config_json, hotel_name).map(|(_, agent)| agent);
        }

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
    }

    let hotel_name =
        hotel_name.context("--hotel is required unless using a subcommand such as `auth`")?;
    let effective_agent_profile =
        effective_agent_profile.unwrap_or_else(|| default_agent_profile_for_hotel(&hotel_name));
    let startup_test = args.test;
    if graph_storage.get_hotel(&hotel_name)?.is_none() {
        info!(
            "Hotel '{}' is missing from the Context Graph. Bootstrapping it now.",
            hotel_name
        );
    }
    let desired_guests = guest_seed_for_profile(&hotel_name, &effective_agent_profile);
    deactivate_legacy_managed_guests(
        &graph_storage,
        &hotel_name,
        &effective_agent_profile,
        &desired_guests,
    )?;
    graph_storage.seed_guests(&hotel_name, &desired_guests)?;
    let mut hotel = reconcile_hotel_record(&graph_storage, &hotel_name)?;

    seed_abstract_tool_catalog(&graph_storage)?;

    if let Some(test) = startup_test {
        prepare_startup_test_binaries(test)?;
        enable_guest_test_overrides(&graph_storage, &hotel_name, test)?;
    }

    let effective_identity = agent_identity_record_for_profile(
        &effective_agent_profile,
        effective_agent_config.as_ref(),
    );
    graph_storage
        .upsert_agent_identity(&effective_identity)
        .with_context(|| {
            format!(
                "Failed to seed effective agent identity bundle for {}",
                effective_identity.agent_id
            )
        })?;

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
        );
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

    let ipc_server = IpcServer::new(
        socket_path,
        caps.node_id.clone(),
        dispatcher_tx.clone(),
        graph_arc.clone(),
    )
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

    // Abstracted Universal Materializer with trait-object storage
    let materializer = Box::new(crate::service::guest_manager::LocalProcessMaterializer::new());
    let guest_manager = Arc::new(crate::service::guest_manager::GuestManager::new(
        hotel_name.clone(),
        graph_arc.clone(),
        materializer,
    ));

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
        AgentProfile, StartupTest, agent_profile_from_config, default_agent_profile_for_hotel,
        default_guest_seed, default_hotel_record, enable_guest_test_overrides,
        execution_reachability_for_hotel, extract_context_graph_entries, guest_seed_for_profile,
        guest_supervision_enabled, hotel_base_port, local_capability_advertisements,
        startup_test_gemini_base_url,
    };
    use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
    use ansible_mesh_core::storage::GraphStorage;

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
        assert_eq!(hotel.capabilities.node_id, "alpha-hotel-ansible-01");
        assert_eq!(hotel.ipc_socket_path, "/tmp/philotic-alpha-hotel.sock");
        assert_eq!(hotel.mesh_port, hotel_base_port("alpha-hotel"));
        assert_eq!(hotel.blob_port, hotel.mesh_port + 1);
        assert_eq!(hotel.execution_port, hotel.mesh_port + 2);
    }

    #[test]
    fn default_guest_seed_injects_hotel_socket_env() {
        let guests = default_guest_seed("beta-hotel");
        assert_eq!(guests.len(), 5);
        let config: serde_json::Value = serde_json::from_str(&guests[0].config_json).unwrap();
        assert_eq!(
            config["env"]["PHILOTIC_HOTEL_SOCKET"].as_str(),
            Some("/tmp/philotic-beta-hotel.sock")
        );
        assert!(guests.iter().all(|guest| guest.hotel_name == "beta-hotel"));
        assert!(guests.iter().any(|guest| guest.role == "model.gemini"));
        assert!(guests.iter().any(|guest| guest.role == "model.elevenlabs"));
        assert!(guests.iter().any(|guest| guest.role == "tool"));
        assert_eq!(
            config["env"]["PHILOTIC_TARGET_AGENT_ID"].as_str(),
            Some("agent-beta-01")
        );
        assert_eq!(
            config["env"]["PHILOTIC_TELEGRAM_BOT_TOKEN_KEY"].as_str(),
            Some("telegram_bot_token")
        );
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
        let membrane: serde_json::Value =
            serde_json::from_str(&guests[0].config_json).expect("membrane config");
        let agent: serde_json::Value =
            serde_json::from_str(&guests[1].config_json).expect("agent config");

        assert_eq!(
            membrane["env"]["PHILOTIC_TARGET_AGENT_ID"].as_str(),
            Some("agent-beacon-01")
        );
        assert_eq!(
            membrane["env"]["PHILOTIC_TELEGRAM_BOT_TOKEN_KEY"].as_str(),
            Some("telegram_bot_token")
        );
        assert_eq!(
            agent["env"]["PHILOTIC_AGENT_ID"].as_str(),
            Some("agent-beacon-01")
        );
        assert!(guests[0].guest_id.contains("beacon"));
        assert!(guests[1].guest_id.contains("beacon"));
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
            .find(|guest| guest.role == "model.gemini")
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
            .find(|guest| guest.role == "model.gemini")
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
