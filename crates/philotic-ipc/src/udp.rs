use crate::{IpcRequest, IpcResponse, PhiloticClient};
use anyhow::{Context, Result};
use std::net::SocketAddr;
use tokio::net::UdpSocket;
use tracing::{debug, error};

pub struct UdpPhiloticClient {
    socket: UdpSocket,
    ansible_addr: SocketAddr,
}

impl UdpPhiloticClient {
    /// Bind an ephemeral local socket and prepare to talk to the Ansible at `ansible_addr`
    pub async fn new(ansible_addr: SocketAddr) -> Result<Self> {
        let socket = UdpSocket::bind("127.0.0.1:0")
            .await
            .context("Failed to bind ephemeral local UDP socket for IPC")?;
        
        Ok(Self {
            socket,
            ansible_addr,
        })
    }
}

#[async_trait::async_trait]
impl PhiloticClient for UdpPhiloticClient {
    async fn connect(&mut self) -> Result<()> {
        debug!("UDP PhiloticClient 'connected' to local Ansible at {}", self.ansible_addr);
        Ok(())
    }

    async fn send_request(&self, req: IpcRequest) -> Result<IpcResponse> {
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

    async fn recv_task(&mut self) -> Result<IpcResponse> {
        let mut buf = vec![0u8; 65535];
        let (len, _src) = self.socket.recv_from(&mut buf).await.context("Failed to receive IPC response")?;
        
        let resp: IpcResponse = serde_json::from_slice(&buf[..len])
            .context("Failed to decode IpcResponse from Ansible")?;
            
        Ok(resp)
    }
}
