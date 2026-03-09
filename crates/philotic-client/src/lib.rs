use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::ErrorKind;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tracing::{debug, info};
use uuid::Uuid;

/// Represents the identity of a Guest materializing in the Hotel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestIdentity {
    pub guest_id: String,
    pub role: String,
    #[serde(default)]
    pub supported_tools: Vec<String>,
}

/// Represents the types of operations a Guest can perform locally over IPC to the Ansible Hotel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", content = "payload")]
#[serde(rename_all = "snake_case")]
pub enum IpcRequest {
    /// Connect and register as an active materialized guest
    Register(GuestIdentity),
    /// Ask the Hotel for configuration data from the local Context Graph
    GetConfig {
        key: String,
    },
    /// Section 6 Blueprint Operations
    PublishMessage {
        target_role: String,
        payload: serde_json::Value,
    },
    CreateTask {
        target_role: String,
        payload: serde_json::Value,
    },
    AckEvent {
        event_id: Uuid,
    },
    UpdateTask {
        task_id: Uuid,
        state: String,
        payload: serde_json::Value,
    },
    CompleteTask {
        task_id: Uuid,
        result: serde_json::Value,
    },
    FailTask {
        task_id: Uuid,
        error_code: String,
        reason: String,
    },
    SubscribeInbox {
        role: String,
    },
    QueryStatus {
        task_id: Uuid,
    },
    QueryTimeline {
        task_id: Uuid,
    },
    /// Drop a task onto the Philotic Web (Legacy)
    EmitTask {
        target_node: String,
        target_role: String,
        #[serde(default)]
        target_guest_id: Option<String>,
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
    Ack {
        req_id: String,
    },
    ConfigData {
        key: String,
        value_json: Option<String>,
    },
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

    pub fn error(
        corr_id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
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
    pending_push: VecDeque<IpcResponse>,
}

pub fn is_ipc_disconnect(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .map(|io_err| {
                matches!(
                    io_err.kind(),
                    ErrorKind::UnexpectedEof
                        | ErrorKind::BrokenPipe
                        | ErrorKind::ConnectionReset
                        | ErrorKind::ConnectionAborted
                        | ErrorKind::NotConnected
                )
            })
            .unwrap_or(false)
    })
}

impl PhiloticClient {
    async fn write_frame(&mut self, payload: &[u8]) -> Result<()> {
        let len = u32::try_from(payload.len()).context("IPC payload too large")?;
        self.stream
            .write_all(&len.to_be_bytes())
            .await
            .context("Failed to send IPC frame header to Ansible")?;
        self.stream
            .write_all(payload)
            .await
            .context("Failed to send IPC frame payload to Ansible")?;
        Ok(())
    }

    async fn read_frame(&mut self) -> Result<Vec<u8>> {
        let mut len_buf = [0u8; 4];
        self.stream
            .read_exact(&mut len_buf)
            .await
            .context("Failed to receive IPC frame header")?;
        let len = u32::from_be_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        self.stream
            .read_exact(&mut buf)
            .await
            .context("Failed to receive IPC frame payload")?;
        Ok(buf)
    }

    fn socket_path() -> String {
        std::env::var("PHILOTIC_HOTEL_SOCKET")
            .unwrap_or_else(|_| "/tmp/philotic-ansible.sock".to_string())
    }

    pub async fn connect_at(socket_path: impl AsRef<str>, identity: GuestIdentity) -> Result<Self> {
        let socket_path = socket_path.as_ref().to_string();
        let stream = UnixStream::connect(&socket_path)
            .await
            .with_context(|| format!("Failed to connect to hotel IPC socket at {}", socket_path))?;

        debug!(
            "PhiloticClient connecting to local Ansible at {}...",
            socket_path
        );

        let mut client = Self {
            stream,
            _identity: identity.clone(),
            pending_push: VecDeque::new(),
        };

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
            _ => {}
        }

        Ok(client)
    }

    /// Connect to the local Ansible daemon automatically, driven by environment variables.
    /// Default Hotel socket is `/tmp/philotic-ansible.sock` unless `PHILOTIC_HOTEL_SOCKET` is specified.
    pub async fn connect(identity: GuestIdentity) -> Result<Self> {
        Self::connect_at(Self::socket_path(), identity).await
    }

    /// Send an IPC request to the local Ansible
    pub async fn send_request(&mut self, req: IpcRequest) -> Result<IpcResponse> {
        let payload = serde_json::to_vec(&req).context("Failed to serialize IpcRequest")?;
        self.write_frame(&payload).await?;

        loop {
            let resp = self.read_response().await?;
            if Self::is_push_message(&resp) {
                self.pending_push.push_back(resp);
                continue;
            }
            return Ok(resp);
        }
    }

    async fn read_response(&mut self) -> Result<IpcResponse> {
        let buf = self.read_frame().await?;
        let resp: IpcResponse =
            serde_json::from_slice(&buf).context("Failed to decode IpcResponse from Ansible")?;

        Ok(resp)
    }

    fn is_push_message(response: &IpcResponse) -> bool {
        matches!(
            response,
            IpcResponse::InboundTask { .. } | IpcResponse::ApartmentUpdate { .. }
        )
    }

    /// Poll for inbound tasks routed from the Philotic Web
    pub async fn recv_task(&mut self) -> Result<IpcResponse> {
        if let Some(pending) = self.pending_push.pop_front() {
            return Ok(pending);
        }

        loop {
            let resp = self.read_response().await?;
            if Self::is_push_message(&resp) {
                return Ok(resp);
            }
            anyhow::bail!(
                "Unexpected non-push IPC response while waiting for inbound task: {:?}",
                resp
            );
        }
    }

    /// Write a memory apartment update to the hotel and consume the response so the IPC stream stays framed.
    pub async fn sync_apartment(
        &mut self,
        agent_id: &str,
        memory_type: &str,
        content_json: serde_json::Value,
    ) -> Result<()> {
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
    use std::io::ErrorKind;
    use std::path::Path;
    use std::sync::{LazyLock, Mutex as StdMutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    static IPC_TEST_ENV_LOCK: LazyLock<StdMutex<()>> = LazyLock::new(|| StdMutex::new(()));

    fn ipc_env_guard() -> std::sync::MutexGuard<'static, ()> {
        IPC_TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn test_socket_path() -> String {
        format!("/tmp/pc-{}.sock", Uuid::new_v4().simple())
    }

    async fn read_frame(stream: &mut tokio::net::UnixStream) -> Vec<u8> {
        let mut len_buf = [0u8; 4];
        stream
            .read_exact(&mut len_buf)
            .await
            .expect("read frame header");
        let len = u32::from_be_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        stream
            .read_exact(&mut buf)
            .await
            .expect("read frame payload");
        buf
    }

    async fn write_frame(stream: &mut tokio::net::UnixStream, payload: &[u8]) {
        let len = u32::try_from(payload.len()).expect("frame length");
        stream
            .write_all(&len.to_be_bytes())
            .await
            .expect("write frame header");
        stream
            .write_all(payload)
            .await
            .expect("write frame payload");
    }

    #[tokio::test]
    async fn connect_and_get_config_over_uds() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let listener = UnixListener::bind(&socket_path).expect("bind test socket");

        let server = tokio::spawn({
            let socket_path = socket_path.clone();
            async move {
                let (mut stream, _) = listener.accept().await.expect("accept client");

                let buf = read_frame(&mut stream).await;
                let req: IpcRequest = serde_json::from_slice(&buf).expect("decode register");
                match req {
                    IpcRequest::Register(identity) => assert_eq!(identity.guest_id, "guest-test-1"),
                    other => panic!("unexpected register request: {other:?}"),
                }
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::success("reg", None)).unwrap(),
                )
                .await;

                let buf = read_frame(&mut stream).await;
                let req: IpcRequest = serde_json::from_slice(&buf).expect("decode get_config");
                match req {
                    IpcRequest::GetConfig { key } => assert_eq!(key, "telegram_bot_token"),
                    other => panic!("unexpected config request: {other:?}"),
                }
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::ConfigData {
                        key: "telegram_bot_token".into(),
                        value_json: Some("\"secret-token\"".into()),
                    })
                    .unwrap(),
                )
                .await;

                let _ = std::fs::remove_file(&socket_path);
            }
        });

        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let identity = GuestIdentity {
            guest_id: "guest-test-1".into(),
            role: "test".into(),
            supported_tools: Vec::new(),
        };
        let mut client = PhiloticClient::connect(identity)
            .await
            .expect("connect client");
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
        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[tokio::test]
    async fn send_request_buffers_interleaved_push_messages() {
        let _env_guard = ipc_env_guard();
        let socket_path = test_socket_path();
        let listener = UnixListener::bind(&socket_path).expect("bind test socket");

        let server = tokio::spawn({
            let socket_path = socket_path.clone();
            async move {
                let (mut stream, _) = listener.accept().await.expect("accept client");
                let buf = read_frame(&mut stream).await;
                let _req: IpcRequest = serde_json::from_slice(&buf).expect("decode register");
                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::success("reg", None)).unwrap(),
                )
                .await;

                let buf = read_frame(&mut stream).await;
                let req: IpcRequest = serde_json::from_slice(&buf).expect("decode get_config");
                match req {
                    IpcRequest::GetConfig { key } => assert_eq!(key, "interleaved"),
                    other => panic!("unexpected config request: {other:?}"),
                }

                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::InboundTask {
                        source_node: "local-ansible-01".into(),
                        task_id: Uuid::nil(),
                        task_json: serde_json::json!({
                            "action": "send_reply",
                            "content": "pushed first"
                        })
                        .to_string(),
                    })
                    .unwrap(),
                )
                .await;

                write_frame(
                    &mut stream,
                    &serde_json::to_vec(&IpcResponse::ConfigData {
                        key: "interleaved".into(),
                        value_json: Some("\"ok\"".into()),
                    })
                    .unwrap(),
                )
                .await;

                let _ = std::fs::remove_file(&socket_path);
            }
        });

        unsafe {
            std::env::set_var("PHILOTIC_HOTEL_SOCKET", &socket_path);
        }

        let identity = GuestIdentity {
            guest_id: "guest-test-2".into(),
            role: "test".into(),
            supported_tools: Vec::new(),
        };
        let mut client = PhiloticClient::connect(identity)
            .await
            .expect("connect client");
        let response = client
            .send_request(IpcRequest::GetConfig {
                key: "interleaved".into(),
            })
            .await
            .expect("send request");

        match response {
            IpcResponse::ConfigData { key, value_json } => {
                assert_eq!(key, "interleaved");
                assert_eq!(value_json.as_deref(), Some("\"ok\""));
            }
            other => panic!("unexpected config response: {other:?}"),
        }

        let pushed = client.recv_task().await.expect("receive buffered push");
        match pushed {
            IpcResponse::InboundTask { task_json, .. } => {
                let payload: serde_json::Value =
                    serde_json::from_str(&task_json).expect("decode pushed task");
                assert_eq!(payload["content"], "pushed first");
            }
            other => panic!("unexpected pushed response: {other:?}"),
        }

        server.await.expect("join server");
        unsafe {
            std::env::remove_var("PHILOTIC_HOTEL_SOCKET");
        }
        if Path::new(&socket_path).exists() {
            let _ = std::fs::remove_file(&socket_path);
        }
    }

    #[test]
    fn disconnect_detection_matches_unexpected_eof() {
        let err = anyhow::Error::new(std::io::Error::from(ErrorKind::UnexpectedEof));
        assert!(is_ipc_disconnect(&err));
    }
}
