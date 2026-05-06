use anyhow::Result;
use clap::Parser;
use model_router::controller::ModelProvider;
use model_router::providers::OllamaProvider;
use model_router::runtime::{ControllerGuestConfig, run_model_controller};
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_GUEST_ID: &str = "model-ollama-01";
const DEFAULT_ROLE: &str = "model.ollama";
const DEFAULT_SOCKET_PATH: &str = "/tmp/philotic-aiua.sock";

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Ollama model controller — text generation, embeddings, and routing oracle"
)]
struct Args {
    /// Ollama base URL.
    #[arg(long, env = "PHILOTIC_OLLAMA_BASE_URL")]
    base_url: Option<String>,

    /// Model for TextGenerate tasks (default: gemma4:e4b).
    #[arg(long, env = "PHILOTIC_OLLAMA_CHAT_MODEL")]
    chat_model: Option<String>,

    /// Model for Embed tasks (default: embeddinggemma).
    #[arg(long, env = "PHILOTIC_OLLAMA_EMBED_MODEL")]
    embed_model: Option<String>,

    /// Model for RouteClassify tasks (default: functiongemma).
    #[arg(long, env = "PHILOTIC_OLLAMA_ORACLE_MODEL")]
    oracle_model: Option<String>,

    /// Guest ID registered with the hotel.
    #[arg(long, env = "PHILOTIC_OLLAMA_GUEST_ID", default_value = DEFAULT_GUEST_ID)]
    guest_id: String,

    /// Role announced to the hotel.
    #[arg(long, env = "PHILOTIC_OLLAMA_ROLE", default_value = DEFAULT_ROLE)]
    role: String,

    /// Path to the hotel IPC socket.
    #[arg(long, env = "PHILOTIC_SOCKET", default_value = DEFAULT_SOCKET_PATH)]
    socket: String,

    /// Check that Ollama is reachable, then exit.
    #[arg(long)]
    check: bool,
}

/// Poll Ollama's /api/version until it responds or we give up (60s, 3s intervals).
/// Returns Ok if Ollama is up, Err if it never responded in time.
async fn wait_for_ollama(client: &reqwest::Client, base_url: &str) -> Result<()> {
    let url = format!("{}/api/version", base_url);
    let max_attempts = 20u32;
    let interval = Duration::from_secs(3);

    for attempt in 1..=max_attempts {
        match client.get(&url).send().await {
            Ok(r) if r.status().is_success() => {
                let body: serde_json::Value = r.json().await.unwrap_or_default();
                tracing::info!(
                    version = body.get("version").and_then(|v| v.as_str()).unwrap_or("unknown"),
                    "Ollama is ready"
                );
                return Ok(());
            }
            Ok(r) => {
                tracing::warn!(attempt, status = %r.status(), "Ollama not ready yet");
            }
            Err(e) => {
                tracing::warn!(attempt, error = %e, "Ollama unreachable, retrying in 3s");
            }
        }
        tokio::time::sleep(interval).await;
    }

    anyhow::bail!("Ollama did not become ready at {} after {}s", base_url, max_attempts * 3)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    let http_client = reqwest::Client::new();

    if args.check {
        let base_url = args.base_url.as_deref().unwrap_or("http://localhost:11434");
        let resp = http_client
            .get(format!("{}/api/version", base_url))
            .send()
            .await;
        match resp {
            Ok(r) if r.status().is_success() => {
                let body: serde_json::Value = r.json().await.unwrap_or_default();
                println!("Ollama OK — version: {}", body.get("version").and_then(|v| v.as_str()).unwrap_or("unknown"));
                return Ok(());
            }
            Ok(r) => anyhow::bail!("Ollama check failed: HTTP {}", r.status()),
            Err(e) => anyhow::bail!("Ollama unreachable: {}", e),
        }
    }

    // Wait for Ollama to be reachable before registering with the hotel.
    // The hotel will respawn this guest on exit, so retrying here avoids log spam.
    let base_url = args.base_url.as_deref().unwrap_or("http://localhost:11434");
    wait_for_ollama(&http_client, base_url).await?;

    let chat_model = args.chat_model.as_deref().unwrap_or("gemma4:e4b").to_string();
    let provider = Arc::new(OllamaProvider::new(
        http_client,
        args.base_url,
        args.chat_model,
    ));

    tracing::info!(
        guest_id = %args.guest_id,
        role = %args.role,
        chat_model = %chat_model,
        "model-controller-ollama starting"
    );

    let guest_id: &'static str = Box::leak(args.guest_id.into_boxed_str());
    let role: &'static str = Box::leak(args.role.into_boxed_str());

    run_model_controller(ControllerGuestConfig {
        guest_id,
        role,
        allow_inline_audio: false,
        providers: Box::new(move |_client, _configs| vec![provider.clone() as Arc<dyn ModelProvider>]),
        live_providers: Box::new(|_client, _configs| vec![]),
    })
    .await
}
