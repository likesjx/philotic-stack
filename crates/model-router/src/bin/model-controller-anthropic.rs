use anyhow::Result;
use model_router::providers::AnthropicProvider;
use model_router::runtime::{ControllerGuestConfig, run_model_controller};

#[tokio::main]
async fn main() -> Result<()> {
    run_model_controller(ControllerGuestConfig {
        guest_id: "model-controller-anthropic-01",
        role: "model.anthropic",
        allow_inline_audio: false,
        providers: Box::new(|http_client, configs| {
            vec![std::sync::Arc::new(AnthropicProvider::new(
                http_client,
                configs.anthropic_api_key.clone(),
                configs.anthropic_base_url.clone(),
                configs.anthropic_default_model.clone(),
                std::env::var("PHILOTIC_ANTHROPIC_HEAVY_MODEL").ok(),
                std::env::var("PHILOTIC_ANTHROPIC_FAST_MODEL").ok(),
            ))]
        }),
        live_providers: Box::new(|_http_client, _configs| Vec::new()),
    })
    .await
}
