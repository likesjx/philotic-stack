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
    /// Serve a read-only MCP endpoint only: the database is opened read-only,
    /// no scan runs, no REST/WS server starts, and only
    /// [`mcp::READ_ONLY_TOOLS`] are listed or callable. Requires a token.
    pub read_only: bool,
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
            read_only: false,
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
    let rejection = match presented {
        Some(p) if token_matches(p, token.as_str()) => None,
        Some(_) => Some("wrong"),
        None => Some("missing"),
    };
    match rejection {
        None => next.run(req).await,
        Some(why) => {
            // One line per rejection so a wave of 401s is visible in the
            // server log. The presented value is never logged.
            eprintln!(
                "auth: 401 {} {}: {} graph token",
                req.method(),
                req.uri().path(),
                why
            );
            (
                StatusCode::UNAUTHORIZED,
                format!(
                    "{} graph token: this server requires `Authorization: Bearer <PHILOTIC_GRAPH_TOKEN>` \
                     (or `x-philotic-graph-token`) on every MCP call and mutating REST call, \
                     loopback included",
                    why
                ),
            )
                .into_response()
        }
    }
}

/// Compare a presented token with the configured one without an early exit
/// on the first differing byte. The length still shows; the value does not.
fn token_matches(presented: &str, expected: &str) -> bool {
    let (a, b) = (presented.as_bytes(), expected.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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

/// What `/api/status` reports about auth, so a client getting 401s can see
/// that a token is required without having to guess. Never includes the token.
fn auth_identity(config: &ServerConfig) -> serde_json::Value {
    serde_json::json!({
        "token_required": config.auth_token.is_some(),
        "token_scope": if config.auth_token.is_some() {
            "every MCP call and every non-GET REST call, loopback included"
        } else {
            "none"
        },
        "read_only": config.read_only,
    })
}

fn server_info_for(config: &ServerConfig) -> serde_json::Value {
    let mut info = server_identity();
    if let Some(obj) = info.as_object_mut() {
        obj.insert("auth".to_string(), auth_identity(config));
    }
    info
}

/// Start both the HTTP/WS server and the MCP server (or, with
/// `config.read_only`, only the read-only MCP server).
pub async fn serve(config: ServerConfig) -> Result<()> {
    if config.read_only {
        return serve_read_only(config).await;
    }

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

    announce_auth(&config);
    let server_info = server_info_for(&config);
    let state = Arc::new(AppState {
        engine: Mutex::new(engine),
        scan_config: config.scan_config,
        repo_root: config.repo_root,
        change_tx,
        server_info,
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

/// Say at startup whether a token is enforced. A token in the launchd or
/// shell environment applies on loopback too, so every local client that does
/// not send it gets 401; this line makes that visible in the log.
fn announce_auth(config: &ServerConfig) {
    if config.auth_token.is_some() && is_loopback_bind(&config.bind_addr) {
        eprintln!(
            "Auth: graph token is set, so every MCP call and mutating REST call needs \
             `Authorization: Bearer <token>`, also on loopback. Clients without it get 401."
        );
    }
}

/// Read-only mode: one MCP endpoint over a read-only database connection.
///
/// No initial scan (that writes), no REST/WS server (it has write routes),
/// and a token is mandatory because this mode exists to be reached by a
/// caller outside the machine through a proxy.
async fn serve_read_only(config: ServerConfig) -> Result<()> {
    if config.auth_token.is_none() {
        anyhow::bail!(
            "refusing to start read-only mode without a token: set PHILOTIC_GRAPH_TOKEN \
             (or --token) to a value only the connector proxy holds"
        );
    }
    if config.db_path == ":memory:" {
        anyhow::bail!("read-only mode needs an existing database file, not :memory:");
    }

    let engine = GraphEngine::open_read_only(&config.db_path)?;
    let (change_tx, _) = broadcast::channel::<ws::ChangeEvent>(16);
    let server_info = server_info_for(&config);
    let state = Arc::new(AppState {
        engine: Mutex::new(engine),
        scan_config: config.scan_config,
        repo_root: config.repo_root,
        change_tx,
        server_info,
    });

    let mcp_addr = format!("{}:{}", config.bind_addr, config.mcp_port);
    let mcp_router = apply_auth(mcp::read_only_router(state), &config.auth_token);
    eprintln!(
        "Read-only MCP server listening on {} ({} tools, database opened read-only)",
        mcp_addr,
        mcp::READ_ONLY_TOOLS.len()
    );
    let listener = tokio::net::TcpListener::bind(&mcp_addr).await?;
    tokio::select! {
        res = axum::serve(listener, mcp_router) => {
            res.map_err(|e| anyhow::anyhow!("MCP server error: {}", e))?;
        }
        _ = tokio::signal::ctrl_c() => {
            eprintln!("Shutting down...");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{is_loopback_bind, token_matches};

    #[test]
    fn token_compare_needs_exact_match() {
        assert!(token_matches("s3cret", "s3cret"));
        assert!(!token_matches("s3cres", "s3cret"));
        assert!(!token_matches("s3cre", "s3cret"));
        assert!(!token_matches("", "s3cret"));
    }

    #[tokio::test]
    async fn read_only_mode_refuses_to_start_without_token() {
        let config = super::ServerConfig {
            read_only: true,
            db_path: "/nonexistent/graph.db".to_string(),
            ..Default::default()
        };
        let err = super::serve(config).await.unwrap_err().to_string();
        assert!(err.contains("without a token"), "{err}");
    }

    #[tokio::test]
    async fn read_only_mode_refuses_missing_database() {
        let config = super::ServerConfig {
            read_only: true,
            db_path: "/nonexistent/dir/graph.db".to_string(),
            auth_token: Some("t".to_string()),
            ..Default::default()
        };
        assert!(super::serve(config).await.is_err());
    }

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
