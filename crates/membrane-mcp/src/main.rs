mod auth;
mod dispatch;
mod protocol;
mod routing;
mod server;
mod transform;

use ansible_mesh_core::mcp_endpoint::McpEndpointConfig;
use ansible_mesh_core::mcp_route::McpRouteRecord;
use anyhow::Result;
use async_trait::async_trait;
use auth::{AllotmentTracker, VaultHashCache, VaultResolver};
use clap::Parser;
use membrane::{LeaseRenewResult, MembraneGuest, OutboundReply};
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use routing::{new_shared_endpoint_table, new_shared_table};
use server::{MembraneState, build_router};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tracing::info;
use tracing::{error, warn};

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "membrane-mcp", about = "Philotic MCP membrane guest")]
struct Args {
    #[arg(short, long, env = "MCP_PORT", default_value_t = 9100)]
    port: u16,

    #[arg(long, env = "MCP_STATIC_ROUTES")]
    static_routes: Option<std::path::PathBuf>,

    #[arg(long, env = "PHILOTIC_HOTEL_SOCKET")]
    ipc_socket: Option<String>,

    #[arg(long, env = "PHILOTIC_GUEST_ID", default_value = "membrane-mcp-01")]
    guest_id: String,

    #[arg(long, env = "PHILOTIC_NODE_ID", default_value = "local-aiua-01")]
    node_id: String,
}

// ── Vault resolver ────────────────────────────────────────────────────────────

struct IpcVaultResolver {
    socket_path: String,
}

impl VaultResolver for IpcVaultResolver {
    fn resolve(&self, vault_ref: &str) -> Result<[u8; 32]> {
        let socket_path = self.socket_path.clone();
        let vault_ref = vault_ref.to_string();

        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async move {
                let mut client = PhiloticClient::connect_at(
                    &socket_path,
                    GuestIdentity {
                        guest_id: "membrane-mcp-vault".into(),
                        role: "mcp-membrane".into(),
                        supported_tools: vec![],
                    },
                )
                .await?;

                let resp = client
                    .send_request(IpcRequest::GetSecret {
                        secret_ref: vault_ref.clone(),
                    })
                    .await?;

                match resp {
                    IpcResponse::SecretData {
                        value_json: Some(json),
                        ..
                    } => {
                        // value_json may be double-encoded (stored plaintext is itself JSON).
                        let step1: String = serde_json::from_str(&json)
                            .unwrap_or_else(|_| json.trim_matches('"').to_string());
                        let hex: String = if step1.starts_with('"') {
                            serde_json::from_str(&step1)
                                .unwrap_or_else(|_| step1.trim_matches('"').to_string())
                        } else {
                            step1
                        };
                        let bytes = hex::decode(&hex)
                            .map_err(|e| anyhow::anyhow!("bad hex in vault: {}", e))?;
                        bytes
                            .try_into()
                            .map_err(|_| anyhow::anyhow!("vault hash must be 32 bytes"))
                    }
                    IpcResponse::SecretData {
                        value_json: None, ..
                    } => {
                        anyhow::bail!("vault_ref '{}' not found", vault_ref)
                    }
                    other => anyhow::bail!("unexpected vault response: {:?}", other),
                }
            })
        })
    }
}

// ── McpMembrane guest ─────────────────────────────────────────────────────────

struct McpMembrane {
    port: u16,
    lease_key_value: String,
    state: Arc<MembraneState>,
}

impl McpMembrane {
    fn new(port: u16, node_id: &str, state: Arc<MembraneState>) -> Self {
        Self {
            port,
            lease_key_value: format!("mcp-membrane:{}", node_id),
            state,
        }
    }
}

#[async_trait]
impl MembraneGuest for McpMembrane {
    fn role(&self) -> &'static str {
        "mcp-membrane"
    }

    fn lease_key(&self) -> String {
        self.lease_key_value.clone()
    }

    async fn setup(&mut self, client: &mut PhiloticClient) -> Result<()> {
        // Acquire MCP membrane lease.
        let req = IpcRequest::AcquireMcpMembraneLease {
            lease_key: self.lease_key_value.clone(),
            port: self.port,
        };
        match client.send_request(req).await {
            Ok(IpcResponse::McpMembraneLease {
                mcp_granted: true, ..
            }) => {
                info!("MCP membrane lease acquired");
            }
            Ok(IpcResponse::McpMembraneLease {
                mcp_granted: false, ..
            }) => {
                anyhow::bail!("MCP membrane lease denied — another instance may be running");
            }
            Ok(other) => {
                warn!(?other, "unexpected lease response — continuing");
            }
            Err(e) => {
                warn!(err = %e, "lease request failed — continuing without hotel lease");
            }
        }

        // Replay any routes that were persisted before this restart.
        match client.send_request(IpcRequest::GetMcpRoutes {}).await {
            Ok(IpcResponse::McpRouteState { agents }) if !agents.is_empty() => {
                let mut table = self.state.routing_table.write().await;
                for entry in &agents {
                    table.upsert_agent_routes(&entry.agent_id, entry.routes.clone());
                }
                info!(
                    count = agents.len(),
                    "replayed persisted MCP routes on startup"
                );
            }
            Ok(_) => {}
            Err(e) => warn!(err = %e, "GetMcpRoutes failed — starting with empty route table"),
        }

        // Start HTTP server (detached task).
        let addr = SocketAddr::from(([0, 0, 0, 0], self.port));
        let router =
            build_router(self.state.clone()).into_make_service_with_connect_info::<SocketAddr>();
        tokio::spawn(async move {
            info!(%addr, "MCP membrane HTTP server starting");
            match tokio::net::TcpListener::bind(addr).await {
                Ok(listener) => {
                    if let Err(e) = axum::serve(listener, router).await {
                        error!(err = %e, "MCP HTTP server error");
                    }
                }
                Err(e) => error!(err = %e, "MCP HTTP server bind failed"),
            }
        });

        Ok(())
    }

    async fn deliver(&mut self, reply: OutboundReply) -> Result<()> {
        let turn_id = reply.turn_id().to_string();

        match &reply {
            OutboundReply::ApprovalRequired { .. } => {
                // Philote acknowledged the approval gate — the HTTP caller continues
                // to wait. The oneshot stays parked; it fires when the operator
                // resolves and the philote sends the final Text/Error reply.
                info!(
                    turn_id,
                    "approval required acknowledged — oneshot remains parked"
                );
                return Ok(());
            }
            OutboundReply::StreamingToken { .. } => {
                // Streaming accumulation over MCP is not yet implemented.
                return Ok(());
            }
            _ => {}
        }

        let sender = {
            let mut pending = self.state.pending_responses.lock().await;
            pending.remove(&turn_id)
        };

        match (reply, sender) {
            (OutboundReply::Text { content, .. }, Some(tx)) => {
                let _ = tx.send(content);
            }
            (OutboundReply::Error { message, .. }, Some(tx)) => {
                let _ = tx.send(serde_json::json!({ "error": message }).to_string());
            }
            (_, None) => {
                warn!(turn_id, "deliver: no pending receiver for turn");
            }
            _ => {}
        }

        Ok(())
    }

    async fn handle_push(&mut self, msg: &IpcResponse) -> Result<bool> {
        let task_json = match msg {
            IpcResponse::InboundTask { task_json, .. } => task_json,
            _ => return Ok(false),
        };

        let payload: serde_json::Value = match serde_json::from_str(task_json) {
            Ok(v) => v,
            Err(_) => return Ok(false),
        };

        let action = match payload.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return Ok(false),
        };

        match action {
            "update_mcp_routes" => {
                let agent_id = payload
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let records: Vec<McpRouteRecord> = payload
                    .get("routes")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let count = records.len();
                let mut table = self.state.routing_table.write().await;
                table.upsert_agent_routes(&agent_id, records);
                info!(agent_id, count, "route table updated from hotel push");
                Ok(true)
            }
            "revoke_mcp_routes" => {
                let agent_id = payload
                    .get("agent_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let mut table = self.state.routing_table.write().await;
                table.revoke_agent_routes(&agent_id);
                info!(agent_id, "agent routes revoked from hotel push");
                Ok(true)
            }
            "update_mcp_config" => {
                let config: McpEndpointConfig = match payload
                    .get("config")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                {
                    Some(c) => c,
                    None => {
                        warn!("update_mcp_config push missing or invalid 'config' field");
                        return Ok(false);
                    }
                };
                let mut table = self.state.endpoint_table.write().await;
                table.update(config);
                Ok(true)
            }
            "revoke_mcp_config" => {
                let mut table = self.state.endpoint_table.write().await;
                table.revoke();
                Ok(true)
            }
            "update_perimeter" => {
                let tier: ansible_mesh_core::ExposureTier = match payload
                    .get("tier")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                {
                    Some(t) => t,
                    None => {
                        warn!("update_perimeter push missing or invalid 'tier' field");
                        return Ok(false);
                    }
                };
                *self.state.ingress_tier.write().unwrap() = tier;
                info!(?tier, "Ingress fence tier updated from hotel push");
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn renew(&mut self, client: &mut PhiloticClient) -> Result<LeaseRenewResult> {
        let req = IpcRequest::AcquireMcpMembraneLease {
            lease_key: self.lease_key_value.clone(),
            port: self.port,
        };
        match client.send_request(req).await {
            Ok(IpcResponse::McpMembraneLease {
                mcp_granted: true, ..
            }) => Ok(LeaseRenewResult::Ok { epoch: 0 }),
            Ok(IpcResponse::McpMembraneLease {
                mcp_granted: false, ..
            }) => Ok(LeaseRenewResult::NeedsReacquire),
            Ok(_) => Ok(LeaseRenewResult::Ok { epoch: 0 }),
            Err(e) => Err(e),
        }
    }

    async fn teardown(&mut self, client: &mut PhiloticClient) {
        let req = IpcRequest::ReleaseMcpMembraneLease {
            lease_key: self.lease_key_value.clone(),
        };
        if let Err(e) = client.send_request(req).await {
            warn!(err = %e, "lease release failed during teardown");
        }
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "membrane_mcp=info,membrane=info".into()),
        )
        .init();

    let args = Args::parse();
    info!(port = args.port, "membrane-mcp starting");

    let table = new_shared_table();

    // Seed from static config file (testing path).
    if let Some(path) = &args.static_routes {
        let raw = std::fs::read_to_string(path)?;
        let records: Vec<McpRouteRecord> = serde_json::from_str(&raw)?;
        let mut t = table.write().await;
        let mut by_agent: std::collections::HashMap<String, Vec<McpRouteRecord>> =
            std::collections::HashMap::new();
        for record in records {
            by_agent
                .entry(record.agent_id.clone())
                .or_default()
                .push(record);
        }
        for (agent_id, routes) in by_agent {
            t.upsert_agent_routes(&agent_id, routes);
        }
        info!(path = %path.display(), "loaded static routes");
    }

    // Create the inbound channel shared between HTTP handlers and the IPC runtime.
    let (inbound_tx, inbound_rx) = mpsc::channel(128);
    let pending_responses = Arc::new(Mutex::new(HashMap::new()));

    let endpoint_table = new_shared_endpoint_table();

    // Default to Local (safest). Updated via update_perimeter push once the hotel connects.
    let ingress_tier = Arc::new(std::sync::RwLock::new(ansible_mesh_core::ExposureTier::Local));

    let state = Arc::new(MembraneState {
        routing_table: table,
        endpoint_table,
        vault_cache: VaultHashCache::new(),
        allotment: AllotmentTracker::new(),
        vault: Box::new(IpcVaultResolver {
            socket_path: args.ipc_socket.clone().unwrap_or_else(|| {
                std::env::var("PHILOTIC_HOTEL_SOCKET")
                    .unwrap_or_else(|_| "/tmp/philotic-aiua.sock".to_string())
            }),
        }),
        node_id: args.node_id.clone(),
        inbound_tx,
        pending_responses,
        ingress_tier,
    });

    let guest = McpMembrane::new(args.port, &args.node_id, state);

    if let Some(socket) = &args.ipc_socket {
        membrane::MembraneRuntime::new(socket, &args.guest_id, &args.node_id)
            .with_inbound_rx(inbound_rx)
            .run(guest)
            .await
    } else {
        // No IPC socket — run HTTP server directly (static-only mode).
        // Inbound envelopes go nowhere in this mode; pending responses will time out.
        info!("running in static-only mode (no IPC socket)");
        let addr = SocketAddr::from(([0, 0, 0, 0], args.port));
        let router =
            build_router(guest.state.clone()).into_make_service_with_connect_info::<SocketAddr>();
        let listener = tokio::net::TcpListener::bind(addr).await?;
        info!(%addr, "MCP membrane listening");
        axum::serve(listener, router).await?;
        Ok(())
    }
}
