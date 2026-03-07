use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tracing::{info, debug};

/// Represents the identity of a Guest materializing in the Hotel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestIdentity {
    pub guest_id: String,
    pub role: String,
}

/// Represents the types of operations a Guest can perform locally over IPC to the Ansible Hotel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", content = "payload")]
#[serde(rename_all = "snake_case")]
pub enum IpcRequest {
    /// Connect and register as an active materialized guest
    Register(GuestIdentity),
    /// Ask the Hotel for configuration data from the local Context Graph
    GetConfig { key: String },
    /// Section 6 Blueprint Operations
    PublishMessage { target_role: String, payload: serde_json::Value },
    CreateTask { target_role: String, payload: serde_json::Value },
    AckEvent { event_id: Uuid },
    UpdateTask { task_id: Uuid, state: String, payload: serde_json::Value },
    CompleteTask { task_id: Uuid, result: serde_json::Value },
    FailTask { task_id: Uuid, error_code: String, reason: String },
    SubscribeInbox { role: String },
    QueryStatus { task_id: Uuid },
    QueryTimeline { task_id: Uuid },
    /// Drop a task onto the Philotic Web (Legacy)
    EmitTask {
        target_node: String,
        target_role: String,
        task_json: String,
    },
    /// Optimistically push a RAM-based memory apartment update to the Hotel's SQLite Graph
    SyncApartment {
        agent_id: String,
        memory_type: String,
        content_json: serde_json::Value,
    },
}

/// Represents the canonical response from the local Ansible back to the Guest via IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IpcResponse {
    Ack { req_id: String },
    ConfigData { key: String, value_json: Option<String> },
    InboundTask {
        source_node: String,
        task_id: Uuid,
        task_json: String,
    },
    Error(String),
    Standard {
        ok: bool,
        code: String,
        message: String,
        corr_id: String,
        data: Option<serde_json::Value>,
    },
    /// Hotel actively pushing a memory apartment conflict resolution or external sync to the Guest
    ApartmentUpdate {
        agent_id: String,
        memory_type: String,
        content_json: serde_json::Value,
    },
}

impl IpcResponse {
    pub fn success(corr_id: impl Into<String>, data: Option<serde_json::Value>) -> Self {
        Self::Standard {
            ok: true,
            code: "OK".into(),
            message: "Success".into(),
            corr_id: corr_id.into(),
            data,
        }
    }
    
    pub fn error(corr_id: impl Into<String>, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Standard {
            ok: false,
            code: code.into(),
            message: message.into(),
            corr_id: corr_id.into(),
            data: None,
        }
    }
}

/// A pushed event delivered to an active inbox subscription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IpcPushEvent {
    pub event_id: Uuid,
    pub source_node: String,
    pub payload: serde_json::Value,
}

/// The concrete Universal Hotel Client SDK
pub struct PhiloticClient {
    stream: UnixStream,
    _identity: GuestIdentity,
}

impl PhiloticClient {
    fn socket_path() -> String {
        std::env::var("PHILOTIC_HOTEL_SOCKET")
            .unwrap_or_else(|_| "/tmp/philotic-ansible.sock".to_string())
    }

    /// Connect to the local Ansible daemon automatically, driven by environment variables.
    /// Default Hotel socket is `/tmp/philotic-ansible.sock` unless `PHILOTIC_HOTEL_SOCKET` is specified.
    pub async fn connect(identity: GuestIdentity) -> Result<Self> {
        let socket_path = Self::socket_path();
        let stream = UnixStream::connect(&socket_path)
            .await
            .with_context(|| format!("Failed to connect to hotel IPC socket at {}", socket_path))?;
            
        debug!("PhiloticClient connecting to local Ansible at {}...", socket_path);
        
        let mut client = Self {
            stream,
            _identity: identity.clone(),
        };
        
        // Execute the Registration Handshake
        info!("Registering as Materialized Guest: {:?}", identity);
        let resp = client.send_request(IpcRequest::Register(identity)).await?;
        info!("Ansible Hotel Registration Response: {:?}", resp);
        
        match resp {
            IpcResponse::Standard { ok, message, .. } if !ok => {
                anyhow::bail!("Hotel rejected registration: {}", message);
            }
            IpcResponse::Error(msg) => {
                anyhow::bail!("Hotel rejected registration: {}", msg);
            }
            _ => {
                // Success
            }
        }
        
        Ok(client)
    }
    
    /// Send an IPC request to the local Ansible
    pub async fn send_request(&mut self, req: IpcRequest) -> Result<IpcResponse> {
        let payload = serde_json::to_vec(&req).context("Failed to serialize IpcRequest")?;
        self.stream
            .write_all(&payload)
            .await
            .context("Failed to send IPC request to Ansible")?;

        // Wait for Ack
        let mut buf = vec![0u8; 65535];
        let len = self.stream.read(&mut buf).await.context("Failed to receive IPC response")?;
        if len == 0 {
            anyhow::bail!("Hotel closed the IPC connection");
        }

        let resp: IpcResponse = serde_json::from_slice(&buf[..len])
            .context("Failed to decode IpcResponse from Ansible")?;
            
        Ok(resp)
    }
    
    /// Poll for inbound tasks routed from the Philotic Web
    pub async fn recv_task(&mut self) -> Result<IpcResponse> {
        let mut buf = vec![0u8; 65535];
        let len = self.stream.read(&mut buf).await.context("Failed to receive IPC task/response")?;
        if len == 0 {
            anyhow::bail!("Hotel closed the IPC connection");
        }
        
        let resp: IpcResponse = serde_json::from_slice(&buf[..len])
            .context("Failed to decode IpcResponse from Ansible")?;
            
        Ok(resp)
    }

    /// Write a memory apartment update to the hotel and consume the response so the IPC stream stays framed.
    pub async fn sync_apartment(&mut self, agent_id: &str, memory_type: &str, content_json: serde_json::Value) -> Result<()> {
        let req = IpcRequest::SyncApartment {
            agent_id: agent_id.to_string(),
            memory_type: memory_type.to_string(),
            content_json,
        };
        let response = self.send_request(req).await?;
        match response {
            IpcResponse::Standard { ok: true, .. } => {}
            IpcResponse::Standard { message, .. } => {
                anyhow::bail!("SyncApartment failed: {}", message);
            }
            other => {
                anyhow::bail!("Unexpected SyncApartment response: {:?}", other);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tokio::net::UnixListener;

    fn test_socket_path() -> String {
        format!("/tmp/pc-{}.sock", Uuid::new_v4().simple())
    }

    #[tokio::test]
    async fn connect_and_get_config_over_uds() {
        let socket_path = test_socket_path();
        let listener = UnixListener::bind(&socket_path).expect("bind test socket");

        let server = tokio::spawn({
            let socket_path = socket_path.clone();
            async move {
                let (mut stream, _) = listener.accept().await.expect("accept client");
                let mut buf = vec![0u8; 65535];

                let len = stream.read(&mut buf).await.expect("read register");
                let req: IpcRequest = serde_json::from_slice(&buf[..len]).expect("decode register");
                match req {
                    IpcRequest::Register(identity) => assert_eq!(identity.guest_id, "guest-test-1"),
                    other => panic!("unexpected register request: {other:?}"),
                }
                stream
                    .write_all(&serde_json::to_vec(&IpcResponse::success("reg", None)).unwrap())
                    .await
                    .expect("write register response");

                let len = stream.read(&mut buf).await.expect("read get_config");
                let req: IpcRequest = serde_json::from_slice(&buf[..len]).expect("decode get_config");
                match req {
                    IpcRequest::GetConfig { key } => assert_eq!(key, "telegram_bot_token"),
                    other => panic!("unexpected config request: {other:?}"),
                }
                stream
                    .write_all(&serde_json::to_vec(&IpcResponse::ConfigData {
                        key: "telegram_bot_token".into(),
                        value_json: Some("\"secret-token\"".into()),
                    }).unwrap())
                    .await
                    .expect("write config response");

                let _ = std::fs::remove_file(&socket_path);
            }
        });

        unsafe { std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path); }

        let identity = GuestIdentity {
            guest_id: "guest-test-1".into(),
            role: "test".into(),
        };
        let mut client = PhiloticClient::connect(identity).await.expect("connect client");
        let response = client
            .send_request(IpcRequest::GetConfig {
                key: "telegram_bot_token".into(),
            })
            .await
            .expect("send request");

        match response {
            IpcResponse::ConfigData { key, value_json } => {
                assert_eq!(key, "telegram_bot_token");
                assert_eq!(value_json.as_deref(), Some("\"secret-token\""));
            }
            other => panic!("unexpected response: {other:?}"),
        }

        server.await.expect("join server");
        unsafe { std::env::remove_var("PHILOTIC_HOTEL_SOCKET"); }
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }
}
