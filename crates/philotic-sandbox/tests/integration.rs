//! Integration tests for the sandbox runtime.
//!
//! These tests start the sandbox worker binary and communicate with it over UDS.
//! They are gated behind `#[cfg(target_os = "linux")]` for OS-specific features,
//! but the basic IPC tests run on all platforms.

use philotic_sandbox::executor;
use philotic_sandbox::ipc::{read_frame, write_frame};
use philotic_sandbox::policy::SandboxPolicy;
use philotic_sandbox::{ExecuteCommandRequest, ExecuteCommandResponse, ExecutionStatus};
use std::collections::HashMap;
use tempfile::TempDir;
use tokio::net::{UnixListener, UnixStream};

fn test_policy() -> SandboxPolicy {
    SandboxPolicy::from_str(
        r#"
[sandbox]
name = "integration-test"
max_timeout_ms = 5000
max_concurrent = 2

[commands]
allowed = ["echo", "cat", "ls", "true", "false", "sleep"]
denied = ["sudo", "rm"]

[filesystem]
read_paths = ["/tmp/**", "/usr/**", "/bin/**"]
write_paths = ["/tmp/sandbox-test/**"]
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
async fn ipc_roundtrip_over_uds() {
    let tmp = TempDir::new().unwrap();
    let socket_path = tmp.path().join("test.sock");

    let listener = UnixListener::bind(&socket_path).unwrap();

    let policy = test_policy();

    // Server task
    let server = tokio::spawn({
        let policy = policy.clone();
        async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();

            let req: ExecuteCommandRequest = read_frame(&mut reader).await.unwrap().unwrap();
            let resp = executor::execute(&policy, req).await;
            write_frame(&mut writer, &resp).await.unwrap();
        }
    });

    // Client
    let stream = UnixStream::connect(&socket_path).await.unwrap();
    let (mut reader, mut writer) = stream.into_split();

    let req = ExecuteCommandRequest {
        request_id: "ipc-test-1".into(),
        command: "echo".into(),
        args: vec!["integration test".into()],
        cwd: None,
        env: HashMap::new(),
        stdin: None,
        timeout_ms: Some(5000),
    };

    write_frame(&mut writer, &req).await.unwrap();
    let resp: ExecuteCommandResponse = read_frame(&mut reader).await.unwrap().unwrap();

    assert_eq!(resp.request_id, "ipc-test-1");
    assert_eq!(resp.status, ExecutionStatus::Success);
    assert_eq!(
        String::from_utf8_lossy(&resp.stdout).trim(),
        "integration test"
    );

    server.await.unwrap();
}

#[tokio::test]
async fn ipc_denied_command() {
    let tmp = TempDir::new().unwrap();
    let socket_path = tmp.path().join("test.sock");

    let listener = UnixListener::bind(&socket_path).unwrap();
    let policy = test_policy();

    let server = tokio::spawn({
        let policy = policy.clone();
        async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();

            let req: ExecuteCommandRequest = read_frame(&mut reader).await.unwrap().unwrap();
            let resp = executor::execute(&policy, req).await;
            write_frame(&mut writer, &resp).await.unwrap();
        }
    });

    let stream = UnixStream::connect(&socket_path).await.unwrap();
    let (mut reader, mut writer) = stream.into_split();

    let req = ExecuteCommandRequest {
        request_id: "ipc-test-2".into(),
        command: "sudo".into(),
        args: vec!["ls".into()],
        cwd: None,
        env: HashMap::new(),
        stdin: None,
        timeout_ms: None,
    };

    write_frame(&mut writer, &req).await.unwrap();
    let resp: ExecuteCommandResponse = read_frame(&mut reader).await.unwrap().unwrap();

    assert_eq!(resp.status, ExecutionStatus::CommandNotAllowed);
    assert!(resp.error.is_some());

    server.await.unwrap();
}

#[tokio::test]
async fn ipc_path_not_allowed() {
    let tmp = TempDir::new().unwrap();
    let socket_path = tmp.path().join("test.sock");

    let listener = UnixListener::bind(&socket_path).unwrap();
    let policy = test_policy();

    let server = tokio::spawn({
        let policy = policy.clone();
        async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();

            let req: ExecuteCommandRequest = read_frame(&mut reader).await.unwrap().unwrap();
            let resp = executor::execute(&policy, req).await;
            write_frame(&mut writer, &resp).await.unwrap();
        }
    });

    let stream = UnixStream::connect(&socket_path).await.unwrap();
    let (mut reader, mut writer) = stream.into_split();

    let req = ExecuteCommandRequest {
        request_id: "ipc-test-3".into(),
        command: "ls".into(),
        args: vec![],
        cwd: Some("/etc/secret".into()),
        env: HashMap::new(),
        stdin: None,
        timeout_ms: None,
    };

    write_frame(&mut writer, &req).await.unwrap();
    let resp: ExecuteCommandResponse = read_frame(&mut reader).await.unwrap().unwrap();

    assert_eq!(resp.status, ExecutionStatus::PathNotAllowed);

    server.await.unwrap();
}

#[tokio::test]
async fn ipc_timeout_enforcement() {
    let tmp = TempDir::new().unwrap();
    let socket_path = tmp.path().join("test.sock");

    let listener = UnixListener::bind(&socket_path).unwrap();
    let mut policy = test_policy();
    policy.sandbox.max_timeout_ms = 500;

    let server = tokio::spawn({
        let policy = policy.clone();
        async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();

            let req: ExecuteCommandRequest = read_frame(&mut reader).await.unwrap().unwrap();
            let resp = executor::execute(&policy, req).await;
            write_frame(&mut writer, &resp).await.unwrap();
        }
    });

    let stream = UnixStream::connect(&socket_path).await.unwrap();
    let (mut reader, mut writer) = stream.into_split();

    let req = ExecuteCommandRequest {
        request_id: "ipc-test-4".into(),
        command: "sleep".into(),
        args: vec!["10".into()],
        cwd: None,
        env: HashMap::new(),
        stdin: None,
        timeout_ms: Some(200),
    };

    write_frame(&mut writer, &req).await.unwrap();
    let resp: ExecuteCommandResponse = read_frame(&mut reader).await.unwrap().unwrap();

    assert_eq!(resp.status, ExecutionStatus::Timeout);

    server.await.unwrap();
}

#[tokio::test]
async fn policy_file_loading() {
    let tmp = TempDir::new().unwrap();
    let policy_path = tmp.path().join("test-policy.toml");

    std::fs::write(
        &policy_path,
        r#"
[sandbox]
name = "file-loaded"
max_timeout_ms = 15000

[commands]
allowed = ["echo"]
"#,
    )
    .unwrap();

    let policy = SandboxPolicy::from_file(&policy_path).unwrap();
    assert_eq!(policy.sandbox.name, "file-loaded");
    assert_eq!(policy.sandbox.max_timeout_ms, 15000);
    assert!(policy.is_command_allowed("echo"));
    assert!(!policy.is_command_allowed("ls"));
}

#[tokio::test]
async fn multiple_requests_on_same_connection() {
    let tmp = TempDir::new().unwrap();
    let socket_path = tmp.path().join("test.sock");

    let listener = UnixListener::bind(&socket_path).unwrap();
    let policy = test_policy();

    let server = tokio::spawn({
        let policy = policy.clone();
        async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = stream.into_split();

            // Handle two requests
            for _ in 0..2 {
                let req: ExecuteCommandRequest = read_frame(&mut reader).await.unwrap().unwrap();
                let resp = executor::execute(&policy, req).await;
                write_frame(&mut writer, &resp).await.unwrap();
            }
        }
    });

    let stream = UnixStream::connect(&socket_path).await.unwrap();
    let (mut reader, mut writer) = stream.into_split();

    // First request
    let req1 = ExecuteCommandRequest {
        request_id: "multi-1".into(),
        command: "echo".into(),
        args: vec!["first".into()],
        cwd: None,
        env: HashMap::new(),
        stdin: None,
        timeout_ms: Some(5000),
    };
    write_frame(&mut writer, &req1).await.unwrap();
    let resp1: ExecuteCommandResponse = read_frame(&mut reader).await.unwrap().unwrap();
    assert_eq!(resp1.request_id, "multi-1");
    assert_eq!(resp1.status, ExecutionStatus::Success);

    // Second request
    let req2 = ExecuteCommandRequest {
        request_id: "multi-2".into(),
        command: "echo".into(),
        args: vec!["second".into()],
        cwd: None,
        env: HashMap::new(),
        stdin: None,
        timeout_ms: Some(5000),
    };
    write_frame(&mut writer, &req2).await.unwrap();
    let resp2: ExecuteCommandResponse = read_frame(&mut reader).await.unwrap().unwrap();
    assert_eq!(resp2.request_id, "multi-2");
    assert_eq!(resp2.status, ExecutionStatus::Success);

    server.await.unwrap();
}
