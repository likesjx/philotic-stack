use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use std::net::SocketAddr;
use tokio::net::UdpSocket;
use tracing::{info, debug, error};

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
    socket: UdpSocket,
    ansible_addr: SocketAddr,
    _identity: GuestIdentity,
}

impl PhiloticClient {
    /// Connect to the local Ansible daemon automatically, driven by environment variables.
    /// Default Hotel port is 9000 unless PHILOTIC_HOTEL_PORT is specified.
    pub async fn connect(identity: GuestIdentity) -> Result<Self> {
        let port_str = std::env::var("PHILOTIC_HOTEL_PORT").unwrap_or_else(|_| "9000".to_string());
        let port: u16 = port_str.parse().unwrap_or(9000);
        let ansible_addr: SocketAddr = format!("127.0.0.1:{}", port).parse()?;
        
        let socket = UdpSocket::bind("127.0.0.1:0")
            .await
            .context("Failed to bind ephemeral local UDP socket for PhiloticClient")?;
            
        debug!("PhiloticClient connecting to local Ansible at {}...", ansible_addr);
        
        let client = Self {
            socket,
            ansible_addr,
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
    pub async fn send_request(&self, req: IpcRequest) -> Result<IpcResponse> {
        let payload = serde_json::to_vec(&req).context("Failed to serialize IpcRequest")?;
        self.socket
            .send_to(&payload, &self.ansible_addr)
            .await
            .context("Failed to send IPC request to Ansible")?;

        // Wait for Ack
        let mut buf = vec![0u8; 65535];
        let (len, src) = self.socket.recv_from(&mut buf).await.context("Failed to receive IPC response")?;
        
        if src != self.ansible_addr {
            error!("Received phantom IPC response from unknown source: {}", src);
        }

        let resp: IpcResponse = serde_json::from_slice(&buf[..len])
            .context("Failed to decode IpcResponse from Ansible")?;
            
        Ok(resp)
    }
    
    /// Poll for inbound tasks routed from the Philotic Web
    pub async fn recv_task(&mut self) -> Result<IpcResponse> {
        let mut buf = vec![0u8; 65535];
        let (len, _src) = self.socket.recv_from(&mut buf).await.context("Failed to receive IPC task/response")?;
        
        let resp: IpcResponse = serde_json::from_slice(&buf[..len])
            .context("Failed to decode IpcResponse from Ansible")?;
            
        Ok(resp)
    }

    /// Optimistically write a memory apartment update to the hotel without waiting for a full response
    pub async fn sync_apartment(&self, agent_id: &str, memory_type: &str, content_json: serde_json::Value) -> Result<()> {
        let req = IpcRequest::SyncApartment {
            agent_id: agent_id.to_string(),
            memory_type: memory_type.to_string(),
            content_json,
        };
        let payload = serde_json::to_vec(&req)?;
        self.socket
            .send_to(&payload, &self.ansible_addr)
            .await
            .context("Failed to dispatch SyncApartment")?;
        Ok(())
    }
}
