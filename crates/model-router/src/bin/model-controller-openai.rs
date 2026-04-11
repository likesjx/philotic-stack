use anyhow::Result;
use model_router::providers::OpenAIProvider;
use model_router::runtime::{ControllerGuestConfig, run_model_controller};

#[tokio::main]
async fn main() -> Result<()> {
    run_model_controller(ControllerGuestConfig {
        guest_id: "model-controller-openai-01",
        role: "model",
        allow_inline_audio: false,
        providers: Box::new(|http_client, configs| {
            vec![std::sync::Arc::new(OpenAIProvider::new(
                http_client,
                OpenAIProvider::auth_from_config(
                    configs.openai_oauth_access_token.clone(),
                    configs.openai_api_key.clone(),
                ),
                configs.openai_base_url.clone(),
                configs.openai_project_id.clone(),
                configs.openai_default_model.clone(),
                configs.openai_default_embedding_model.clone(),
            ))]
        }),
        live_providers: Box::new(|_http_client, _configs| Vec::new()),
    })
    .await
}
