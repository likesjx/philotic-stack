use ansible_mesh_core::beacon::BeaconDaemon;
use ansible_mesh_core::{NodeCapabilities, NodeRole};
use anyhow::{Context, Result};
use clap::Parser;
use philotic_ipc::{IpcRequest, IpcResponse};
use std::fs;
use std::path::Path;
use tokio::net::UdpSocket;
use tokio::process::Command;
use tracing::{error, info, warn};

mod graph;
use graph::ContextGraph;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Optional path to a JSON file containing configuration to load into the Context Graph
    #[arg(long)]
    load_config: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    info!("Starting Philotic Ansible Daemon Boot Sequence...");

    // Initialize the always-on Context Graph DB
    let db_path = Path::new("ansible_context.db");
    let graph = ContextGraph::open(db_path)?;

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
                
                graph.conn.execute(
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
    let caps = match graph.load_node_capabilities()? {
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
            graph.save_node_capabilities(&default_caps)?;
            
            // Seed a few demo Guests to be materialized 
            info!("Seeding End-to-End E2E Guests into Materialization Queue...");
            graph.conn.execute(
                "INSERT OR REPLACE INTO materialized_guests (guest_id, role, config_json) VALUES 
                ('hegemon-gateway', 'hegemon', '{\"command\": \"target/debug/hegemon\", \"args\": []}'),
                ('agent-core-jane', 'agent', '{\"command\": \"target/debug/agent-core\", \"args\": []}'),
                ('model-router-gemini', 'model', '{\"command\": \"target/debug/model-router\", \"args\": []}')",
                [],
            )?;
            
            default_caps
        }
    };
    
    // MATERIALIZATION LOOP: Spin up all guests defined in the DB as child processes
    info!("--- BEGIN UNIVERSAL MATERIALIZATION ---");
    
    // Broadcast channel to tell tasks to kill their child process on shutdown
    let (shutdown_tx, _) = tokio::sync::broadcast::channel::<()>(16);
    
    let mut stmt = graph.conn.prepare("SELECT guest_id, role, config_json, active_pid FROM materialized_guests WHERE is_active = 1")?;
    let mut rows = stmt.query([])?;
    
    while let Some(row) = rows.next()? {
        let guest_id: String = row.get(0)?;
        let role: String = row.get(1)?;
        let config_json: String = row.get(2)?;
        let active_pid: Option<u32> = row.get(3).unwrap_or(None);
        
        info!("Materializing Guest [{}] (Role: {})", guest_id, role);
        
        let config: serde_json::Value = serde_json::from_str(&config_json).unwrap_or_default();
        if let Some(cmd) = config.get("command").and_then(|c| c.as_str()) {
            let mut command = Command::new(cmd);
            if let Some(args) = config.get("args").and_then(|a| a.as_array()) {
                for arg in args {
                    if let Some(s) = arg.as_str() {
                        command.arg(s);
                    }
                }
            }
            
            // --- GHOST RECLAMATION (Context Graph) ---
            // Before spawning, hunt down any orphans listed in the Graph
            if let Some(pid) = active_pid {
                info!("Context Graph shows Ghost PID {} for Guest [{}]. Reclaiming identity...", pid, guest_id);
                // Use standard OS command to forcefully terminate the orphan
                let _ = std::process::Command::new("kill").arg("-9").arg(pid.to_string()).status();
            }

            let mut shutdown_rx = shutdown_tx.subscribe();
            tokio::spawn(async move {
                match command.spawn() {
                    Ok(mut child) => {
                        let child_pid = child.id().unwrap_or(0);
                        info!("✨ Successfully spawned child process for Guest [{}] (PID: {})", guest_id, child_pid);
                        
                        // Bind this incarnation's soul to the Context Graph natively using a local SQLite connection
                        if let Ok(local_graph) = ContextGraph::open("ansible_context.db") {
                            let _ = local_graph.conn.execute("UPDATE materialized_guests SET active_pid = ? WHERE guest_id = ?", rusqlite::params![child_pid, guest_id]);
                        }
                        
                        tokio::select! {
                            _ = child.wait() => {
                                warn!("Guest [{}] process has exited voluntarily.", guest_id);
                            }
                            _ = shutdown_rx.recv() => {
                                info!("Shutting down Guest [{}]...", guest_id);
                                let _ = child.kill().await;
                            }
                        }
                        // Clean up the tombstone from the Graph
                        if let Ok(local_graph) = ContextGraph::open("ansible_context.db") {
                            let _ = local_graph.conn.execute("UPDATE materialized_guests SET active_pid = NULL WHERE guest_id = ?", rusqlite::params![guest_id]);
                        }
                    }
                    Err(e) => error!("❌ Failed to materialize Guest [{}]: {}", guest_id, e),
                }
            });
        }
    }
    info!("--- END UNIVERSAL MATERIALIZATION ---");

    // Spawning the "Hotel Front Desk" local IPC listener for Materialized Guests
    let ipc_port = 9000;
    
    // We need a cloned graph connection to query config in the async UDP handler
    let db_path_clone = db_path.to_owned();
    
    tokio::spawn(async move {
        // Open a thread-local SQLite connection for the UDP worker
        let graph = match ContextGraph::open(&db_path_clone) {
            Ok(g) => g,
            Err(e) => {
                error!("Failed to open local Context Graph for UDP Front Desk: {}", e);
                return;
            }
        };

        let addr = format!("127.0.0.1:{}", ipc_port);
        let socket = match UdpSocket::bind(&addr).await {
            Ok(s) => {
                info!("Hotel Front Desk listening for Guest IPC on {}", addr);
                s
            }
            Err(e) => {
                error!("Failed to bind Front Desk IPC socket: {}", e);
                return;
            }
        };

        // Cache of registered guests to route EmitTask
        // guest_id -> SocketAddr
        let mut active_guests: std::collections::HashMap<String, std::net::SocketAddr> = std::collections::HashMap::new();
        // role -> SocketAddr (simplistic routing for role-based targeting)
        let mut active_roles: std::collections::HashMap<String, std::net::SocketAddr> = std::collections::HashMap::new();

        let mut buf = vec![0u8; 65535];
        loop {
            match socket.recv_from(&mut buf).await {
                Ok((len, src)) => {
                    match serde_json::from_slice::<IpcRequest>(&buf[..len]) {
                        Ok(req) => {
                            // Only debug log to avoid spam
                            // info!("Front Desk received IPC request from Guest [{}]: {:?}", src, req);
                            
                            match req {
                                IpcRequest::Register(identity) => {
                                    info!("Front Desk registering Guest [{}] (Role: {}) at {}", identity.guest_id, identity.role, src);
                                    active_guests.insert(identity.guest_id.clone(), src);
                                    active_roles.insert(identity.role.clone(), src);
                                    
                                    let resp = IpcResponse::Ack { req_id: "reg-ack".into() };
                                    if let Ok(bytes) = serde_json::to_vec(&resp) {
                                        let _ = socket.send_to(&bytes, &src).await;
                                    }
                                }
                                IpcRequest::GetConfig { key } => {
                                    // Query SQLite
                                    let value_json = {
                                        let mut stmt = graph.conn.prepare("SELECT value_json FROM node_config WHERE key = ?").unwrap();
                                        if let Ok(mut rows) = stmt.query([&key]) {
                                            if let Ok(Some(row)) = rows.next() {
                                                if let Ok(val) = row.get::<_, String>(0) {
                                                    Some(val)
                                                } else { None }
                                            } else { None }
                                        } else { None }
                                    };
                                    
                                    let resp = IpcResponse::ConfigData { key, value_json };
                                    if let Ok(bytes) = serde_json::to_vec(&resp) {
                                        let _ = socket.send_to(&bytes, &src).await;
                                    }
                                }
                                IpcRequest::EmitTask { target_node: _, target_role, task_json } => {
                                    // Route to local active guest by role
                                    info!("Routing EmitTask to Role [{}]", target_role);
                                    if let Some(target_addr) = active_roles.get(&target_role) {
                                        // Package as InboundTask
                                        let fwd = IpcResponse::InboundTask {
                                            task_id: uuid::Uuid::new_v4(),
                                            source_node: "local-ansible-01".into(), // We should look up the sender's guest ID if we could, but fake it for now
                                            task_json,
                                        };
                                        if let Ok(bytes) = serde_json::to_vec(&fwd) {
                                            let _ = socket.send_to(&bytes, target_addr).await;
                                        }
                                        
                                        // Ack original sender
                                        let ack = IpcResponse::Ack { req_id: "emit-ack".into() };
                                        if let Ok(bytes) = serde_json::to_vec(&ack) {
                                            let _ = socket.send_to(&bytes, &src).await;
                                        }
                                    } else {
                                        warn!("Cannot route task. No active Guest found for role: {}", target_role);
                                    }
                                }
                                _ => {
                                    // Generic Ack
                                    let resp = IpcResponse::Ack { req_id: "hotel-ack".into() };
                                    if let Ok(bytes) = serde_json::to_vec(&resp) {
                                        let _ = socket.send_to(&bytes, &src).await;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Front Desk received malformed IPC payload from {}: {}", src, e);
                        }
                    }
                }
                Err(e) => {
                    error!("Front Desk socket error: {}", e);
                }
            }
        }
    });

    let mesh_port = 8999;
    let addr = format!("0.0.0.0:{}", mesh_port);
    info!("Starting Philotic Ansible Daemon '{}' on {}", caps.node_id, addr);

    let daemon = BeaconDaemon::bind(&addr, caps).await?;
    
    // Run the daemon loop and catch Ctrl+C to gracefully kill children
    tokio::select! {
        res = daemon.run_loop() => {
            if let Err(e) = res {
                error!("Beacon Daemon error: {}", e);
            }
        }
        _ = tokio::signal::ctrl_c() => {
            warn!("Ctrl-C received! Initiating shutdown of all Materialized Guests...");
            let _ = shutdown_tx.send(());
            // Give children a brief moment to be killed before the main process exits
            tokio::time::sleep(tokio::time::Duration::from_millis(1500)).await;
            info!("Ansible Daemon shutdown complete.");
        }
    }
    
    Ok(())
}
