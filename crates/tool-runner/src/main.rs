use anyhow::Result;
use clap::Parser;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::json;
use std::time::Duration;
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let _args = Args::parse();

    let identity = GuestIdentity {
        guest_id: "tool-runner-01".into(),
        role: "tool".into(),
        supported_tools: vec!["echo".into()],
    };
    let mut ipc_client = PhiloticClient::connect(identity).await?;
    let _ = ipc_client
        .send_request(IpcRequest::SubscribeInbox {
            role: "tool.echo".into(),
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

                if task.get("action").and_then(serde_json::Value::as_str) != Some("execute_tool")
                {
                    continue;
                }

                let tool_name = task
                    .get("tool_name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let arguments = task
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
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

                let result_content = match tool_name.as_str() {
                    "echo" => arguments
                        .get("text")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    _ => "unsupported tool".into(),
                };

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
