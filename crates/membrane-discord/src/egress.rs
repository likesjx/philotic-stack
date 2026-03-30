use anyhow::Result;
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{error, info};

use crate::markdown::{split_for_discord, to_discord_markdown};

const DISCORD_API_BASE: &str = "https://discord.com/api/v10";

/// Send a text reply to a Discord channel.
pub async fn send_text_reply(
    http: &Client,
    bot_token: &str,
    channel_id: &str,
    content: &str,
    reply_to_message_id: Option<&str>,
) -> Result<Value> {
    let discord_text = to_discord_markdown(content);
    let chunks = split_for_discord(&discord_text);
    let mut last_response = Value::Null;

    for (i, chunk) in chunks.iter().enumerate() {
        let mut body = json!({ "content": chunk });

        // Only add message_reference on the first chunk
        if i == 0 {
            if let Some(msg_id) = reply_to_message_id {
                body["message_reference"] = json!({
                    "message_id": msg_id,
                    "fail_if_not_exists": false,
                });
            }
        }

        let resp = http
            .post(format!("{}/channels/{}/messages", DISCORD_API_BASE, channel_id))
            .header("Authorization", format!("Bot {}", bot_token))
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body_text = resp.text().await.unwrap_or_default();
            error!("Discord send_message failed [{} {}]: {}", channel_id, status, body_text);
            anyhow::bail!("Discord API error {}: {}", status, body_text);
        }

        last_response = resp.json().await?;
        info!("Sent Discord message chunk {}/{} to channel [{}]", i + 1, chunks.len(), channel_id);
    }

    Ok(last_response)
}

/// Edit an existing Discord message (for streaming partial replies).
pub async fn edit_message(
    http: &Client,
    bot_token: &str,
    channel_id: &str,
    message_id: &str,
    content: &str,
) -> Result<()> {
    let discord_text = to_discord_markdown(content);

    // Truncate at 2000 chars for in-progress edits
    let truncated = if discord_text.len() > 1997 {
        format!("{}…", &discord_text[..1997])
    } else {
        discord_text
    };

    let resp = http
        .patch(format!(
            "{}/channels/{}/messages/{}",
            DISCORD_API_BASE, channel_id, message_id
        ))
        .header("Authorization", format!("Bot {}", bot_token))
        .json(&json!({ "content": truncated }))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        error!("Discord edit_message failed: {} {}", status, body_text);
        anyhow::bail!("Discord API error {}: {}", status, body_text);
    }

    Ok(())
}

/// Respond to a slash command interaction (required within 3 seconds).
pub async fn respond_to_interaction(
    http: &Client,
    bot_token: &str,
    interaction_id: &str,
    interaction_token: &str,
    content: &str,
) -> Result<()> {
    let discord_text = to_discord_markdown(content);
    let chunks = split_for_discord(&discord_text);
    let first = chunks.first().map(|s| s.as_str()).unwrap_or("");

    let resp = http
        .post(format!(
            "{}/interactions/{}/{}/callback",
            DISCORD_API_BASE, interaction_id, interaction_token
        ))
        .header("Authorization", format!("Bot {}", bot_token))
        .json(&json!({
            "type": 4,  // CHANNEL_MESSAGE_WITH_SOURCE
            "data": {
                "content": first
            }
        }))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        error!("Discord interaction response failed: {} {}", status, body_text);
        anyhow::bail!("Discord API error {}: {}", status, body_text);
    }

    Ok(())
}

/// Send a typing indicator to the channel.
pub async fn send_typing(http: &Client, bot_token: &str, channel_id: &str) {
    let _ = http
        .post(format!("{}/channels/{}/typing", DISCORD_API_BASE, channel_id))
        .header("Authorization", format!("Bot {}", bot_token))
        .send()
        .await;
}
