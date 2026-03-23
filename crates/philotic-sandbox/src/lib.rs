pub mod client;
pub mod executor;
pub mod ipc;
pub mod policy;

#[cfg(target_os = "linux")]
pub mod landlock;
#[cfg(target_os = "linux")]
pub mod seccomp;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Request to execute a command inside the sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteCommandRequest {
    /// Unique request ID for correlation.
    pub request_id: String,
    /// The command to execute (must be in policy allowlist).
    pub command: String,
    /// Command arguments.
    pub args: Vec<String>,
    /// Working directory (must be within policy-allowed paths).
    pub cwd: Option<String>,
    /// Environment variables to set (filtered by policy).
    pub env: HashMap<String, String>,
    /// Optional stdin data.
    pub stdin: Option<Vec<u8>>,
    /// Timeout in milliseconds (capped by policy max).
    pub timeout_ms: Option<u64>,
}

/// Response from a sandbox command execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteCommandResponse {
    pub request_id: String,
    pub status: ExecutionStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// Policy violations or sandbox errors.
    pub error: Option<SandboxError>,
}

/// Status of a command execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionStatus {
    Success,
    CommandNotAllowed,
    PathNotAllowed,
    EnvNotAllowed,
    Timeout,
    SandboxError,
    ProcessError,
}

/// Error details from the sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxError {
    pub code: String,
    pub message: String,
}

/// The shell execution mode, selected via configuration.
#[derive(Debug, Clone)]
pub enum ShellExecutionMode {
    /// Direct execution via std::process::Command (dev mode).
    Direct,
    /// Sandboxed execution via the sandbox worker over IPC.
    Sandboxed { socket_path: String },
}

/// Trait that both DirectShellExecutor and SandboxedShellExecutor implement.
#[async_trait::async_trait]
pub trait ShellExecutor: Send + Sync {
    async fn execute(
        &self,
        request: ExecuteCommandRequest,
    ) -> anyhow::Result<ExecuteCommandResponse>;
}

/// Direct executor — current behavior, for development.
pub struct DirectShellExecutor;

#[async_trait::async_trait]
impl ShellExecutor for DirectShellExecutor {
    async fn execute(
        &self,
        request: ExecuteCommandRequest,
    ) -> anyhow::Result<ExecuteCommandResponse> {
        use std::time::Instant;

        let start = Instant::now();
        let mut cmd = std::process::Command::new(&request.command);
        cmd.args(&request.args);

        if let Some(ref cwd) = request.cwd {
            cmd.current_dir(cwd);
        }

        cmd.env_clear();
        for (k, v) in &request.env {
            cmd.env(k, v);
        }

        if request.stdin.is_some() {
            cmd.stdin(std::process::Stdio::piped());
        }
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("failed to spawn command '{}': {}", request.command, e)
        })?;

        if let Some(ref stdin_data) = request.stdin {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                let _ = stdin.write_all(stdin_data);
            }
        }

        let output = child.wait_with_output()?;
        let duration_ms = start.elapsed().as_millis() as u64;

        let status = if output.status.success() {
            ExecutionStatus::Success
        } else {
            ExecutionStatus::ProcessError
        };

        Ok(ExecuteCommandResponse {
            request_id: request.request_id,
            status,
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.status.code(),
            duration_ms,
            error: None,
        })
    }
}
