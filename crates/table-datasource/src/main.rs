use anyhow::{Context, Result};
use datasource::runtime::{DatasourceGuestConfig, run_datasource_controller};
use std::path::PathBuf;
use std::sync::Arc;
use table_datasource::SqliteTableProvider;
use tracing::info;

fn guest_id() -> String {
    std::env::var("PHILOTIC_TABLE_DATASOURCE_ID")
        .unwrap_or_else(|_| "table-datasource-01".to_string())
}

fn db_path() -> PathBuf {
    if let Ok(p) = std::env::var("PHILOTIC_TABLE_DB") {
        return PathBuf::from(p);
    }
    let profile = std::env::var("PHILOTIC_PROFILE").unwrap_or_else(|_| "default".to_string());
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join(".philotic")
        .join(profile)
        .join("tables.db")
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let id: &'static str = Box::leak(guest_id().into_boxed_str());
    let path = db_path();

    info!(guest_id = id, db = %path.display(), "table-datasource starting");

    let provider = Arc::new(
        SqliteTableProvider::open(&path)
            .with_context(|| format!("failed to open table DB at {}", path.display()))?,
    );

    run_datasource_controller(DatasourceGuestConfig {
        guest_id: id,
        role: "table-datasource",
        providers: Box::new(move || vec![provider.clone()]),
    })
    .await
    .context("table-datasource controller exited with error")
}
