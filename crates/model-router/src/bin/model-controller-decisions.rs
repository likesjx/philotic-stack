//! Controller guest for typed judgments (`decisions.evaluate`, role
//! `model.decisions`). Serves TypeSafe's Jev over its native API when a native
//! key is present in the environment (early access, ephemeral/CI use), and
//! otherwise over OpenRouter's alpha decisions endpoint using the hotel's
//! existing OpenRouter key. Not in any fallback tier: a decision is never a
//! turn reply.

use anyhow::Result;
use model_router::providers::DecisionsProvider;
use model_router::runtime::{ControllerGuestConfig, run_model_controller};
use std::sync::Arc;

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

#[tokio::main]
async fn main() -> Result<()> {
    run_model_controller(ControllerGuestConfig {
        guest_id: "model-controller-decisions-01",
        role: "model.decisions",
        allow_inline_audio: false,
        providers: Box::new(|http_client, configs| {
            let provider = match env_nonempty("PHILOTIC_TYPESAFE_API_KEY") {
                Some(native_key) => DecisionsProvider::native(
                    http_client,
                    Some(native_key),
                    env_nonempty("PHILOTIC_TYPESAFE_BASE_URL"),
                ),
                None => DecisionsProvider::openrouter(
                    http_client,
                    configs.openrouter_api_key.clone(),
                    configs.openrouter_base_url.clone(),
                    // Unset means the pinned `typesafe/jev-1.13`, never the
                    // moving `~typesafe/jev-latest` alias.
                    env_nonempty("PHILOTIC_DECISIONS_MODEL"),
                ),
            };
            vec![Arc::new(provider)]
        }),
        live_providers: Box::new(|_http_client, _configs| Vec::new()),
    })
    .await
}
