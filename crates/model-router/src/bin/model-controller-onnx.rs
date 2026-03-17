use anyhow::Result;
use model_router::providers::OnnxProvider;
use model_router::providers::onnx::OnnxProviderConfig;
use model_router::runtime::{ControllerGuestConfig, run_model_controller};
use model_router::sidecar::run_sidecar;
use onnx_runner::EmbeddingsConfig;
use std::sync::Arc;

const DEFAULT_SIDECAR_ADDR: &str = "127.0.0.1:11435";
const DEFAULT_EMBED_REPO: &str = "onnx-community/embeddinggemma-300m-ONNX";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let sidecar_addr =
        std::env::var("PHILOTIC_ONNX_SIDECAR_ADDR").unwrap_or_else(|_| DEFAULT_SIDECAR_ADDR.into());
    let embed_repo =
        std::env::var("PHILOTIC_ONNX_EMBED_REPO").unwrap_or_else(|_| DEFAULT_EMBED_REPO.into());
    let prefer_quantized = std::env::var("PHILOTIC_ONNX_PREFER_QUANTIZED")
        .ok()
        .as_deref()
        != Some("0");

    let config = OnnxProviderConfig {
        embeddings: EmbeddingsConfig {
            repo_id: embed_repo,
            prefer_quantized,
            max_seq_len: 512,
        },
        prefer_quantized,
    };

    // Load the provider and extract the shared backend handle before moving
    // the provider into the factory closure.
    let provider = Arc::new(OnnxProvider::load(config)?);
    let shared_embeddings = provider.shared_embeddings();

    let provider_for_factory = Arc::clone(&provider);
    let ipc_task = run_model_controller(ControllerGuestConfig {
        guest_id: "model-onnx-01",
        role: "model.local",
        allow_inline_audio: false,
        providers: Box::new(move |_http_client, _configs| {
            vec![Arc::clone(&provider_for_factory) as Arc<dyn model_router::controller::ModelProvider>]
        }),
    });

    let sidecar_task = run_sidecar(&sidecar_addr, shared_embeddings);

    tokio::select! {
        res = ipc_task => {
            tracing::error!("IPC controller exited: {:?}", res);
            res
        }
        res = sidecar_task => {
            tracing::error!("HTTP sidecar exited: {:?}", res);
            res
        }
    }
}
