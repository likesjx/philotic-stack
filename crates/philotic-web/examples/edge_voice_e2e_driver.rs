//! Edge STREAMING VOICE e2e driver — drives a REAL running stack
//! (aiua + guests + philotic-web) over the edge WebSocket to prove two-way
//! streaming voice against real providers (Gemini transcription/generation,
//! ElevenLabs sentence-pipelined TTS).
//!
//! Flow: enroll -> Hello/HelloAck -> audio_stream_start -> audio_chunk×N
//! (~8KB each) -> audio_stream_end -> record EVERY inbound frame with a
//! wall-clock timestamp until the Final TurnEvent (+ a short late-frame
//! grace window), then assert:
//!   a. Status "accepted" arrives after stream end
//!   b. Token (partial) TurnEvents stream in
//!   c. multiple voice_reply frames, monotonic chunk_seq, FIRST voice_reply
//!      arrives BEFORE the final send_reply TurnEvent (streaming proof)
//!   d. each voice_reply audio decodes to nonempty audio (chunks written to
//!      EDGE_E2E_OUT_DIR, sizes + magic bytes reported)
//!   e. Final TurnEvent arrives strictly AFTER the last voice_reply
//!   f. the reply visibly corresponds to the spoken prompt (mentions weather)
//!
//! Environment:
//!   EDGE_E2E_BASE        http base (e.g. http://127.0.0.1:7810)   [required]
//!   EDGE_E2E_INVITE      edge enrollment invite code              [required]
//!   EDGE_E2E_TARGET_NODE hotel node id hosting the agent          [required]
//!   EDGE_E2E_AGENT_ID    target agent guest id (e.g. agent-vox)   [required]
//!   EDGE_E2E_AUDIO       path to the spoken-audio file (m4a)      [required]
//!   EDGE_E2E_MIME        mime of the audio (default audio/mp4)
//!   EDGE_E2E_OUT_DIR     where voice_reply chunks are written
//!                        (default /tmp/edge-voice-e2e-out)
//!
//! Exits 0 only if every assertion passed. Prints `ASSERT <x> PASS|FAIL`.

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use philotic_edge_protocol::{
    EdgeCapabilities, EdgeEnvelope, EdgeHello, EdgeMessage, EnrollmentRequest, EnrollmentResponse,
    TurnEventKind, PROTOCOL_VERSION,
};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const CHUNK_BYTES: usize = 8 * 1024;

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("missing required env {key}"))
}

async fn send_envelope(ws: &mut WsStream, seq: u64, msg: EdgeMessage) {
    let frame = serde_json::to_string(&EdgeEnvelope::new(seq, None, msg)).expect("encode");
    ws.send(WsMessage::Text(frame)).await.expect("ws send");
}

/// One recorded inbound frame with its arrival time relative to t0.
struct Recorded {
    at: Duration,
    envelope: EdgeEnvelope,
}

/// Receive the next text frame (skipping pings/pongs). None on timeout.
async fn recv_envelope_opt(ws: &mut WsStream, secs: u64) -> Option<EdgeEnvelope> {
    loop {
        let message = match tokio::time::timeout(Duration::from_secs(secs), ws.next()).await {
            Ok(next) => next
                .expect("ws closed while waiting for frame")
                .expect("ws error while waiting for frame"),
            Err(_) => return None,
        };
        match message {
            WsMessage::Text(text) => {
                return Some(serde_json::from_str(&text).expect("decode edge envelope"))
            }
            WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
            other => panic!("unexpected ws frame: {other:?}"),
        }
    }
}

fn describe(msg: &EdgeMessage) -> String {
    match msg {
        EdgeMessage::TurnEvent {
            event_kind,
            content,
            ..
        } => {
            let preview: String = content.chars().take(90).collect();
            format!("turn_event kind={event_kind:?} content={preview:?}")
        }
        EdgeMessage::VoiceReply {
            audio_base64,
            mime_type,
            chunk_seq,
            is_final,
            transcript,
            ..
        } => {
            let t: String = transcript
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(60)
                .collect();
            format!(
                "voice_reply chunk_seq={chunk_seq:?} is_final={is_final:?} mime={mime_type} b64_len={} transcript={t:?}",
                audio_base64.len()
            )
        }
        other => format!("{other:?}").chars().take(120).collect(),
    }
}

#[tokio::main]
async fn main() {
    let base = env("EDGE_E2E_BASE");
    let invite = env("EDGE_E2E_INVITE");
    let target_node = env("EDGE_E2E_TARGET_NODE");
    let agent_id = env("EDGE_E2E_AGENT_ID");
    let audio_path = env("EDGE_E2E_AUDIO");
    let mime = std::env::var("EDGE_E2E_MIME").unwrap_or_else(|_| "audio/mp4".into());
    let out_dir =
        std::env::var("EDGE_E2E_OUT_DIR").unwrap_or_else(|_| "/tmp/edge-voice-e2e-out".into());
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    let audio = std::fs::read(&audio_path).expect("read audio file");
    println!(
        "input audio: {audio_path} ({} bytes, mime {mime}, {} chunks of {CHUNK_BYTES}B)",
        audio.len(),
        audio.len().div_ceil(CHUNK_BYTES)
    );

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .expect("http client");

    // ── health + enroll + hello ──────────────────────────────────────────────
    let health = http
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("GET /health");
    assert!(health.status().is_success(), "health check failed");
    println!("STEP health PASS");

    let response = http
        .post(format!("{base}/api/edge/enroll"))
        .json(&EnrollmentRequest {
            invite_code: invite,
            device_pubkey_b64: "ZWRnZS12b2ljZS1lMmUtcHVia2V5".into(),
            device_name: "Edge Voice E2E Driver".into(),
            platform: "macos".into(),
        })
        .send()
        .await
        .expect("enroll request");
    assert_eq!(response.status(), 200, "enrollment should succeed");
    let enrollment: EnrollmentResponse = response.json().await.expect("enrollment body");
    println!("STEP enroll PASS — node_id={}", enrollment.node_id);

    let ws_base = base.replacen("http", "ws", 1);
    let mut request = format!("{ws_base}/api/edge/ws")
        .into_client_request()
        .expect("ws request");
    request.headers_mut().insert(
        AUTHORIZATION,
        format!("Bearer {}", enrollment.edge_token)
            .parse()
            .expect("bearer header"),
    );
    let mut ws = connect_async(request).await.expect("ws upgrade").0;
    send_envelope(
        &mut ws,
        1,
        EdgeMessage::Hello(EdgeHello {
            node_id: enrollment.node_id.clone(),
            capabilities: EdgeCapabilities {
                device_name: "Edge Voice E2E Driver".into(),
                platform: "macos".into(),
                roles: vec!["ClientNode".into()],
                tools: vec![],
                models: vec![],
            },
            cursor: None,
        }),
    )
    .await;
    let ack = recv_envelope_opt(&mut ws, 15).await.expect("hello ack");
    match &ack.msg {
        EdgeMessage::HelloAck { session_id, .. } => {
            assert_eq!(ack.v, PROTOCOL_VERSION);
            println!("STEP hello-ack PASS — session_id={session_id}");
        }
        other => panic!("expected HelloAck, got {other:?}"),
    }

    // ── uplink audio stream ──────────────────────────────────────────────────
    let mut seq = 2u64;
    send_envelope(
        &mut ws,
        seq,
        EdgeMessage::AudioStreamStart {
            stream_id: "vstream-1".into(),
            target_node_id: target_node.clone(),
            target_agent_id: agent_id.clone(),
            conversation_id: Some("conv-voice-e2e".into()),
            mime_type: mime.clone(),
        },
    )
    .await;
    for (i, chunk) in audio.chunks(CHUNK_BYTES).enumerate() {
        seq += 1;
        send_envelope(
            &mut ws,
            seq,
            EdgeMessage::AudioChunk {
                stream_id: "vstream-1".into(),
                chunk_seq: i as u64,
                data_base64: base64::engine::general_purpose::STANDARD.encode(chunk),
            },
        )
        .await;
    }
    seq += 1;
    send_envelope(
        &mut ws,
        seq,
        EdgeMessage::AudioStreamEnd {
            stream_id: "vstream-1".into(),
            cancel: false,
        },
    )
    .await;
    let t0 = Instant::now();
    let wall0 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    println!(
        "STEP uplink-sent — audio_stream_end sent at t0 (unix {}.{:03})",
        wall0.as_secs(),
        wall0.subsec_millis()
    );

    // ── record every inbound frame until Final (+3s late-frame grace) ───────
    let mut frames: Vec<Recorded> = Vec::new();
    let mut saw_final = false;
    loop {
        // 180s budget while waiting for the turn; 3s grace after the Final to
        // catch ordering violations (e.g. a voice_reply arriving after Final).
        let timeout = if saw_final { 3 } else { 180 };
        let Some(envelope) = recv_envelope_opt(&mut ws, timeout).await else {
            if saw_final {
                break;
            }
            panic!("timed out waiting for turn frames ({timeout}s)");
        };
        let at = t0.elapsed();
        println!(
            "  [ +{:>8.3}s ] seq={} {}",
            at.as_secs_f64(),
            envelope.seq,
            describe(&envelope.msg)
        );
        if let EdgeMessage::TurnEvent {
            event_kind,
            content,
            ..
        } = &envelope.msg
        {
            if *event_kind == TurnEventKind::Error {
                println!("TURN ERROR: {content}");
                std::process::exit(1);
            }
            if *event_kind == TurnEventKind::Final {
                saw_final = true;
            }
        }
        frames.push(Recorded { at, envelope });
    }
    ws.close(None).await.ok();

    // ── assertions ───────────────────────────────────────────────────────────
    let mut failures = 0usize;
    let mut check = |name: &str, ok: bool, evidence: String| {
        println!("ASSERT {name} {} — {evidence}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            failures += 1;
        }
    };

    // (a) accepted status after stream end
    let accepted = frames.iter().find(|r| {
        matches!(&r.envelope.msg, EdgeMessage::TurnEvent { event_kind, content, .. }
            if *event_kind == TurnEventKind::Status && content == "accepted")
    });
    check(
        "a-accepted",
        accepted.is_some(),
        accepted
            .map(|r| format!("accepted at +{:.3}s", r.at.as_secs_f64()))
            .unwrap_or_else(|| "no accepted status frame".into()),
    );

    // (b) token TurnEvents streamed
    let tokens: Vec<&Recorded> = frames
        .iter()
        .filter(|r| {
            matches!(&r.envelope.msg, EdgeMessage::TurnEvent { event_kind, .. }
                if *event_kind == TurnEventKind::Token)
        })
        .collect();
    check(
        "b-tokens",
        !tokens.is_empty(),
        format!(
            "{} token events, first at +{:.3}s",
            tokens.len(),
            tokens.first().map(|r| r.at.as_secs_f64()).unwrap_or(-1.0)
        ),
    );

    // (c) multiple voice_reply frames, monotonic chunk_seq, first BEFORE Final
    let voice: Vec<&Recorded> = frames
        .iter()
        .filter(|r| matches!(&r.envelope.msg, EdgeMessage::VoiceReply { .. }))
        .collect();
    let final_frame = frames.iter().find(|r| {
        matches!(&r.envelope.msg, EdgeMessage::TurnEvent { event_kind, .. }
            if *event_kind == TurnEventKind::Final)
    });
    let seqs: Vec<Option<u64>> = voice
        .iter()
        .map(|r| match &r.envelope.msg {
            EdgeMessage::VoiceReply { chunk_seq, .. } => *chunk_seq,
            _ => None,
        })
        .collect();
    let monotonic = seqs.windows(2).all(|w| match (w[0], w[1]) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    }) && seqs.iter().all(Option::is_some);
    let overlap = match (voice.first(), final_frame) {
        (Some(first_voice), Some(fin)) => first_voice.at < fin.at,
        _ => false,
    };
    check(
        "c-streaming-overlap",
        voice.len() >= 2 && monotonic && overlap,
        format!(
            "{} voice_reply frames, chunk_seqs={:?}, first voice at +{:.3}s vs Final at +{:.3}s",
            voice.len(),
            seqs,
            voice.first().map(|r| r.at.as_secs_f64()).unwrap_or(-1.0),
            final_frame.map(|r| r.at.as_secs_f64()).unwrap_or(-1.0),
        ),
    );

    // (d) every voice_reply decodes to nonempty audio; write chunks out
    let mut d_ok = !voice.is_empty();
    let mut d_evidence = Vec::new();
    for r in &voice {
        let EdgeMessage::VoiceReply {
            audio_base64,
            chunk_seq,
            mime_type,
            ..
        } = &r.envelope.msg
        else {
            unreachable!()
        };
        match base64::engine::general_purpose::STANDARD.decode(audio_base64) {
            Ok(bytes) if !bytes.is_empty() => {
                let magic_mp3 = bytes.len() > 2
                    && (bytes.starts_with(b"ID3") || (bytes[0] == 0xFF && bytes[1] & 0xE0 == 0xE0));
                let path = format!(
                    "{out_dir}/voice-chunk-{}.{}",
                    chunk_seq.map_or("whole".to_string(), |s| s.to_string()),
                    if mime_type.contains("mpeg") { "mp3" } else { "bin" }
                );
                std::fs::write(&path, &bytes).expect("write voice chunk");
                d_evidence.push(format!(
                    "chunk {:?}: {} bytes -> {path} (mp3_magic={magic_mp3})",
                    chunk_seq,
                    bytes.len()
                ));
            }
            _ => {
                d_ok = false;
                d_evidence.push(format!("chunk {chunk_seq:?}: EMPTY/UNDECODABLE"));
            }
        }
    }
    check("d-audio-decodes", d_ok, d_evidence.join("; "));

    // (e) Final strictly after the last voice_reply (arrival order)
    let e_ok = match (voice.last(), final_frame) {
        (Some(last_voice), Some(fin)) => last_voice.at < fin.at,
        _ => false,
    };
    check(
        "e-final-last",
        e_ok,
        format!(
            "last voice at +{:.3}s, Final at +{:.3}s",
            voice.last().map(|r| r.at.as_secs_f64()).unwrap_or(-1.0),
            final_frame.map(|r| r.at.as_secs_f64()).unwrap_or(-1.0)
        ),
    );

    // (f) reply corresponds to the spoken prompt (weather)
    let final_content = final_frame
        .map(|r| match &r.envelope.msg {
            EdgeMessage::TurnEvent { content, .. } => content.clone(),
            _ => String::new(),
        })
        .unwrap_or_default();
    let f_ok = final_content.to_lowercase().contains("weather");
    check(
        "f-transcription-corresponds",
        f_ok,
        format!("final reply: {final_content:?}"),
    );

    if failures == 0 {
        println!("EDGE VOICE E2E DRIVER: ALL ASSERTIONS PASSED");
    } else {
        println!("EDGE VOICE E2E DRIVER: {failures} ASSERTION(S) FAILED");
        std::process::exit(1);
    }
}
