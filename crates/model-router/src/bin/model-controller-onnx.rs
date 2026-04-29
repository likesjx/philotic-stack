use anyhow::Result;
use clap::Parser;
use model_router::controller::ModelProvider;
use model_router::providers::OnnxProvider;
use model_router::providers::onnx::OnnxProviderConfig;
use model_router::runtime::{ControllerGuestConfig, run_model_controller};
use model_router::sidecar::run_sidecar;
use onnx_runner::{EmbeddingsConfig, KokoroConfig, TranscribeConfig};
use std::sync::Arc;

const DEFAULT_SIDECAR_ADDR: &str = "127.0.0.1:11435";
const DEFAULT_EMBED_REPO: &str = "onnx-community/embeddinggemma-300m-ONNX";
const DEFAULT_WHISPER_REPO: &str = "onnx-community/whisper-small";
const DEFAULT_KOKORO_REPO: &str = "onnx-community/Kokoro-82M-v1.0-ONNX";
const DEFAULT_KOKORO_VOICE: &str = "af_heart";
const DEFAULT_GUEST_ID: &str = "model-onnx-01";
const DEFAULT_ROLE: &str = "model.local";

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "ONNX local model controller with Ollama-compat HTTP sidecar"
)]
struct Args {
    /// Skip IPC hotel registration and serve the HTTP sidecar only.
    /// Useful for smoke tests and standalone embedding use without a running hotel.
    #[arg(long, env = "PHILOTIC_ONNX_SIDECAR_ONLY")]
    sidecar_only: bool,

    /// Address for the Ollama-compat HTTP sidecar (overrides PHILOTIC_ONNX_SIDECAR_ADDR).
    #[arg(long, env = "PHILOTIC_ONNX_SIDECAR_ADDR", default_value = DEFAULT_SIDECAR_ADDR)]
    sidecar_addr: String,

    /// HuggingFace repo id for the embedding model (overrides PHILOTIC_ONNX_EMBED_REPO).
    #[arg(long, env = "PHILOTIC_ONNX_EMBED_REPO", default_value = DEFAULT_EMBED_REPO)]
    embed_repo: String,

    /// HuggingFace repo id for the Whisper transcription model (overrides PHILOTIC_ONNX_WHISPER_REPO).
    #[arg(long, env = "PHILOTIC_ONNX_WHISPER_REPO", default_value = DEFAULT_WHISPER_REPO)]
    whisper_repo: String,

    /// HuggingFace repo id for the Kokoro TTS model.
    #[arg(long, env = "PHILOTIC_ONNX_KOKORO_REPO", default_value = DEFAULT_KOKORO_REPO)]
    kokoro_repo: String,

    /// Default Kokoro voice name (e.g. af_heart, bm_george, bf_emma).
    #[arg(long, env = "PHILOTIC_ONNX_KOKORO_VOICE", default_value = DEFAULT_KOKORO_VOICE)]
    kokoro_voice: String,

    /// Prefer the quantized ONNX variant ("true"/"false" or bare --prefer-quantized).
    #[arg(long, env = "PHILOTIC_ONNX_PREFER_QUANTIZED", default_value = "true", num_args = 0..=1, default_missing_value = "true")]
    prefer_quantized: bool,

    /// Guest ID registered with the hotel (overrides PHILOTIC_ONNX_GUEST_ID).
    #[arg(long, env = "PHILOTIC_ONNX_GUEST_ID", default_value = DEFAULT_GUEST_ID)]
    guest_id: String,

    /// Role announced to the hotel (overrides PHILOTIC_ONNX_ROLE).
    #[arg(long, env = "PHILOTIC_ONNX_ROLE", default_value = DEFAULT_ROLE)]
    role: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let config = OnnxProviderConfig {
        embeddings: EmbeddingsConfig {
            repo_id: args.embed_repo.clone(),
            prefer_quantized: args.prefer_quantized,
            max_seq_len: 512,
        },
        transcribe: TranscribeConfig {
            repo_id: args.whisper_repo.clone(),
            prefer_quantized: args.prefer_quantized,
            max_new_tokens: 448,
        },
        kokoro: KokoroConfig {
            repo_id: args.kokoro_repo.clone(),
            default_voice: args.kokoro_voice.clone(),
            prefer_quantized: args.prefer_quantized,
            ..KokoroConfig::default()
        },
        prefer_quantized: args.prefer_quantized,
    };

    tracing::info!(
        embed_repo = %args.embed_repo,
        sidecar_addr = %args.sidecar_addr,
        guest_id = %args.guest_id,
        role = %args.role,
        sidecar_only = args.sidecar_only,
        "model-controller-onnx starting"
    );

    // Load the provider and extract the shared backend handle before moving
    // the provider into the factory closure.
    let provider = Arc::new(OnnxProvider::load(config)?);
    let shared_embeddings = provider.shared_embeddings();
    let shared_whisper = provider.shared_whisper();
    let shared_kokoro = provider.shared_kokoro();

    if args.sidecar_only {
        tracing::info!("sidecar-only mode: skipping hotel IPC registration");
        run_sidecar(&args.sidecar_addr, shared_embeddings, shared_whisper, shared_kokoro).await
    } else {
        // Leak the strings into 'static so the ControllerGuestConfig closure
        // can hold them without lifetime issues.
        let guest_id: &'static str = Box::leak(args.guest_id.into_boxed_str());
        let role: &'static str = Box::leak(args.role.into_boxed_str());

        let provider_for_factory = Arc::clone(&provider);
        let ipc_task = run_model_controller(ControllerGuestConfig {
            guest_id,
            role,
            allow_inline_audio: false,
            providers: Box::new(move |_http_client, _configs| {
                vec![Arc::clone(&provider_for_factory) as Arc<dyn ModelProvider>]
            }),
            live_providers: Box::new(|_http_client, _configs| Vec::new()),
        });

        let sidecar_task = run_sidecar(&args.sidecar_addr, shared_embeddings, shared_whisper, shared_kokoro);

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
}
