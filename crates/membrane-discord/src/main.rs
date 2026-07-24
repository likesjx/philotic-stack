mod commands;
mod crypto;
mod egress;
mod envelope;
mod gateway;
mod lease;
mod markdown;
mod session;
mod voice_bridge;
mod voice_gateway;
mod voice_udp;

use anyhow::{Result, anyhow, bail};
use clap::Parser;
use membrane::LeaseEvent;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient};
use serde_json::{Value, json};
use session::{ActiveTurn, ActiveTurns, GuildCache};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tokio::time::Duration;
use tracing::{debug, error, info, warn};
use voice_bridge::{VoiceBridge, VoiceUtteranceEvent};

const MEMBRANE_ERROR_BACKOFF_INITIAL_SECS: u64 = 1;
const MEMBRANE_ERROR_BACKOFF_MAX_SECS: u64 = 600;

fn next_error_backoff_secs(current_secs: u64) -> u64 {
    current_secs.saturating_mul(2).clamp(
        MEMBRANE_ERROR_BACKOFF_INITIAL_SECS,
        MEMBRANE_ERROR_BACKOFF_MAX_SECS,
    )
}

/// Discord Membrane — bridges Discord text/voice into the Philotic hotel.
#[derive(Clone, Debug, Parser)]
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

/// Pending voice connection — waiting for both VoiceStateUpdate and VoiceServerUpdate
/// before we can connect the voice gateway.
struct PendingVoiceConnect {
    guild_id: String,
    channel_id: String,
    /// session_id from VOICE_STATE_UPDATE (needed for voice gateway Identify).
    voice_session_id: Option<String>,
    /// endpoint from VOICE_SERVER_UPDATE.
    endpoint: Option<String>,
    /// token from VOICE_SERVER_UPDATE.
    token: Option<String>,
    /// channel where /join was invoked — replies go here.
    text_channel_id: String,
}

impl PendingVoiceConnect {
    fn is_ready(&self) -> bool {
        self.voice_session_id.is_some() && self.endpoint.is_some() && self.token.is_some()
    }
}

async fn run_once(args: Args) -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .ok();
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

    // Voice: shared utterance channel (all bridges → main loop)
    let (utterance_tx, mut utterance_rx) = mpsc::channel::<VoiceUtteranceEvent>(64);

    // State
    let mut bot_user_id = String::new();
    let mut active_turns: ActiveTurns = HashMap::new();
    let mut guild_cache = GuildCache::default();
    // Pending voice connections keyed by guild_id.
    let mut pending_voice: HashMap<String, PendingVoiceConnect> = HashMap::new();
    // Active voice bridges keyed by guild_id.
    let mut active_bridges: HashMap<String, VoiceBridge> = HashMap::new();

    let http = reqwest::Client::new();

    // Ctrl-C shutdown
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    info!(
        "membrane-discord seat loop started for agent [{}]",
        args.agent_id
    );

    loop {
        tokio::select! {
            // Lease lifecycle — the shared LeaseDriver schedules renewals,
            // re-acquires in place after lapses/errors, and backs off on IPC
            // failures. Only losing the lease to ANOTHER holder ends the seat;
            // everything else is survivable and already scheduled for retry.
            _ = lease_deadline(&gateway_lease) => {
                match gateway_lease.tick(&mut ipc).await {
                    LeaseEvent::Lost { owner } => {
                        return Err(anyhow!(
                            "Discord gateway lease lost to [{}] — yielding seat",
                            owner.as_deref().unwrap_or("unknown")
                        ));
                    }
                    LeaseEvent::Reacquired { epoch } => {
                        info!("Discord gateway lease re-acquired in place — epoch {}", epoch);
                    }
                    LeaseEvent::BackingOff { retry_in, error } => {
                        warn!(
                            "Discord gateway lease attempt failed ({}); retrying in {:?}",
                            error, retry_in
                        );
                    }
                    LeaseEvent::Renewed { epoch } => {
                        debug!("Discord gateway lease renewed — epoch {}", epoch);
                    }
                    _ => {}
                }
            }

            // Voice utterance from a bridge — emit to hotel as voice.dialogue task
            utterance = utterance_rx.recv() => {
                if let Some(ev) = utterance {
                    let session_id = ev.session_id.clone();

                    // Look up the text channel for this voice session so the reply lands
                    // in the right Discord text channel.
                    let text_channel_id = active_bridges
                        .get(&ev.guild_id)
                        .map(|b| b.text_channel_id.clone())
                        .unwrap_or_else(|| ev.channel_id.clone());

                    // Register an ActiveTurn so handle_agent_reply can route the reply.
                    active_turns.insert(
                        session_id.clone(),
                        session::ActiveTurn {
                            session_id: session_id.clone(),
                            channel_id: ev.channel_id.clone(),
                            guild_id: ev.guild_id.clone(),
                            draft_message_id: None,
                            trigger_message_id: None,
                            reply_channel_id: text_channel_id.clone(),
                        },
                    );

                    let task_json = json!({
                        "action": "voice.dialogue",
                        "session_id": &session_id,
                        "chat_id": &text_channel_id,
                        "guild_id": &ev.guild_id,
                        "channel_id": &ev.channel_id,
                        "speaker_ssrc": ev.speaker_ssrc,
                        "pcm_b64": ev.pcm_b64,
                        "sample_rate": voice_bridge::SAMPLE_RATE,
                        "source": "discord",
                        "transport": "discord",
                        "final_reply_to": &args.node_id,
                        "final_reply_role": "membrane",
                        "final_reply_guest_id": &args.guest_id,
                    });

                    if let Err(e) = ipc.send_request(IpcRequest::EmitTask {
                        target_node: args.node_id.clone(),
                        target_role: "agent".to_string(),
                        target_guest_id: None,
                        task_json: task_json.to_string(),
                    }).await {
                        error!("Failed to emit voice.dialogue task: {}", e);
                    }
                }
            }

            // Inbound gateway event
            event = event_rx.recv() => {
                let Some(event) = event else {
                    return Err(anyhow!("Gateway event channel closed"));
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
                            let guild_id = interaction["guild_id"].as_str().unwrap_or("").to_string();

                            // Check for membrane-local commands
                            if let Some(cmd) = &envelope.command {
                                match cmd.as_str() {
                                    "/join" => {
                                        // Kick off voice join flow for this guild
                                        // Bot must already be in a voice channel (done via Discord client
                                        // or a separate API call — for now we prime the pending state and
                                        // wait for the gateway events to arrive).
                                        pending_voice.entry(guild_id.clone()).or_insert_with(|| {
                                            PendingVoiceConnect {
                                                guild_id: guild_id.clone(),
                                                channel_id: String::new(),
                                                voice_session_id: None,
                                                endpoint: None,
                                                token: None,
                                                text_channel_id: channel_id.clone(),
                                            }
                                        });

                                        let _ = egress::respond_to_interaction(
                                            &http,
                                            &bot_token,
                                            &interaction_id,
                                            &interaction_token,
                                            "Joining voice channel...",
                                        ).await;
                                        continue;
                                    }

                                    "/leave" => {
                                        active_bridges.remove(&guild_id);
                                        pending_voice.remove(&guild_id);
                                        let _ = egress::respond_to_interaction(
                                            &http,
                                            &bot_token,
                                            &interaction_id,
                                            &interaction_token,
                                            "Left voice channel.",
                                        ).await;
                                        continue;
                                    }

                                    _ => {}
                                }

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

                    gateway::GatewayEvent::VoiceStateUpdate(d) => {
                        // Received when the bot successfully joins/leaves a voice channel.
                        // We need: user_id == bot_user_id, session_id, guild_id, channel_id.
                        let uid = d["user_id"].as_str().unwrap_or("");
                        if uid != bot_user_id {
                            debug!("VoiceStateUpdate for other user {}, skipping", uid);
                        } else {
                            let guild_id = d["guild_id"].as_str().unwrap_or("").to_string();
                            let channel_id = d["channel_id"].as_str().unwrap_or("").to_string();
                            let voice_session_id = d["session_id"].as_str().unwrap_or("").to_string();

                            if channel_id.is_empty() {
                                // Bot left voice
                                debug!("VoiceStateUpdate: bot left voice in guild {}", guild_id);
                                active_bridges.remove(&guild_id);
                                pending_voice.remove(&guild_id);
                            } else {
                                info!(
                                    "VoiceStateUpdate: bot joined voice channel {} in guild {} (session {})",
                                    channel_id, guild_id, voice_session_id
                                );

                                let pending = pending_voice.entry(guild_id.clone()).or_insert_with(|| {
                                    PendingVoiceConnect {
                                        guild_id: guild_id.clone(),
                                        channel_id: channel_id.clone(),
                                        voice_session_id: None,
                                        endpoint: None,
                                        token: None,
                                        text_channel_id: channel_id.clone(),
                                    }
                                });
                                pending.channel_id = channel_id;
                                pending.voice_session_id = Some(voice_session_id);

                                if pending.is_ready() {
                                    attempt_voice_connect(
                                        &mut pending_voice,
                                        &guild_id,
                                        &bot_user_id,
                                        utterance_tx.clone(),
                                        &mut active_bridges,
                                    ).await;
                                }
                            }
                        }
                    }

                    gateway::GatewayEvent::VoiceServerUpdate(d) => {
                        let guild_id = d["guild_id"].as_str().unwrap_or("").to_string();
                        let endpoint = d["endpoint"].as_str().unwrap_or("").to_string();
                        let token = d["token"].as_str().unwrap_or("").to_string();

                        info!(
                            "VoiceServerUpdate: guild={} endpoint={}",
                            guild_id, endpoint
                        );

                        let text_channel_id = pending_voice
                            .get(&guild_id)
                            .map(|p| p.text_channel_id.clone())
                            .unwrap_or_default();

                        let pending = pending_voice.entry(guild_id.clone()).or_insert_with(|| {
                            PendingVoiceConnect {
                                guild_id: guild_id.clone(),
                                channel_id: String::new(),
                                voice_session_id: None,
                                endpoint: None,
                                token: None,
                                text_channel_id,
                            }
                        });
                        pending.endpoint = Some(endpoint);
                        pending.token = Some(token);

                        if pending.is_ready() {
                            attempt_voice_connect(
                                &mut pending_voice,
                                &guild_id,
                                &bot_user_id,
                                utterance_tx.clone(),
                                &mut active_bridges,
                            ).await;
                        }
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
                            &mut active_bridges,
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
                        return Err(anyhow!("IPC recv_task error: {}", e));
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
    info!(
        "membrane-discord shutting down for agent [{}]",
        args.agent_id
    );
    let _ = shutdown_tx.send(());
    if let Err(e) = gateway_lease.release(&mut ipc).await {
        warn!("Failed to release Discord gateway lease: {}", e);
    }

    Ok(())
}

/// Sleep until the lease driver's next scheduled attempt. Pends forever once
/// the driver reaches a terminal state (lost/released) — the Lost event has
/// already been surfaced by the tick that entered that state.
async fn lease_deadline(gateway_lease: &lease::DiscordGatewayLease) {
    match gateway_lease.next_deadline() {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

async fn run_with_backoff(args: Args) -> Result<()> {
    let mut backoff_secs = MEMBRANE_ERROR_BACKOFF_INITIAL_SECS;

    loop {
        match run_once(args.clone()).await {
            Ok(()) => return Ok(()),
            Err(err) => {
                warn!(
                    "membrane-discord runtime failed for agent [{}]: {}. Retrying in {}s.",
                    args.agent_id, err, backoff_secs
                );
                tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                backoff_secs = next_error_backoff_secs(backoff_secs);
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    run_with_backoff(args).await
}

/// Attempt to connect the voice gateway once both VoiceStateUpdate and
/// VoiceServerUpdate have been received for a guild.
async fn attempt_voice_connect(
    pending_voice: &mut HashMap<String, PendingVoiceConnect>,
    guild_id: &str,
    bot_user_id: &str,
    utterance_tx: mpsc::Sender<VoiceUtteranceEvent>,
    active_bridges: &mut HashMap<String, VoiceBridge>,
) {
    let Some(pending) = pending_voice.remove(guild_id) else {
        return;
    };

    let (voice_session_id, endpoint, token) =
        match (pending.voice_session_id, pending.endpoint, pending.token) {
            (Some(s), Some(e), Some(t)) => (s, e, t),
            _ => {
                warn!(
                    "attempt_voice_connect: missing state for guild {}",
                    guild_id
                );
                return;
            }
        };

    let hotel_session_id = format!("discord:{}:{}", guild_id, pending.channel_id);

    info!(
        "Voice: connecting gateway for guild={} channel={} endpoint={}",
        guild_id, pending.channel_id, endpoint
    );

    match voice_gateway::connect_voice_gateway(
        &endpoint,
        guild_id,
        &pending.channel_id,
        bot_user_id,
        &voice_session_id,
        &token,
    )
    .await
    {
        Ok((session, _event_rx)) => {
            match VoiceBridge::start(
                session,
                utterance_tx,
                hotel_session_id,
                pending.text_channel_id.clone(),
            ) {
                Ok(bridge) => {
                    info!(
                        "VoiceBridge: active for guild={} channel={}",
                        guild_id, bridge.channel_id
                    );
                    active_bridges.insert(guild_id.to_string(), bridge);
                }
                Err(e) => error!("VoiceBridge::start failed for guild {}: {}", guild_id, e),
            }
        }
        Err(e) => error!("connect_voice_gateway failed for guild {}: {}", guild_id, e),
    }
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
    active_bridges: &mut HashMap<String, VoiceBridge>,
    http: &reqwest::Client,
    bot_token: &str,
) -> Result<()> {
    let task: Value = serde_json::from_str(task_json)?;

    let action = task["action"].as_str().unwrap_or("");
    let session_id = task["session_id"].as_str().unwrap_or("");
    let content = task["content"].as_str().unwrap_or("");
    let chat_id = task["chat_id"].as_str().unwrap_or("");

    let mut reply_channel = active_turns
        .get(session_id)
        .map(|t| t.reply_channel_id.clone())
        .unwrap_or_else(|| chat_id.to_string());

    // Text replies with no Discord origin — e.g. self-heal escalations the
    // hotel delivers to this seat — have no channel, and POSTing to
    // `/channels//messages` earns a Discord 405 on every attempt (live
    // 2026-07-20: recurring `discord_reply_405` heal item). Route them to the
    // operator channel when one is configured; otherwise skip cleanly — an
    // undeliverable reply is not an API error. Voice replies are unaffected
    // (they route via the UDP bridge keyed off session_id).
    if reply_channel.is_empty() && matches!(action, "partial_reply" | "send_reply") {
        match std::env::var("PHILOTIC_DISCORD_OPERATOR_CHANNEL_ID")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
        {
            Some(operator_channel) => {
                info!(
                    session_id,
                    "agent reply has no Discord channel; routing to operator channel"
                );
                reply_channel = operator_channel;
            }
            None => {
                info!(
                    session_id,
                    action,
                    "skipping agent reply with no Discord channel (set PHILOTIC_DISCORD_OPERATOR_CHANNEL_ID to receive these)"
                );
                return Ok(());
            }
        }
    }

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
                    let _ =
                        egress::edit_message(http, bot_token, &reply_channel, draft_id, content)
                            .await;
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
                            turn.draft_message_id = msg["id"].as_str().map(String::from);
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

        "send_voice_reply" => {
            // Hotel synthesized speech — decode PCM and send to Discord UDP.
            // session_id format: "discord:{guild_id}:{channel_id}"
            let guild_id = session_id
                .strip_prefix("discord:")
                .and_then(|s| s.split(':').next())
                .unwrap_or("");

            if guild_id.is_empty() {
                warn!(
                    "send_voice_reply: can't parse guild_id from session_id [{}]",
                    session_id
                );
                return Ok(());
            }

            let audio_b64 = task["audio_b64"].as_str().unwrap_or("");
            if audio_b64.is_empty() {
                warn!(
                    "send_voice_reply: empty audio_b64 for session [{}]",
                    session_id
                );
                return Ok(());
            }

            match voice_bridge::decode_pcm_b64(audio_b64) {
                Ok(samples) => {
                    if let Some(bridge) = active_bridges.get(guild_id) {
                        bridge.send_pcm(samples).await;
                    } else {
                        warn!(
                            "send_voice_reply: no active bridge for guild [{}]",
                            guild_id
                        );
                    }
                }
                Err(e) => warn!("send_voice_reply: decode_pcm_b64 failed: {}", e),
            }
        }

        "turn_event" => {
            let event = task["event"].as_str().unwrap_or("");
            if event == "model_fallback" || event == "model_fallback_cleared" {
                // Model-tier fallback/recovery notice (Slice 3 of Model
                // Failover Layers): a one-time plain operational message, not
                // an assistant reply — sent directly, no typing indicator, no
                // draft tracking, no TTS. `TurnEventPayload` carries the
                // formatted text in `partial_content` (not `content`).
                let message = task["partial_content"].as_str().unwrap_or("");
                if !message.is_empty() {
                    let _ = egress::send_text_reply(http, bot_token, &reply_channel, message, None)
                        .await;
                }
            } else {
                // Events like waiting_tool, waiting_model — show typing to keep Discord engaged
                egress::send_typing(http, bot_token, &reply_channel).await;
            }
        }

        _ => {
            warn!(
                "Unknown agent reply action [{}] for session [{}]",
                action, session_id
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::next_error_backoff_secs;

    #[test]
    fn error_backoff_doubles_and_caps_at_ten_minutes() {
        assert_eq!(next_error_backoff_secs(1), 2);
        assert_eq!(next_error_backoff_secs(2), 4);
        assert_eq!(next_error_backoff_secs(300), 600);
        assert_eq!(next_error_backoff_secs(600), 600);
        assert_eq!(next_error_backoff_secs(900), 600);
    }
}
