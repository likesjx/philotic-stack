use crate::muninn_bridge::MuninnBridgeConfig;
use graph_intelligence::scanner::ScanConfig;
use serde::{Deserialize, Serialize};

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
            db_path: "philotic-graph.db".into(),
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
