use crate::muninn_bridge::MuninnBridgeConfig;
use graph_intelligence::scanner::ScanConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub fn resolve_graph_db_path() -> String {
    std::env::var("PHILOTIC_GRAPH_DB")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("PHILOTIC_GRAPH_DB_PATH")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| {
            default_graph_db_path()
                .to_string_lossy()
                .to_string()
        })
}

fn default_graph_db_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local")
        .join("share")
        .join("philotic")
        .join("graph.db")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhiloticGraphConfig {
    /// Path to the graph database file
    pub db_path: String,

    /// HTTP port for REST API + WebSocket
    pub http_port: u16,

    /// MCP port for agent tool access
    pub mcp_port: u16,

    /// Rust source directories to scan (relative to repo root)
    pub rust_roots: Vec<String>,

    /// Documentation directories to scan
    pub doc_roots: Vec<String>,

    /// Muninn bridge configuration
    pub muninn: MuninnBridgeConfig,

    /// Auto-scan on file changes
    pub watch: bool,
}

impl Default for PhiloticGraphConfig {
    fn default() -> Self {
        Self {
            db_path: resolve_graph_db_path(),
            http_port: 8900,
            mcp_port: 8901,
            rust_roots: vec!["crates/".into()],
            doc_roots: vec!["docs/".into()],
            muninn: MuninnBridgeConfig::default(),
            watch: false,
        }
    }
}

impl PhiloticGraphConfig {
    pub fn to_scan_config(&self) -> ScanConfig {
        ScanConfig {
            rust_roots: self.rust_roots.clone(),
            doc_roots: self.doc_roots.clone(),
            git_repo: ".".into(),
            worktree: "develop".into(),
        }
    }

    pub fn to_server_config(&self, repo_root: &str) -> graph_intelligence::server::ServerConfig {
        graph_intelligence::server::ServerConfig {
            http_port: self.http_port,
            mcp_port: self.mcp_port,
            db_path: self.db_path.clone(),
            scan_config: self.to_scan_config(),
            repo_root: repo_root.to_string(),
        }
    }
}
