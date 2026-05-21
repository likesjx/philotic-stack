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

fn base_dir() -> PathBuf {
    // PHILOTIC_TABLE_DB: if it ends in .db treat it as a single-file legacy path;
    // otherwise treat it as the base directory directly.
    if let Ok(p) = std::env::var("PHILOTIC_TABLE_DB") {
        let path = PathBuf::from(&p);
        if p.ends_with(".db") {
            return path.parent().unwrap_or(&path).to_path_buf();
        }
        return path;
    }
    let profile = std::env::var("PHILOTIC_PROFILE").unwrap_or_else(|_| "default".to_string());
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".philotic").join(profile)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let id: &'static str = Box::leak(guest_id().into_boxed_str());
    let dir = base_dir();

    info!(guest_id = id, base_dir = %dir.display(), "table-datasource starting");

    let provider = Arc::new(
        SqliteTableProvider::new(&dir)
            .with_context(|| format!("failed to init table DB at {}", dir.display()))?,
    );

    run_datasource_controller(DatasourceGuestConfig {
        guest_id: id,
        role: "table-datasource",
        providers: Box::new(move || vec![provider.clone()]),
    })
    .await
    .context("table-datasource controller exited with error")
}
