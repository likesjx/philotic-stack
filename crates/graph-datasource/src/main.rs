use anyhow::{Context, Result};
use datasource::runtime::{DatasourceGuestConfig, run_datasource_controller};
use graph_datasource::SqliteCypherProvider;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

fn guest_id() -> String {
    std::env::var("PHILOTIC_GRAPH_DATASOURCE_ID")
        .or_else(|_| std::env::var("PHILOTIC_GRAPH_RUNNER_ID"))
        .unwrap_or_else(|_| "graph-datasource".to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let guest_id_static: &'static str = Box::leak(guest_id().into_boxed_str());

    let db_base_path = std::env::var("PHILOTIC_GRAPH_DATABASE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".philotic/graphs")
        });

    info!(
        guest_id = guest_id_static,
        db_dir = ?db_base_path,
        "graph-datasource starting with SqliteCypherProvider"
    );

    run_datasource_controller(DatasourceGuestConfig {
        guest_id: guest_id_static,
        role: "graph-datasource",
        providers: Box::new(move || {
            let provider = SqliteCypherProvider::new(db_base_path.clone());
            vec![Arc::new(provider)]
        }),
    })
    .await
    .context("failed to run datasource controller")
}
