/// Discord Voice Gateway WebSocket client.
///
/// Handles the second WebSocket connection opened when the bot joins a voice channel.
/// Sequence: Identify → Ready → IP Discovery → Select Protocol → Session Description
///
/// Reference: https://discord.com/developers/docs/topics/voice-connections
use anyhow::{Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::time::{interval, timeout};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{debug, error, info, warn};

use crate::crypto::{EncryptionMode, VoiceEncryptionState};
use crate::voice_udp::VoiceUdpTransport;

/// All state resolved after a successful voice gateway handshake.
pub struct VoiceSession {
    pub bot_ssrc: u32,
    pub udp_transport: VoiceUdpTransport,
    pub crypto: VoiceEncryptionState,
    pub mode: EncryptionMode,
    pub guild_id: String,
    pub channel_id: String,
    pub server_session_id: String,
}

/// Events emitted by the voice gateway session after handshake.
#[derive(Debug)]
pub enum VoiceGatewayEvent {
    Speaking {
        user_id: String,
        ssrc: u32,
        speaking: bool,
    },
    ClientDisconnect {
        user_id: String,
    },
    Resumed,
}

/// Connect to the Discord Voice Gateway, negotiate the session, perform IP
/// discovery, and return a fully-initialised `VoiceSession` plus a channel
/// for ongoing voice gateway events.
///
/// After this returns the caller owns:
/// - `VoiceSession` — ready for `send_opus_frame` / `recv_rtp_packet`
/// - An `mpsc::Receiver<VoiceGatewayEvent>` for speaking-state updates
/// - A `tokio::task::JoinHandle` for the background heartbeat + event loop
pub async fn connect_voice_gateway(
    voice_endpoint: &str, // from VOICE_SERVER_UPDATE (without wss:// prefix)
    server_id: &str,      // guild_id
    channel_id: &str,
    user_id: &str,    // bot user id
    session_id: &str, // from VOICE_STATE_UPDATE
    token: &str,      // from VOICE_SERVER_UPDATE
) -> Result<(VoiceSession, tokio::sync::mpsc::Receiver<VoiceGatewayEvent>)> {
    // Discord voice gateway URL uses ?v=4
    let url = format!("wss://{}?v=4", voice_endpoint.trim_start_matches("wss://"));
    info!("Connecting to Voice Gateway: {}", url);

    let (ws_stream, _) = connect_async(&url).await?;
    let (mut write, mut read) = ws_stream.split();

    // Step 1: receive Hello (opcode 8)
    let hello = recv_json(&mut read).await?;
    let heartbeat_interval_ms = hello["d"]["heartbeat_interval"].as_f64().unwrap_or(13750.0) as u64;
    info!(
        "Voice GW Hello — heartbeat interval {}ms",
        heartbeat_interval_ms
    );

    // Step 2: send Identify (opcode 0)
    write
        .send(Message::Text(
            json!({
                "op": 0,
                "d": {
                    "server_id": server_id,
                    "user_id": user_id,
                    "session_id": session_id,
                    "token": token
                }
            })
            .to_string()
            .into(),
        ))
        .await?;

    // Step 3: receive Ready (opcode 2)
    let ready = recv_opcode(&mut read, 2).await?;
    let bot_ssrc = ready["d"]["ssrc"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("Voice GW Ready missing ssrc"))? as u32;
    let discord_ip = ready["d"]["ip"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Voice GW Ready missing ip"))?
        .to_string();
    let discord_port = ready["d"]["port"]
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("Voice GW Ready missing port"))?
        as u16;
    let modes: Vec<String> = ready["d"]["modes"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    info!(
        "Voice GW Ready — SSRC {}, SFU {}:{}, modes {:?}",
        bot_ssrc, discord_ip, discord_port, modes
    );

    // Step 4: IP Discovery
    let (udp_transport, external_ip, external_port) =
        VoiceUdpTransport::connect_and_discover(&discord_ip, discord_port, bot_ssrc).await?;

    // Step 5: Select best encryption mode
    let selected_mode = EncryptionMode::preference_list()
        .iter()
        .find(|&&mode| modes.contains(&mode.to_string()))
        .copied()
        .and_then(EncryptionMode::from_str)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "No supported encryption mode among Discord's offered modes: {:?}",
                modes
            )
        })?;

    info!(
        "Voice GW: selected encryption mode [{}]",
        selected_mode.as_str()
    );

    // Step 6: send Select Protocol (opcode 1)
    write
        .send(Message::Text(
            json!({
                "op": 1,
                "d": {
                    "protocol": "udp",
                    "data": {
                        "address": external_ip.to_string(),
                        "port": external_port,
                        "mode": selected_mode.as_str()
                    }
                }
            })
            .to_string()
            .into(),
        ))
        .await?;

    // Step 7: receive Session Description (opcode 4)
    let session_desc = recv_opcode(&mut read, 4).await?;
    let mode_str = session_desc["d"]["mode"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Session Description missing mode"))?;
    let secret_key: Vec<u8> = session_desc["d"]["secret_key"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Session Description missing secret_key"))?
        .iter()
        .filter_map(|v| v.as_u64().map(|b| b as u8))
        .collect();

    if secret_key.len() != 32 {
        bail!(
            "Voice session secret_key must be 32 bytes, got {}",
            secret_key.len()
        );
    }

    let confirmed_mode = EncryptionMode::from_str(mode_str)
        .ok_or_else(|| anyhow::anyhow!("Unknown mode in Session Description: {}", mode_str))?;

    info!(
        "Voice GW Session Description — mode [{}], key len {}",
        mode_str,
        secret_key.len()
    );

    // Step 8: send Speaking (opcode 5) to signal we're ready
    write
        .send(Message::Text(
            json!({
                "op": 5,
                "d": {
                    "speaking": 1,
                    "delay": 0,
                    "ssrc": bot_ssrc
                }
            })
            .to_string()
            .into(),
        ))
        .await?;

    let crypto = VoiceEncryptionState::new(confirmed_mode.clone(), secret_key);

    let session = VoiceSession {
        bot_ssrc,
        udp_transport,
        crypto,
        mode: confirmed_mode,
        guild_id: server_id.to_string(),
        channel_id: channel_id.to_string(),
        server_session_id: session_id.to_string(),
    };

    // Step 9: start background loop for heartbeats + ongoing events
    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<VoiceGatewayEvent>(32);

    // Convert the split write half back into a usable sink for the background task
    // We need to reunite the split stream for the background loop.
    // Since reunite isn't always possible after the handshake steps, we spawn a
    // forwarding task that only needs to send heartbeats.
    // The write half needs to be sent to the background task.
    //
    // We use an mpsc channel to forward heartbeat messages from the timer to the
    // write half, both living in the same task.
    let (hb_tx, mut hb_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();
    let hb_tx_clone = hb_tx.clone();

    // Heartbeat timer task
    tokio::spawn(async move {
        let mut tick = interval(Duration::from_millis(heartbeat_interval_ms));
        let mut nonce: u64 = 0;
        loop {
            tick.tick().await;
            let msg = Message::Text(json!({ "op": 3, "d": nonce }).to_string().into());
            if hb_tx_clone.send(msg).is_err() {
                break;
            }
            nonce += 1;
        }
    });

    // Write half + event loop
    tokio::spawn(async move {
        let mut write_half = write;
        loop {
            tokio::select! {
                // Outbound heartbeat
                msg = hb_rx.recv() => {
                    match msg {
                        Some(m) => { let _ = write_half.send(m).await; }
                        None => break,
                    }
                }
                // Inbound voice gateway events
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            if let Ok(payload) = serde_json::from_str::<Value>(&text) {
                                handle_voice_event(&payload, &event_tx).await;
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            warn!("Voice gateway connection closed");
                            break;
                        }
                        Some(Err(e)) => {
                            error!("Voice gateway error: {}", e);
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }
    });

    info!("Voice gateway handshake complete — session ready");
    Ok((session, event_rx))
}

/// Send a Speaking opcode (5) on a write half of the voice gateway.
/// Used to signal start/stop of speaking to Discord's SFU.
///
/// `speaking`: 1 = start, 0 = stop
/// `ssrc`: the bot's SSRC from Ready
pub async fn send_speaking_state<S>(write: &mut S, speaking: u8, ssrc: u32) -> Result<()>
where
    S: SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    write
        .send(Message::Text(
            json!({
                "op": 5,
                "d": { "speaking": speaking, "delay": 0, "ssrc": ssrc }
            })
            .to_string()
            .into(),
        ))
        .await
        .map_err(|e| anyhow::anyhow!("Send speaking failed: {}", e))
}

// ── Internal helpers ──────────────────────────────────────────────────────────

async fn recv_json<S>(stream: &mut S) -> Result<Value>
where
    S: StreamExt<Item = std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
{
    loop {
        match timeout(Duration::from_secs(15), stream.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                return Ok(serde_json::from_str(&text)?);
            }
            Ok(Some(Ok(_))) => continue, // ignore ping/binary
            Ok(Some(Err(e))) => bail!("Voice GW WebSocket error: {}", e),
            Ok(None) => bail!("Voice GW connection closed"),
            Err(_) => bail!("Timeout waiting for Voice GW message"),
        }
    }
}

async fn recv_opcode<S>(stream: &mut S, expected_op: u64) -> Result<Value>
where
    S: StreamExt<Item = std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>
        + Unpin,
{
    loop {
        let msg = recv_json(stream).await?;
        let op = msg["op"].as_u64().unwrap_or(u64::MAX);
        if op == expected_op {
            return Ok(msg);
        }
        // Heartbeat ACK (op 6) can arrive at any time during handshake — ignore
        if op == 6 {
            debug!("Voice GW: heartbeat ACK during handshake");
            continue;
        }
        debug!(
            "Voice GW: ignoring op {} while waiting for op {}",
            op, expected_op
        );
    }
}

async fn handle_voice_event(
    payload: &Value,
    event_tx: &tokio::sync::mpsc::Sender<VoiceGatewayEvent>,
) {
    let op = payload["op"].as_u64().unwrap_or(u64::MAX);
    match op {
        5 => {
            // Speaking state update
            if let (Some(user_id), Some(ssrc), Some(speaking)) = (
                payload["d"]["user_id"].as_str(),
                payload["d"]["ssrc"].as_u64(),
                payload["d"]["speaking"].as_u64(),
            ) {
                let _ = event_tx
                    .send(VoiceGatewayEvent::Speaking {
                        user_id: user_id.to_string(),
                        ssrc: ssrc as u32,
                        speaking: speaking != 0,
                    })
                    .await;
            }
        }
        13 => {
            // Client disconnect
            if let Some(user_id) = payload["d"]["user_id"].as_str() {
                let _ = event_tx
                    .send(VoiceGatewayEvent::ClientDisconnect {
                        user_id: user_id.to_string(),
                    })
                    .await;
            }
        }
        9 => {
            // Session description update (e.g. after reconnect)
            let _ = event_tx.send(VoiceGatewayEvent::Resumed).await;
        }
        6 => {} // Heartbeat ACK
        _ => {
            debug!("Voice GW: unhandled op {}", op);
        }
    }
}
