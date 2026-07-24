//! Edge e2e smoke driver — drives a REAL running stack (aiua + philotic-web)
//! over HTTP + WebSocket using the philotic-edge-protocol wire types.
//!
//! Flow: enroll -> Hello/HelloAck -> TurnSubmit -> TurnEvent stream ->
//! kill connection -> reconnect with cursor -> verify replay ->
//! GET /health + /api/mesh/roster (+ /api/sessions stub).
//!
//! Environment:
//!   EDGE_E2E_BASE          http base (e.g. http://127.0.0.1:18990)  [required]
//!   EDGE_E2E_INVITE        edge enrollment invite code              [required]
//!   EDGE_E2E_TARGET_NODE   hotel node id hosting the agent          [required]
//!   EDGE_E2E_AGENT_ID      target agent guest id (e.g. agent-jane)  [required]
//!   EDGE_E2E_EXPECT        substring expected in the Final event    [optional]
//!
//! Exits 0 only if every step passed. Prints `STEP <name> PASS|FAIL — evidence`.

use futures_util::{SinkExt, StreamExt};
use philotic_edge_protocol::{
    EdgeCapabilities, EdgeEnvelope, EdgeHello, EdgeMessage, EnrollmentRequest, EnrollmentResponse,
    TurnEventKind, PROTOCOL_VERSION,
};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type WsStream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("missing required env {key}"))
}

async fn send_envelope(ws: &mut WsStream, seq: u64, msg: EdgeMessage) {
    let frame = serde_json::to_string(&EdgeEnvelope::new(seq, None, msg)).expect("encode");
    ws.send(WsMessage::Text(frame)).await.expect("ws send");
}

async fn recv_envelope(ws: &mut WsStream, secs: u64) -> EdgeEnvelope {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(secs), ws.next())
            .await
            .expect("timed out waiting for ws frame")
            .expect("ws closed while waiting for frame")
            .expect("ws error while waiting for frame");
        match message {
            WsMessage::Text(text) => {
                return serde_json::from_str(&text).expect("decode edge envelope")
            }
            WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
            other => panic!("unexpected ws frame: {other:?}"),
        }
    }
}

async fn ws_connect(base: &str, bearer: &str) -> WsStream {
    let ws_base = base.replacen("http", "ws", 1);
    let mut request = format!("{ws_base}/api/edge/ws")
        .into_client_request()
        .expect("ws request");
    request.headers_mut().insert(
        AUTHORIZATION,
        format!("Bearer {bearer}").parse().expect("bearer header"),
    );
    connect_async(request).await.expect("ws upgrade").0
}

async fn hello(base: &str, bearer: &str, node_id: &str, cursor: Option<&str>) -> WsStream {
    let mut ws = ws_connect(base, bearer).await;
    send_envelope(
        &mut ws,
        1,
        EdgeMessage::Hello(EdgeHello {
            node_id: node_id.to_string(),
            capabilities: EdgeCapabilities {
                device_name: "Edge E2E Driver".into(),
                platform: "macos".into(),
                roles: vec!["ClientNode".into()],
                tools: vec![],
                models: vec![],
            },
            cursor: cursor.map(str::to_string),
        }),
    )
    .await;
    let ack = recv_envelope(&mut ws, 15).await;
    match &ack.msg {
        EdgeMessage::HelloAck {
            session_id,
            replay_from,
        } => {
            assert_eq!(ack.v, PROTOCOL_VERSION);
            println!(
                "STEP hello-ack PASS — session_id={session_id} replay_from={replay_from:?} cursor={cursor:?}"
            );
        }
        other => panic!("expected HelloAck, got {other:?}"),
    }
    ws
}

fn turn_event_parts(envelope: &EdgeEnvelope) -> (TurnEventKind, String, String) {
    match &envelope.msg {
        EdgeMessage::TurnEvent {
            conversation_id,
            event_kind,
            content,
            ..
        } => (*event_kind, conversation_id.clone(), content.clone()),
        other => panic!("expected TurnEvent, got {other:?}"),
    }
}

#[tokio::main]
async fn main() {
    let base = env("EDGE_E2E_BASE");
    let invite = env("EDGE_E2E_INVITE");
    let target_node = env("EDGE_E2E_TARGET_NODE");
    let agent_id = env("EDGE_E2E_AGENT_ID");
    let expect = std::env::var("EDGE_E2E_EXPECT").ok();

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .expect("http client");

    // ── 1. GET /health ───────────────────────────────────────────────────────
    let health = http
        .get(format!("{base}/health"))
        .send()
        .await
        .expect("GET /health");
    let health_status = health.status();
    let health_body = health.text().await.unwrap_or_default();
    assert!(
        health_status.is_success(),
        "health returned {health_status}: {health_body}"
    );
    println!("STEP health PASS — {health_status} {health_body}");

    // ── 2. Enroll ────────────────────────────────────────────────────────────
    let response = http
        .post(format!("{base}/api/edge/enroll"))
        .json(&EnrollmentRequest {
            invite_code: invite.clone(),
            device_pubkey_b64: "ZWRnZS1lMmUtZHJpdmVyLXB1YmtleQ".into(),
            device_name: "Edge E2E Driver".into(),
            platform: "macos".into(),
        })
        .send()
        .await
        .expect("enroll request");
    assert_eq!(response.status(), 200, "enrollment should succeed");
    let enrollment: EnrollmentResponse = response.json().await.expect("enrollment body");
    println!(
        "STEP enroll PASS — node_id={} token_prefix={}",
        enrollment.node_id,
        &enrollment.edge_token[..enrollment.edge_token.len().min(12)]
    );

    // ── 3. Hello/HelloAck ────────────────────────────────────────────────────
    let mut ws = hello(&base, &enrollment.edge_token, &enrollment.node_id, None).await;

    // ── 4. TurnSubmit → TurnEvent stream ─────────────────────────────────────
    send_envelope(
        &mut ws,
        2,
        EdgeMessage::TurnSubmit {
            target_node_id: target_node.clone(),
            target_agent_id: agent_id.clone(),
            conversation_id: Some("conv-edge-e2e".into()),
            content: "edge e2e smoke: please reply".into(),
            blob_refs: vec![],
            message_kind: None,
        },
    )
    .await;

    let mut events: Vec<EdgeEnvelope> = Vec::new();
    loop {
        let envelope = recv_envelope(&mut ws, 90).await;
        let (kind, conv, content) = turn_event_parts(&envelope);
        println!(
            "  turn-event seq={} kind={kind:?} conv={conv} content={content:?}",
            envelope.seq
        );
        let done = kind == TurnEventKind::Final;
        let failed = kind == TurnEventKind::Error;
        events.push(envelope);
        if failed {
            println!("STEP turn-stream FAIL — Error event: {content}");
            std::process::exit(1);
        }
        if done {
            break;
        }
    }
    let (_, _, final_content) = turn_event_parts(events.last().expect("events"));
    if let Some(expect) = &expect {
        assert!(
            final_content.contains(expect),
            "final content {final_content:?} missing expected {expect:?}"
        );
    }
    for pair in events.windows(2) {
        assert!(pair[0].seq < pair[1].seq, "outbound seqs must increase");
    }
    println!(
        "STEP turn-stream PASS — {} events, final={final_content:?}",
        events.len()
    );

    // ── 5. Kill connection (no clean close), reconnect with cursor ──────────
    // Simulate the device dying mid-conversation: drop the TCP stream.
    drop(ws);
    // Cursor = the first turn event's seq: everything after it must replay.
    let cursor = events[0].seq.to_string();
    let mut ws = hello(
        &base,
        &enrollment.edge_token,
        &enrollment.node_id,
        Some(&cursor),
    )
    .await;
    let mut replayed = Vec::new();
    for expected in &events[1..] {
        let frame = recv_envelope(&mut ws, 30).await;
        assert_eq!(
            &frame, expected,
            "replayed frame must be byte-identical to live frame"
        );
        replayed.push(frame);
    }
    // Quiescence probe: next frame after a Ping must be its Pong (replay done).
    send_envelope(&mut ws, 2, EdgeMessage::Ping { nonce: 424242 }).await;
    let pong = recv_envelope(&mut ws, 15).await;
    assert_eq!(
        pong.msg,
        EdgeMessage::Pong { nonce: 424242 },
        "expected Pong right after replay"
    );
    println!(
        "STEP cursor-replay PASS — {} frames replayed identically after cursor={cursor}, then Pong",
        replayed.len()
    );
    ws.close(None).await.ok();

    // ── 6. GET /api/mesh/roster with the edge bearer ────────────────────────
    let roster = http
        .get(format!("{base}/api/mesh/roster"))
        .header(AUTHORIZATION, format!("Bearer {}", enrollment.edge_token))
        .send()
        .await
        .expect("GET /api/mesh/roster");
    let roster_status = roster.status();
    let roster_body = roster.text().await.unwrap_or_default();
    assert!(
        roster_status.is_success(),
        "roster returned {roster_status}: {roster_body}"
    );
    println!(
        "STEP roster PASS — {roster_status} {}",
        &roster_body[..roster_body.len().min(300)]
    );

    // ── 7. GET /api/sessions (documented stub — REST turns route not wired) ──
    let sessions = http
        .get(format!("{base}/api/sessions"))
        .header(AUTHORIZATION, format!("Bearer {}", enrollment.edge_token))
        .send()
        .await
        .expect("GET /api/sessions");
    let sessions_status = sessions.status();
    let sessions_body = sessions.text().await.unwrap_or_default();
    println!("STEP sessions-stub INFO — {sessions_status} {sessions_body}");

    println!("EDGE E2E DRIVER: ALL STEPS PASSED");
}
