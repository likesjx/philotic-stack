pub mod api;
pub mod mcp;
pub mod ws;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    Router,
};
use tokio::sync::{broadcast, Mutex};

use crate::engine::GraphEngine;
use crate::scanner::{full_scan, ScanConfig};

/// Configuration for the graph-intelligence servers.
pub struct ServerConfig {
    /// Interface to bind (default: 127.0.0.1). Non-loopback binds require an
    /// auth token or an explicit insecure opt-in.
    pub bind_addr: String,
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
    /// Bearer token required on mutating REST calls and all MCP calls when set
    pub auth_token: Option<String>,
    /// Allow a non-loopback bind without an auth token (explicit opt-in)
    pub allow_insecure_bind: bool,
    /// MemPalace-owned episodic adapter. Relative paths resolve from repo_root.
    pub episodic_adapter: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1".to_string(),
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
            auth_token: None,
            allow_insecure_bind: false,
            episodic_adapter: "scripts/mempalace_episode.py".to_string(),
        }
    }
}

fn is_loopback_bind(addr: &str) -> bool {
    matches!(addr, "127.0.0.1" | "::1" | "localhost")
        || addr
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::is_loopback_bind;

    #[test]
    fn loopback_binds_are_recognized() {
        assert!(is_loopback_bind("127.0.0.1"));
        assert!(is_loopback_bind("::1"));
        assert!(is_loopback_bind("localhost"));
        assert!(!is_loopback_bind("0.0.0.0"));
        assert!(!is_loopback_bind("100.64.230.106"));
        assert!(!is_loopback_bind("::"));
    }
}

/// Reject requests lacking the configured bearer token. GET/HEAD/OPTIONS pass
/// unauthenticated so the read-only web UI and health checks keep working; all
/// mutating methods (which includes every MCP JSON-RPC POST) require the token.
async fn require_token(
    axum::extract::State(token): axum::extract::State<Arc<String>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let method = req.method();
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }
    let presented = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or_else(|| {
            req.headers()
                .get("x-philotic-graph-token")
                .and_then(|v| v.to_str().ok())
        });
    if presented == Some(token.as_str()) {
        next.run(req).await
    } else {
        (StatusCode::UNAUTHORIZED, "missing or invalid graph token").into_response()
    }
}

fn apply_auth(router: Router, token: &Option<String>) -> Router {
    match token {
        Some(t) => {
            let shared = Arc::new(t.clone());
            router.layer(middleware::from_fn_with_state(shared, require_token))
        }
        None => router,
    }
}

/// Shared application state passed to all handlers.
pub struct AppState {
    pub engine: Mutex<GraphEngine>,
    pub scan_config: ScanConfig,
    pub repo_root: String,
    pub change_tx: broadcast::Sender<ws::ChangeEvent>,
    /// Identity of the running server: version, start time, binary path+hash.
    /// Surfaced in /api/status so stale-binary drift is observable (the server
    /// once ran 12 days from a deleted binary with no way to tell).
    pub server_info: serde_json::Value,
    /// Compatibility invocation path only; MemPalace remains the data owner.
    pub episodic_adapter: std::path::PathBuf,
}

fn server_identity() -> serde_json::Value {
    let exe = std::env::current_exe().ok();
    let binary_sha256 = exe.as_ref().and_then(|p| {
        std::fs::read(p).ok().map(|bytes| {
            use sha2::{Digest, Sha256};
            format!("{:x}", Sha256::digest(&bytes))
        })
    });
    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "started_at": chrono::Utc::now().to_rfc3339(),
        "binary_path": exe.as_ref().map(|p| p.display().to_string()),
        "binary_sha256": binary_sha256,
        "pid": std::process::id(),
    })
}

/// Start both the HTTP/WS server and the MCP server.
pub async fn serve(config: ServerConfig) -> Result<()> {
    if !is_loopback_bind(&config.bind_addr)
        && config.auth_token.is_none()
        && !config.allow_insecure_bind
    {
        anyhow::bail!(
            "refusing to bind {} without an auth token: the graph exposes unauthenticated \
             write endpoints. Set PHILOTIC_GRAPH_TOKEN (or --token), bind loopback, or pass \
             --insecure-bind / PHILOTIC_GRAPH_INSECURE=1 to override.",
            config.bind_addr
        );
    }

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

    let episodic_adapter = {
        let configured = std::path::PathBuf::from(&config.episodic_adapter);
        if configured.is_absolute() {
            configured
        } else {
            Path::new(&config.repo_root).join(configured)
        }
    };

    let state = Arc::new(AppState {
        engine: Mutex::new(engine),
        scan_config: config.scan_config,
        repo_root: config.repo_root,
        change_tx,
        server_info: server_identity(),
        episodic_adapter,
    });

    // Build the HTTP + WebSocket router
    let http_router = apply_auth(
        api::router(state.clone()).merge(ws::router(state.clone())),
        &config.auth_token,
    );

    let http_addr = format!("{}:{}", config.bind_addr, config.http_port);
    let mcp_addr = format!("{}:{}", config.bind_addr, config.mcp_port);

    eprintln!("HTTP/WS server listening on {}", http_addr);
    eprintln!("Web UI available at http://localhost:{}", config.http_port);
    eprintln!("MCP server listening on {}", mcp_addr);
    if !is_loopback_bind(&config.bind_addr) {
        if config.auth_token.is_some() {
            eprintln!("Non-loopback bind: bearer token required on mutating calls");
        } else {
            eprintln!("WARNING: non-loopback bind WITHOUT auth token (insecure override)");
        }
    }

    // Build the MCP router
    let mcp_router = apply_auth(mcp::router(state.clone()), &config.auth_token);

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
