pub mod udp;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Represents the identity of a Guest materializing in the Hotel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestIdentity {
    pub guest_id: String,
    pub role: String,
}

/// Represents the types of operations a Guest can perform locally over IPC to the Ansible.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum IpcRequest {
    /// Connect and register as an active materialized guest
    Register(GuestIdentity),
    /// Drop a task onto the Philotic Web
    EmitTask {
        target_node: String,
        target_role: String,
        task_json: String,
    },
    /// Ask the Hotel for configuration data from the local Context Graph
    GetConfig { key: String },
}

/// Represents the response from the local Ansible back to the Guest via IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum IpcResponse {
    Ack { req_id: String },
    ConfigData { key: String, value_json: Option<String> },
    /// An inbound task routed from the Philotic Web destined for this Guest
    InboundTask {
        source_node: String,
        task_id: Uuid,
        task_json: String,
    },
    Error(String),
}

/// The core trait any materialized capability (Ruby script, Rust binary) must 
/// implement conceptually (or wrap natively) to check into the local Philotic Ansible Hotel.
#[async_trait::async_trait]
pub trait PhiloticClient {
    /// Connect to the local Ansible daemon (usually via 127.0.0.1:8999 or a UDS socket)
    async fn connect(&mut self) -> Result<()>;
    
    /// Send an IPC request to the local Ansible
    async fn send_request(&self, req: IpcRequest) -> Result<IpcResponse>;
    
    /// Poll for inbound tasks routed from the Philotic Web
    async fn recv_task(&mut self) -> Result<IpcResponse>;
}
