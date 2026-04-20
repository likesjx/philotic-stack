mod auth;
mod dispatch;
mod protocol;
mod routing;
mod server;

use anyhow::{Context, Result};
use ansible_mesh_core::mcp_route::McpRouteRecord;
use auth::{AllotmentTracker, VaultHashCache, VaultResolver};
use clap::Parser;
use dispatch::StubDispatcher;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use routing::new_shared_table;
use server::{MembraneState, build_router};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{error, info};

// ── CLI ───────────────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "membrane-mcp", about = "Philotic MCP membrane guest")]
struct Args {
    /// Port to listen on for MCP HTTP requests.
    #[arg(short, long, env = "MCP_PORT", default_value_t = 9100)]
    port: u16,

    /// Path to a JSON file containing static McpRouteRecord array (for Slice 1 testing).
    #[arg(long, env = "MCP_STATIC_ROUTES")]
    static_routes: Option<std::path::PathBuf>,

    /// IPC socket path for hotel communication.
    #[arg(long, env = "PHILOTIC_IPC_SOCKET")]
    ipc_socket: Option<String>,
}

// ── Vault stub (Slice 1) ──────────────────────────────────────────────────────

/// Slice 1 vault: resolves via IPC `GetSecret` synchronously.
/// In Slice 2 this becomes an async-cached implementation.
struct IpcVaultResolver {
    // placeholder — Slice 2 wires in a PhiloticClient reference
}

impl VaultResolver for IpcVaultResolver {
    fn resolve(&self, vault_ref: &str) -> Result<[u8; 32]> {
        // Slice 1 stub: return a zeroed hash so the server starts.
        // Actual vault lookup via IpcRequest::GetSecret lands in Slice 2.
        tracing::warn!(vault_ref, "vault resolver stub — returning zero hash (Slice 1)");
        Ok([0u8; 32])
    }
}

// ── Local guest identity ──────────────────────────────────────────────────────

fn local_node_id() -> String {
    std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string())
}

fn local_guest_id() -> String {
    std::env::var("PHILOTIC_GUEST_ID").unwrap_or_else(|_| "membrane-mcp-01".to_string())
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "membrane_mcp=info".into()),
        )
        .init();

    let args = Args::parse();
    info!(port = args.port, "membrane-mcp starting");

    // Build shared routing table.
    let table = new_shared_table();

    // Seed from static config file if provided (Slice 1 testing path).
    if let Some(path) = &args.static_routes {
        let raw =
            std::fs::read_to_string(path).context("failed to read static routes config")?;
        let records: Vec<McpRouteRecord> =
            serde_json::from_str(&raw).context("failed to parse static routes config")?;

        let mut t = table.write().await;
        // Group by agent_id and upsert.
        let mut by_agent: std::collections::HashMap<String, Vec<McpRouteRecord>> =
            std::collections::HashMap::new();
        for record in records {
            by_agent.entry(record.agent_id.clone()).or_default().push(record);
        }
        for (agent_id, routes) in by_agent {
            t.upsert_agent_routes(&agent_id, routes);
        }
        drop(t);
        info!(path = %path.display(), "loaded static routes");
    }

    // Register with hotel if IPC socket is configured.
    if let Some(socket_path) = &args.ipc_socket {
        let identity = GuestIdentity {
            guest_id: local_guest_id(),
            role: "mcp-membrane".into(),
            supported_tools: vec![],
        };
        match PhiloticClient::connect_at(socket_path, identity).await {
            Ok(mut client) => {
                // Acquire MCP membrane lease.
                let req = IpcRequest::AcquireMcpMembraneLease {
                    lease_key: format!("mcp-membrane:{}", local_node_id()),
                    port: args.port,
                };
                match client.send_request(req).await {
                    Ok(IpcResponse::McpMembraneLease { mcp_granted: true, .. }) => {
                        info!("MCP membrane lease acquired");
                    }
                    Ok(IpcResponse::McpMembraneLease { mcp_granted: false, .. }) => {
                        error!("MCP membrane lease denied — another instance may be running");
                    }
                    Ok(other) => {
                        tracing::warn!(?other, "unexpected lease response");
                    }
                    Err(e) => {
                        tracing::warn!(err = %e, "lease request failed — continuing without lease");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(err = %e, "IPC registration failed — running in static-only mode");
            }
        }
    }

    // Build shared state.
    let state = Arc::new(MembraneState {
        routing_table: table,
        vault_cache: VaultHashCache::new(),
        allotment: AllotmentTracker::new(),
        dispatcher: Box::new(StubDispatcher),
        vault: Box::new(IpcVaultResolver {}),
    });

    // Start HTTP server.
    let addr = SocketAddr::from(([0, 0, 0, 0], args.port));
    info!(%addr, "MCP membrane listening");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    let router = build_router(state).into_make_service_with_connect_info::<SocketAddr>();
    axum::serve(listener, router).await?;

    Ok(())
}
