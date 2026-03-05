use anyhow::{Context, Result};
use clap::Parser;
use philotic_ipc::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use philotic_ipc::udp::UdpPhiloticClient;
use serde_json::{json, Value};
use std::net::SocketAddr;
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
    let args = Args::parse();

    info!("Starting Materialized Mind (Model Router) Guest Process...");

    let ansible_addr = format!("127.0.0.1:{}", args.ansible_port).parse::<SocketAddr>()?;
    let mut ipc_client = UdpPhiloticClient::new(ansible_addr).await?;

    ipc_client.connect().await?;

    let identity = GuestIdentity {
        guest_id: "model-router-gemini-01".into(),
        role: "model".into(),
    };

    info!("Registering as Materialized Guest: {:?}", identity);
    let resp = ipc_client.send_request(IpcRequest::Register(identity)).await?;
    info!("Ansible Hotel Response: {:?}", resp);

    // Pull configuration from the Hotel Graph dynamically
    info!("Requesting Gemini API Key from Ansible Context Graph...");
    let config_req = IpcRequest::GetConfig { key: "gemini_api_key".into() };
    
    let api_key = match ipc_client.send_request(config_req).await? {
        IpcResponse::ConfigData { key: _, value_json } => {
            if let Some(json_str) = value_json {
                if let Ok(val) = serde_json::from_str::<Value>(&json_str) {
                    val.as_str().unwrap_or("").to_string()
                } else {
                    json_str // fallback
                }
            } else {
                warn!("Gemini API key found, but value was empty in Context Graph.");
                "dummy_key".to_string()
            }
        }
        _ => {
            warn!("Failed to retrieve Gemini API Key from Context Graph.");
            "dummy_key".to_string()
        }
    };
    
    if api_key.is_empty() || api_key == "dummy_key" {
        warn!("No valid Gemini API Key found. Inference will fail.");
    }

    let http_client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build()?;
    let gemini_url = format!("https://generativelanguage.googleapis.com/v1beta/models/gemini-flash-latest:generateContent?key={}", api_key);

    info!("Listening for inbound Inference tasks from the Philotic Web...");
    
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ipc_client.recv_task()).await {
            Ok(Ok(IpcResponse::InboundTask { source_node, task_id, task_json })) => {
                info!("Model Router received task [{}] from [{}]", task_id, source_node);
                
                if let Ok(task) = serde_json::from_str::<Value>(&task_json) {
                    if let Some(prompt) = task.get("prompt").and_then(|c| c.as_str()) {
                        let reply_to = task.get("reply_to").and_then(|r| r.as_str()).unwrap_or("hegemon").to_string();
                        let chat_id = task.get("chat_id").and_then(|id| id.as_str()).unwrap_or_default().to_string();
                        
                        info!("Model Router executing inference for prompt snippet: {}...", &prompt.chars().take(50).collect::<String>());
                        
                        // Fake making the HTTP request to start (can build real one next)
                        let payload = json!({
                            "contents": [{"parts":[{"text": prompt}]}]
                        });
                        
                        match http_client.post(&gemini_url).json(&payload).send().await {
                            Ok(res) => {
                                let status = res.status();
                                if let Ok(json_res) = res.json::<Value>().await {
                                    let mut response_text = "I am Jane, the Materialized AI. This is a stub response.".to_string();
                                    
                                    if !status.is_success() {
                                        response_text = format!("Gemini API Error: HTTP {}", status.as_u16());
                                        if let Some(error_obj) = json_res.get("error") {
                                            if let Some(msg) = error_obj.get("message").and_then(|m| m.as_str()) {
                                                response_text = format!("Gemini API Error ({}): {}", status.as_u16(), msg);
                                            }
                                        } else {
                                            response_text = format!("Gemini API Error ({}): {:?}", status.as_u16(), json_res);
                                        }
                                        error!("{}", response_text);
                                    } else {
                                        if let Some(candidates) = json_res.get("candidates").and_then(|c| c.as_array()) {
                                            if let Some(first) = candidates.first() {
                                                if let Some(content) = first.get("content").and_then(|c| c.get("parts")).and_then(|p| p.as_array()) {
                                                    if let Some(part) = content.first() {
                                                        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                                                            response_text = text.to_string();
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    
                                    // 5. Route the final answer back to Hegemon over IPC!
                                    let reply_req = IpcRequest::EmitTask {
                                        target_node: reply_to.clone(),
                                        target_role: "hegemon".into(),
                                        task_json: json!({
                                            "action": "send_reply",
                                            "chat_id": chat_id,
                                            "content": response_text
                                        }).to_string(),
                                    };
                                    
                                    info!("Routing inference answer back to Gateway [{}]", reply_to);
                                    let _ = ipc_client.send_request(reply_req).await;
                                }
                            }
                            Err(e) => error!("Gemini API Call Failed: {}", e),
                        }
                    }
                }
            }
            Ok(Ok(other)) => {
                info!("Model Router received non-task IPC message: {:?}", other);
            }
            Ok(Err(e)) => warn!("IPC Recv error: {}", e),
            Err(_) => { /* timeout, loop */ }
        }
    }
}
