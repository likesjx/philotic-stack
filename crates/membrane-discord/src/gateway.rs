use anyhow::{Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::{interval, sleep, timeout};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

const GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";

/// Max consecutive failed connect attempts against a resume URL before the
/// resume state is discarded and the loop falls back to a fresh IDENTIFY on
/// the base gateway URL. Any HTTP 4xx on connect discards immediately.
const MAX_RESUME_CONNECT_FAILURES: u32 = 2;

/// Resume state proven by THIS process lifetime. Never persisted: per Discord
/// docs, `resume_gateway_url` + `session_id` are only valid for resuming a
/// session this process still owns. A fresh process start must always dial
/// the base gateway URL and IDENTIFY.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResumeState {
    session_id: String,
    resume_url: String,
    last_sequence: u64,
}

/// Pick the URL for the next connect attempt: the resume URL only when we
/// hold live in-process resume state, otherwise the base gateway URL.
fn select_connect_url(resume: Option<&ResumeState>) -> &str {
    match resume {
        Some(state) => state.resume_url.as_str(),
        None => GATEWAY_URL,
    }
}

/// Build a dialable WebSocket URL from Discord's `resume_gateway_url`.
///
/// The path slash before the query string is required: `wss://host?v=10...`
/// (no path) makes tungstenite emit a malformed request line, which Discord's
/// edge rejects with HTTP 400 Bad Request.
fn resume_ws_url(resume_gateway_url: &str) -> String {
    format!(
        "{}/?v=10&encoding=json",
        resume_gateway_url.trim_end_matches('/')
    )
}

/// True when the connect error chain contains an HTTP 4xx handshake response
/// (e.g. `400 Bad Request` from dialing a stale or malformed resume URL).
fn is_http_4xx_connect_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<tokio_tungstenite::tungstenite::Error>(),
            Some(tokio_tungstenite::tungstenite::Error::Http(resp))
                if resp.status().is_client_error()
        )
    })
}

/// Decide whether resume state should be discarded after a failed connect.
fn should_discard_resume(http_4xx: bool, consecutive_failures: u32) -> bool {
    http_4xx || consecutive_failures >= MAX_RESUME_CONNECT_FAILURES
}

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
    // In-process only: a fresh process start always begins with no resume
    // state and therefore dials the base gateway URL with an IDENTIFY.
    let mut resume: Option<ResumeState> = None;
    let mut resume_connect_failures: u32 = 0;

    loop {
        let connect_url = select_connect_url(resume.as_ref()).to_string();

        info!("Connecting to Discord Gateway: {}", connect_url);

        match connect_and_identify(
            &connect_url,
            &bot_token,
            resume.as_ref(),
            event_tx.clone(),
            &mut shutdown_rx,
        )
        .await
        {
            Ok(GatewayExitReason::CleanResume { state }) => {
                info!("Gateway: clean resume requested");
                resume = Some(state);
                resume_connect_failures = 0;
                sleep(Duration::from_secs(1)).await;
            }
            Ok(GatewayExitReason::Reconnect) => {
                info!("Gateway: reconnect (no resume)");
                resume = None;
                resume_connect_failures = 0;
                sleep(Duration::from_secs(5)).await;
            }
            Ok(GatewayExitReason::Shutdown) => {
                info!("Gateway: shutdown requested");
                return;
            }
            Err(e) => {
                if resume.is_some() {
                    resume_connect_failures += 1;
                    let http_4xx = is_http_4xx_connect_error(&e);
                    if should_discard_resume(http_4xx, resume_connect_failures) {
                        warn!(
                            "Gateway: discarding stale resume state after {} failed resume connect(s) (http_4xx={}) — falling back to fresh IDENTIFY on {}",
                            resume_connect_failures, http_4xx, GATEWAY_URL
                        );
                        resume = None;
                        resume_connect_failures = 0;
                    }
                }
                error!("Gateway error: {:#} — reconnecting in 5s", e);
                sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

enum GatewayExitReason {
    CleanResume { state: ResumeState },
    Reconnect,
    Shutdown,
}

async fn connect_and_identify(
    url: &str,
    bot_token: &str,
    resume: Option<&ResumeState>,
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
    let identify_payload = if let Some(state) = resume {
        // Resume
        json!({
            "op": 6,
            "d": {
                "token": bot_token,
                "session_id": state.session_id,
                "seq": state.last_sequence
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

    let mut current_sequence: Option<u64> = resume.map(|state| state.last_sequence);
    let mut current_session_id: Option<String> = resume.map(|state| state.session_id.clone());
    let mut current_resume_url: Option<String> = resume.map(|state| state.resume_url.clone());

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
                    Message::Close(frame) => {
                        // Discord signals fatal handshake problems via the close
                        // frame code (4004 auth failed, 4013 invalid intents, 4014
                        // disallowed/unenabled privileged intents). Dropping it left
                        // the gateway reconnect-looping with no diagnosable reason.
                        match frame {
                            Some(cf) => warn!(
                                "Gateway closed by Discord: code={} reason={}",
                                cf.code, cf.reason
                            ),
                            None => warn!("Gateway closed by Discord (no close frame)"),
                        }
                        return Ok(GatewayExitReason::Reconnect);
                    }
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
                                // Fall back to the bare base host (not GATEWAY_URL,
                                // which already carries the query string that
                                // resume_ws_url appends).
                                let resume_gateway = d["resume_gateway_url"]
                                    .as_str()
                                    .unwrap_or("wss://gateway.discord.gg")
                                    .to_string();

                                current_session_id = Some(sid.clone());
                                current_resume_url = Some(resume_ws_url(&resume_gateway));

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
                        if let (Some(sid), Some(rurl), Some(seq)) =
                            (current_session_id, current_resume_url, current_sequence)
                        {
                            return Ok(GatewayExitReason::CleanResume {
                                state: ResumeState {
                                    session_id: sid,
                                    resume_url: rurl,
                                    last_sequence: seq,
                                },
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
                            if let (Some(sid), Some(rurl), Some(seq)) =
                                (current_session_id, current_resume_url, current_sequence)
                            {
                                return Ok(GatewayExitReason::CleanResume {
                                    state: ResumeState {
                                        session_id: sid,
                                        resume_url: rurl,
                                        last_sequence: seq,
                                    },
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
///
/// MESSAGE_CONTENT is the only privileged intent requested. Discord rejects the
/// entire IDENTIFY with close code 4014 ("Disallowed intent(s)") if it is
/// requested while disabled in the developer portal, so the bot cannot even
/// reach READY. It is gated behind `PHILOTIC_DISCORD_MESSAGE_CONTENT` (default
/// enabled) so a seat can be brought up without it — the bot then reaches READY
/// and handles mentions/DMs/slash commands, just without in-guild message text —
/// until the portal toggle is confirmed. Set the env to `0`/`false`/`off` to omit it.
fn compute_intents() -> u64 {
    let want_message_content = std::env::var("PHILOTIC_DISCORD_MESSAGE_CONTENT")
        .ok()
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            )
        })
        .unwrap_or(true);
    intents_mask(want_message_content)
}

/// Pure intent-bitmask builder (env-free, for testing).
fn intents_mask(want_message_content: bool) -> u64 {
    let guilds: u64 = 1 << 0;
    let guild_voice_states: u64 = 1 << 7;
    let guild_messages: u64 = 1 << 9;
    let message_content: u64 = 1 << 15;
    let base = guilds | guild_voice_states | guild_messages;
    if want_message_content {
        base | message_content
    } else {
        base
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GATEWAY_URL, ResumeState, intents_mask, is_http_4xx_connect_error, resume_ws_url,
        select_connect_url, should_discard_resume,
    };
    use tokio_tungstenite::tungstenite;

    fn sample_resume_state() -> ResumeState {
        ResumeState {
            session_id: "sess-123".to_string(),
            resume_url: "wss://gateway-us-east1-b.discord.gg/?v=10&encoding=json".to_string(),
            last_sequence: 42,
        }
    }

    #[test]
    fn fresh_start_dials_base_gateway_url() {
        // No in-process resume state (e.g. right after process start) must
        // always dial the base gateway URL for a fresh IDENTIFY.
        assert_eq!(select_connect_url(None), GATEWAY_URL);
    }

    #[test]
    fn live_resume_state_dials_resume_url() {
        let state = sample_resume_state();
        assert_eq!(select_connect_url(Some(&state)), state.resume_url);
    }

    #[test]
    fn post_4xx_fallback_clears_state_and_returns_to_base_url() {
        // Mirror the run_gateway Err arm: a resume connect that fails with an
        // HTTP 4xx discards the resume state immediately, and the next URL
        // selection falls back to the base gateway URL.
        let mut resume = Some(sample_resume_state());
        assert_ne!(select_connect_url(resume.as_ref()), GATEWAY_URL);

        let consecutive_failures = 1;
        assert!(should_discard_resume(true, consecutive_failures));
        resume = None;
        assert_eq!(select_connect_url(resume.as_ref()), GATEWAY_URL);
    }

    #[test]
    fn non_4xx_connect_failures_discard_after_max_attempts() {
        assert!(!should_discard_resume(false, 1));
        assert!(should_discard_resume(false, 2));
        assert!(should_discard_resume(false, 3));
    }

    #[test]
    fn resume_ws_url_keeps_path_slash_before_query() {
        // The missing "/" before the query produced a malformed request line
        // and Discord's edge answered HTTP 400 (the vps-jane crash loop).
        assert_eq!(
            resume_ws_url("wss://gateway-us-east1-b.discord.gg"),
            "wss://gateway-us-east1-b.discord.gg/?v=10&encoding=json"
        );
        assert_eq!(
            resume_ws_url("wss://gateway-us-east1-b.discord.gg/"),
            "wss://gateway-us-east1-b.discord.gg/?v=10&encoding=json"
        );
    }

    #[test]
    fn http_4xx_detection_matches_client_errors_only() {
        let resp_400 = tungstenite::http::Response::builder()
            .status(400)
            .body(None)
            .unwrap();
        let err_400 = anyhow::Error::from(tungstenite::Error::Http(resp_400));
        assert!(is_http_4xx_connect_error(&err_400));

        let resp_502 = tungstenite::http::Response::builder()
            .status(502)
            .body(None)
            .unwrap();
        let err_502 = anyhow::Error::from(tungstenite::Error::Http(resp_502));
        assert!(!is_http_4xx_connect_error(&err_502));

        let non_http = anyhow::anyhow!("Timeout waiting for Hello");
        assert!(!is_http_4xx_connect_error(&non_http));
    }

    #[test]
    fn message_content_toggles_only_the_privileged_bit() {
        let with = intents_mask(true);
        let without = intents_mask(false);
        assert_eq!(
            with ^ without,
            1 << 15,
            "only MESSAGE_CONTENT should differ"
        );
        assert_eq!(
            without & (1 << 15),
            0,
            "disabled mask must omit MESSAGE_CONTENT"
        );
        // Non-privileged intents are always present regardless of the toggle.
        for bit in [1u64 << 0, 1 << 7, 1 << 9] {
            assert_eq!(without & bit, bit);
            assert_eq!(with & bit, bit);
        }
    }
}
