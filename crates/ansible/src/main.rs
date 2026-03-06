use ansible_mesh_core::beacon::BeaconDaemon;
use ansible_mesh_core::storage::GraphStorage;
use ansible_mesh_core::{NodeCapabilities, NodeRole};
use anyhow::{Context, Result};
use clap::Parser;
use std::fs;
use std::path::Path;
use tokio::sync::mpsc;
use tracing::{error, info, warn, debug};

mod graph;

mod service;
use service::ipc::IpcServer;
use service::blob::BlobService;
use std::sync::Arc;

use ansible_mesh_core::event::EventEnvelope;

/// Instructions for the strictly-serialized DB writer thread
pub enum LedgerCommand {
    /// A new event spawned locally via IPC that needs to be durably outboxed
    AppendLocal(EventEnvelope),
    /// A batch of events received over the mesh that need to be durably inboxed
    CommitInboundBatch { events: Vec<EventEnvelope>, source_node: String },
    /// An ACK from a remote node that advances our delivery cursor
    ProcessAck { consumer_node_id: String, acked_seq: u64 },
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Optional path to a JSON file containing configuration to load into the Context Graph
    #[arg(long)]
    load_config: Option<String>,
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
            enable_rust_auth: std::env::var("PHILOTIC_ENABLE_RUST_AUTH").map(|v| v == "true" || v == "1").unwrap_or(false),
            enable_rust_dispatcher: std::env::var("PHILOTIC_ENABLE_RUST_DISPATCHER").map(|v| v == "true" || v == "1").unwrap_or(false),
            enable_rust_task_lifecycle: std::env::var("PHILOTIC_ENABLE_RUST_TASK_LIFECYCLE").map(|v| v == "true" || v == "1").unwrap_or(false),
        }
    }
}



#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    info!("Starting Philotic Ansible Daemon Boot Sequence...");
    
    let flags = AnsibleCutoverFlags::from_env();
    info!("--- CUTOVER FLAGS ---");
    info!("Rust Auth Validation: {}", if flags.enable_rust_auth { "ENABLED" } else { "PASSTHROUGH" });
    info!("Rust Outbound Dispatcher: {}", if flags.enable_rust_dispatcher { "ENABLED" } else { "DISABLED" });
    info!("Rust Task Lifecycle Ledger: {}", if flags.enable_rust_task_lifecycle { "ENABLED" } else { "DISABLED" });
    info!("---------------------");

    // Initialize the always-on Context Graph DB via the abstract storage trait
    let db_path = Path::new("ansible_context.db");
    let graph_storage = ansible_mesh_core::sqlite_storage::SqliteGraphStorage::open(db_path)?;

    // Handle Config Loading if requested
    if let Some(config_path) = args.load_config {
        info!("Loading configuration from '{}' into the Context Graph...", config_path);
        let config_data = fs::read_to_string(&config_path).context("Failed to read config file")?;
        let config_json: serde_json::Value = serde_json::from_str(&config_data).context("Invalid JSON config file")?;
        
        if let Some(obj) = config_json.as_object() {
            let mut count = 0;
            for (key, value) in obj {
                let val_str = if value.is_string() {
                    // Store strings as-is (with quotes, so they remain valid JSON strings in the db)
                    serde_json::to_string(value)?
                } else {
                    value.to_string()
                };
                
                let conn = graph_storage.raw_conn().lock().unwrap();
                conn.execute(
                    "INSERT OR REPLACE INTO node_config (key, value_json) VALUES (?, ?)",
                    [key, &val_str],
                )?;
                count += 1;
            }
            info!("Successfully injected {} configuration keys into Context Graph.", count);
        } else {
            warn!("Config file must be a JSON object mapping string keys to values.");
        }
    }

    // Load capabilities from the Graph, or initialize with a master default
    let caps = match graph_storage.load_node_capabilities()? {
        Some(c) => c,
        None => {
            info!("Context Graph is empty. Bootstrapping initial Hegemon configuration.");
            let default_caps = NodeCapabilities {
                node_id: "local-ansible-01".into(),
                roles: vec![NodeRole::AnsibleNode, NodeRole::Other("hegemon".into())],
                models: vec![],
                tools: vec![],
                constraints: Default::default(),
            };
            graph_storage.save_node_capabilities(&default_caps)?;
            
            // Seed a few demo Guests to be materialized 
            info!("Seeding End-to-End E2E Guests into Materialization Queue...");
            let conn = graph_storage.raw_conn().lock().unwrap();
            conn.execute(
                "INSERT OR REPLACE INTO materialized_guests (guest_id, role, config_json) VALUES 
                ('hegemon-gateway', 'hegemon', '{\"command\": \"target/debug/hegemon\", \"args\": []}'),
                ('agent-core-jane', 'agent', '{\"command\": \"target/debug/agent-core\", \"args\": []}'),
                ('model-router-gemini', 'model', '{\"command\": \"target/debug/model-router\", \"args\": []}')",
                [],
            )?;
            
            default_caps
        }
    };

    let mesh_port = 8999;
    let addr = format!("0.0.0.0:{}", mesh_port);
    info!("Starting Philotic Ansible Daemon '{}' on {}", caps.node_id, addr);
    
    // Channel for inbound mesh UDP payloads bubbled up by the BeaconDaemon
    let (inbox_tx, mut inbox_rx) = mpsc::channel::<ansible_mesh_core::BeaconMessage>(1024);
    
    // PORT-BP-006: Pre-Shared Key for mesh authentication
    let mesh_psk = std::env::var("PHILOTIC_MESH_PSK").unwrap_or_else(|_| "INSECURE_DEV_DEFAULT_PSK".to_string());
    
    let daemon = BeaconDaemon::bind(&addr, caps.clone(), inbox_tx, &mesh_psk, db_path.to_str().unwrap_or(""), flags.enable_rust_auth).await?;
    
    // Channel for pushing generated SDP Answers back out to the mesh
    let (webrtc_signal_tx, mut webrtc_signal_rx) = mpsc::channel::<ansible_mesh_core::webrtc::WebRtcSignalMessage>(32);
    
    // MATERIALIZATION LOOP: Spin up all guests defined in the DB as child processes
    info!("--- BEGIN UNIVERSAL MATERIALIZATION ---");
    
    // Broadcast channel to tell tasks to kill their child process on shutdown
    let (shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel::<()>(16);
    
    // Abstracted Universal Materializer with trait-object storage
    let graph_arc: Arc<dyn ansible_mesh_core::storage::GraphStorage> = Arc::new(graph_storage);
    let materializer = Box::new(crate::service::guest_manager::LocalProcessMaterializer::new());
    let guest_manager = Arc::new(crate::service::guest_manager::GuestManager::new(graph_arc.clone(), materializer));
    
    if let Err(e) = guest_manager.materialize_all(shutdown_rx.resubscribe()).await {
        error!("Universal Materialization failed: {}", e);
    }
    
    let gm_clone = Arc::clone(&guest_manager);
    let rx_supervise = shutdown_rx.resubscribe();
    tokio::spawn(async move {
        gm_clone.supervise_guests(rx_supervise).await;
    });

    // Spawning the "Hotel Front Desk" local IPC listener for Materialized Guests
    let socket_path = "/tmp/philotic-ansible.sock";
    
    // Create the memory channel dispatcher for PORT-BP-003 to pick up
    // In PORT-BP-003, this receiver will hand off to the persistent mesh_events ledger 
    let (dispatcher_tx, mut dispatcher_rx) = mpsc::channel::<LedgerCommand>(1024);

    // PORT-BP-004: Strictly Serialized Single Writer Thread for Durable Event Ledger
    let db_path_writer = db_path.to_owned();
    
    // Initialize Mutable State Components First
    let ledger = Arc::new(match ansible_mesh_core::ledger::EventLedger::open(&db_path_writer) {
        Ok(l) => l,
        Err(e) => {
            error!("Failed to open Event Ledger: {}", e);
            std::process::exit(1);
        }
    });
    
    let tracker = Arc::new(match ansible_mesh_core::cursor::CursorTracker::open(&db_path_writer) {
        Ok(t) => t,
        Err(e) => {
            error!("Failed to open Cursor Tracker: {}", e);
            std::process::exit(1);
        }
    });

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
                            error!("Failed to durably commit local event {}: {}", evt.event_id, e);
                        }
                    }
                    LedgerCommand::CommitInboundBatch { events, source_node: _ } => {
                        let mut max_seq = 0;
                        for mut evt in events {
                            if evt.seq > max_seq { max_seq = evt.seq; }
                            if let Err(e) = ledger_writer.append_event(&mut evt) {
                                error!("Failed to durably commit inbound event {}: {}", evt.event_id, e);
                            }
                        }
                        // Typically we would now trigger an ACK back to source_node with max_seq
                        // For MVP, that logic is built into the mesh receiver hook.
                    }
                    LedgerCommand::ProcessAck { consumer_node_id, acked_seq } => {
                        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
                        if let Err(e) = tracker_writer.advance_cursor(&consumer_node_id, acked_seq, ts) {
                            error!("Failed to advance cursor for node {}: {}", consumer_node_id, e);
                        } else {
                            info!("Cursor for node {} advanced to seq {}", consumer_node_id, acked_seq);
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

    // PORT-BP-003: Mesh Outbound Dispatcher (Periodic Queuing Loop)
    if flags.enable_rust_dispatcher {
        let dispatcher_ledger = ledger.clone();
        let dispatcher_tracker = tracker.clone();
        let dispatcher_socket = daemon.socket();
        // MVP: Hardcode target for now or leave generic for extension
        let targets = vec![
            ("central-hotel".to_string(), "127.0.0.1:9099".to_string())
        ];
        
        let rx_dispatch = shutdown_rx.resubscribe();
        tokio::spawn(crate::service::mesh_dispatcher::outbound_dispatcher(
            dispatcher_ledger,
            dispatcher_tracker,
            dispatcher_socket,
            caps.node_id.clone(),
            targets,
            rx_dispatch
        ));
    }

    // PORT-BP-005: Large Payload Transport via Dedicated HTTP Server
    let blob_port = 9000;
    let blob_addr = format!("0.0.0.0:{}", blob_port);
    let blob_dir = std::path::Path::new(db_path).parent().unwrap_or(std::path::Path::new(".")).join("blobs");
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
                            let _ = dispatcher_inbound_tx.send(LedgerCommand::CommitInboundBatch { 
                                events, 
                                source_node: msg.src_node.clone() 
                            }).await; // The DB writer pushes this durably to the Inbox
                            
                            // ACK immediately per idempotent design
                            let _ack_payload = serde_json::json!({ "acked_seq": max_seq }).to_string();
                            // In a real scenario, this ACK would be enqueued back out to the remote node.
                            // For MVP, if we had a socket handle here, we'd fire an ACK UDP packet back.
                        }
                    }
                }
                ansible_mesh_core::MsgType::MeshEventAck => {
                debug!("Received MeshEventAck from {}", msg.src_node);
                // Dispatch to the single writer thread to handle cursor advancement
                if let Ok(ack_payload) = serde_json::from_slice::<serde_json::Value>(&msg.payload) {
                    if let Some(acked_seq) = ack_payload.get("acked_seq").and_then(|v| v.as_u64()) {
                        let _ = dispatcher_inbound_tx.send(LedgerCommand::ProcessAck { 
                            consumer_node_id: msg.src_node.clone(), 
                            acked_seq 
                        }).await;
                    }
                }
            }
            ansible_mesh_core::MsgType::WebRtcSignal => {
                info!("Received WebRTC Signaling Payload from {}", msg.src_node);
                if let Ok(signal_msg) = serde_json::from_slice::<ansible_mesh_core::webrtc::WebRtcSignalMessage>(&msg.payload) {
                    let webrtc_signal_tx = webrtc_signal_tx.clone();
                    tokio::spawn(async move {
                        // In MVP 2 this channels to a long-running Guest Manager
                        // For MVP 1 we just spin off a detached Guest directly
                        if let ansible_mesh_core::webrtc::SignalPayload::Offer(sdp) = signal_msg.signal {
                            let guest = crate::service::webrtc_guest::WebRtcGuest::new(
                                signal_msg.session_id, 
                                msg.src_node, 
                                webrtc_signal_tx
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
    let local_node_id = "local-ansible-01".to_string(); // In a real app we get this from config
    
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
                Ok(l) => l, Err(_) => return,
            };
            let tracker = match ansible_mesh_core::cursor::CursorTracker::open(&db_path_dispatcher) {
                Ok(t) => t, Err(_) => return,
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
                                
                            let hmac = mesh_auth_dispatcher.sign(&msg_id, seq as u64, &payload_bytes, timestamp);
                            
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
                                if let Err(e) = socket_dispatcher.send_to(&packet, target_addr).await {
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
            info!("Ansible Daemon shutdown complete.");
        }
    }
    
    Ok(())
}
