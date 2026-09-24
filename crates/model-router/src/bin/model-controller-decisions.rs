//! Controller guest for typed judgments (`decisions.evaluate`, role
//! `model.decisions`). Serves TypeSafe's Jev over its native API when a native
//! key is present in the environment (early access, ephemeral/CI use), and
//! otherwise over OpenRouter's alpha decisions endpoint with the hotel's existing
//! `openrouter` vault key, which this role and `heal-dispatcher` are allowed to
//! read (`aiua auth sync-roles --provider openrouter` adds them to an entry sealed
//! earlier). Not in any fallback tier: a decision is never a turn reply.

use anyhow::Result;
use decisions_client::DecisionsClient;
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
            let client = match env_nonempty("PHILOTIC_TYPESAFE_API_KEY") {
                Some(native_key) => DecisionsClient::native(
                    http_client,
                    Some(native_key),
                    env_nonempty("PHILOTIC_TYPESAFE_BASE_URL"),
                ),
                // `configs.decisions` is filled by the decisions handler from the
                // dedicated vault key. An unset model means the pinned
                // `typesafe/jev-1.13`, never the moving `~typesafe/jev-latest`.
                None => DecisionsClient::openrouter(
                    http_client,
                    configs.decisions.api_key.clone(),
                    configs.decisions.base_url.clone(),
                    configs.decisions.model.clone(),
                ),
            };
            vec![Arc::new(DecisionsProvider::new(client))]
        }),
        live_providers: Box::new(|_http_client, _configs| Vec::new()),
    })
    .await
}
