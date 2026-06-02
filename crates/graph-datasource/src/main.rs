use anyhow::{Context, Result};
use datasource::{
    controller::DatasourceProvider,
    runtime::{DatasourceGuestConfig, run_datasource_controller},
};
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

fn provider_mode() -> String {
    std::env::var("PHILOTIC_GRAPH_PROVIDER").unwrap_or_else(|_| "sqlite".to_string())
}

fn build_providers(mode: &str, db_base_path: PathBuf) -> Result<Vec<Arc<dyn DatasourceProvider>>> {
    match mode {
        "sqlite" | "sqlite-cypher" => Ok(vec![Arc::new(SqliteCypherProvider::new(db_base_path))]),
        "memgraph" | "memgraph-cypher" => build_memgraph_provider(),
        other => anyhow::bail!("unsupported PHILOTIC_GRAPH_PROVIDER mode: {other}"),
    }
}

#[cfg(feature = "memgraph-provider")]
fn build_memgraph_provider() -> Result<Vec<Arc<dyn DatasourceProvider>>> {
    Ok(vec![Arc::new(
        graph_datasource::MemgraphCypherProvider::from_env(),
    )])
}

#[cfg(not(feature = "memgraph-provider"))]
fn build_memgraph_provider() -> Result<Vec<Arc<dyn DatasourceProvider>>> {
    anyhow::bail!(
        "PHILOTIC_GRAPH_PROVIDER=memgraph requires graph-datasource built with feature `memgraph-provider`"
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let guest_id_static: &'static str = Box::leak(guest_id().into_boxed_str());
    let db_base_path = base_dir();
    let graph_provider = provider_mode();

    info!(
        guest_id = guest_id_static,
        db_dir = ?db_base_path,
        provider = graph_provider,
        "graph-datasource starting"
    );

    run_datasource_controller(DatasourceGuestConfig {
        guest_id: guest_id_static,
        role: "graph-datasource",
        providers: Box::new(move || {
            build_providers(&graph_provider, db_base_path.clone())
                .expect("failed to initialize graph datasource providers")
        }),
    })
    .await
    .context("failed to run datasource controller")
}

#[cfg(test)]
mod tests {
    use super::{base_dir, build_providers, provider_mode};
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

    #[test]
    fn provider_mode_defaults_to_sqlite() {
        unsafe {
            std::env::remove_var("PHILOTIC_GRAPH_PROVIDER");
        }

        assert_eq!(provider_mode(), "sqlite");
    }

    #[test]
    fn sqlite_provider_mode_builds_provider() {
        let providers =
            build_providers("sqlite", PathBuf::from("/tmp/philotic-graph-test")).unwrap();
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].id(), "sqlite-cypher");
    }
}
