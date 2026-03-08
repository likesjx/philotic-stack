use anyhow::Result;
use clap::Parser;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::json;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {}

fn resolve_workspace_root(workspace_ref: Option<&str>) -> PathBuf {
    let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(workspace_ref) = workspace_ref
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return current;
    };

    if let Some(path) = workspace_ref.strip_prefix("file://") {
        return PathBuf::from(path);
    }

    if workspace_ref.starts_with("workspace://") {
        return current;
    }

    PathBuf::from(workspace_ref)
}

fn resolve_workspace_path(root: &Path, requested: &str) -> Result<PathBuf, String> {
    let requested_path = Path::new(requested.trim());
    if requested_path.as_os_str().is_empty() {
        return Ok(root.to_path_buf());
    }

    if requested_path.is_absolute() {
        return Err("absolute paths are not allowed for workspace tools".into());
    }

    if requested_path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("path traversal is not allowed for workspace tools".into());
    }

    Ok(root.join(requested_path))
}

fn execute_tool(
    tool_name: &str,
    arguments: &serde_json::Value,
    workspace_ref: Option<&str>,
) -> String {
    match tool_name {
        "echo" => arguments
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        "workspace.list" => {
            let root = resolve_workspace_root(workspace_ref);
            let requested = arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(".");
            let path = match resolve_workspace_path(&root, requested) {
                Ok(path) => path,
                Err(err) => return format!("workspace.list error: {err}"),
            };
            let mut entries = match fs::read_dir(&path) {
                Ok(entries) => entries
                    .filter_map(|entry| entry.ok())
                    .map(|entry| {
                        let file_name = entry.file_name().to_string_lossy().to_string();
                        if entry.path().is_dir() {
                            format!("{file_name}/")
                        } else {
                            file_name
                        }
                    })
                    .collect::<Vec<_>>(),
                Err(err) => return format!("workspace.list error: {err}"),
            };
            entries.sort();
            entries.join("\n")
        }
        "workspace.read" => {
            let root = resolve_workspace_root(workspace_ref);
            let Some(requested) = arguments.get("path").and_then(serde_json::Value::as_str) else {
                return "workspace.read error: missing `path`".into();
            };
            let path = match resolve_workspace_path(&root, requested) {
                Ok(path) => path,
                Err(err) => return format!("workspace.read error: {err}"),
            };
            match fs::read_to_string(&path) {
                Ok(contents) => contents,
                Err(err) => format!("workspace.read error: {err}"),
            }
        }
        _ => "unsupported tool".into(),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let _args = Args::parse();

    let identity = GuestIdentity {
        guest_id: "tool-runner-01".into(),
        role: "tool".into(),
        supported_tools: vec![
            "echo".into(),
            "workspace.list".into(),
            "workspace.read".into(),
        ],
    };
    let mut ipc_client = PhiloticClient::connect(identity).await?;
    let _ = ipc_client
        .send_request(IpcRequest::SubscribeInbox {
            role: "tool.echo".into(),
        })
        .await?;
    let _ = ipc_client
        .send_request(IpcRequest::SubscribeInbox {
            role: "tool.workspace.list".into(),
        })
        .await?;
    let _ = ipc_client
        .send_request(IpcRequest::SubscribeInbox {
            role: "tool.workspace.read".into(),
        })
        .await?;

    info!("Listening for tool execution tasks...");

    loop {
        match tokio::time::timeout(Duration::from_secs(5), ipc_client.recv_task()).await {
            Ok(Ok(IpcResponse::InboundTask { task_json, .. })) => {
                let Ok(task) = serde_json::from_str::<serde_json::Value>(&task_json) else {
                    warn!("Could not parse tool task payload");
                    continue;
                };

                if task.get("action").and_then(serde_json::Value::as_str) != Some("execute_tool") {
                    continue;
                }

                let tool_name = task
                    .get("tool_name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let arguments = task.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let session_id = task
                    .get("session_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let turn_id = task
                    .get("turn_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let chat_id = task
                    .get("chat_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let reply_to = task
                    .get("reply_to")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("local-ansible-01")
                    .to_string();
                let reply_role = task
                    .get("reply_role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("agent")
                    .to_string();
                let final_reply_to = task
                    .get("final_reply_to")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("local-ansible-01")
                    .to_string();
                let final_reply_role = task
                    .get("final_reply_role")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("hegemon")
                    .to_string();
                let workspace_ref = task
                    .get("workspace_ref")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);

                let result_content = execute_tool(&tool_name, &arguments, workspace_ref.as_deref());

                ipc_client
                    .send_request(IpcRequest::EmitTask {
                        target_node: reply_to,
                        target_role: reply_role,
                        task_json: json!({
                            "action": "tool_result",
                            "session_id": session_id,
                            "turn_id": turn_id,
                            "chat_id": chat_id,
                            "tool_name": tool_name,
                            "content": result_content,
                            "final_reply_to": final_reply_to,
                            "final_reply_role": final_reply_role
                        })
                        .to_string(),
                    })
                    .await?;
            }
            Ok(Ok(_)) => {}
            Ok(Err(err)) => warn!("IPC Recv error: {}", err),
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::execute_tool;
    use serde_json::json;
    use std::fs;

    #[test]
    fn workspace_list_returns_directory_entries() {
        let temp = tempfile::tempdir().expect("temp dir");
        fs::write(temp.path().join("alpha.txt"), "hello").expect("write file");
        fs::create_dir(temp.path().join("nested")).expect("mkdir");

        let output = execute_tool(
            "workspace.list",
            &json!({ "path": "." }),
            temp.path().to_str(),
        );
        assert!(output.contains("alpha.txt"));
        assert!(output.contains("nested/"));
    }

    #[test]
    fn workspace_read_returns_file_contents() {
        let temp = tempfile::tempdir().expect("temp dir");
        fs::write(temp.path().join("note.txt"), "remember this").expect("write file");

        let output = execute_tool(
            "workspace.read",
            &json!({ "path": "note.txt" }),
            temp.path().to_str(),
        );
        assert_eq!(output, "remember this");
    }

    #[test]
    fn workspace_tools_reject_parent_traversal() {
        let temp = tempfile::tempdir().expect("temp dir");
        let output = execute_tool(
            "workspace.read",
            &json!({ "path": "../secret.txt" }),
            temp.path().to_str(),
        );
        assert!(output.contains("path traversal"));
    }
}
