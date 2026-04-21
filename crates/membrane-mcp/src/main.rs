mod auth;
mod dispatch;
mod protocol;
mod routing;
mod server;

use anyhow::Result;
use ansible_mesh_core::mcp_route::McpRouteRecord;
use async_trait::async_trait;
use auth::{AllotmentTracker, VaultHashCache, VaultResolver};
use clap::Parser;
use membrane::{LeaseRenewResult, MembraneGuest, OutboundReply};
use philotic_client::{IpcRequest, IpcResponse, PhiloticClient};
use tracing::info;
use routing::new_shared_table;
use server::{MembraneState, build_router};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tracing::{error, warn};

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "membrane-mcp", about = "Philotic MCP membrane guest")]
struct Args {
    #[arg(short, long, env = "MCP_PORT", default_value_t = 9100)]
    port: u16,

    #[arg(long, env = "MCP_STATIC_ROUTES")]
    static_routes: Option<std::path::PathBuf>,

    #[arg(long, env = "PHILOTIC_IPC_SOCKET")]
    ipc_socket: Option<String>,

    #[arg(long, env = "PHILOTIC_GUEST_ID", default_value = "membrane-mcp-01")]
    guest_id: String,

    #[arg(long, env = "PHILOTIC_NODE_ID", default_value = "local-aiua-01")]
    node_id: String,
}

// ── Vault stub (Slice 1) ──────────────────────────────────────────────────────

struct IpcVaultResolver;

impl VaultResolver for IpcVaultResolver {
    fn resolve(&self, vault_ref: &str) -> Result<[u8; 32]> {
        warn!(vault_ref, "vault resolver stub — Slice 2 wires real IPC lookup");
        Ok([0u8; 32])
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
            Ok(IpcResponse::McpMembraneLease { mcp_granted: true, .. }) => {
                info!("MCP membrane lease acquired");
            }
            Ok(IpcResponse::McpMembraneLease { mcp_granted: false, .. }) => {
                anyhow::bail!("MCP membrane lease denied — another instance may be running");
            }
            Ok(other) => {
                warn!(?other, "unexpected lease response — continuing");
            }
            Err(e) => {
                warn!(err = %e, "lease request failed — continuing without hotel lease");
            }
        }

        // Start HTTP server (detached task).
        let addr = SocketAddr::from(([0, 0, 0, 0], self.port));
        let router = build_router(self.state.clone()).into_make_service_with_connect_info::<SocketAddr>();
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
                info!(turn_id, "approval required acknowledged — oneshot remains parked");
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
                let _ = tx.send(
                    serde_json::json!({ "error": message }).to_string(),
                );
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
            _ => Ok(false),
        }
    }

    async fn renew(&mut self, client: &mut PhiloticClient) -> Result<LeaseRenewResult> {
        let req = IpcRequest::AcquireMcpMembraneLease {
            lease_key: self.lease_key_value.clone(),
            port: self.port,
        };
        match client.send_request(req).await {
            Ok(IpcResponse::McpMembraneLease { mcp_granted: true, .. }) => {
                Ok(LeaseRenewResult::Ok { epoch: 0 })
            }
            Ok(IpcResponse::McpMembraneLease { mcp_granted: false, .. }) => {
                Ok(LeaseRenewResult::NeedsReacquire)
            }
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
            by_agent.entry(record.agent_id.clone()).or_default().push(record);
        }
        for (agent_id, routes) in by_agent {
            t.upsert_agent_routes(&agent_id, routes);
        }
        info!(path = %path.display(), "loaded static routes");
    }

    // Create the inbound channel shared between HTTP handlers and the IPC runtime.
    let (inbound_tx, inbound_rx) = mpsc::channel(128);
    let pending_responses = Arc::new(Mutex::new(HashMap::new()));

    let state = Arc::new(MembraneState {
        routing_table: table,
        vault_cache: VaultHashCache::new(),
        allotment: AllotmentTracker::new(),
        vault: Box::new(IpcVaultResolver),
        inbound_tx,
        pending_responses,
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
        let router = build_router(guest.state.clone())
            .into_make_service_with_connect_info::<SocketAddr>();
        let listener = tokio::net::TcpListener::bind(addr).await?;
        info!(%addr, "MCP membrane listening");
        axum::serve(listener, router).await?;
        Ok(())
    }
}
