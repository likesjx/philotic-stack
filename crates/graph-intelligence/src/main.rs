use clap::Parser;
use std::path::PathBuf;

use graph_intelligence::scanner::ScanConfig;
use graph_intelligence::server::{serve, ServerConfig};

#[derive(Parser)]
#[command(name = "graph-intelligence")]
#[command(about = "Graph intelligence server for the Philotic Stack")]
struct Args {
    /// HTTP/WebSocket server port
    #[arg(short, long, default_value = "8900")]
    port: u16,

    /// MCP server port
    #[arg(short, long, default_value = "8901")]
    mcp_port: u16,

    /// Path to SQLite database
    #[arg(short, long, default_value = "graph.db")]
    db: String,

    /// Path to repository root
    #[arg(short, long, default_value = ".")]
    worktree: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let config = ServerConfig {
        http_port: args.port,
        mcp_port: args.mcp_port,
        db_path: args.db,
        scan_config: ScanConfig {
            rust_roots: vec!["crates".to_string(), "src".to_string()],
            doc_roots: vec!["docs".to_string()],
            git_repo: ".".to_string(),
            worktree: "develop".to_string(),
        },
        repo_root: args.worktree.to_string_lossy().to_string(),
    };

    serve(config).await
}
