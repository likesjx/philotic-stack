mod commands;
mod crypto;
mod egress;
mod envelope;
mod gateway;
mod lease;
mod markdown;
mod session;
mod voice_gateway;
mod voice_udp;

use anyhow::{bail, Result};
use clap::Parser;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::Value;
use session::{ActiveTurn, ActiveTurns, GuildCache};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tracing::{debug, error, info, warn};

/// Discord Membrane — bridges Discord text/voice into the Philotic hotel.
#[derive(Debug, Parser)]
#[command(name = "membrane-discord", version)]
struct Args {
    /// Agent ID this membrane instance serves.
    #[arg(long, env = "PHILOTIC_TARGET_AGENT_ID", default_value = "agent-01")]
    agent_id: String,

    /// Guest ID for this membrane instance.
    #[arg(long, env = "PHILOTIC_GUEST_ID", default_value = "membrane-discord-01")]
    guest_id: String,

    /// Hotel socket path.
    #[arg(
        long,
        env = "PHILOTIC_HOTEL_SOCKET",
        default_value = "/tmp/philotic-aiua.sock"
    )]
    hotel_socket: String,

    /// Discord bot token. Prefer secret: URI via hotel vault.
    #[arg(long, env = "DISCORD_BOT_TOKEN")]
    bot_token: Option<String>,

    /// Config key to fetch bot token from hotel.
    #[arg(
        long,
        env = "DISCORD_BOT_TOKEN_KEY",
        default_value = "discord_bot_token"
    )]
    bot_token_key: String,

    /// Discord application ID (required for slash command registration).
    #[arg(long, env = "DISCORD_APPLICATION_ID")]
    application_id: Option<String>,

    /// Node ID of this hotel.
    #[arg(long, env = "PHILOTIC_NODE_ID", default_value = "local-aiua-01")]
    node_id: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    info!("membrane-discord starting for agent [{}]", args.agent_id);

    // Connect to hotel IPC
    let identity = GuestIdentity {
        guest_id: args.guest_id.clone(),
        role: "membrane".to_string(),
        supported_tools: vec![],
    };

    let mut ipc = PhiloticClient::connect(identity).await?;
    info!("Connected to hotel IPC at [{}]", args.hotel_socket);

    // Resolve bot token
    let bot_token = resolve_bot_token(&mut ipc, &args).await?;
    info!("Discord bot token resolved");

    // Acquire gateway lease (prevents split-brain)
    let mut gateway_lease =
        lease::DiscordGatewayLease::acquire(&mut ipc, &bot_token, &args.agent_id).await?;
    info!("Discord gateway lease acquired");

    // Subscribe to membrane inbox for inbound replies from the agent
    ipc.send_request(IpcRequest::SubscribeInbox {
        role: "membrane".to_string(),
    })
    .await?;

    // Register slash commands if application_id is configured
    if let Some(ref app_id) = args.application_id {
        let http = reqwest::Client::new();
        if let Err(e) = commands::register_global_commands(&http, &bot_token, app_id).await {
            warn!("Slash command registration failed (non-fatal): {}", e);
        }
    }

    // Start the gateway in a background task
    let (event_tx, mut event_rx) = mpsc::channel::<gateway::GatewayEvent>(256);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();
    let token_clone = bot_token.clone();
    tokio::spawn(async move {
        gateway::run_gateway(token_clone, event_tx, shutdown_rx).await;
    });

    // State
    let mut bot_user_id = String::new();
    let mut active_turns: ActiveTurns = HashMap::new();
    let mut guild_cache = GuildCache::default();
    let http = reqwest::Client::new();
    let mut lease_renew_tick = interval(Duration::from_secs(
        lease::DiscordGatewayLease::renew_interval_secs(),
    ));
    lease_renew_tick.tick().await; // consume immediate tick

    // Ctrl-C shutdown
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    info!("membrane-discord seat loop started for agent [{}]", args.agent_id);

    loop {
        tokio::select! {
            // Periodic lease renewal
            _ = lease_renew_tick.tick() => {
                if let Err(e) = gateway_lease.renew(&mut ipc).await {
                    error!("Discord gateway lease lost: {} — shutting down", e);
                    break;
                }
            }

            // Inbound gateway event
            event = event_rx.recv() => {
                let Some(event) = event else {
                    warn!("Gateway event channel closed — exiting");
                    break;
                };

                match event {
                    gateway::GatewayEvent::Ready {
                        bot_user_id: uid,
                        bot_username,
                        ..
                    } => {
                        bot_user_id = uid;
                        info!("Bot ready: {} [{}]", bot_username, bot_user_id);
                    }

                    gateway::GatewayEvent::GuildCreate(guild) => {
                        guild_cache.on_guild_create(&guild);
                    }

                    gateway::GatewayEvent::MessageCreate(msg) => {
                        if let Some(envelope) = envelope::normalize_message(
                            &msg,
                            &bot_user_id,
                            &args.agent_id,
                        ) {
                            let channel_id = envelope.chat_id.clone();
                            let msg_id = msg["id"].as_str().unwrap_or("").to_string();

                            // Show typing indicator
                            egress::send_typing(&http, &bot_token, &channel_id).await;

                            // Store active turn
                            active_turns.insert(
                                envelope.session_id.clone(),
                                ActiveTurn {
                                    session_id: envelope.session_id.clone(),
                                    channel_id: channel_id.clone(),
                                    guild_id: msg["guild_id"]
                                        .as_str()
                                        .unwrap_or("dm")
                                        .to_string(),
                                    draft_message_id: None,
                                    trigger_message_id: Some(msg_id.clone()),
                                    reply_channel_id: channel_id.clone(),
                                },
                            );

                            // Emit task to philote
                            let task_json = envelope.to_task_json(
                                &args.node_id,
                                "membrane",
                                &args.guest_id,
                            );
                            if let Err(e) = ipc.send_request(IpcRequest::EmitTask {
                                target_node: args.node_id.clone(),
                                target_role: "agent".to_string(),
                                target_guest_id: None,
                                task_json: task_json.to_string(),
                            }).await {
                                error!("Failed to emit task for message: {}", e);
                            }
                        }
                    }

                    gateway::GatewayEvent::InteractionCreate(interaction) => {
                        if let Some(envelope) = envelope::normalize_interaction(
                            &interaction,
                            &bot_user_id,
                            &args.agent_id,
                        ) {
                            let channel_id = envelope.chat_id.clone();
                            let interaction_id = interaction["id"].as_str().unwrap_or("").to_string();
                            let interaction_token = interaction["token"].as_str().unwrap_or("").to_string();

                            // Check for membrane-local commands
                            if let Some(cmd) = &envelope.command {
                                if let Some(local_reply) = commands::handle_local_command(cmd) {
                                    let _ = egress::respond_to_interaction(
                                        &http,
                                        &bot_token,
                                        &interaction_id,
                                        &interaction_token,
                                        &local_reply,
                                    ).await;
                                    continue;
                                }
                            }

                            // Acknowledge the interaction immediately (required within 3s)
                            // then forward to agent
                            let _ = egress::respond_to_interaction(
                                &http,
                                &bot_token,
                                &interaction_id,
                                &interaction_token,
                                "Working on it...",
                            ).await;

                            active_turns.insert(
                                envelope.session_id.clone(),
                                ActiveTurn {
                                    session_id: envelope.session_id.clone(),
                                    channel_id: channel_id.clone(),
                                    guild_id: interaction["guild_id"]
                                        .as_str()
                                        .unwrap_or("dm")
                                        .to_string(),
                                    draft_message_id: None,
                                    trigger_message_id: None,
                                    reply_channel_id: channel_id.clone(),
                                },
                            );

                            let task_json = envelope.to_task_json(
                                &args.node_id,
                                "membrane",
                                &args.guest_id,
                            );
                            if let Err(e) = ipc.send_request(IpcRequest::EmitTask {
                                target_node: args.node_id.clone(),
                                target_role: "agent".to_string(),
                                target_guest_id: None,
                                task_json: task_json.to_string(),
                            }).await {
                                error!("Failed to emit interaction task: {}", e);
                            }
                        }
                    }

                    gateway::GatewayEvent::VoiceStateUpdate(_d) => {
                        // Slice 1: voice lifecycle — deferred
                        debug!("VoiceStateUpdate received (voice not yet implemented)");
                    }

                    gateway::GatewayEvent::VoiceServerUpdate(_d) => {
                        // Slice 1: voice server session setup — deferred
                        debug!("VoiceServerUpdate received (voice not yet implemented)");
                    }

                    gateway::GatewayEvent::Unknown { t, .. } => {
                        debug!("Unhandled gateway event: {}", t);
                    }
                }
            }

            // Inbound task from agent (reply delivery)
            task = ipc.recv_task() => {
                match task {
                    Ok(IpcResponse::InboundTask {
                        source_node: _,
                        task_id: _,
                        task_json,
                    }) => {
                        if let Err(e) = handle_agent_reply(
                            &task_json,
                            &mut active_turns,
                            &http,
                            &bot_token,
                        ).await {
                            error!("Failed to handle agent reply: {}", e);
                        }
                    }
                    Ok(other) => {
                        warn!("Unexpected IPC response in recv_task: {:?}", other);
                    }
                    Err(e) => {
                        error!("IPC recv_task error: {} — connection may be closed", e);
                        break;
                    }
                }
            }

            // Ctrl-C
            _ = &mut ctrl_c => {
                info!("Ctrl-C received — shutting down membrane-discord");
                break;
            }
        }
    }

    // Clean shutdown
    info!("membrane-discord shutting down for agent [{}]", args.agent_id);
    let _ = shutdown_tx.send(());
    if let Err(e) = gateway_lease.release(&mut ipc).await {
        warn!("Failed to release Discord gateway lease: {}", e);
    }

    Ok(())
}

async fn resolve_bot_token(ipc: &mut PhiloticClient, args: &Args) -> Result<String> {
    // Direct env var takes priority
    if let Some(ref token) = args.bot_token {
        return Ok(token.clone());
    }

    // Try hotel config graph
    let response = ipc
        .send_request(IpcRequest::GetConfig {
            key: args.bot_token_key.clone(),
        })
        .await?;

    if let IpcResponse::ConfigData {
        value_json: Some(v),
        ..
    } = response
    {
        let token: String = serde_json::from_str(&v)
            .or_else(|_| -> Result<String> { Ok(v.trim_matches('"').to_string()) })?;
        if !token.is_empty() {
            return Ok(token);
        }
    }

    bail!(
        "Discord bot token not found. Set DISCORD_BOT_TOKEN or configure [{}] in the hotel.",
        args.bot_token_key
    )
}

async fn handle_agent_reply(
    task_json: &str,
    active_turns: &mut ActiveTurns,
    http: &reqwest::Client,
    bot_token: &str,
) -> Result<()> {
    let task: Value = serde_json::from_str(task_json)?;

    let action = task["action"].as_str().unwrap_or("");
    let session_id = task["session_id"].as_str().unwrap_or("");
    let content = task["content"].as_str().unwrap_or("");
    let chat_id = task["chat_id"].as_str().unwrap_or("");

    let reply_channel = active_turns
        .get(session_id)
        .map(|t| t.reply_channel_id.clone())
        .unwrap_or_else(|| chat_id.to_string());

    let trigger_msg_id = active_turns
        .get(session_id)
        .and_then(|t| t.trigger_message_id.as_deref())
        .map(String::from);

    match action {
        "partial_reply" => {
            // Progressive edit of draft message
            let turn = active_turns.get_mut(session_id);
            if let Some(turn) = turn {
                if let Some(ref draft_id) = turn.draft_message_id.clone() {
                    let _ = egress::edit_message(http, bot_token, &reply_channel, draft_id, content).await;
                } else {
                    // Send first draft
                    match egress::send_text_reply(
                        http,
                        bot_token,
                        &reply_channel,
                        content,
                        trigger_msg_id.as_deref(),
                    )
                    .await
                    {
                        Ok(msg) => {
                            turn.draft_message_id =
                                msg["id"].as_str().map(String::from);
                        }
                        Err(e) => warn!("Failed to send partial reply: {}", e),
                    }
                }
            }
        }

        "send_reply" => {
            // Final reply — send fresh message (don't reuse draft to avoid edit confusion)
            egress::send_text_reply(
                http,
                bot_token,
                &reply_channel,
                content,
                trigger_msg_id.as_deref(),
            )
            .await?;

            // Clean up active turn
            active_turns.remove(session_id);
        }

        "turn_event" => {
            // Events like waiting_tool, waiting_model — show typing to keep Discord engaged
            egress::send_typing(http, bot_token, &reply_channel).await;
        }

        _ => {
            warn!("Unknown agent reply action [{}] for session [{}]", action, session_id);
        }
    }

    Ok(())
}

