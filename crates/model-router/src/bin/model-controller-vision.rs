use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine as _;
use datasource::controller::{DatasourceProvider, DatasourceTask, ProviderOutput, TaskKind};
use datasource::runtime::{DatasourceGuestConfig, run_datasource_controller};
use onnx_runner::{ModelCache, VisionBackend, VisionConfig};
use serde_json::{Value, json};
use std::sync::Arc;
use tracing::info;

fn guest_id() -> String {
    std::env::var("PHILOTIC_VISION_GUEST_ID")
        .unwrap_or_else(|_| "vision-01".to_string())
}

fn vision_repo_id() -> String {
    std::env::var("PHILOTIC_VISION_REPO_ID")
        .unwrap_or_else(|_| "onnx-community/Florence-2-base-ft".to_string())
}

// ── OnnxVisionProvider ────────────────────────────────────────────────────────

struct OnnxVisionProvider {
    backend: Arc<VisionBackend>,
    http_client: reqwest::Client,
}

impl OnnxVisionProvider {
    fn new(backend: VisionBackend) -> Self {
        Self {
            backend: Arc::new(backend),
            http_client: reqwest::Client::new(),
        }
    }

    async fn fetch_image(&self, params: &Value) -> Result<Vec<u8>> {
        if let Some(b64) = params.get("image_base64").and_then(Value::as_str) {
            return base64::engine::general_purpose::STANDARD
                .decode(b64)
                .context("decode image_base64");
        }

        if let Some(url) = params.get("image_url").and_then(Value::as_str) {
            if let Some(path) = url.strip_prefix("file://") {
                return tokio::fs::read(path)
                    .await
                    .with_context(|| format!("read image file {path}"));
            }

            let bytes = self
                .http_client
                .get(url)
                .send()
                .await
                .with_context(|| format!("fetch image from {url}"))?
                .error_for_status()
                .with_context(|| format!("image fetch returned error for {url}"))?
                .bytes()
                .await
                .context("read image response body")?;

            return Ok(bytes.to_vec());
        }

        anyhow::bail!("vision task requires image_url or image_base64 parameter")
    }
}

#[async_trait]
impl DatasourceProvider for OnnxVisionProvider {
    fn id(&self) -> &str {
        "onnx-vision"
    }

    fn supports(&self, task: &DatasourceTask) -> bool {
        matches!(
            &task.kind,
            TaskKind::Custom(k) if k == "image.ocr" || k == "image.ground"
        )
    }

    async fn invoke(&self, task: &DatasourceTask) -> Result<ProviderOutput> {
        let image_bytes = self.fetch_image(&task.parameters).await?;
        let task_kind = task.kind.as_str().to_string();
        let params = task.parameters.clone();
        let backend = Arc::clone(&self.backend);

        let result: Value = tokio::task::spawn_blocking(move || -> Result<Value> {
            match task_kind.as_str() {
                "image.ocr" => {
                    let out = backend.ocr(&image_bytes).context("Florence-2 OCR")?;
                    Ok(json!({ "text": out.text, "model_gen": out.model_gen }))
                }
                "image.ground" => {
                    let query = params
                        .get("query")
                        .and_then(Value::as_str)
                        .unwrap_or("object");
                    let out = backend.ground(&image_bytes, query).context("Florence-2 grounding")?;
                    let boxes: Vec<Value> = out
                        .results
                        .iter()
                        .map(|b| {
                            json!({
                                "label": b.label,
                                "x1": b.x1, "y1": b.y1,
                                "x2": b.x2, "y2": b.y2
                            })
                        })
                        .collect();
                    Ok(json!({ "results": boxes, "model_gen": out.model_gen }))
                }
                other => anyhow::bail!("unsupported vision task: {other}"),
            }
        })
        .await
        .context("vision inference blocking task panicked")??;

        Ok(ProviderOutput::ResultSet(result))
    }
}

// ── main ──────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let id: &'static str = Box::leak(guest_id().into_boxed_str());
    let repo_id = vision_repo_id();

    info!(guest_id = id, repo_id = %repo_id, "model-controller-vision starting (ONNX/Florence-2)");

    let config = VisionConfig {
        repo_id: repo_id.clone(),
        ..VisionConfig::default()
    };

    let backend = tokio::task::spawn_blocking(move || -> Result<VisionBackend> {
        let cache = ModelCache::new().context("init HF Hub model cache")?;
        let handle = cache
            .pull_vision(&config.repo_id)
            .with_context(|| format!("download Florence-2 from {}", config.repo_id))?;
        VisionBackend::load(&handle, &config).context("load VisionBackend")
    })
    .await
    .context("vision model load task panicked")??;

    let provider = Arc::new(OnnxVisionProvider::new(backend));

    run_datasource_controller(DatasourceGuestConfig {
        guest_id: id,
        role: "model-controller-vision",
        providers: Box::new(move || vec![provider.clone()]),
    })
    .await
    .context("model-controller-vision controller exited with error")
}
