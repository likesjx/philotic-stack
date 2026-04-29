use anyhow::Result;
use clap::Parser;
use datasource::runtime::{DatasourceGuestConfig, run_datasource_controller};
use tracing::warn;

fn guest_id() -> String {
    std::env::var("PHILOTIC_GRAPH_DATASOURCE_ID")
        .or_else(|_| std::env::var("PHILOTIC_GRAPH_RUNNER_ID"))
        .unwrap_or_else(|_| "graph-datasource".to_string())
}

#[derive(Parser, Debug)]
#[command(author, version, about = "Philotic graph datasource guest scaffold")]
struct Args {}

#[tokio::main]
async fn main() -> Result<()> {
    let _args = Args::parse();
    let guest_id_static: &'static str = Box::leak(guest_id().into_boxed_str());

    warn!(
        guest_id = guest_id_static,
        "graph-datasource scaffold started without providers; graph-runner migration seam remains open"
    );

    run_datasource_controller(DatasourceGuestConfig {
        guest_id: guest_id_static,
        role: "graph-datasource",
        providers: Box::new(Vec::new),
    })
    .await
}
