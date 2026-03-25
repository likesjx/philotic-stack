use serde::{Deserialize, Serialize};

/// Top-level fleet configuration for the MLX model controller.
/// Loaded once at startup from a JSON file (path in PHILOTIC_MLX_CONFIG).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MlxFleetConfig {
    /// How often to run background health checks, in seconds. Default: 300.
    #[serde(default = "default_health_interval")]
    pub health_check_interval_secs: u64,
    /// Python interpreter path for managed instances and whisper subprocess.
    /// Required when any model has mode=managed or class=transcribe.
    /// Example: "/usr/local/bin/python3" or "/home/user/.venv/bin/python".
    /// Omit for attached-only fleets — no Python needed.
    pub python_path: Option<String>,
    pub models: Vec<MlxModelConfig>,
}

impl MlxFleetConfig {
    /// Return the python interpreter path, or an error if it is required but absent.
    pub fn require_python_path(&self) -> anyhow::Result<&str> {
        self.python_path
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!(
                "fleet config requires `python_path` for managed instances or transcribe, \
                 but it is not set. Add e.g. \"python_path\": \"/usr/local/bin/python3\" \
                 to your fleet config."
            ))
    }

    /// True if any configured model requires a Python subprocess.
    pub fn needs_python(&self) -> bool {
        self.models.iter().any(|m| {
            m.mode == MlxMode::Managed || m.class == MlxModelClass::Transcribe
        })
    }
}

fn default_health_interval() -> u64 {
    300
}

impl MlxFleetConfig {
    pub fn from_json_file(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read MLX fleet config {path}: {e}"))?;
        serde_json::from_str(&content)
            .map_err(|e| anyhow::anyhow!("failed to parse MLX fleet config {path}: {e}"))
    }

    pub fn from_env() -> anyhow::Result<Self> {
        let path = std::env::var("PHILOTIC_MLX_CONFIG")
            .map_err(|_| anyhow::anyhow!("PHILOTIC_MLX_CONFIG not set; provide a fleet config JSON path"))?;
        Self::from_json_file(&path)
    }

    /// Try to fetch fleet config from the local hotel IPC (via `GetConfig { key: "component:{guest_id}" }`).
    /// Returns `Ok(Some(config))` on success, `Ok(None)` if the key is not set.
    /// Falls back gracefully on connection failure so the binary remains usable without a running hotel.
    pub async fn from_hotel(socket_path: &str, guest_id: &str) -> anyhow::Result<Option<Self>> {
        use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
        let identity = GuestIdentity {
            guest_id: format!("{guest_id}-cfg-probe"),
            role: "model.mlx".to_string(),
            supported_tools: vec![],
        };
        let mut client = PhiloticClient::connect_at(socket_path, identity).await?;
        let key = format!("component:{guest_id}");
        let resp = client.send_request(IpcRequest::GetConfig { key }).await?;
        match resp {
            IpcResponse::ConfigData { value_json: Some(json), .. } => {
                let config: Self = serde_json::from_str(&json)
                    .map_err(|e| anyhow::anyhow!("failed to parse fleet config from hotel: {e}"))?;
                Ok(Some(config))
            }
            IpcResponse::ConfigData { value_json: None, .. } => Ok(None),
            other => anyhow::bail!("unexpected response from hotel for GetConfig: {:?}", other),
        }
    }
}

/// Per-model instance configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MlxModelConfig {
    /// Functional class: text, multimodal, or transcribe.
    pub class: MlxModelClass,
    /// HuggingFace repo id or local path. Used to verify the loaded model in attached mode.
    pub repo_id: String,
    /// Connection mode (default: managed).
    #[serde(default)]
    pub mode: MlxMode,
    /// Hostname for HTTP connections (default: "127.0.0.1").
    #[serde(default = "default_host")]
    pub host: String,
    /// HTTP port for mlx_lm.server (required for text/multimodal; unused for transcribe).
    pub port: Option<u16>,
    /// Priority weight. Higher = preferred. Convention: 100+ primary, 50 secondary, 10 tertiary.
    #[serde(default = "default_priority")]
    pub priority: u32,
    /// Extra CLI args forwarded verbatim to mlx_lm.server (e.g. ["--quantize", "--q-bits", "4"]).
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Server binary variant. Default: MlxLm. Use MlxVlm for multimodal (deferred).
    #[serde(default)]
    pub server_variant: MlxServerVariant,
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_priority() -> u32 {
    50
}

/// Functional class of an MLX model instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MlxModelClass {
    #[default]
    Text,
    Multimodal,
    Transcribe,
}

/// How the controller connects to a model instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MlxMode {
    /// Controller attaches to an already-running server at host:port (default).
    /// The user is responsible for starting and managing the mlx_lm.server process.
    #[default]
    Attached,
    /// Controller spawns and supervises the mlx_lm.server subprocess.
    /// Requires `python_path` to be set in the fleet config.
    Managed,
}

/// Which server binary serves this model. Accommodates future mlx_vlm support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MlxServerVariant {
    /// mlx_lm.server — text generation and tool calling.
    #[default]
    MlxLm,
    /// mlx_vlm — vision + language (deferred; not yet implemented).
    MlxVlm,
}
