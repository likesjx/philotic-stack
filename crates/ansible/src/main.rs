use ansible_mesh_core::beacon::BeaconDaemon;
use ansible_mesh_core::storage::{AgentIdentityRecord, GraphStorage, GuestRecord, HotelRecord};
use ansible_mesh_core::{NodeCapabilities, NodeRole};
use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use clap::{Parser, ValueEnum};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use std::fs;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

mod graph;

mod service;
use service::blob::BlobService;
use service::ipc::IpcServer;
use std::sync::Arc;

use ansible_mesh_core::event::EventEnvelope;

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
    /// Name of the hotel to boot from the Context Graph
    #[arg(long)]
    hotel: String,

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

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum StartupTest {
    #[value(name = "text-roundtrip", alias = "text-round-trip")]
    TextRoundTrip,
    VoiceSample,
}

const STARTUP_TEST_TEXT_REPLY: &str = "startup text smoke ok";

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

fn default_hotel_record(hotel_name: &str) -> HotelRecord {
    let safe_name = sanitize_hotel_name(hotel_name);
    let base_port = hotel_base_port(&safe_name);

    HotelRecord {
        hotel_name: hotel_name.to_string(),
        capabilities: NodeCapabilities {
            node_id: format!("{safe_name}-ansible-01"),
            roles: vec![NodeRole::AnsibleNode, NodeRole::Other("hegemon".into())],
            models: vec![],
            tools: vec![],
            constraints: Default::default(),
        },
        mesh_port: base_port,
        blob_port: base_port + 1,
        ipc_socket_path: format!("/tmp/philotic-{safe_name}.sock"),
        active_pid: None,
    }
}

fn default_guest_seed(hotel_name: &str) -> Vec<GuestRecord> {
    let socket_path = default_hotel_record(hotel_name).ipc_socket_path;
    vec![
        GuestRecord {
            hotel_name: hotel_name.to_string(),
            guest_id: format!("{hotel_name}:hegemon-gateway"),
            role: "hegemon".into(),
            config_json: serde_json::json!({
                "command": "target/debug/hegemon",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path.clone()
                }
            })
            .to_string(),
            is_active: true,
            active_pid: None,
        },
        GuestRecord {
            hotel_name: hotel_name.to_string(),
            guest_id: format!("{hotel_name}:agent-core-jane"),
            role: "agent".into(),
            config_json: serde_json::json!({
                "command": "target/debug/agent-core",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path.clone()
                }
            })
            .to_string(),
            is_active: true,
            active_pid: None,
        },
        GuestRecord {
            hotel_name: hotel_name.to_string(),
            guest_id: format!("{hotel_name}:model-controller-gemini"),
            role: "model.gemini".into(),
            config_json: serde_json::json!({
                "command": "target/debug/model-controller-gemini",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path.clone()
                }
            })
            .to_string(),
            is_active: true,
            active_pid: None,
        },
        GuestRecord {
            hotel_name: hotel_name.to_string(),
            guest_id: format!("{hotel_name}:model-controller-elevenlabs"),
            role: "model.elevenlabs".into(),
            config_json: serde_json::json!({
                "command": "target/debug/model-controller-elevenlabs",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path.clone()
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
                "command": "target/debug/tool-runner",
                "args": [],
                "env": {
                    "PHILOTIC_HOTEL_SOCKET": socket_path
                }
            })
            .to_string(),
            is_active: true,
            active_pid: None,
        },
    ]
}

fn maybe_load_text(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn extract_context_graph_entries(
    config_json: &serde_json::Value,
) -> Vec<(String, serde_json::Value)> {
    let Some(obj) = config_json.as_object() else {
        return Vec::new();
    };

    if let Some(context_graph) = obj
        .get("context_graph")
        .and_then(serde_json::Value::as_object)
    {
        return context_graph
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
    }

    obj.iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
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
    }

    graph.seed_guests(hotel_name, &guests)?;
    Ok(())
}

fn prepare_startup_test_binaries(test: StartupTest) -> Result<()> {
    let status = match test {
        StartupTest::TextRoundTrip => std::process::Command::new("cargo")
            .args([
                "build",
                "-p",
                "hegemon",
                "-p",
                "agent-core",
                "-p",
                "tool-runner",
                "-p",
                "model-router",
                "--bins",
            ])
            .status()
            .context("failed to launch cargo build for startup test binaries")?,
        StartupTest::VoiceSample => std::process::Command::new("cargo")
            .args([
                "build",
                "-p",
                "hegemon",
                "-p",
                "agent-core",
                "-p",
                "tool-runner",
                "-p",
                "model-router",
                "--bins",
            ])
            .status()
            .context("failed to launch cargo build for startup test binaries")?,
    };

    if !status.success() {
        anyhow::bail!("startup test binary build failed with status {}", status);
    }

    Ok(())
}

async fn run_startup_test(
    test: StartupTest,
    socket_path: &str,
    output: Option<&str>,
    text: Option<&str>,
) -> Result<()> {
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
                        target_node: "local-ansible-01".into(),
                        target_role: "agent".into(),
                        target_guest_id: None,
                        task_json: serde_json::json!({
                            "source": "startup-test",
                            "session_id": "startup-test:text-roundtrip",
                            "turn_id": format!("startup-test-turn-{attempt}"),
                            "chat_id": "startup-test-chat",
                            "content": text,
                            "final_reply_to": "local-ansible-01",
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
                    Ok(Ok(other)) => {
                        anyhow::bail!("unexpected startup text envelope: {other:?}");
                    }
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
                    target_node: "local-ansible-01".into(),
                    target_role: "model.elevenlabs".into(),
                    target_guest_id: None,
                    task_json: serde_json::json!({
                        "kind": "voice.synthesize",
                        "session_id": "startup-test:voice-sample",
                        "turn_id": "startup-test-turn-1",
                        "chat_id": "startup-test-chat",
                        "text": text,
                        "reply_to": "local-ansible-01",
                        "reply_role": "ansible-startup-test",
                        "final_reply_to": "local-ansible-01",
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
        }
    }

    Ok(())
}

fn vps_jane_identity_bundle() -> serde_json::Value {
    let Some(home) = std::env::var_os("HOME") else {
        return serde_json::json!({});
    };
    let workspace = Path::new(&home).join(".openclaw/workspace-vps-jane");

    serde_json::json!({
        "source_kind": "openclaw_workspace",
        "source_agent": "vps-jane",
        "workspace_path": workspace,
        "soul_text": maybe_load_text(&workspace.join("SOUL.md")),
        "identity_text": maybe_load_text(&workspace.join("IDENTITY.md")),
        "user_context_text": maybe_load_text(&workspace.join("USER.md")),
        "agents_text": maybe_load_text(&workspace.join("AGENTS.md")),
        "memory_summary": maybe_load_text(&workspace.join("MEMORY.md")),
    })
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

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

    // Handle Config Loading if requested
    if let Some(config_path) = args.load_config {
        info!(
            "Loading configuration from '{}' into the Context Graph...",
            config_path
        );
        let config_data = fs::read_to_string(&config_path).context("Failed to read config file")?;
        let config_json: serde_json::Value =
            serde_json::from_str(&config_data).context("Invalid JSON config file")?;

        let entries = extract_context_graph_entries(&config_json);
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

    let hotel_name = args.hotel.clone();
    let mut hotel = match graph_storage.get_hotel(&hotel_name)? {
        Some(hotel) => hotel,
        None => {
            info!(
                "Hotel '{}' is missing from the Context Graph. Bootstrapping it now.",
                hotel_name
            );
            let hotel = default_hotel_record(&hotel_name);
            graph_storage.upsert_hotel(&hotel)?;
            let guests = default_guest_seed(&hotel_name);
            graph_storage.seed_guests(&hotel_name, &guests)?;
            hotel
        }
    };

    if let Some(test) = args.test {
        prepare_startup_test_binaries(test)?;
        enable_guest_test_overrides(&graph_storage, &hotel_name, test)?;
    }

    graph_storage
        .upsert_agent_identity(&AgentIdentityRecord {
            agent_id: "agent-jane-01".into(),
            persona_name: "Jane".into(),
            bundle_json: vps_jane_identity_bundle(),
        })
        .context("Failed to seed default agent identity bundle")?;

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

    // Create the memory channel dispatcher for PORT-BP-003 to pick up
    // In PORT-BP-003, this receiver will hand off to the persistent mesh_events ledger
    let (dispatcher_tx, mut dispatcher_rx) = mpsc::channel::<LedgerCommand>(1024);

    // PORT-BP-004: Strictly Serialized Single Writer Thread for Durable Event Ledger
    let db_path_writer = db_path.to_owned();

    // Initialize Mutable State Components First
    let ledger = Arc::new(
        match ansible_mesh_core::ledger::EventLedger::open(&db_path_writer) {
            Ok(l) => l,
            Err(e) => {
                error!("Failed to open Event Ledger: {}", e);
                std::process::exit(1);
            }
        },
    );

    let tracker = Arc::new(
        match ansible_mesh_core::cursor::CursorTracker::open(&db_path_writer) {
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

    let ipc_server = IpcServer::new(socket_path, dispatcher_tx.clone(), graph_arc.clone());

    tokio::spawn(async move {
        if let Err(e) = ipc_server.run().await {
            error!("Hotel Front Desk (UDS) failed: {}", e);
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

    if guest_supervision_enabled() {
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

    if let Some(test) = args.test {
        let test_result = run_startup_test(
            test,
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
        // MVP: Hardcode target for now or leave generic for extension
        let targets = vec![("central-hotel".to_string(), "127.0.0.1:9099".to_string())];

        let rx_dispatch = shutdown_rx.resubscribe();
        tokio::spawn(crate::service::mesh_dispatcher::outbound_dispatcher(
            dispatcher_ledger,
            dispatcher_tracker,
            dispatcher_socket,
            caps.node_id.clone(),
            targets,
            rx_dispatch,
        ));
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

    // PORT-BP-004: Async Mesh Inbound Router
    // Receives BeaconMessages from the UDP socket and forwards them to the single DB writer thread
    let dispatcher_inbound_tx = dispatcher_tx.clone();
    tokio::spawn(async move {
        while let Some(msg) = inbox_rx.recv().await {
            match msg.msg_type {
                ansible_mesh_core::MsgType::MeshEventBatch => {
                    if let Ok(events) = serde_json::from_slice::<Vec<EventEnvelope>>(&msg.payload) {
                        if !events.is_empty() {
                            let max_seq = events.iter().map(|e| e.seq).max().unwrap_or(0);
                            let _ = dispatcher_inbound_tx
                                .send(LedgerCommand::CommitInboundBatch {
                                    events,
                                    source_node: msg.src_node.clone(),
                                })
                                .await; // The DB writer pushes this durably to the Inbox

                            // ACK immediately per idempotent design
                            let _ack_payload =
                                serde_json::json!({ "acked_seq": max_seq }).to_string();
                            // In a real scenario, this ACK would be enqueued back out to the remote node.
                            // For MVP, if we had a socket handle here, we'd fire an ACK UDP packet back.
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

    // PORT-BP-004: Async Mesh Outbound Dispatcher Loop
    // Polls unacked events and packages them into UDP batches over the WireGuard interface
    let db_path_dispatcher = db_path.to_owned();
    let socket_dispatcher = daemon.socket().clone();
    let local_node_id_dispatcher = local_node_id.clone();
    let mesh_auth_dispatcher = ansible_mesh_core::authz::MeshAuth::new(&mesh_psk);

    if flags.enable_rust_dispatcher {
        tokio::spawn(async move {
            let ledger = match ansible_mesh_core::ledger::EventLedger::open(&db_path_dispatcher) {
                Ok(l) => l,
                Err(_) => return,
            };
            let tracker = match ansible_mesh_core::cursor::CursorTracker::open(&db_path_dispatcher)
            {
                Ok(t) => t,
                Err(_) => return,
            };

            let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
            loop {
                interval.tick().await;

                // For MVP: Target node is remote-ansible-02
                let target_node = "remote-ansible-02";
                let cursor = tracker.get_cursor(target_node).unwrap_or(0);

                if let Ok(events) = ledger.query_unacked_events(target_node, cursor, 50) {
                    if !events.is_empty() {
                        // trace!("Dispatcher pushing {} unacked events to {}", events.len(), target_node);

                        if let Ok(payload_bytes) = serde_json::to_vec(&events) {
                            let msg_id = uuid::Uuid::new_v4();
                            let seq = 0;
                            let timestamp = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();

                            let hmac = mesh_auth_dispatcher.sign(
                                &msg_id,
                                seq as u64,
                                &payload_bytes,
                                timestamp,
                            );

                            let msg = ansible_mesh_core::BeaconMessage {
                                version: 1,
                                msg_id,
                                src_node: local_node_id_dispatcher.clone(),
                                dest_node: target_node.to_string(),
                                msg_type: ansible_mesh_core::MsgType::MeshEventBatch,
                                seq,
                                total: 1,
                                timestamp,
                                payload: payload_bytes,
                                hmac,
                            };

                            // UDP MTU is ~1420 bytes. For MVP, assuming the batch fits.
                            // For larger payloads, PORT_BLUEPRINT requires attachment by reference TCP.
                            if let Ok(packet) = serde_json::to_vec(&msg) {
                                let target_addr = "127.0.0.1:8999";
                                if let Err(e) =
                                    socket_dispatcher.send_to(&packet, target_addr).await
                                {
                                    tracing::error!("UDP send failed: {}", e);
                                }
                            }
                        }
                    }
                }
            }
        });
    }

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
        STARTUP_TEST_TEXT_REPLY, StartupTest, default_guest_seed, default_hotel_record,
        enable_guest_test_overrides, extract_context_graph_entries, guest_supervision_enabled,
        hotel_base_port, run_startup_test,
    };
    use crate::service::ipc::IpcServer;
    use ansible_mesh_core::sqlite_storage::SqliteGraphStorage;
    use ansible_mesh_core::storage::GraphStorage;
    use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
    use serde_json::json;
    use std::path::Path;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use uuid::Uuid;

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
        assert!(guests.iter().any(|guest| guest.role == "tool"));
        assert!(guests.iter().any(|guest| guest.role == "model.gemini"));
        assert!(guests.iter().any(|guest| guest.role == "model.elevenlabs"));
    }

    #[test]
    fn context_graph_entries_support_nested_section() {
        let entries = extract_context_graph_entries(&serde_json::json!({
            "context_graph": {
                "telegram_bot_token": "token",
                "elevenlabs_api_key": "key"
            },
            "ignored": {
                "not": "imported"
            }
        }));

        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|(key, _)| key == "telegram_bot_token"));
        assert!(entries.iter().any(|(key, _)| key == "elevenlabs_api_key"));
    }

    #[test]
    fn context_graph_entries_preserve_flat_config_compatibility() {
        let entries = extract_context_graph_entries(&serde_json::json!({
            "telegram_bot_token": "token",
            "gemini_api_key": "key"
        }));

        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|(key, _)| key == "telegram_bot_token"));
        assert!(entries.iter().any(|(key, _)| key == "gemini_api_key"));
    }

    #[test]
    fn text_startup_test_injects_stub_response_into_gemini_guest() {
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
        let gemini = stored
            .into_iter()
            .find(|guest| guest.role == "model.gemini")
            .expect("gemini guest should exist");
        let config: serde_json::Value =
            serde_json::from_str(&gemini.config_json).expect("config should decode");

        assert_eq!(
            config["env"]["PHILOTIC_MODEL_ROUTER_STUB_RESPONSE"].as_str(),
            Some(STARTUP_TEST_TEXT_REPLY)
        );
    }

    #[tokio::test]
    async fn text_startup_test_round_trips_through_fake_agent_and_model() {
        let socket_path = format!("/tmp/ansible-startup-{}.sock", Uuid::new_v4().simple());
        let (dispatcher_tx, _dispatcher_rx) = mpsc::channel(8);
        let graph_store = SqliteGraphStorage::open(":memory:").expect("open sqlite graph store");
        let graph: Arc<dyn GraphStorage> = Arc::new(graph_store);
        let server = IpcServer::new(socket_path.clone(), dispatcher_tx, graph);

        let server_task = tokio::spawn(async move {
            server.run().await.expect("ipc server should run");
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;

        let agent_socket = socket_path.clone();
        let agent_task = tokio::spawn(async move {
            let mut agent = PhiloticClient::connect_at(
                &agent_socket,
                GuestIdentity {
                    guest_id: "fake-agent".into(),
                    role: "agent".into(),
                    supported_tools: Vec::new(),
                },
            )
            .await
            .expect("agent connect");

            let inbound = agent.recv_task().await.expect("agent inbound");
            let IpcResponse::InboundTask { task_json, .. } = inbound else {
                panic!("unexpected agent inbound");
            };
            let payload: serde_json::Value =
                serde_json::from_str(&task_json).expect("decode agent payload");

            agent
                .send_request(IpcRequest::EmitTask {
                    target_node: "local-ansible-01".into(),
                    target_role: "model.gemini".into(),
                    target_guest_id: None,
                    task_json: json!({
                        "action": "generate_text",
                        "session_id": payload["session_id"],
                        "turn_id": payload["turn_id"],
                        "prompt": payload["content"],
                        "user_content": payload["content"],
                        "chat_id": payload["chat_id"],
                        "reply_to": "local-ansible-01",
                        "reply_role": "ansible-startup-test",
                        "final_reply_to": "local-ansible-01",
                        "final_reply_role": "ansible-startup-test",
                        "final_reply_guest_id": "ansible-startup-test-client"
                    })
                    .to_string(),
                })
                .await
                .expect("agent emit model request");
        });

        let model_socket = socket_path.clone();
        let model_task = tokio::spawn(async move {
            let mut model = PhiloticClient::connect_at(
                &model_socket,
                GuestIdentity {
                    guest_id: "fake-model".into(),
                    role: "model.gemini".into(),
                    supported_tools: Vec::new(),
                },
            )
            .await
            .expect("model connect");

            let inbound = model.recv_task().await.expect("model inbound");
            let IpcResponse::InboundTask { task_json, .. } = inbound else {
                panic!("unexpected model inbound");
            };
            let payload: serde_json::Value =
                serde_json::from_str(&task_json).expect("decode model payload");

            model
                .send_request(IpcRequest::EmitTask {
                    target_node: "local-ansible-01".into(),
                    target_role: "ansible-startup-test".into(),
                    target_guest_id: Some("ansible-startup-test-client".into()),
                    task_json: json!({
                        "action": "send_reply",
                        "session_id": payload["session_id"],
                        "turn_id": payload["turn_id"],
                        "chat_id": payload["chat_id"],
                        "content": STARTUP_TEST_TEXT_REPLY
                    })
                    .to_string(),
                })
                .await
                .expect("model emit final reply");
        });

        run_startup_test(
            StartupTest::TextRoundTrip,
            &socket_path,
            None,
            Some("hello startup test"),
        )
        .await
        .expect("startup text roundtrip should succeed");

        agent_task.await.expect("join agent task");
        model_task.await.expect("join model task");
        server_task.abort();
        let _ = server_task.await;
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }
}
