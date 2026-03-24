use anyhow::Result;
use clap::Parser;
use mlx_runner::MlxFleetConfig;
use model_router::controller::ModelProvider;
use model_router::providers::MlxProvider;
use model_router::runtime::{ControllerGuestConfig, run_model_controller};
use std::sync::Arc;

const DEFAULT_GUEST_ID: &str = "model-mlx-01";
const DEFAULT_ROLE: &str = "model.mlx";

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "MLX local model controller (multi-model fleet via mlx_lm.server)"
)]
struct Args {
    /// Path to the MLX fleet config JSON (overrides PHILOTIC_MLX_CONFIG).
    #[arg(long, env = "PHILOTIC_MLX_CONFIG")]
    config: Option<String>,

    /// Guest ID registered with the hotel.
    #[arg(long, env = "PHILOTIC_MLX_GUEST_ID", default_value = DEFAULT_GUEST_ID)]
    guest_id: String,

    /// Role announced to the hotel.
    #[arg(long, env = "PHILOTIC_MLX_ROLE", default_value = DEFAULT_ROLE)]
    role: String,

    /// Discover mode: start fleet, print loaded models, then exit.
    /// Useful for smoke testing without a running hotel.
    #[arg(long)]
    discover: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    // Load fleet config from CLI arg or PHILOTIC_MLX_CONFIG env var.
    let fleet_config = match args.config {
        Some(ref path) => MlxFleetConfig::from_json_file(path)?,
        None => MlxFleetConfig::from_env()?,
    };

    let health_interval = fleet_config.health_check_interval_secs;

    tracing::info!(
        models = fleet_config.models.len(),
        health_interval_secs = health_interval,
        guest_id = %args.guest_id,
        role = %args.role,
        "model-controller-mlx starting"
    );

    // Check mlx_lm is importable only if the fleet needs Python (managed instances or transcribe).
    if fleet_config.needs_python() {
        let python_path = fleet_config.require_python_path()?;
        mlx_runner::server::MlxServerHandle::check_available(python_path)?;
    }

    // Build the provider fleet (starts managed instances, attaches to running ones).
    let provider = Arc::new(MlxProvider::from_config(fleet_config).await?);

    // --discover: print what's loaded and exit without connecting to hotel.
    if args.discover {
        let summary = provider.fleet_summary().await;
        for entry in &summary {
            println!("{}", entry);
        }
        tracing::info!("discover mode: exiting after fleet startup");
        return Ok(());
    }

    // Spawn background health ticker.
    MlxProvider::spawn_health_ticker(Arc::clone(&provider), health_interval);

    tracing::info!(guest_id = %args.guest_id, role = %args.role, "MLX fleet ready; entering IPC loop");

    let guest_id: &'static str = Box::leak(args.guest_id.into_boxed_str());
    let role: &'static str = Box::leak(args.role.into_boxed_str());

    let provider_for_factory = Arc::clone(&provider);
    run_model_controller(ControllerGuestConfig {
        guest_id,
        role,
        allow_inline_audio: false,
        providers: Box::new(move |_http_client, _configs| {
            vec![Arc::clone(&provider_for_factory) as Arc<dyn ModelProvider>]
        }),
    })
    .await
}
