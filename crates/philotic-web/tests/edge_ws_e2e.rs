//! End-to-end integration tests for the edge WebSocket termination
//! (`POST /api/edge/enroll` + `GET /api/edge/ws` in `philotic-web serve`).
//!
//! Each test spawns the real `philotic-web` binary (via `CARGO_BIN_EXE_*`)
//! bound to loopback on a fresh port, backed by a **fake hotel IPC daemon**:
//! a Unix-socket server speaking the real 4-byte big-endian length-prefixed
//! JSON framing with the actual `philotic_client::{IpcRequest, IpcResponse}`
//! wire types. The fake hotel grants the desktop-membrane lease, serves a
//! single local operator target, and answers `EmitTask` with a canned
//! streaming reply (turn_event → partial_reply×2 → send_reply), so the whole
//! edge path — enrollment, bearer auth at upgrade, Hello handshake, TurnSubmit
//! mux, and cursor replay from the ring buffer — is exercised over a real
//! TCP WebSocket against the production router.

use futures_util::{SinkExt, StreamExt};
use philotic_client::{IpcRequest, IpcResponse, LeaseEnvelope, LeaseStatus, OperatorTargetView};
use philotic_edge_protocol::{
    EdgeCapabilities, EdgeEnvelope, EdgeHello, EdgeMessage, EnrollmentRequest, EnrollmentResponse,
    TurnEventKind, PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UnixListener, UnixStream};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::AUTHORIZATION;
use tokio_tungstenite::tungstenite::{Error as WsError, Message as WsMessage};
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Node id the fake hotel reports for its (only, local) operator target.
const LOCAL_TARGET_NODE: &str = "test-local";
const CANNED_TOKENS: [&str; 2] = ["Hel", "lo"];
const CANNED_FINAL: &str = "Hello from the fake hotel";

// ── Fake hotel IPC daemon ─────────────────────────────────────────────────────

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn lease_envelope(lease_key: &str) -> LeaseEnvelope {
    LeaseEnvelope {
        lease_type: "desktop_membrane".into(),
        lease_scope: lease_key.to_string(),
        authority_hotel: "default".into(),
        authority_component: None,
        owner_guest_id: "philotic-web-membrane-test".into(),
        owner_hotel: Some("default".into()),
        owner_component_type: None,
        lease_epoch: 1,
        lease_expires_at: now_secs() + 600,
        last_heartbeat_at: now_secs(),
        status: LeaseStatus::Active,
        delegated_from: None,
        metadata: json!({}),
    }
}

fn local_operator_target() -> OperatorTargetView {
    OperatorTargetView {
        target_node_id: LOCAL_TARGET_NODE.into(),
        target_hotel: "default".into(),
        source_hotel: "default".into(),
        is_local: true,
        roles: vec!["agent".into()],
        models: vec![],
        tools: vec![],
        advertised_roles: vec![],
        freshness_state: "fresh".into(),
        freshness_age_secs: 0,
        freshness_ttl_secs: 600,
        reachability: None,
    }
}

async fn read_ipc_frame(stream: &mut UnixStream) -> std::io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match stream.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).await?;
    Ok(Some(payload))
}

async fn write_ipc_response(
    stream: &mut UnixStream,
    response: &IpcResponse,
) -> std::io::Result<()> {
    let payload = serde_json::to_vec(response).expect("serialize IpcResponse");
    let len = u32::try_from(payload.len()).expect("frame length");
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&payload).await
}

fn ok_response() -> IpcResponse {
    IpcResponse::success("fake-hotel", None)
}

/// Serve one IPC connection from the philotic-web binary. Grants leases,
/// serves the operator-target registry, and answers `EmitTask` with a canned
/// streamed reply pushed as `InboundTask` frames on the same connection
/// (mirroring how the real hotel routes the `SubscribeInbox` reply leg).
async fn fake_hotel_conn(mut stream: UnixStream) {
    while let Ok(Some(payload)) = read_ipc_frame(&mut stream).await {
        let request: IpcRequest = match serde_json::from_slice(&payload) {
            Ok(request) => request,
            Err(err) => {
                let _ = write_ipc_response(
                    &mut stream,
                    &IpcResponse::error("fake-hotel", "BAD_REQUEST", err.to_string()),
                )
                .await;
                continue;
            }
        };
        let write_result = match request {
            IpcRequest::AcquireDesktopMembraneLease { lease_key, .. }
            | IpcRequest::RenewDesktopMembraneLease { lease_key, .. } => {
                write_ipc_response(
                    &mut stream,
                    &IpcResponse::DesktopMembraneLease {
                        desktop_granted: true,
                        desktop_lease: Some(lease_envelope(&lease_key)),
                    },
                )
                .await
            }
            IpcRequest::GetDesktopMembraneLeaseOwner { lease_key } => {
                write_ipc_response(
                    &mut stream,
                    &IpcResponse::DesktopMembraneLeaseStatus {
                        desktop_active: true,
                        desktop_lease: Some(lease_envelope(&lease_key)),
                    },
                )
                .await
            }
            IpcRequest::QueryOperatorTargets => {
                write_ipc_response(
                    &mut stream,
                    &IpcResponse::OperatorTargetsView {
                        operator_targets: vec![local_operator_target()],
                    },
                )
                .await
            }
            IpcRequest::EmitTask { task_json, .. } => {
                let ack = write_ipc_response(&mut stream, &ok_response()).await;
                if ack.is_err() {
                    break;
                }
                stream_canned_reply(&mut stream, &task_json).await
            }
            // Register, SubscribeInbox, ReleaseDesktopMembraneLease, and
            // anything else this surface sends only needs a Standard OK.
            _ => write_ipc_response(&mut stream, &ok_response()).await,
        };
        if write_result.is_err() {
            break;
        }
    }
}

/// Push the canned streaming reply for one emitted operator-chat task:
/// a `turn_event` status, two `partial_reply` tokens, then the `send_reply`.
async fn stream_canned_reply(stream: &mut UnixStream, task_json: &str) -> std::io::Result<()> {
    let task: Value = serde_json::from_str(task_json).expect("emitted task_json is JSON");
    let field = |key: &str| {
        task.get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let (session_id, turn_id, chat_id) = (field("session_id"), field("turn_id"), field("chat_id"));
    let reply = |mut body: Value| {
        body["session_id"] = json!(session_id);
        body["turn_id"] = json!(turn_id);
        body["chat_id"] = json!(chat_id);
        IpcResponse::InboundTask {
            source_node: LOCAL_TARGET_NODE.into(),
            task_id: uuid::Uuid::new_v4(),
            task_json: body.to_string(),
        }
    };

    let mut frames = vec![reply(json!({"action": "turn_event", "event": "thinking"}))];
    for token in CANNED_TOKENS {
        frames.push(reply(json!({"action": "partial_reply", "content": token})));
    }
    frames.push(reply(
        json!({"action": "send_reply", "content": CANNED_FINAL}),
    ));
    for frame in &frames {
        write_ipc_response(stream, frame).await?;
    }
    Ok(())
}

// ── Test server (fake hotel + spawned philotic-web binary) ───────────────────

struct TestServer {
    port: u16,
    invite: String,
    http: reqwest::Client,
    dir: PathBuf,
    hotel_task: tokio::task::JoinHandle<()>,
    _child: tokio::process::Child,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.hotel_task.abort();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn free_loopback_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("ephemeral local addr")
        .port()
}

async fn spawn_server() -> TestServer {
    let dir = std::env::temp_dir().join(format!(
        "pw-edge-e2e-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..12]
    ));
    std::fs::create_dir_all(&dir).expect("create test dir");

    // Fake hotel must be listening before the binary starts: `serve` acquires
    // the desktop-membrane lease during startup and exits if it cannot.
    let socket_path = dir.join("hotel.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind fake hotel socket");
    let hotel_task = tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(fake_hotel_conn(stream));
        }
    });

    let port = free_loopback_port();
    let invite = format!("INV-E2E-{}", uuid::Uuid::new_v4().simple());
    // env_clear: shield the child from PHILOTIC_PROFILE / PHILOTIC_WEB_* on the
    // host. PATH points at the (binary-free) test dir so the post-bind
    // browser auto-open (`open <url>`) fails silently instead of popping a
    // browser during the test run.
    let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_philotic-web"))
        .args([
            "serve",
            "--port",
            &port.to_string(),
            "--db",
            dir.join("context.db").to_str().expect("utf8 db path"),
            "--config",
            dir.join("config.json").to_str().expect("utf8 config path"),
        ])
        .current_dir(&dir)
        .env_clear()
        .env("PATH", &dir)
        .env("HOME", &dir)
        .env(
            "PHILOTIC_HOTEL_SOCKET",
            socket_path.to_str().expect("utf8 socket path"),
        )
        .env("PHILOTIC_WEB_EDGE_INVITE", &invite)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn philotic-web serve");

    let server = TestServer {
        port,
        invite,
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("build http client"),
        dir,
        hotel_task,
        _child: child,
    };
    server.wait_until_ready().await;
    server
}

impl TestServer {
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    async fn wait_until_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(response) = self.http.get(self.url("/health")).send().await {
                if response.status().is_success() {
                    return;
                }
            }
            assert!(
                Instant::now() < deadline,
                "philotic-web did not become ready on port {} within 30s",
                self.port
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn enroll(&self, pubkey: &str) -> EnrollmentResponse {
        let response = self
            .http
            .post(self.url("/api/edge/enroll"))
            .json(&EnrollmentRequest {
                invite_code: self.invite.clone(),
                device_pubkey_b64: pubkey.to_string(),
                device_name: "E2E Test iPhone".into(),
                platform: "ios".into(),
            })
            .send()
            .await
            .expect("enroll request");
        assert_eq!(response.status(), 200, "enrollment should succeed");
        response.json().await.expect("enrollment response body")
    }

    async fn ws_connect(&self, bearer: &str) -> Result<WsStream, WsError> {
        let mut request = format!("ws://127.0.0.1:{}/api/edge/ws", self.port)
            .into_client_request()
            .expect("ws request");
        request.headers_mut().insert(
            AUTHORIZATION,
            format!("Bearer {bearer}").parse().expect("bearer header"),
        );
        connect_async(request).await.map(|(ws, _)| ws)
    }

    /// Enroll a device, open the WS, and complete the Hello handshake.
    /// Returns the open stream and the enrollment.
    async fn open_session(
        &self,
        pubkey: &str,
        cursor: Option<&str>,
    ) -> (WsStream, EnrollmentResponse) {
        let enrollment = self.enroll(pubkey).await;
        let ws = self
            .hello(&enrollment.edge_token, &enrollment.node_id, cursor)
            .await;
        (ws, enrollment)
    }

    /// Open the WS with `bearer` and complete the Hello handshake for `node_id`.
    async fn hello(&self, bearer: &str, node_id: &str, cursor: Option<&str>) -> WsStream {
        let mut ws = self.ws_connect(bearer).await.expect("ws upgrade");
        send_envelope(&mut ws, 1, EdgeMessage::Hello(edge_hello(node_id, cursor))).await;
        let ack = recv_envelope(&mut ws).await;
        match &ack.msg {
            EdgeMessage::HelloAck {
                session_id,
                replay_from,
            } => {
                assert!(
                    session_id.starts_with("edge-sess-"),
                    "unexpected session id {session_id}"
                );
                assert_eq!(
                    replay_from.as_deref(),
                    cursor.filter(|c| c.parse::<u64>().is_ok())
                );
            }
            other => panic!("expected HelloAck, got {other:?}"),
        }
        assert_eq!(ack.v, PROTOCOL_VERSION);
        assert_eq!(ack.ack, Some(1), "HelloAck should ack the hello seq");
        ws
    }
}

// ── WS client helpers ─────────────────────────────────────────────────────────

fn edge_hello(node_id: &str, cursor: Option<&str>) -> EdgeHello {
    EdgeHello {
        node_id: node_id.to_string(),
        capabilities: EdgeCapabilities {
            device_name: "E2E Test iPhone".into(),
            platform: "ios".into(),
            roles: vec!["ClientNode".into()],
            tools: vec![],
            models: vec![],
        },
        cursor: cursor.map(str::to_string),
    }
}

async fn send_envelope(ws: &mut WsStream, seq: u64, msg: EdgeMessage) {
    let frame = serde_json::to_string(&EdgeEnvelope::new(seq, None, msg)).expect("encode envelope");
    ws.send(WsMessage::Text(frame)).await.expect("send frame");
}

async fn recv_envelope(ws: &mut WsStream) -> EdgeEnvelope {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(15), ws.next())
            .await
            .expect("timed out waiting for a websocket frame")
            .expect("websocket closed while waiting for a frame")
            .expect("websocket error while waiting for a frame");
        match message {
            WsMessage::Text(text) => {
                return serde_json::from_str(&text).expect("decode edge envelope")
            }
            WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
            other => panic!("unexpected non-text websocket frame: {other:?}"),
        }
    }
}

/// True when the stream yields Close / clean end / a closed-connection error
/// within the timeout — i.e. the server really tore the session down.
async fn assert_closed(ws: &mut WsStream) {
    loop {
        match tokio::time::timeout(Duration::from_secs(15), ws.next())
            .await
            .expect("timed out waiting for the server to close the socket")
        {
            None | Some(Ok(WsMessage::Close(_))) | Some(Err(_)) => return,
            Some(Ok(WsMessage::Ping(_))) | Some(Ok(WsMessage::Pong(_))) => continue,
            Some(Ok(other)) => panic!("expected close, got {other:?}"),
        }
    }
}

/// Receive TurnEvent frames until the Final event arrives; returns all of
/// them (including the Final) in arrival order.
async fn collect_turn_events_until_final(ws: &mut WsStream) -> Vec<EdgeEnvelope> {
    let mut events = Vec::new();
    loop {
        let envelope = recv_envelope(ws).await;
        let EdgeMessage::TurnEvent { event_kind, .. } = &envelope.msg else {
            panic!("expected TurnEvent, got {:?}", envelope.msg);
        };
        let done = *event_kind == TurnEventKind::Final;
        events.push(envelope);
        if done {
            return events;
        }
    }
}

fn turn_event_parts(envelope: &EdgeEnvelope) -> (TurnEventKind, &str, &str) {
    match &envelope.msg {
        EdgeMessage::TurnEvent {
            conversation_id,
            event_kind,
            content,
            ..
        } => (*event_kind, conversation_id.as_str(), content.as_str()),
        other => panic!("expected TurnEvent, got {other:?}"),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// 1. enroll → token → ws connect → Hello → HelloAck happy path
///    (plus the invite-code gate on the enrollment route itself).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enroll_then_hello_handshake_happy_path() {
    let server = spawn_server().await;

    // A wrong invite code is rejected before any device is created.
    let forbidden = server
        .http
        .post(server.url("/api/edge/enroll"))
        .json(&EnrollmentRequest {
            invite_code: "WRONG-INVITE".into(),
            device_pubkey_b64: "cHVia2V5".into(),
            device_name: "E2E Test iPhone".into(),
            platform: "ios".into(),
        })
        .send()
        .await
        .expect("enroll request");
    assert_eq!(forbidden.status(), 403);

    let enrollment = server.enroll("cHVia2V5LWhhcHB5").await;
    assert!(enrollment.node_id.starts_with("edge-"));
    assert!(enrollment.edge_token.starts_with("edge-tok-"));

    // Full handshake over a real WebSocket (asserts HelloAck internally).
    let mut ws = server
        .hello(&enrollment.edge_token, &enrollment.node_id, None)
        .await;

    // The session is live: Ping → Pong.
    send_envelope(&mut ws, 2, EdgeMessage::Ping { nonce: 41 }).await;
    let pong = recv_envelope(&mut ws).await;
    assert_eq!(
        pong.msg,
        EdgeMessage::Pong { nonce: 41 },
        "session should answer Ping with Pong"
    );
    ws.close(None).await.ok();
}

/// 2. A non-Hello first frame is a protocol violation: fatal Error, then close.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn frame_before_hello_is_fatal() {
    let server = spawn_server().await;
    let enrollment = server.enroll("cHVia2V5LWZhdGFs").await;

    let mut ws = server
        .ws_connect(&enrollment.edge_token)
        .await
        .expect("ws upgrade");
    send_envelope(&mut ws, 1, EdgeMessage::Ping { nonce: 1 }).await;

    let error = recv_envelope(&mut ws).await;
    match error.msg {
        EdgeMessage::Error { code, fatal, .. } => {
            assert_eq!(code, "protocol");
            assert!(fatal, "pre-hello violations must be fatal");
        }
        other => panic!("expected fatal Error frame, got {other:?}"),
    }
    assert_closed(&mut ws).await;
}

/// 3. A bad (or missing) bearer is rejected at the HTTP upgrade with 401.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bad_bearer_rejected_at_upgrade() {
    let server = spawn_server().await;
    // Enroll a real device so the registry is non-empty — the bogus token
    // must be rejected against actual candidates, not an empty registry.
    let enrollment = server.enroll("cHVia2V5LWF1dGg").await;

    let denied = server
        .ws_connect("edge-tok-completely-bogus")
        .await
        .expect_err("bogus bearer must not upgrade");
    match denied {
        WsError::Http(response) => assert_eq!(response.status(), 401),
        other => panic!("expected HTTP 401 rejection, got {other:?}"),
    }

    // No Authorization header at all is also rejected at upgrade.
    let request = format!("ws://127.0.0.1:{}/api/edge/ws", server.port)
        .into_client_request()
        .expect("ws request");
    match connect_async(request).await {
        Err(WsError::Http(response)) => assert_eq!(response.status(), 401),
        other => panic!("expected HTTP 401 rejection, got {other:?}"),
    }

    // Sanity: the enrolled token still upgrades fine on the same server.
    let mut ws = server
        .hello(&enrollment.edge_token, &enrollment.node_id, None)
        .await;
    ws.close(None).await.ok();
}

/// 4. TurnSubmit produces the full TurnEvent stream from the (fake) hotel:
///    accepted status → thinking status → streamed tokens → final.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn turn_submit_streams_turn_events() {
    let server = spawn_server().await;
    let (mut ws, _enrollment) = server.open_session("cHVia2V5LXR1cm4", None).await;

    send_envelope(
        &mut ws,
        2,
        EdgeMessage::TurnSubmit {
            target_node_id: LOCAL_TARGET_NODE.into(),
            target_agent_id: "jane".into(),
            conversation_id: Some("conv-e2e".into()),
            content: "hi there".into(),
            blob_refs: vec![],
            message_kind: None,
        },
    )
    .await;

    let events = collect_turn_events_until_final(&mut ws).await;
    let parts: Vec<(TurnEventKind, &str, &str)> = events.iter().map(turn_event_parts).collect();
    assert_eq!(
        parts,
        vec![
            (TurnEventKind::Status, "conv-e2e", "accepted"),
            (TurnEventKind::Status, "conv-e2e", "thinking"),
            (TurnEventKind::Token, "conv-e2e", CANNED_TOKENS[0]),
            (TurnEventKind::Token, "conv-e2e", CANNED_TOKENS[1]),
            (TurnEventKind::Final, "conv-e2e", CANNED_FINAL),
        ],
        "unexpected turn event stream: {events:#?}"
    );

    // Outbound seqs are strictly increasing, and every frame carries a turn id.
    for pair in events.windows(2) {
        assert!(pair[0].seq < pair[1].seq, "seqs must increase: {events:#?}");
    }
    for envelope in &events {
        match &envelope.msg {
            EdgeMessage::TurnEvent { turn_id, .. } => {
                assert!(turn_id.is_some(), "turn events should carry the turn id")
            }
            _ => unreachable!(),
        }
    }
    ws.close(None).await.ok();
}

/// 5. Disconnect, then reconnect with a cursor: the ring buffer replays every
///    retained frame after the cursor, byte-identical to the live delivery.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_with_cursor_replays_missed_frames() {
    let server = spawn_server().await;
    let (mut ws, enrollment) = server.open_session("cHVia2V5LXJlcGxheQ", None).await;

    // Session A: run one full turn. The client never sends an `ack`, so all
    // five TurnEvent frames stay retained in the server's replay ring.
    send_envelope(
        &mut ws,
        2,
        EdgeMessage::TurnSubmit {
            target_node_id: LOCAL_TARGET_NODE.into(),
            target_agent_id: "jane".into(),
            conversation_id: Some("conv-replay".into()),
            content: "stream me".into(),
            blob_refs: vec![],
            message_kind: None,
        },
    )
    .await;
    let live = collect_turn_events_until_final(&mut ws).await;
    assert_eq!(live.len(), 5, "expected 5 turn events: {live:#?}");

    // Drop the connection mid-conversation (from the client's perspective the
    // device went offline after durably processing only the first two frames).
    ws.close(None).await.ok();
    drop(ws);

    // Session B: reconnect presenting the cursor of the last processed frame.
    let cursor = live[1].seq.to_string();
    let mut ws = server
        .hello(&enrollment.edge_token, &enrollment.node_id, Some(&cursor))
        .await;

    // Everything after the cursor replays, in order, identical to the live
    // frames (same seqs, same payloads).
    for expected in &live[2..] {
        let replayed = recv_envelope(&mut ws).await;
        assert_eq!(&replayed, expected, "replayed frame must match live frame");
    }

    // Replay is complete — the session is live again and quiescent: the next
    // frame after a Ping is its Pong, not more replay.
    send_envelope(&mut ws, 2, EdgeMessage::Ping { nonce: 99 }).await;
    let pong = recv_envelope(&mut ws).await;
    assert_eq!(pong.msg, EdgeMessage::Pong { nonce: 99 });

    // A cursor at the newest retained frame replays nothing.
    ws.close(None).await.ok();
    drop(ws);
    let newest = live.last().expect("live frames").seq.to_string();
    let mut ws = server
        .hello(&enrollment.edge_token, &enrollment.node_id, Some(&newest))
        .await;
    send_envelope(&mut ws, 2, EdgeMessage::Ping { nonce: 7 }).await;
    let pong = recv_envelope(&mut ws).await;
    assert_eq!(
        pong.msg,
        EdgeMessage::Pong { nonce: 7 },
        "no frames should replay from the newest cursor"
    );
    ws.close(None).await.ok();
}

/// 6. Turn events broadcast while the device is DISCONNECTED are still
///    retained and recoverable: the client submits a turn, sees only the
///    `accepted` status, drops the connection before the agent streams
///    anything, and reconnects with the accepted frame's seq as its cursor —
///    the thinking/token/final events that fired during the gap must all
///    arrive (via replay and/or live delivery, deduplicated, in seq order).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn events_broadcast_while_disconnected_are_recovered_on_reconnect() {
    let server = spawn_server().await;
    let (mut ws, enrollment) = server.open_session("cHVia2V5LW9mZmxpbmU", None).await;

    send_envelope(
        &mut ws,
        2,
        EdgeMessage::TurnSubmit {
            target_node_id: LOCAL_TARGET_NODE.into(),
            target_agent_id: "jane".into(),
            conversation_id: Some("conv-offline".into()),
            content: "answer while I am offline".into(),
            blob_refs: vec![],
            message_kind: None,
        },
    )
    .await;

    // Read ONLY the accepted status, then drop the socket immediately — the
    // WiFi-flap window the store-and-forward tier exists to cover.
    let accepted = recv_envelope(&mut ws).await;
    assert_eq!(
        turn_event_parts(&accepted).0,
        TurnEventKind::Status,
        "first event should be the accepted status: {accepted:#?}"
    );
    ws.close(None).await.ok();
    drop(ws);

    // Give the fake hotel time to finish streaming into the void.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Reconnect from the last processed frame.
    let cursor = accepted.seq.to_string();
    let mut ws = server
        .hello(&enrollment.edge_token, &enrollment.node_id, Some(&cursor))
        .await;
    let recovered = collect_turn_events_until_final(&mut ws).await;
    let parts: Vec<(TurnEventKind, &str, &str)> = recovered.iter().map(turn_event_parts).collect();
    assert_eq!(
        parts,
        vec![
            (TurnEventKind::Status, "conv-offline", "thinking"),
            (TurnEventKind::Token, "conv-offline", CANNED_TOKENS[0]),
            (TurnEventKind::Token, "conv-offline", CANNED_TOKENS[1]),
            (TurnEventKind::Final, "conv-offline", CANNED_FINAL),
        ],
        "events from the disconnect window must be recovered exactly once: {recovered:#?}"
    );
    for pair in recovered.windows(2) {
        assert!(
            pair[0].seq < pair[1].seq,
            "recovered seqs must increase: {recovered:#?}"
        );
    }
    assert!(
        recovered[0].seq > accepted.seq,
        "recovery must start after the presented cursor"
    );
    ws.close(None).await.ok();
}

/// 7. Only one live session per node: a second Hello for the same node kicks
///    the first session, and the new session receives each turn event exactly
///    once (no duplicate retention from a zombie session).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn second_hello_kicks_previous_session_and_avoids_duplicates() {
    let server = spawn_server().await;
    let (mut first, enrollment) = server.open_session("cHVia2V5LWtpY2s", None).await;

    // Second connection for the same node — the server must supersede the
    // first session (which we deliberately leave open, mimicking half-open TCP).
    let mut second = server
        .hello(&enrollment.edge_token, &enrollment.node_id, None)
        .await;
    assert_closed(&mut first).await;

    // A turn on the surviving session streams each event exactly once.
    send_envelope(
        &mut second,
        2,
        EdgeMessage::TurnSubmit {
            target_node_id: LOCAL_TARGET_NODE.into(),
            target_agent_id: "jane".into(),
            conversation_id: Some("conv-kick".into()),
            content: "no doubles please".into(),
            blob_refs: vec![],
            message_kind: None,
        },
    )
    .await;
    let events = collect_turn_events_until_final(&mut second).await;
    let parts: Vec<(TurnEventKind, &str, &str)> = events.iter().map(turn_event_parts).collect();
    assert_eq!(
        parts,
        vec![
            (TurnEventKind::Status, "conv-kick", "accepted"),
            (TurnEventKind::Status, "conv-kick", "thinking"),
            (TurnEventKind::Token, "conv-kick", CANNED_TOKENS[0]),
            (TurnEventKind::Token, "conv-kick", CANNED_TOKENS[1]),
            (TurnEventKind::Final, "conv-kick", CANNED_FINAL),
        ],
        "kicked sessions must not duplicate retained frames: {events:#?}"
    );
    second.close(None).await.ok();
}
