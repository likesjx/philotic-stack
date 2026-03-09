use anyhow::Result;
use model_router::providers::GeminiProvider;
use model_router::runtime::{ControllerGuestConfig, run_model_controller};

#[tokio::main]
async fn main() -> Result<()> {
    run_model_controller(ControllerGuestConfig {
        guest_id: "model-controller-gemini-01",
        role: "model.gemini",
        allow_inline_audio: false,
        providers: Box::new(|http_client, configs| {
            vec![std::sync::Arc::new(GeminiProvider::new(
                http_client,
                configs.gemini_api_key.clone(),
            ))]
        }),
    })
    .await
}
