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

fn base_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("PHILOTIC_GRAPH_DATABASE_DIR") {
        return PathBuf::from(dir);
    }

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let profile = std::env::var("PHILOTIC_PROFILE").unwrap_or_else(|_| "default".to_string());
    PathBuf::from(home)
        .join(".philotic")
        .join(profile)
        .join("graphs")
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let guest_id_static: &'static str = Box::leak(guest_id().into_boxed_str());
    let db_base_path = base_dir();

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

#[cfg(test)]
mod tests {
    use super::base_dir;
    use std::path::PathBuf;

    #[test]
    fn base_dir_uses_explicit_dir_or_profile_scoped_default() {
        unsafe {
            std::env::remove_var("PHILOTIC_GRAPH_DATABASE_DIR");
            std::env::set_var("HOME", "/tmp/philotic-home");
            std::env::set_var("PHILOTIC_PROFILE", "jane");
        }

        assert_eq!(
            base_dir(),
            PathBuf::from("/tmp/philotic-home/.philotic/jane/graphs")
        );

        unsafe {
            std::env::set_var("PHILOTIC_GRAPH_DATABASE_DIR", "/opt/philotic/data/graphs");
            std::env::set_var("HOME", "/tmp/ignored-home");
            std::env::set_var("PHILOTIC_PROFILE", "ignored");
        }

        assert_eq!(base_dir(), PathBuf::from("/opt/philotic/data/graphs"));

        unsafe {
            std::env::remove_var("PHILOTIC_GRAPH_DATABASE_DIR");
            std::env::remove_var("PHILOTIC_PROFILE");
            std::env::remove_var("HOME");
        }
    }
}
