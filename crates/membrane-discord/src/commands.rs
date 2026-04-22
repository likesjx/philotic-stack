use anyhow::Result;
use reqwest::Client;
use serde_json::{Value, json};
use tracing::info;

const DISCORD_API_BASE: &str = "https://discord.com/api/v10";

/// Register global slash commands for the bot application.
/// These are the membrane-native commands; agent-specific commands come from
/// the command manifest fetched from the hotel.
pub async fn register_global_commands(
    http: &Client,
    bot_token: &str,
    application_id: &str,
) -> Result<()> {
    let commands = membrane_native_commands();

    let resp = http
        .put(format!(
            "{}/applications/{}/commands",
            DISCORD_API_BASE, application_id
        ))
        .header("Authorization", format!("Bot {}", bot_token))
        .json(&commands)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Failed to register slash commands: {} {}", status, body);
    }

    info!("Registered {} global slash commands", commands.len());
    Ok(())
}

/// Register commands scoped to a specific guild (faster for development).
pub async fn register_guild_commands(
    http: &Client,
    bot_token: &str,
    application_id: &str,
    guild_id: &str,
) -> Result<()> {
    let commands = membrane_native_commands();

    let resp = http
        .put(format!(
            "{}/applications/{}/guilds/{}/commands",
            DISCORD_API_BASE, application_id, guild_id
        ))
        .header("Authorization", format!("Bot {}", bot_token))
        .json(&commands)
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!(
            "Failed to register guild slash commands: {} {}",
            status,
            body
        );
    }

    info!(
        "Registered {} slash commands for guild [{}]",
        commands.len(),
        guild_id
    );
    Ok(())
}

fn membrane_native_commands() -> Vec<Value> {
    vec![
        json!({
            "name": "status",
            "description": "Show agent status",
            "type": 1
        }),
        json!({
            "name": "join",
            "description": "Join your current voice channel",
            "type": 1
        }),
        json!({
            "name": "leave",
            "description": "Leave the current voice channel",
            "type": 1
        }),
        json!({
            "name": "tts",
            "description": "Toggle voice response mode",
            "type": 1
        }),
        json!({
            "name": "new",
            "description": "Start a new conversation session",
            "type": 1
        }),
    ]
}

/// Handle membrane-local slash commands that don't need to be forwarded to the agent.
/// Returns Some(reply_text) if handled locally, None if the command should be forwarded.
pub fn handle_local_command(command: &str) -> Option<String> {
    match command {
        "status" => None, // Forward to agent for a real status response
        "new" => Some("Starting a new session...".to_string()),
        _ => None,
    }
}
