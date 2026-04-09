use anyhow::Result;
use clap::Parser;
use model_router::providers::{ElevenLabsProvider, GeminiProvider, OllamaProvider};
use model_router::runtime::{ControllerGuestConfig, run_model_controller};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value_t = 9000)]
    ansible_port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    let _args = Args::parse();

    run_model_controller(ControllerGuestConfig {
        guest_id: "model-router-legacy-01",
        role: "model",
        allow_inline_audio: std::env::var("PHILOTIC_MODEL_CONTROLLER_INLINE_AUDIO")
            .ok()
            .as_deref()
            == Some("1"),
        providers: Box::new(|http_client, configs| {
            vec![
                std::sync::Arc::new(GeminiProvider::new(
                    http_client.clone(),
                    GeminiProvider::auth_from_config(
                        configs.gemini_oauth_access_token.clone(),
                        configs.gemini_oauth_project_id.clone(),
                        configs.gemini_api_key.clone(),
                    ),
                    configs.gemini_base_url.clone(),
                )),
                std::sync::Arc::new(ElevenLabsProvider::new(
                    http_client.clone(),
                    configs.elevenlabs_api_key.clone(),
                    configs.elevenlabs_default_voice_id.clone(),
                )),
                std::sync::Arc::new(OllamaProvider::new(
                    http_client,
                    configs.ollama_base_url.clone(),
                    configs.ollama_model.clone(),
                )),
            ]
        }),
    })
    .await
}
