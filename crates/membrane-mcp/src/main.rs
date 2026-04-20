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
use dispatch::StubDispatcher;
use membrane::{LeaseRenewResult, MembraneGuest, OutboundReply};
use philotic_client::{IpcRequest, IpcResponse, PhiloticClient};
use routing::new_shared_table;
use server::{MembraneState, build_router};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{error, info, warn};

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
        // For MCP, delivery completes a pending HTTP response via the
        // pending_responses map in MembraneState. Slice 2 wires this.
        warn!(
            turn_id = reply.turn_id(),
            "MCP deliver stub — Slice 2 wires pending response completion"
        );
        Ok(())
    }

    async fn renew(&mut self, client: &mut PhiloticClient) -> Result<LeaseRenewResult> {
        // Slice 1: no lease epoch tracking yet — just re-acquire.
        // Slice 2 stores the epoch from setup and passes it here.
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

    // Seed from static config file (Slice 1 testing path).
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

    let state = Arc::new(MembraneState {
        routing_table: table,
        vault_cache: VaultHashCache::new(),
        allotment: AllotmentTracker::new(),
        dispatcher: Box::new(StubDispatcher),
        vault: Box::new(IpcVaultResolver),
    });

    let guest = McpMembrane::new(args.port, &args.node_id, state);

    if let Some(socket) = &args.ipc_socket {
        membrane::MembraneRuntime::new(socket, &args.guest_id, &args.node_id)
            .run(guest)
            .await
    } else {
        // No IPC socket — run HTTP server directly (static-only mode).
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
