use anyhow::Result;
use model_router::providers::GeminiProvider;
use model_router::runtime::{ControllerGuestConfig, run_model_controller};

#[tokio::main]
async fn main() -> Result<()> {
    run_model_controller(ControllerGuestConfig {
        guest_id: "model-controller-gemini-01",
        role: "model",
        allow_inline_audio: false,
        providers: Box::new(|http_client, configs| {
            vec![std::sync::Arc::new(GeminiProvider::new(
                http_client,
                GeminiProvider::auth_from_config(
                    configs.gemini_oauth_access_token.clone(),
                    configs.gemini_oauth_project_id.clone(),
                    configs.gemini_api_key.clone(),
                ),
                configs.gemini_base_url.clone(),
            ))]
        }),
        live_providers: Box::new(|http_client, configs| {
            vec![std::sync::Arc::new(GeminiProvider::new(
                http_client,
                GeminiProvider::auth_from_config(
                    configs.gemini_oauth_access_token.clone(),
                    configs.gemini_oauth_project_id.clone(),
                    configs.gemini_api_key.clone(),
                ),
                configs.gemini_base_url.clone(),
            ))]
        }),
    })
    .await
}
