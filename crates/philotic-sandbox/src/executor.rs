use crate::policy::SandboxPolicy;
use crate::{ExecuteCommandRequest, ExecuteCommandResponse, ExecutionStatus, SandboxError};
use std::time::Instant;
use tokio::time::{timeout, Duration};
use tracing::{debug, warn};

/// Execute a command request against the sandbox policy.
pub async fn execute(
    policy: &SandboxPolicy,
    request: ExecuteCommandRequest,
) -> ExecuteCommandResponse {
    let start = Instant::now();
    let request_id = request.request_id.clone();

    // 1. Validate command against allowlist/denylist
    if !policy.is_command_allowed(&request.command) {
        return ExecuteCommandResponse {
            request_id,
            status: ExecutionStatus::CommandNotAllowed,
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit_code: None,
            duration_ms: start.elapsed().as_millis() as u64,
            error: Some(SandboxError {
                code: "COMMAND_NOT_ALLOWED".into(),
                message: format!(
                    "command '{}' is not in the policy allowlist",
                    request.command
                ),
            }),
        };
    }

    // 2. Validate working directory against read paths
    if let Some(ref cwd) = request.cwd {
        if !policy.is_read_path_allowed(cwd) {
            return ExecuteCommandResponse {
                request_id,
                status: ExecutionStatus::PathNotAllowed,
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit_code: None,
                duration_ms: start.elapsed().as_millis() as u64,
                error: Some(SandboxError {
                    code: "PATH_NOT_ALLOWED".into(),
                    message: format!("working directory '{}' is not in allowed read paths", cwd),
                }),
            };
        }
    }

    // 3. Filter environment variables
    let filtered_env = policy.filter_env(&request.env);

    // 4. Compute effective timeout
    let timeout_ms = policy.effective_timeout_ms(request.timeout_ms);

    debug!(
        request_id = %request_id,
        command = %request.command,
        args = ?request.args,
        timeout_ms = timeout_ms,
        "executing sandboxed command"
    );

    // 5. Build the command
    let mut cmd = tokio::process::Command::new(&request.command);
    cmd.args(&request.args);

    if let Some(ref cwd) = request.cwd {
        cmd.current_dir(cwd);
    }

    cmd.env_clear();
    for (k, v) in &filtered_env {
        cmd.env(k, v);
    }

    if request.stdin.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    } else {
        cmd.stdin(std::process::Stdio::null());
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    // 6. Spawn and wait with timeout
    let result = timeout(Duration::from_millis(timeout_ms), async {
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                return Err(e);
            }
        };

        // Write stdin if provided
        if let Some(ref stdin_data) = request.stdin {
            if let Some(ref mut stdin) = child.stdin {
                use tokio::io::AsyncWriteExt;
                let _ = stdin.write_all(stdin_data).await;
                let _ = stdin.shutdown().await;
            }
        }

        child.wait_with_output().await
    })
    .await;

    let duration_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok(Ok(output)) => {
            let status = if output.status.success() {
                ExecutionStatus::Success
            } else {
                ExecutionStatus::ProcessError
            };

            ExecuteCommandResponse {
                request_id,
                status,
                stdout: output.stdout,
                stderr: output.stderr,
                exit_code: output.status.code(),
                duration_ms,
                error: None,
            }
        }
        Ok(Err(e)) => {
            warn!(request_id = %request_id, error = %e, "process spawn/execution failed");
            ExecuteCommandResponse {
                request_id,
                status: ExecutionStatus::ProcessError,
                stdout: Vec::new(),
                stderr: format!("failed to execute command: {}", e).into_bytes(),
                exit_code: None,
                duration_ms,
                error: Some(SandboxError {
                    code: "PROCESS_ERROR".into(),
                    message: e.to_string(),
                }),
            }
        }
        Err(_) => {
            warn!(request_id = %request_id, timeout_ms = timeout_ms, "command timed out");
            ExecuteCommandResponse {
                request_id,
                status: ExecutionStatus::Timeout,
                stdout: Vec::new(),
                stderr: Vec::new(),
                exit_code: None,
                duration_ms,
                error: Some(SandboxError {
                    code: "TIMEOUT".into(),
                    message: format!("command exceeded {}ms timeout", timeout_ms),
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::SandboxPolicy;
    use std::collections::HashMap;

    fn test_policy() -> SandboxPolicy {
        SandboxPolicy::from_str(
            r#"
[sandbox]
name = "test"
max_timeout_ms = 5000

[commands]
allowed = ["echo", "cat", "ls", "true", "false", "sleep"]
denied = ["sudo", "rm"]

[filesystem]
read_paths = ["/tmp/**", "/usr/**"]
write_paths = ["/tmp/test-output/**"]
execute_paths = ["/usr/bin/**", "/bin/**"]

[environment]
allowlist = ["PATH", "HOME"]
forced = { SANDBOX = "1" }

[network]
mode = "none"

[seccomp]
profile = "shell_executor"
"#,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn allowed_command_succeeds() {
        let policy = test_policy();
        let req = ExecuteCommandRequest {
            request_id: "test-1".into(),
            command: "echo".into(),
            args: vec!["hello".into()],
            cwd: None,
            env: HashMap::new(),
            stdin: None,
            timeout_ms: Some(5000),
        };

        let resp = execute(&policy, req).await;
        assert_eq!(resp.status, ExecutionStatus::Success);
        assert_eq!(String::from_utf8_lossy(&resp.stdout).trim(), "hello");
    }

    #[tokio::test]
    async fn denied_command_rejected() {
        let policy = test_policy();
        let req = ExecuteCommandRequest {
            request_id: "test-2".into(),
            command: "sudo".into(),
            args: vec!["ls".into()],
            cwd: None,
            env: HashMap::new(),
            stdin: None,
            timeout_ms: None,
        };

        let resp = execute(&policy, req).await;
        assert_eq!(resp.status, ExecutionStatus::CommandNotAllowed);
        assert!(resp.error.is_some());
    }

    #[tokio::test]
    async fn unlisted_command_rejected() {
        let policy = test_policy();
        let req = ExecuteCommandRequest {
            request_id: "test-3".into(),
            command: "wget".into(),
            args: vec![],
            cwd: None,
            env: HashMap::new(),
            stdin: None,
            timeout_ms: None,
        };

        let resp = execute(&policy, req).await;
        assert_eq!(resp.status, ExecutionStatus::CommandNotAllowed);
    }

    #[tokio::test]
    async fn disallowed_cwd_rejected() {
        let policy = test_policy();
        let req = ExecuteCommandRequest {
            request_id: "test-4".into(),
            command: "ls".into(),
            args: vec![],
            cwd: Some("/etc/secret".into()),
            env: HashMap::new(),
            stdin: None,
            timeout_ms: None,
        };

        let resp = execute(&policy, req).await;
        assert_eq!(resp.status, ExecutionStatus::PathNotAllowed);
    }

    #[tokio::test]
    async fn timeout_enforced() {
        let mut policy = test_policy();
        policy.sandbox.max_timeout_ms = 500;

        let req = ExecuteCommandRequest {
            request_id: "test-5".into(),
            command: "sleep".into(),
            args: vec!["10".into()],
            cwd: None,
            env: HashMap::new(),
            stdin: None,
            timeout_ms: Some(200),
        };

        let resp = execute(&policy, req).await;
        assert_eq!(resp.status, ExecutionStatus::Timeout);
    }

    #[tokio::test]
    async fn env_filtered_and_forced() {
        let policy = test_policy();
        let mut env = HashMap::new();
        env.insert("PATH".into(), "/usr/bin:/bin".into());
        env.insert("SECRET".into(), "should_not_appear".into());

        let req = ExecuteCommandRequest {
            request_id: "test-6".into(),
            command: "echo".into(),
            args: vec!["ok".into()],
            cwd: None,
            env,
            stdin: None,
            timeout_ms: None,
        };

        let resp = execute(&policy, req).await;
        assert_eq!(resp.status, ExecutionStatus::Success);
    }

    #[tokio::test]
    async fn nonexistent_command_returns_process_error() {
        let policy = SandboxPolicy::from_str(
            r#"
[sandbox]
name = "test"

[commands]
allowed = ["nonexistent_command_12345"]
"#,
        )
        .unwrap();

        let req = ExecuteCommandRequest {
            request_id: "test-7".into(),
            command: "nonexistent_command_12345".into(),
            args: vec![],
            cwd: None,
            env: HashMap::new(),
            stdin: None,
            timeout_ms: Some(5000),
        };

        let resp = execute(&policy, req).await;
        assert_eq!(resp.status, ExecutionStatus::ProcessError);
    }
}
