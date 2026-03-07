use anyhow::Result;
use clap::Parser;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::{json, Value};
use std::time::Duration;
use tracing::{info, warn, error};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Port of the local Ansible daemon Hotel Manager (IPC port)
    #[arg(short, long, default_value_t = 9000)]
    ansible_port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let _args = Args::parse();

    info!("Starting Materialized Hegemon (Telegram Gateway) Guest Process...");

    let identity = GuestIdentity {
        guest_id: "hegemon-telegram-01".into(),
        role: "hegemon".into(),
    };

    let mut ipc_client = PhiloticClient::connect(identity).await?;

    // Pull configuration from the Hotel Graph dynamically
    info!("Requesting Telegram Configuration from Ansible Context Graph...");
    let config_req = IpcRequest::GetConfig { key: "telegram_bot_token".into() };
    
    let bot_token = match ipc_client.send_request(config_req).await? {
        IpcResponse::ConfigData { key: _, value_json } => {
            if let Some(json_str) = value_json {
                if let Ok(val) = serde_json::from_str::<Value>(&json_str) {
                    val.as_str().unwrap_or("").to_string()
                } else {
                    json_str // fallback if it was stored as raw string
                }
            } else {
                warn!("Telegram Bot Token key found, but value was empty in Context Graph. Using Dummy Token.");
                "dummy_token".to_string()
            }
        }
        _ => {
            warn!("Failed to retrieve Telegram Bot Token from Context Graph. Using Dummy Token.");
            "dummy_token".to_string()
        }
    };
    
    if bot_token.is_empty() || bot_token == "dummy_token" {
        warn!("No valid Telegram Bot Token found. Polling will fail until configured.");
    }

    // Boot the reqwest client for Telegram API
    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
        
    let tg_base = format!("https://api.telegram.org/bot{}/", bot_token);
    let mut offset: i64 = 0;

    info!("Starting Telegram long-polling loop...");
    
    // Main Long-Polling Loop
    loop {
        let url = format!("{}getUpdates", tg_base);
        let params = [
            ("offset", offset.to_string()),
            ("timeout", "30".to_string()),
            ("allowed_updates", "[\"message\"]".to_string()),
        ];
        
        tokio::select! {
            // Branch 1: Wait for Telegram Updates (Long Polling)
            http_result = http_client.get(&url).query(&params).send() => {
                match http_result {
                    Ok(res) => {
                        if let Ok(json) = res.json::<Value>().await {
                            if let Some(result) = json.get("result").and_then(|r| r.as_array()) {
                                for update in result {
                                    if let Some(update_id) = update.get("update_id").and_then(|id| id.as_i64()) {
                                        offset = update_id + 1; // Ack the message
                                        
                                        if let Some(message) = update.get("message") {
                                            if let Some(text) = message.get("text").and_then(|t| t.as_str()) {
                                                let chat_id = message.get("chat").and_then(|c| c.get("id")).map(|id| id.to_string()).unwrap_or_default();
                                                let turn_id = format!("telegram-update-{}", update_id);
                                                let session_id = format!("telegram:{}:agent-jane-01", chat_id);
                                                
                                                info!("Received Telegram Message from Chat [{}]: {}", chat_id, text);
                                                
                                                // 3. Route inbound message over Philotic Web IPC
                                                let task_req = IpcRequest::EmitTask {
                                                    target_node: "local-ansible-01".into(),
                                                    target_role: "agent".into(),
                                                    task_json: json!({
                                                        "source": "telegram",
                                                        "session_id": session_id,
                                                        "turn_id": turn_id,
                                                        "chat_id": chat_id,
                                                        "content": text,
                                                        "final_reply_to": "local-ansible-01",
                                                        "final_reply_role": "hegemon"
                                                    }).to_string(),
                                                };
                                                
                                                match ipc_client.send_request(task_req).await {
                                                    Ok(_) => info!("Routed message to Ansible successfully."),
                                                    Err(e) => error!("Failed to route message to Ansible: {}", e),
                                                }
                                            }
                                        }
                                    }
                                }
                            } else if let Some(desc) = json.get("description").and_then(|d| d.as_str()) {
                                error!("Telegram API Error: {}", desc);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Telegram Long Polling failed: {}. Retrying in 5s...", e);
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }
            
            // Branch 2: Wait for outbound IPC tasks (LLM Responses)
            ipc_result = ipc_client.recv_task() => {
                match ipc_result {
                    Ok(IpcResponse::InboundTask { source_node, task_id, task_json }) => {
                        info!("Hegemon received final response [{}] from Mesh Node [{}]", task_id, source_node);
                        
                        if let Ok(task) = serde_json::from_str::<Value>(&task_json) {
                            if let Some(content) = task.get("content").and_then(|c| c.as_str()) {
                                let chat_id = task.get("chat_id").and_then(|id| id.as_str()).unwrap_or_default();
                                
                                if !chat_id.is_empty() {
                                    let send_url = format!("{}sendMessage", tg_base);
                                    let payload = json!({
                                        "chat_id": chat_id,
                                        "text": content
                                    });
                                    
                                    info!("Sending final response back to Telegram Chat [{}]...", chat_id);
                                    let http_client_clone = http_client.clone();
                                    
                                    tokio::spawn(async move {
                                        match http_client_clone.post(&send_url).json(&payload).send().await {
                                            Ok(_) => info!("Telegram Response Sent Successfully!"),
                                            Err(e) => error!("Failed to send Telegram Response: {}", e),
                                        }
                                    });
                                } else {
                                    warn!("Received a model response but 'chat_id' was missing. Cannot route to Telegram.");
                                }
                            }
                        }
                    }
                    Ok(other) => info!("Hegemon received non-task IPC message: {:?}", other),
                    Err(e) => warn!("IPC Recv error: {}", e),
                }
            }
        }
    }
}
