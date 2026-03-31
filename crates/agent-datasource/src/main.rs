use agent_datasource::AgentDatasourceProvider;
use ansible_mesh_core::agent_graph_storage::SqliteAgentGraphStorage;
use anyhow::{Context, Result};
use clap::Parser;
use datasource::runtime::{run_datasource_controller, DatasourceGuestConfig};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

fn agent_id() -> String {
    std::env::var("PHILOTIC_AGENT_ID").unwrap_or_else(|_| "unknown-agent".into())
}

fn guest_id() -> String {
    std::env::var("PHILOTIC_GRAPH_RUNNER_ID")
        .unwrap_or_else(|_| format!("agent-datasource-{}", agent_id()))
}

fn db_path() -> PathBuf {
    std::env::var("PHILOTIC_AGENT_GRAPH_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home)
                .join(".philotic")
                .join(format!("agent-graph-{}.db", agent_id()))
        })
}

#[derive(Parser, Debug)]
#[command(author, version, about = "Philotic Agent Datasource Guest")]
struct Args {}

#[tokio::main]
async fn main() -> Result<()> {
    // Note: tracing_subscriber::fmt::init() is already called in run_datasource_controller
    let _args = Args::parse();

    let agent_id = agent_id();
    // Using Box::leak to provide a static string slice as required by config
    let guest_id_static: &'static str = Box::leak(guest_id().into_boxed_str());
    let db = db_path();

    info!(
        agent_id = %agent_id,
        guest_id = %guest_id_static,
        db = %db.display(),
        "agent-datasource starting via datasource controller"
    );

    if let Some(parent) = db.parent() {
        std::fs::create_dir_all(parent).context("Failed to create agent graph db directory")?;
    }

    let storage = Arc::new(
        SqliteAgentGraphStorage::open(&agent_id, &db)
            .context("Failed to open agent graph SQLite database")?,
    );

    let config = DatasourceGuestConfig {
        guest_id: guest_id_static,
        role: "agent-graph",
        providers: Box::new(move || {
            vec![std::sync::Arc::new(AgentDatasourceProvider::new(
                agent_id.clone(),
                storage.clone(),
            ))]
        }),
    };

    run_datasource_controller(config).await
}
