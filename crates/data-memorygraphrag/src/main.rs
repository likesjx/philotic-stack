mod provider;

use anyhow::Result;
use data_memorygraphrag::hygiene;
use datasource::runtime::{DatasourceGuestConfig, run_datasource_controller};
use provider::LifeGraphProvider;
use std::sync::Arc;
use std::time::Duration;
use tracing::{info, warn};

fn guest_id() -> String {
    std::env::var("PHILOTIC_LIFE_GRAPH_RUNNER_ID")
        .or_else(|_| std::env::var("PHILOTIC_GRAPH_RUNNER_ID"))
        .unwrap_or_else(|_| "life-graph-runner".to_string())
}

/// Initial delay before the first hygiene sweep of a fresh runner process —
/// lets the runner finish registering/settling before it starts issuing
/// bulk Memgraph writes on its own initiative.
const HYGIENE_INITIAL_DELAY: Duration = Duration::from_secs(5 * 60);

/// Spawn the internal nightly hygiene-sweep timer, gated on
/// `PHILOTIC_LIFE_HYGIENE_ENABLED` (default OFF). Non-fatal by design: a
/// sweep error is logged and the loop keeps ticking — it must never crash
/// the runner or affect `life.observe`/`life.recall` availability.
fn spawn_hygiene_sweep_timer() {
    if !hygiene::hygiene_enabled_from_env() {
        info!(
            env = hygiene::HYGIENE_ENABLED_ENV,
            "life-graph hygiene sweep disabled (set to \"1\"/\"true\"/\"yes\" to enable)"
        );
        return;
    }
    let interval = Duration::from_secs(hygiene::interval_hours_from_env().saturating_mul(3600));
    tokio::spawn(async move {
        tokio::time::sleep(HYGIENE_INITIAL_DELAY).await;
        loop {
            let provider = LifeGraphProvider::from_env();
            match provider.hygiene_sweep().await {
                Ok(summary) => info!(
                    retired_stale = summary.retired_stale,
                    collapsed_duplicates = summary.collapsed_duplicates,
                    capped = summary.capped,
                    "life-graph hygiene sweep tick completed"
                ),
                Err(e) => warn!("life-graph hygiene sweep tick failed (non-fatal): {e:#}"),
            }
            tokio::time::sleep(interval).await;
        }
    });
}

#[tokio::main]
async fn main() -> Result<()> {
    let guest_id_static: &'static str = Box::leak(guest_id().into_boxed_str());

    info!(
        guest_id = guest_id_static,
        memgraph_uri = %std::env::var("PHILOTIC_MEMGRAPH_URI").unwrap_or_else(|_| "127.0.0.1:7687".to_string()),
        "life-graph-runner starting"
    );

    spawn_hygiene_sweep_timer();

    run_datasource_controller(DatasourceGuestConfig {
        guest_id: guest_id_static,
        role: "life-graph-runner",
        providers: Box::new(|| vec![Arc::new(LifeGraphProvider::from_env())]),
    })
    .await
}
