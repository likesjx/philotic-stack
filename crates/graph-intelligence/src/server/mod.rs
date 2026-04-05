pub mod api;
pub mod mcp;
pub mod ws;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{broadcast, Mutex};

use crate::engine::GraphEngine;
use crate::scanner::{full_scan, ScanConfig};

/// Configuration for the graph-intelligence servers.
pub struct ServerConfig {
    /// REST API + WebSocket port (default: 8900)
    pub http_port: u16,
    /// MCP tool server port (default: 8901)
    pub mcp_port: u16,
    /// Path to the SQLite database file (or ":memory:")
    pub db_path: String,
    /// Scanner configuration
    pub scan_config: ScanConfig,
    /// Path to the repository root
    pub repo_root: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            http_port: 8900,
            mcp_port: 8901,
            db_path: "graph.db".to_string(),
            scan_config: ScanConfig {
                rust_roots: vec!["crates".to_string(), "src".to_string()],
                doc_roots: vec!["docs".to_string()],
                git_repo: ".".to_string(),
                worktree: "develop".to_string(),
            },
            repo_root: ".".to_string(),
        }
    }
}

/// Shared application state passed to all handlers.
pub struct AppState {
    pub engine: Mutex<GraphEngine>,
    pub scan_config: ScanConfig,
    pub repo_root: String,
    pub change_tx: broadcast::Sender<ws::ChangeEvent>,
}

/// Start both the HTTP/WS server and the MCP server.
pub async fn serve(config: ServerConfig) -> Result<()> {
    // Initialize the graph engine
    let mut engine = GraphEngine::open(&config.db_path)?;

    // Run initial scan
    let root = Path::new(&config.repo_root);
    let result = full_scan(root, &config.scan_config, &mut engine)?;
    eprintln!(
        "Initial scan complete: {} crates, {} modules, {} types, {} functions in {}ms",
        result.crates, result.modules, result.types, result.functions, result.duration_ms
    );

    // Create broadcast channel for WebSocket events
    let (change_tx, _) = broadcast::channel::<ws::ChangeEvent>(256);

    let state = Arc::new(AppState {
        engine: Mutex::new(engine),
        scan_config: config.scan_config,
        repo_root: config.repo_root,
        change_tx,
    });

    // Build the HTTP + WebSocket router
    let http_router = api::router(state.clone()).merge(ws::router(state.clone()));

    let http_addr = format!("0.0.0.0:{}", config.http_port);
    let mcp_addr = format!("0.0.0.0:{}", config.mcp_port);

    eprintln!("HTTP/WS server listening on {}", http_addr);
    eprintln!("Web UI available at http://localhost:{}", config.http_port);
    eprintln!("MCP server listening on {}", mcp_addr);

    // Build the MCP router
    let mcp_router = mcp::router(state.clone());

    let http_listener = tokio::net::TcpListener::bind(&http_addr).await?;
    let mcp_listener = tokio::net::TcpListener::bind(&mcp_addr).await?;

    // Run both servers concurrently
    tokio::select! {
        res = axum::serve(http_listener, http_router) => {
            res.map_err(|e| anyhow::anyhow!("HTTP server error: {}", e))?;
        }
        res = axum::serve(mcp_listener, mcp_router) => {
            res.map_err(|e| anyhow::anyhow!("MCP server error: {}", e))?;
        }
        _ = tokio::signal::ctrl_c() => {
            eprintln!("Shutting down...");
        }
    }

    Ok(())
}
