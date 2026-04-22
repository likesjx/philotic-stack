use crate::ipc::{read_frame, write_frame};
use crate::{ExecuteCommandRequest, ExecuteCommandResponse, ShellExecutor};
use anyhow::{Context, Result};
use tokio::net::UnixStream;

/// IPC client that connects to the sandbox worker over a Unix domain socket.
pub struct SandboxedShellExecutor {
    socket_path: String,
}

impl SandboxedShellExecutor {
    pub fn new(socket_path: String) -> Self {
        Self { socket_path }
    }

    /// Send a request and receive a response over a fresh UDS connection.
    async fn send_request(
        &self,
        request: &ExecuteCommandRequest,
    ) -> Result<ExecuteCommandResponse> {
        let stream = UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| {
                format!(
                    "failed to connect to sandbox worker at {}",
                    self.socket_path
                )
            })?;

        let (mut reader, mut writer) = stream.into_split();

        write_frame(&mut writer, request)
            .await
            .context("failed to send request to sandbox worker")?;

        let response: ExecuteCommandResponse = read_frame(&mut reader)
            .await
            .context("failed to read response from sandbox worker")?
            .context("sandbox worker closed connection without responding")?;

        Ok(response)
    }
}

#[async_trait::async_trait]
impl ShellExecutor for SandboxedShellExecutor {
    async fn execute(&self, request: ExecuteCommandRequest) -> Result<ExecuteCommandResponse> {
        self.send_request(&request).await
    }
}
