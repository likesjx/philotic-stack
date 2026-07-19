use anyhow::Result;
use model_router::providers::OpenAIProvider;
use model_router::runtime::{
    ControllerGuestConfig, DEFAULT_OPENROUTER_MODEL, run_model_controller,
};

#[tokio::main]
async fn main() -> Result<()> {
    run_model_controller(ControllerGuestConfig {
        guest_id: "model-controller-openrouter-01",
        role: "model.openrouter",
        allow_inline_audio: false,
        providers: Box::new(|http_client, configs| {
            vec![std::sync::Arc::new(OpenAIProvider::new_compatible(
                "openrouter",
                http_client,
                OpenAIProvider::auth_from_config(None, configs.openrouter_api_key.clone()),
                configs
                    .openrouter_base_url
                    .clone()
                    .or_else(|| Some("https://openrouter.ai/api".to_string())),
                None,
                configs
                    .openrouter_default_model
                    .clone()
                    .or_else(|| Some(DEFAULT_OPENROUTER_MODEL.to_string())),
                configs.openrouter_default_embedding_model.clone(),
                configs.openrouter_fallback_models.clone(),
                configs.openrouter_route.clone().or_else(|| {
                    if configs.openrouter_fallback_models.is_empty() {
                        None
                    } else {
                        Some("fallback".to_string())
                    }
                }),
            ))]
        }),
        live_providers: Box::new(|_http_client, _configs| Vec::new()),
    })
    .await
}
