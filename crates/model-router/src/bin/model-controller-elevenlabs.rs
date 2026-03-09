use anyhow::Result;
use model_router::providers::ElevenLabsProvider;
use model_router::runtime::{ControllerGuestConfig, run_model_controller};

#[tokio::main]
async fn main() -> Result<()> {
    run_model_controller(ControllerGuestConfig {
        guest_id: "model-controller-elevenlabs-01",
        role: "model.elevenlabs",
        allow_inline_audio: std::env::var("PHILOTIC_MODEL_CONTROLLER_INLINE_AUDIO")
            .ok()
            .as_deref()
            == Some("1"),
        providers: Box::new(|http_client, configs| {
            vec![std::sync::Arc::new(ElevenLabsProvider::new(
                http_client,
                configs.elevenlabs_api_key.clone(),
                configs.elevenlabs_default_voice_id.clone(),
            ))]
        }),
    })
    .await
}
