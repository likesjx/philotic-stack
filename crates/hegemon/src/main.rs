use anyhow::Result;
use clap::Parser;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient, is_ipc_disconnect};
use serde_json::{Value, json};
use std::time::Duration;
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Port of the local Ansible daemon Hotel Manager (IPC port)
    #[arg(short, long, default_value_t = 9000)]
    ansible_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TelegramMessageEnvelope {
    session_id: String,
    turn_id: String,
    chat_id: String,
    thread_id: Option<String>,
    sender_id: Option<String>,
    sender_username: Option<String>,
    message_kind: &'static str,
    content: String,
    command: Option<String>,
    raw_transport_event: Value,
}

fn telegram_text_envelope(
    update: &Value,
    update_id: i64,
    agent_id: &str,
) -> Option<TelegramMessageEnvelope> {
    let message = update.get("message")?;
    let text = message.get("text")?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }

    let chat_id = message
        .get("chat")
        .and_then(|chat| chat.get("id"))
        .and_then(value_to_id_string)?;
    let thread_id = message
        .get("message_thread_id")
        .and_then(value_to_id_string)
        .filter(|id| !id.is_empty());
    let sender_id = message
        .get("from")
        .and_then(|from| from.get("id"))
        .and_then(value_to_id_string)
        .filter(|id| !id.is_empty());
    let sender_username = message
        .get("from")
        .and_then(|from| from.get("username"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|name| !name.is_empty());
    let session_id = match thread_id.as_deref() {
        Some(thread_id) => format!("telegram:{chat_id}:{thread_id}:{agent_id}"),
        None => format!("telegram:{chat_id}:{agent_id}"),
    };

    Some(TelegramMessageEnvelope {
        session_id,
        turn_id: format!("telegram-update-{update_id}"),
        chat_id,
        thread_id,
        sender_id,
        sender_username,
        message_kind: "text",
        content: text.to_string(),
        command: telegram_command(text),
        raw_transport_event: update.clone(),
    })
}

fn telegram_command(text: &str) -> Option<String> {
    text.split_whitespace()
        .next()
        .filter(|token| token.starts_with('/'))
        .map(str::to_string)
}

fn value_to_id_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let _args = Args::parse();

    info!("Starting Materialized Hegemon (Telegram Gateway) Guest Process...");

    let identity = GuestIdentity {
        guest_id: "hegemon-telegram-01".into(),
        role: "hegemon".into(),
        supported_tools: Vec::new(),
    };

    let mut ipc_client = PhiloticClient::connect(identity).await?;

    // Pull configuration from the Hotel Graph dynamically
    info!("Requesting Telegram Configuration from Ansible Context Graph...");
    let config_req = IpcRequest::GetConfig {
        key: "telegram_bot_token".into(),
    };

    let bot_token = match ipc_client.send_request(config_req).await? {
        IpcResponse::ConfigData { key: _, value_json } => {
            if let Some(json_str) = value_json {
                if let Ok(val) = serde_json::from_str::<Value>(&json_str) {
                    val.as_str().unwrap_or("").to_string()
                } else {
                    json_str // fallback if it was stored as raw string
                }
            } else {
                warn!(
                    "Telegram Bot Token key found, but value was empty in Context Graph. Using Dummy Token."
                );
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

                                        if let Some(envelope) = telegram_text_envelope(update, update_id, "agent-jane-01") {
                                            info!(
                                                "Received Telegram {} message from chat [{}]{}: {}",
                                                envelope.message_kind,
                                                envelope.chat_id,
                                                envelope
                                                    .thread_id
                                                    .as_deref()
                                                    .map(|thread| format!(" thread [{}]", thread))
                                                    .unwrap_or_default(),
                                                envelope.content
                                            );

                                            let task_req = IpcRequest::EmitTask {
                                                target_node: "local-ansible-01".into(),
                                                target_role: "agent".into(),
                                                target_guest_id: None,
                                                task_json: json!({
                                                    "source": "telegram",
                                                    "transport": "telegram",
                                                    "session_id": envelope.session_id,
                                                    "turn_id": envelope.turn_id,
                                                    "chat_id": envelope.chat_id,
                                                    "thread_id": envelope.thread_id,
                                                    "sender_id": envelope.sender_id,
                                                    "sender_username": envelope.sender_username,
                                                    "message_kind": envelope.message_kind,
                                                    "content": envelope.content,
                                                    "attachments": [],
                                                    "command": envelope.command,
                                                    "callback_data": Value::Null,
                                                    "raw_transport_event": envelope.raw_transport_event,
                                                    "final_reply_to": "local-ansible-01",
                                                    "final_reply_role": "hegemon",
                                                    "final_reply_guest_id": "hegemon-telegram-01"
                                                }).to_string(),
                                            };

                                            match ipc_client.send_request(task_req).await {
                                                Ok(_) => info!("Routed message to Ansible successfully."),
                                                Err(e) => error!("Failed to route message to Ansible: {}", e),
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
                    Err(e) => {
                        if is_ipc_disconnect(&e) {
                            info!("Hotel IPC disconnected; hegemon exiting.");
                            return Ok(());
                        }
                        warn!("IPC Recv error: {}", e);
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{telegram_command, telegram_text_envelope};
    use serde_json::json;

    #[test]
    fn telegram_text_envelope_normalizes_threaded_message() {
        let update = json!({
            "update_id": 99,
            "message": {
                "message_thread_id": 77,
                "text": "/status show me the room",
                "chat": { "id": -10012345 },
                "from": { "id": 888, "username": "jared" }
            }
        });

        let envelope = telegram_text_envelope(&update, 99, "agent-jane-01")
            .expect("text update should normalize");

        assert_eq!(envelope.session_id, "telegram:-10012345:77:agent-jane-01");
        assert_eq!(envelope.turn_id, "telegram-update-99");
        assert_eq!(envelope.chat_id, "-10012345");
        assert_eq!(envelope.thread_id.as_deref(), Some("77"));
        assert_eq!(envelope.sender_id.as_deref(), Some("888"));
        assert_eq!(envelope.sender_username.as_deref(), Some("jared"));
        assert_eq!(envelope.message_kind, "text");
        assert_eq!(envelope.content, "/status show me the room");
        assert_eq!(envelope.command.as_deref(), Some("/status"));
        assert_eq!(envelope.raw_transport_event, update);
    }

    #[test]
    fn telegram_command_returns_only_slash_token() {
        assert_eq!(
            telegram_command("/approve use staging"),
            Some("/approve".into())
        );
        assert_eq!(telegram_command("hello there"), None);
    }
}
