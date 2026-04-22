use anyhow::{Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{interval, sleep, timeout};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

const GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";

/// Events dispatched from the gateway loop to the main seat loop.
#[derive(Debug)]
pub enum GatewayEvent {
    Ready {
        bot_user_id: String,
        bot_username: String,
        session_id: String,
        resume_url: String,
    },
    MessageCreate(Value),
    InteractionCreate(Value),
    VoiceStateUpdate(Value),
    VoiceServerUpdate(Value),
    GuildCreate(Value),
    Unknown {
        t: String,
        d: Value,
    },
}

/// Internal control messages to the gateway task.
#[derive(Debug)]
enum GatewayControl {
    Reconnect,
}

/// Run the Discord Gateway WebSocket connection.
/// Sends parsed GatewayEvents to `event_tx`.
/// Loops on disconnect/reconnect until `shutdown_rx` fires.
pub async fn run_gateway(
    bot_token: String,
    event_tx: mpsc::Sender<GatewayEvent>,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    let mut resume_url: Option<String> = None;
    let mut session_id: Option<String> = None;
    let mut last_sequence: Option<u64> = None;

    loop {
        let connect_url = resume_url.as_deref().unwrap_or(GATEWAY_URL).to_string();

        info!("Connecting to Discord Gateway: {}", connect_url);

        match connect_and_identify(
            &connect_url,
            &bot_token,
            session_id.as_deref(),
            last_sequence,
            event_tx.clone(),
            &mut shutdown_rx,
        )
        .await
        {
            Ok(GatewayExitReason::CleanResume {
                new_session_id,
                new_resume_url,
                new_sequence,
            }) => {
                info!("Gateway: clean resume requested");
                session_id = Some(new_session_id);
                resume_url = Some(new_resume_url);
                last_sequence = new_sequence;
                sleep(Duration::from_secs(1)).await;
            }
            Ok(GatewayExitReason::Reconnect) => {
                info!("Gateway: reconnect (no resume)");
                session_id = None;
                resume_url = None;
                sleep(Duration::from_secs(5)).await;
            }
            Ok(GatewayExitReason::Shutdown) => {
                info!("Gateway: shutdown requested");
                return;
            }
            Err(e) => {
                error!("Gateway error: {:#} — reconnecting in 5s", e);
                sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

enum GatewayExitReason {
    CleanResume {
        new_session_id: String,
        new_resume_url: String,
        new_sequence: Option<u64>,
    },
    Reconnect,
    Shutdown,
}

async fn connect_and_identify(
    url: &str,
    bot_token: &str,
    session_id: Option<&str>,
    last_sequence: Option<u64>,
    event_tx: mpsc::Sender<GatewayEvent>,
    shutdown_rx: &mut tokio::sync::oneshot::Receiver<()>,
) -> Result<GatewayExitReason> {
    let (ws_stream, _) = connect_async(url).await?;
    let (mut write, mut read) = ws_stream.split();

    // Read Hello (Opcode 10) to get heartbeat interval
    let hello = read_json_message(&mut read).await?;
    let heartbeat_interval_ms = hello["d"]["heartbeat_interval"].as_u64().unwrap_or(41250);

    info!(
        "Gateway Hello received — heartbeat interval: {}ms",
        heartbeat_interval_ms
    );

    // Send Identify or Resume
    let identify_payload = if let Some(sid) = session_id {
        // Resume
        json!({
            "op": 6,
            "d": {
                "token": bot_token,
                "session_id": sid,
                "seq": last_sequence
            }
        })
    } else {
        // Identify
        json!({
            "op": 2,
            "d": {
                "token": bot_token,
                "intents": compute_intents(),
                "properties": {
                    "os": "linux",
                    "browser": "philotic-membrane-discord",
                    "device": "philotic-membrane-discord"
                }
            }
        })
    };

    write
        .send(Message::Text(identify_payload.to_string().into()))
        .await?;

    let mut heartbeat_interval = interval(Duration::from_millis(heartbeat_interval_ms));
    heartbeat_interval.tick().await; // consume the immediate first tick

    let mut current_sequence: Option<u64> = last_sequence;
    let mut current_session_id: Option<String> = session_id.map(String::from);
    let mut current_resume_url: Option<String> = None;

    loop {
        tokio::select! {
            // Heartbeat tick
            _ = heartbeat_interval.tick() => {
                let hb = json!({ "op": 1, "d": current_sequence });
                if let Err(e) = write.send(Message::Text(hb.to_string().into())).await {
                    warn!("Gateway heartbeat send failed: {}", e);
                    return Ok(GatewayExitReason::Reconnect);
                }
                debug!("Gateway: sent heartbeat (seq={:?})", current_sequence);
            }

            // Inbound message
            msg = read.next() => {
                let msg = match msg {
                    Some(Ok(m)) => m,
                    Some(Err(e)) => {
                        warn!("Gateway read error: {}", e);
                        return Ok(GatewayExitReason::Reconnect);
                    }
                    None => {
                        warn!("Gateway connection closed");
                        return Ok(GatewayExitReason::Reconnect);
                    }
                };

                let text = match msg {
                    Message::Text(t) => t,
                    Message::Close(_) => return Ok(GatewayExitReason::Reconnect),
                    _ => continue,
                };

                let payload: Value = match serde_json::from_str(&text) {
                    Ok(v) => v,
                    Err(e) => {
                        warn!("Gateway: failed to parse message: {}", e);
                        continue;
                    }
                };

                let op = payload["op"].as_u64().unwrap_or(0);

                // Update sequence number
                if let Some(s) = payload.get("s").and_then(|v| v.as_u64()) {
                    current_sequence = Some(s);
                }

                match op {
                    0 => {
                        // Dispatch
                        let t = payload["t"].as_str().unwrap_or("").to_string();
                        let d = payload["d"].clone();

                        match t.as_str() {
                            "READY" => {
                                let bot_user_id = d["user"]["id"]
                                    .as_str()
                                    .unwrap_or("")
                                    .to_string();
                                let bot_username = d["user"]["username"]
                                    .as_str()
                                    .unwrap_or("")
                                    .to_string();
                                let sid = d["session_id"]
                                    .as_str()
                                    .unwrap_or("")
                                    .to_string();
                                let resume = d["resume_gateway_url"]
                                    .as_str()
                                    .unwrap_or(GATEWAY_URL)
                                    .to_string();

                                current_session_id = Some(sid.clone());
                                current_resume_url = Some(format!("{}?v=10&encoding=json", resume.trim_end_matches('/')));

                                info!("Gateway READY — bot: {}#{}", bot_username, bot_user_id);

                                let _ = event_tx
                                    .send(GatewayEvent::Ready {
                                        bot_user_id,
                                        bot_username,
                                        session_id: sid,
                                        resume_url: current_resume_url.clone().unwrap_or_default(),
                                    })
                                    .await;
                            }
                            "RESUMED" => {
                                info!("Gateway RESUMED — sequence {:?}", current_sequence);
                            }
                            "MESSAGE_CREATE" => {
                                let _ = event_tx.send(GatewayEvent::MessageCreate(d)).await;
                            }
                            "INTERACTION_CREATE" => {
                                let _ = event_tx.send(GatewayEvent::InteractionCreate(d)).await;
                            }
                            "VOICE_STATE_UPDATE" => {
                                let _ = event_tx.send(GatewayEvent::VoiceStateUpdate(d)).await;
                            }
                            "VOICE_SERVER_UPDATE" => {
                                let _ = event_tx.send(GatewayEvent::VoiceServerUpdate(d)).await;
                            }
                            "GUILD_CREATE" => {
                                let _ = event_tx.send(GatewayEvent::GuildCreate(d)).await;
                            }
                            other => {
                                debug!("Gateway: unhandled event type [{}]", other);
                                let _ = event_tx
                                    .send(GatewayEvent::Unknown {
                                        t: other.to_string(),
                                        d,
                                    })
                                    .await;
                            }
                        }
                    }

                    1 => {
                        // Heartbeat request — respond immediately
                        let hb = json!({ "op": 1, "d": current_sequence });
                        let _ = write.send(Message::Text(hb.to_string().into())).await;
                    }

                    7 => {
                        // Reconnect
                        info!("Gateway: server requested reconnect");
                        if let (Some(sid), Some(rurl)) = (current_session_id, current_resume_url) {
                            return Ok(GatewayExitReason::CleanResume {
                                new_session_id: sid,
                                new_resume_url: rurl,
                                new_sequence: current_sequence,
                            });
                        }
                        return Ok(GatewayExitReason::Reconnect);
                    }

                    9 => {
                        // Invalid session
                        let resumable = payload["d"].as_bool().unwrap_or(false);
                        warn!("Gateway: invalid session (resumable={})", resumable);
                        sleep(Duration::from_secs(if resumable { 1 } else { 5 })).await;
                        if resumable {
                            if let (Some(sid), Some(rurl)) = (current_session_id, current_resume_url) {
                                return Ok(GatewayExitReason::CleanResume {
                                    new_session_id: sid,
                                    new_resume_url: rurl,
                                    new_sequence: current_sequence,
                                });
                            }
                        }
                        return Ok(GatewayExitReason::Reconnect);
                    }

                    11 => {
                        // Heartbeat ACK
                        debug!("Gateway: heartbeat ACK");
                    }

                    _ => {
                        debug!("Gateway: unhandled opcode {}", op);
                    }
                }
            }

            // Shutdown signal
            _ = &mut *shutdown_rx => {
                info!("Gateway: shutdown signal received");
                let _ = write.send(Message::Close(None)).await;
                return Ok(GatewayExitReason::Shutdown);
            }
        }
    }
}

async fn read_json_message<S>(stream: &mut S) -> Result<Value>
where
    S: StreamExt<Item = std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
{
    match timeout(Duration::from_secs(30), stream.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => Ok(serde_json::from_str(&text)?),
        Ok(Some(Ok(other))) => bail!("Expected text message, got {:?}", other),
        Ok(Some(Err(e))) => bail!("WebSocket error: {}", e),
        Ok(None) => bail!("Connection closed before Hello"),
        Err(_) => bail!("Timeout waiting for Hello"),
    }
}

/// Discord intents bitmask for Slice 0 (text + voice state).
///
/// Bit meanings:
///   GUILDS             = 1 << 0  = 1
///   GUILD_MESSAGES     = 1 << 9  = 512
///   GUILD_VOICE_STATES = 1 << 7  = 128
///   MESSAGE_CONTENT    = 1 << 15 = 32768  (privileged intent, must be enabled in dev portal)
fn compute_intents() -> u64 {
    let guilds: u64 = 1 << 0;
    let guild_voice_states: u64 = 1 << 7;
    let guild_messages: u64 = 1 << 9;
    let message_content: u64 = 1 << 15;
    guilds | guild_voice_states | guild_messages | message_content
}
