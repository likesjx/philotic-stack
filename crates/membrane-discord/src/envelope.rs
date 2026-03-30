use serde_json::{json, Value};

/// Normalized inbound transport envelope — same shape as the Telegram membrane uses,
/// extended with optional Discord-specific fields.
#[derive(Debug, Clone)]
pub struct DiscordIngressEnvelope {
    pub transport: &'static str, // always "discord"
    pub session_id: String,      // "discord:{guild_id}:{channel_id}"
    pub turn_id: String,         // message snowflake
    pub chat_id: String,         // channel_id
    pub thread_id: Option<String>,
    pub sender_id: Option<String>,
    pub sender_username: Option<String>,
    pub message_kind: &'static str, // "text" | "slash_command" | "reaction"
    pub content: String,
    pub attachments: Vec<Value>,
    pub command: Option<String>,       // slash command name (without /)
    pub command_options: Option<String>, // options serialized as string
    pub callback_data: Option<String>,
    pub raw_transport_event: Value,
}

impl DiscordIngressEnvelope {
    /// Convert to the task JSON shape expected by philote (matches Telegram membrane pattern).
    pub fn to_task_json(
        &self,
        final_reply_node: &str,
        final_reply_role: &str,
        final_reply_guest_id: &str,
    ) -> Value {
        json!({
            "source": "discord",
            "transport": "discord",
            "session_id": self.session_id,
            "turn_id": self.turn_id,
            "chat_id": self.chat_id,
            "thread_id": self.thread_id,
            "sender_id": self.sender_id,
            "sender_username": self.sender_username,
            "message_kind": self.message_kind,
            "content": self.content,
            "attachments": self.attachments,
            "command": self.command,
            "command_options": self.command_options,
            "callback_data": self.callback_data,
            "raw_transport_event": self.raw_transport_event,
            "final_reply_to": final_reply_node,
            "final_reply_role": final_reply_role,
            "final_reply_guest_id": final_reply_guest_id,
        })
    }
}

/// Build a session_id from guild + channel.
pub fn session_id(guild_id: &str, channel_id: &str) -> String {
    format!("discord:{}:{}", guild_id, channel_id)
}

/// Parse a Discord MESSAGE_CREATE event into a DiscordIngressEnvelope.
/// Returns None if the event is not a processable message (e.g. bot's own message).
pub fn normalize_message(
    event: &Value,
    bot_user_id: &str,
    _agent_id: &str,
) -> Option<DiscordIngressEnvelope> {
    let message = event;

    // Don't process our own messages
    let author_id = message["author"]["id"].as_str().unwrap_or("");
    if author_id == bot_user_id {
        return None;
    }

    let message_id = message["id"].as_str()?;
    let channel_id = message["channel_id"].as_str()?;
    let guild_id = message.get("guild_id").and_then(|v| v.as_str()).unwrap_or("dm");
    let content = message["content"].as_str().unwrap_or("").to_string();
    let sender_id = message["author"]["id"].as_str().map(String::from);
    let sender_username = message["author"]["username"].as_str().map(String::from);

    // Check for slash command (application commands arrive via INTERACTION_CREATE, not here,
    // but prefix commands like /status via MESSAGE_CREATE are handled here)
    let (command, message_kind, effective_content) = if content.starts_with('/') {
        let parts: Vec<&str> = content.splitn(2, ' ').collect();
        let cmd = parts[0].trim_start_matches('/').to_string();
        let rest = parts.get(1).copied().unwrap_or("").to_string();
        (Some(cmd), "slash_command", rest)
    } else {
        (None, "text", content.clone())
    };

    let attachments = extract_attachments(message);

    Some(DiscordIngressEnvelope {
        transport: "discord",
        session_id: session_id(guild_id, channel_id),
        turn_id: format!("discord-msg-{}", message_id),
        chat_id: channel_id.to_string(),
        thread_id: None,
        sender_id,
        sender_username,
        message_kind,
        content: effective_content,
        attachments,
        command,
        command_options: None,
        callback_data: None,
        raw_transport_event: event.clone(),
    })
}

/// Parse a Discord INTERACTION_CREATE (slash command) event.
pub fn normalize_interaction(
    event: &Value,
    _bot_user_id: &str,
    _agent_id: &str,
) -> Option<DiscordIngressEnvelope> {
    // Only handle APPLICATION_COMMAND type (type=2)
    if event["type"].as_u64() != Some(2) {
        return None;
    }

    let interaction_id = event["id"].as_str()?;
    let channel_id = event["channel_id"].as_str()?;
    let guild_id = event.get("guild_id").and_then(|v| v.as_str()).unwrap_or("dm");
    let data = event.get("data")?;
    let command_name = data["name"].as_str()?;
    let user = event.get("member")
        .and_then(|m| m.get("user"))
        .or_else(|| event.get("user"));
    let sender_id = user.and_then(|u| u["id"].as_str()).map(String::from);
    let sender_username = user.and_then(|u| u["username"].as_str()).map(String::from);

    // Serialize options as a simple string for the agent
    let options_str = data.get("options")
        .and_then(|o| serde_json::to_string(o).ok());

    // Extract a human-readable content string from options
    let content = options_str.as_deref().unwrap_or("").to_string();

    Some(DiscordIngressEnvelope {
        transport: "discord",
        session_id: session_id(guild_id, channel_id),
        turn_id: format!("discord-interaction-{}", interaction_id),
        chat_id: channel_id.to_string(),
        thread_id: None,
        sender_id,
        sender_username,
        message_kind: "slash_command",
        content,
        attachments: vec![],
        command: Some(command_name.to_string()),
        command_options: options_str,
        callback_data: None,
        raw_transport_event: event.clone(),
    })
}

fn extract_attachments(message: &Value) -> Vec<Value> {
    let mut attachments = vec![];

    if let Some(arr) = message["attachments"].as_array() {
        for att in arr {
            attachments.push(json!({
                "kind": "file",
                "url": att["url"],
                "filename": att["filename"],
                "content_type": att["content_type"],
                "size": att["size"],
            }));
        }
    }

    attachments
}
