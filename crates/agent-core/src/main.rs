use anyhow::Result;
use clap::Parser;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::{json, Value};
use std::time::Duration;
use tracing::{info, warn, error};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value_t = 9000)]
    ansible_port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let _args = Args::parse();

    info!("Starting Materialized Persona (Agent Core) Guest Process...");

    let identity = GuestIdentity {
        guest_id: "agent-jane-01".into(),
        role: "agent".into(),
    };

    let mut ipc_client = PhiloticClient::connect(identity).await?;

    info!("Listening for inbound Persona tasks from the Philotic Web...");
    
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ipc_client.recv_task()).await {
            Ok(Ok(IpcResponse::InboundTask { source_node, task_id, task_json })) => {
                info!("Jane received task [{}] from [{}]", task_id, source_node);
                
                if let Ok(task) = serde_json::from_str::<Value>(&task_json) {
                    if let Some(content) = task.get("content").and_then(|c| c.as_str()) {
                        let chat_id = task.get("chat_id").and_then(|id| id.as_str()).unwrap_or_default();
                        
                        info!("Jane is thinking about: '{}' for Chat [{}]", content, chat_id);
                        
                        // Fake retrieving Context / Building Prompt
                        let system_prompt = "You are Jane, a hyper-intelligent Hegemon AI. The user says: ";
                        let full_prompt = format!("{} {}", system_prompt, content);
                        
                        // 4. Route Task to the Model Router & Pass Chat ID along
                        let inference_req = IpcRequest::EmitTask {
                            target_node: "local-ansible-01".into(),
                            target_role: "model".into(),
                            task_json: json!({
                                "action": "generate_text",
                                "prompt": full_prompt,
                                "chat_id": chat_id,
                                "reply_to": source_node // Route response back to the node that asked (hegemon)
                            }).to_string(),
                        };
                        
                        info!("Asking the Hotel to route inference to the Model Router...");
                        match ipc_client.send_request(inference_req).await {
                            Ok(_) => info!("Inference task routed!"),
                            Err(e) => error!("Failed to route model task: {}", e),
                        }
                    }
                }
            }
            Ok(Ok(other)) => {
                info!("Jane received non-task IPC message: {:?}", other);
            }
            Ok(Err(e)) => warn!("IPC Recv error: {}", e),
            Err(_) => { /* timeout, loop */ }
        }
    }
}
