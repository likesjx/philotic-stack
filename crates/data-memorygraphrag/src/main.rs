mod provider;

use anyhow::Result;
use datasource::runtime::{DatasourceGuestConfig, run_datasource_controller};
use provider::LifeGraphProvider;
use std::sync::Arc;
use tracing::info;

fn guest_id() -> String {
    std::env::var("PHILOTIC_LIFE_GRAPH_RUNNER_ID")
        .or_else(|_| std::env::var("PHILOTIC_GRAPH_RUNNER_ID"))
        .unwrap_or_else(|_| "life-graph-runner".to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    let guest_id_static: &'static str = Box::leak(guest_id().into_boxed_str());

    info!(
        guest_id = guest_id_static,
        memgraph_uri = %std::env::var("PHILOTIC_MEMGRAPH_URI").unwrap_or_else(|_| "127.0.0.1:7687".to_string()),
        "life-graph-runner starting"
    );

    run_datasource_controller(DatasourceGuestConfig {
        guest_id: guest_id_static,
        role: "life-graph-runner",
        providers: Box::new(|| vec![Arc::new(LifeGraphProvider::from_env())]),
    })
    .await
}
