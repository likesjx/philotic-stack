use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ParakeetConfig {
    /// Python interpreter path (must have nemo-toolkit installed).
    pub python_path: String,
    /// NeMo model name or HuggingFace repo id.
    #[serde(default = "default_model_name")]
    pub model_name: String,
    /// Priority weight. Higher = preferred. Convention: 100+ primary, 50 secondary, 10 tertiary.
    #[serde(default = "default_priority")]
    pub priority: u32,
}

fn default_model_name() -> String {
    crate::DEFAULT_MODEL.to_string()
}
fn default_priority() -> u32 {
    10
}

impl ParakeetConfig {
    /// Try to fetch config from the local hotel IPC.
    /// Returns `Ok(Some(cfg))` on success, `Ok(None)` if no key is set,
    /// and falls back gracefully on connection failure.
    pub async fn from_hotel(socket_path: &str, guest_id: &str) -> Result<Option<Self>> {
        use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
        let identity = GuestIdentity {
            guest_id: format!("{guest_id}-cfg-probe"),
            role: "model.parakeet".to_string(),
            supported_tools: vec![],
        };
        let mut client = PhiloticClient::connect_at(socket_path, identity).await?;
        let key = format!("component:{guest_id}");
        let resp = client.send_request(IpcRequest::GetConfig { key }).await?;
        match resp {
            IpcResponse::ConfigData {
                value_json: Some(json),
                ..
            } => {
                let config: Self = serde_json::from_str(&json)
                    .map_err(|e| anyhow::anyhow!("failed to parse parakeet config from hotel: {e}"))?;
                Ok(Some(config))
            }
            IpcResponse::ConfigData {
                value_json: None, ..
            } => Ok(None),
            other => anyhow::bail!(
                "unexpected response from hotel for GetConfig: {:?}",
                other
            ),
        }
    }
}
