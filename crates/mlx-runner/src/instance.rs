use crate::client::{ChatMessage, ChatRequest, MlxClient};
use crate::config::{MlxModelClass, MlxModelConfig, MlxMode};
use crate::server::MlxServerHandle;
use anyhow::{Context, Result};
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tracing::{info, warn};

/// Health state of a single mlx_lm.server instance.
#[derive(Debug, Clone)]
pub enum HealthState {
    Starting,
    Healthy { checked_at: Instant },
    /// Degraded after a request failure or transient health-check failure.
    Degraded { since: Instant, failures: u32 },
    /// Confirmed down after repeated health-check failures.
    Down { since: Instant },
}

impl HealthState {
    pub fn is_healthy(&self) -> bool {
        matches!(self, HealthState::Healthy { .. })
    }
}

/// A single mlx_lm.server instance managed by the MLX controller.
pub struct MlxModelInstance {
    pub config: MlxModelConfig,
    /// HTTP client wired to this instance's host:port.
    pub client: MlxClient,
    /// Subprocess handle (Some only in managed mode).
    _server: Option<MlxServerHandle>,
    /// The model id actually loaded, as reported by /v1/models at startup.
    pub discovered_model_id: Option<String>,
    health: Arc<Mutex<HealthState>>,
}

impl MlxModelInstance {
    /// Start or attach to an instance according to its config.
    /// `python_path` is required when mode is Managed; ignored for Attached.
    /// Performs startup health check and model discovery.
    pub async fn start(config: MlxModelConfig, python_path: Option<&str>) -> Result<Self> {
        let port = config
            .port
            .ok_or_else(|| anyhow::anyhow!("model {} has no port configured", config.repo_id))?;
        let client = MlxClient::new(&config.host, port)
            .with_context(|| format!("failed to create client for {}", config.repo_id))?;

        let server = match config.mode {
            MlxMode::Managed => {
                let python_path = python_path
                    .ok_or_else(|| anyhow::anyhow!(
                        "model {} requires managed mode but fleet config has no python_path",
                        config.repo_id
                    ))?;
                let handle = MlxServerHandle::spawn(python_path, &config.repo_id, port, &config.extra_args)
                    .with_context(|| format!("failed to spawn server for {}", config.repo_id))?;
                handle
                    .wait_ready(Duration::from_secs(120))
                    .await
                    .with_context(|| format!("server for {} did not start in time", config.repo_id))?;
                Some(handle)
            }
            MlxMode::Attached => None,
        };

        // Discover what model is actually loaded (trust but verify).
        let discovered_model_id = match client.discover_loaded_model().await {
            Ok(info) => {
                if info.model_id != config.repo_id {
                    warn!(
                        configured = %config.repo_id,
                        loaded = %info.model_id,
                        "mlx instance: loaded model differs from configured repo_id"
                    );
                }
                info!(
                    repo_id = %config.repo_id,
                    loaded = %info.model_id,
                    "mlx instance ready"
                );
                Some(info.model_id)
            }
            Err(e) => {
                warn!(repo_id = %config.repo_id, "startup discovery failed: {e}");
                None
            }
        };

        let health = if discovered_model_id.is_some() {
            HealthState::Healthy { checked_at: Instant::now() }
        } else {
            HealthState::Degraded { since: Instant::now(), failures: 1 }
        };

        Ok(Self {
            config,
            client,
            _server: server,
            discovered_model_id,
            health: Arc::new(Mutex::new(health)),
        })
    }

    pub fn class(&self) -> MlxModelClass {
        self.config.class
    }

    pub fn priority(&self) -> u32 {
        self.config.priority
    }

    pub async fn health(&self) -> HealthState {
        self.health.lock().await.clone()
    }

    pub async fn is_healthy(&self) -> bool {
        self.health.lock().await.is_healthy()
    }

    /// Run a background health check. Called by the periodic ticker.
    pub async fn run_health_check(&self) {
        match self.client.health_check().await {
            Ok(()) => {
                let mut h = self.health.lock().await;
                *h = HealthState::Healthy { checked_at: Instant::now() };
            }
            Err(e) => {
                let mut h = self.health.lock().await;
                let (since, failures) = match &*h {
                    HealthState::Degraded { since, failures } => (*since, *failures + 1),
                    _ => (Instant::now(), 1),
                };
                warn!(
                    repo_id = %self.config.repo_id,
                    failures,
                    "health check failed: {e}"
                );
                *h = if failures >= 5 {
                    HealthState::Down { since }
                } else {
                    HealthState::Degraded { since, failures }
                };
            }
        }
    }

    /// Mark this instance degraded immediately (e.g. on a request failure).
    pub async fn mark_degraded(&self) {
        let mut h = self.health.lock().await;
        let (since, failures) = match &*h {
            HealthState::Degraded { since, failures } => (*since, *failures + 1),
            _ => (Instant::now(), 1),
        };
        *h = HealthState::Degraded { since, failures };
        warn!(
            repo_id = %self.config.repo_id,
            failures,
            "instance marked degraded after request failure"
        );
    }

    /// Send a chat request to this instance.
    /// Returns Err on failure; caller should call mark_degraded() and reroute.
    pub async fn chat(
        &self,
        messages: Vec<ChatMessage>,
        tools: Option<Vec<Value>>,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
    ) -> Result<crate::client::ChatResponse> {
        let model = self
            .discovered_model_id
            .clone()
            .unwrap_or_else(|| self.config.repo_id.clone());
        let req = ChatRequest {
            model,
            messages,
            tools,
            tool_choice: None,
            temperature,
            max_tokens,
        };
        self.client.chat(&req).await
    }
}
