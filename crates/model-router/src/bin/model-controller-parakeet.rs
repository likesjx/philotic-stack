use anyhow::Result;
use clap::Parser;
use model_router::controller::ModelProvider;
use model_router::providers::ParakeetProvider;
use model_router::runtime::{ControllerGuestConfig, run_model_controller};
use parakeet_runner::{CAPABILITY, DEFAULT_MODEL, ParakeetConfig};
use std::sync::Arc;

const DEFAULT_GUEST_ID: &str = "model-parakeet-01";
const DEFAULT_ROLE: &str = "model.parakeet";
const DEFAULT_PYTHON: &str = "python3";
const DEFAULT_SOCKET_PATH: &str = "/tmp/philotic-aiua.sock";

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Parakeet ASR controller (NeMo parakeet-tdt-0.6b-v2 via Python subprocess)"
)]
struct Args {
    /// Hotel IPC socket path (used to fetch component config when not set explicitly).
    #[arg(long, env = "PHILOTIC_SOCKET", default_value = DEFAULT_SOCKET_PATH)]
    socket: String,

    /// Python interpreter path — fallback if hotel config has no python_path.
    #[arg(long, env = "PHILOTIC_PARAKEET_PYTHON", default_value = DEFAULT_PYTHON)]
    python: String,

    /// NeMo model name — fallback if hotel config has no model_name.
    #[arg(long, env = "PHILOTIC_PARAKEET_MODEL", default_value = DEFAULT_MODEL)]
    model: String,

    /// Provider priority fallback.
    #[arg(long, env = "PHILOTIC_PARAKEET_PRIORITY", default_value_t = 10)]
    priority: u32,

    /// Guest ID registered with the hotel.
    #[arg(long, env = "PHILOTIC_PARAKEET_GUEST_ID", default_value = DEFAULT_GUEST_ID)]
    guest_id: String,

    /// Role announced to the hotel.
    #[arg(long, env = "PHILOTIC_PARAKEET_ROLE", default_value = DEFAULT_ROLE)]
    role: String,

    /// Smoke-test mode: verify Python + NeMo import, then exit.
    #[arg(long)]
    check: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    if args.check {
        return check_nemo(&args.python);
    }

    // Config resolution priority:
    //   1. Hotel IPC readback via GetConfig { key: "component:{guest_id}" }
    //   2. CLI args / env vars as fallback
    let (python_path, model_name, priority) =
        match ParakeetConfig::from_hotel(&args.socket, &args.guest_id).await {
            Ok(Some(cfg)) => {
                tracing::info!(
                    guest_id = %args.guest_id,
                    socket = %args.socket,
                    model = %cfg.model_name,
                    "parakeet config loaded from hotel"
                );
                (cfg.python_path, cfg.model_name, cfg.priority)
            }
            Ok(None) => {
                tracing::info!(
                    guest_id = %args.guest_id,
                    "no hotel component config found; using CLI args"
                );
                (args.python, args.model, args.priority)
            }
            Err(e) => {
                tracing::warn!(
                    guest_id = %args.guest_id,
                    "hotel config readback failed ({}); using CLI args",
                    e
                );
                (args.python, args.model, args.priority)
            }
        };

    tracing::info!(
        model = %model_name,
        python = %python_path,
        guest_id = %args.guest_id,
        role = %args.role,
        capability = CAPABILITY,
        "model-controller-parakeet starting"
    );

    let provider = Arc::new(ParakeetProvider::new(&model_name, &python_path, priority));

    let guest_id: &'static str = Box::leak(args.guest_id.into_boxed_str());
    let role: &'static str = Box::leak(args.role.into_boxed_str());

    run_model_controller(ControllerGuestConfig {
        guest_id,
        role,
        allow_inline_audio: false,
        providers: Box::new(move |_http_client, _configs| {
            vec![Arc::clone(&provider) as Arc<dyn ModelProvider>]
        }),
        live_providers: Box::new(|_http_client, _configs| Vec::new()),
    })
    .await
}

fn check_nemo(python_path: &str) -> Result<()> {
    let output = std::process::Command::new(python_path)
        .args(["-c", "import nemo.collections.asr as nemo_asr; print('nemo_asr ok')"])
        .output()?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        tracing::info!("nemo check passed: {}", stdout.trim());
        println!("nemo_asr import: OK");
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("nemo_asr import failed:\n{stderr}");
    }
}
