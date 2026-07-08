//! Edge WebSocket termination — device enrollment + the edge-mesh session mux.
//!
//! This module is the hotel-side termination point for the edge-mesh protocol
//! (`philotic-edge-protocol`): the client-server tier spoken by edge devices
//! (the iOS/macOS app). Edge devices never hold backbone MeshAuth PSKs — they
//! authenticate with a per-device bearer token issued at enrollment.
//!
//! ## Surfaces
//!
//! - `POST /api/edge/enroll` — one-time device enrollment. The operator-issued
//!   invite code (`PHILOTIC_WEB_EDGE_INVITE` env / config `web_edge_invite`)
//!   is the credential for this route, so the perimeter fence deliberately
//!   leaves it reachable without a session. Successful enrollment persists the
//!   device (node_id, device pubkey, name, platform) to a small JSON store
//!   next to the hotel context DB (`edge-devices.json`, mode 0600) and returns
//!   an [`EnrollmentResponse`] whose `edge_token` is accepted by the same
//!   bearer path as the shared `PHILOTIC_WEB_EDGE_TOKEN`.
//! - `GET /api/edge/ws` — bearer-authenticated WebSocket speaking JSON
//!   [`EdgeEnvelope`] text frames. The first client frame MUST be `Hello`;
//!   the server validates the node is enrolled (and, when the bearer is a
//!   device token, that the hello's `node_id` matches the token's device),
//!   then replies `HelloAck { session_id, replay_from }`.
//!
//! ## Session mux (v0)
//!
//! - `TurnSubmit` drives the same hotel IPC path as the HTTP chat handler
//!   (`POST /api/mesh/targets/:node/agents/:agent/chat`): an `EmitTask` to the
//!   target agent with a `SubscribeInbox` reply leg. Reply traffic fans out on
//!   the shared broadcast bus tagged with an `operator_session_id` of
//!   `edge:{node_id}`; this session filters those events and re-emits them as
//!   `TurnEvent` frames (`token` / `final` / `status` / `error`).
//! - `Ping` → `Pong` (and WS-level pings are answered with WS pongs).
//! - `ApprovalResolve` / `ToolResult` / `CapabilitiesUpdate` are accepted and
//!   logged — routing for these lands in a later slice.
//! - Undecodable or unknown frames get a non-fatal `Error` frame; protocol
//!   violations (non-Hello first frame, duplicate `Hello`, wrong protocol
//!   version, client sending server-only messages) are fatal and close the
//!   connection.
//!
//! ## Cursor / replay (v0 — TRANSITIONAL)
//!
//! Outbound envelopes are seq-numbered per edge node from an **in-memory ring
//! buffer** ([`EDGE_RING_CAPACITY`] retained frames per node). On `Hello` with
//! a cursor, buffered frames after that cursor are replayed. In v0 the cursor
//! is the decimal string of the last envelope `seq` the client durably
//! processed (clients should still treat it as opaque). Control frames
//! (`HelloAck`, `Pong`, `Error`) consume a seq but are not retained, so
//! replayed streams may legitimately skip seq values.
//!
//! Retention is **independent of live sessions**: [`spawn_edge_retainer`]
//! runs one process-wide task that subscribes to the broadcast bus and mints
//! every `edge:{node_id}`-tagged operator-chat event into that node's ring,
//! so turn events broadcast while the device is disconnected are still
//! replayable on the next cursor-bearing `Hello`. Live sessions deliver
//! retained frames from the per-node delivery channel (in seq order, deduped
//! against the replay) rather than translating broadcasts themselves.
//!
//! At most one live session per edge node: a new accepted `Hello` for a node
//! kicks any previous session (classic half-open-TCP reconnect race), so a
//! zombie session can never double-deliver or consume seqs.
//!
//! Enrollment is throttled: after [`MAX_ENROLL_FAILURES`] bad invite codes
//! within [`ENROLL_FAILURE_WINDOW`] the endpoint answers 429 for the rest of
//! the window, bounding invite brute-force. The perimeter fence additionally
//! refuses `POST /api/edge/enroll` on an Internet-tier bind.
//!
//! TODO(apple-edge-client/edge-cursor-ledger): this ring buffer is a named
//! follow-up seam. The durable design routes server→edge events through
//! aiua's `EventStorage` (`mesh_events`) with per-device ACK cursors in
//! `CursorStorage` (`mesh_cursors`) and ledger-minted opaque cursors, so
//! replay survives philotic-web restarts. Replace `EdgeState::rings` (and the
//! cursor parse in [`process_hello`]) when that lands; the wire contract
//! already treats cursors as opaque.

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use philotic_edge_protocol::{
    EdgeEnvelope, EdgeMessage, EnrollmentRequest, EnrollmentResponse, TurnEventKind,
    PROTOCOL_VERSION,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, Notify};

use super::AppState;

/// Retained outbound frames per edge node. Oldest frames are evicted first;
/// a client resuming from a cursor older than the window simply misses them
/// (v0 limitation — see the module docs' cursor-ledger seam).
pub(crate) const EDGE_RING_CAPACITY: usize = 256;

/// How long the server waits for the client's `Hello` after the upgrade.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// Bad-invite budget: after this many failed invite comparisons within
/// [`ENROLL_FAILURE_WINDOW`], enrollment answers 429 until the window drains.
pub(crate) const MAX_ENROLL_FAILURES: usize = 5;

/// Sliding window for [`MAX_ENROLL_FAILURES`].
pub(crate) const ENROLL_FAILURE_WINDOW: Duration = Duration::from_secs(60);

// ── Device registry + per-node outbound rings ────────────────────────────────

/// One enrolled edge device, persisted to the JSON store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct EdgeDeviceRecord {
    pub node_id: String,
    pub device_pubkey_b64: String,
    pub device_name: String,
    pub platform: String,
    /// Bearer token the device presents on every connection.
    pub edge_token: String,
    pub enrolled_at_ms: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct EdgeRegistryFile {
    devices: Vec<EdgeDeviceRecord>,
}

#[derive(Debug)]
struct EdgeRing {
    /// Last seq handed out for this node (0 = none yet).
    next_seq: u64,
    /// Retained (seq, serialized envelope) pairs, oldest first.
    entries: VecDeque<(u64, String)>,
    /// Live-delivery fanout for retained frames: whichever session is
    /// currently open for this node forwards `(seq, frame)` pairs from here
    /// (frames are minted under the rings lock, so channel order == seq
    /// order). Sending with no subscriber is fine — the frame stays retained
    /// in `entries` for replay.
    delivery: broadcast::Sender<(u64, String)>,
}

impl Default for EdgeRing {
    fn default() -> Self {
        let (delivery, _) = broadcast::channel(EDGE_RING_CAPACITY);
        Self {
            next_seq: 0,
            entries: VecDeque::new(),
            delivery,
        }
    }
}

/// Shared edge-tier state hanging off [`AppState`]: the enrolled-device
/// registry (JSON-file backed) and the per-node in-memory outbound rings.
///
/// Locks are `std::sync::Mutex` and are never held across an await point.
#[derive(Clone)]
pub(crate) struct EdgeState {
    registry_path: Arc<PathBuf>,
    /// Operator-issued invite code gating enrollment; `None` disables it.
    invite: Option<Arc<String>>,
    registry: Arc<Mutex<Vec<EdgeDeviceRecord>>>,
    rings: Arc<Mutex<HashMap<String, EdgeRing>>>,
    /// One kick handle per node with a live WS session — a new accepted
    /// `Hello` replaces (and notifies) the previous session's handle.
    sessions: Arc<Mutex<HashMap<String, Arc<Notify>>>>,
    /// Timestamps of recent failed invite comparisons (enrollment throttle).
    enroll_failures: Arc<Mutex<VecDeque<Instant>>>,
}

#[derive(Debug)]
pub(crate) enum EnrollError {
    /// No invite code configured — enrollment is disabled on this hotel.
    Disabled,
    /// Presented invite code does not match the configured one.
    BadInvite,
    /// Too many recent failed invite attempts — try again later.
    Throttled,
    InvalidRequest(String),
    Storage(anyhow::Error),
}

/// Who the edge bearer token identifies.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum EdgeBearerIdentity {
    /// The shared PHILOTIC_WEB_EDGE_TOKEN / config `web_edge_token`.
    Shared,
    /// A per-device token issued at enrollment — carries the device node id.
    Device(String),
}

impl EdgeState {
    /// Load the device registry from `registry_path` (missing file = empty
    /// registry; a corrupt file is logged and treated as empty).
    pub(crate) fn load(registry_path: PathBuf, invite: Option<String>) -> Self {
        let devices = match std::fs::read_to_string(&registry_path) {
            Ok(raw) => match serde_json::from_str::<EdgeRegistryFile>(&raw) {
                Ok(file) => file.devices,
                Err(err) => {
                    eprintln!(
                        "philotic-web: ignoring corrupt edge device registry {}: {err}",
                        registry_path.display()
                    );
                    Vec::new()
                }
            },
            Err(_) => Vec::new(),
        };
        Self {
            registry_path: Arc::new(registry_path),
            invite: invite.map(Arc::new),
            registry: Arc::new(Mutex::new(devices)),
            rings: Arc::new(Mutex::new(HashMap::new())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            enroll_failures: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub(crate) fn enrollment_enabled(&self) -> bool {
        self.invite.is_some()
    }

    pub(crate) fn device_count(&self) -> usize {
        self.registry.lock().expect("edge registry lock").len()
    }

    /// Enroll (or re-enroll) a device. Re-enrolling the same `device_pubkey_b64`
    /// keeps the node_id stable and rotates the token (recovery UX: a wiped
    /// device re-enrolls with the same invite and the old token dies).
    pub(crate) fn enroll(
        &self,
        request: &EnrollmentRequest,
    ) -> Result<EnrollmentResponse, EnrollError> {
        let Some(expected_invite) = self.invite.as_deref() else {
            return Err(EnrollError::Disabled);
        };
        if self.enroll_throttled() {
            return Err(EnrollError::Throttled);
        }
        if !super::constant_time_eq(
            request.invite_code.trim().as_bytes(),
            expected_invite.as_bytes(),
        ) {
            self.record_enroll_failure();
            return Err(EnrollError::BadInvite);
        }
        let pubkey = request.device_pubkey_b64.trim();
        if pubkey.is_empty() {
            return Err(EnrollError::InvalidRequest(
                "device_pubkey_b64 must not be empty".into(),
            ));
        }
        let device_name = request.device_name.trim();
        if device_name.is_empty() {
            return Err(EnrollError::InvalidRequest(
                "device_name must not be empty".into(),
            ));
        }

        let edge_token = format!("edge-tok-{}", random_hex(32));
        let enrolled_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let mut registry = self.registry.lock().expect("edge registry lock");
        let node_id = match registry
            .iter_mut()
            .find(|record| record.device_pubkey_b64 == pubkey)
        {
            Some(existing) => {
                existing.edge_token = edge_token.clone();
                existing.device_name = device_name.to_string();
                existing.platform = request.platform.trim().to_string();
                existing.enrolled_at_ms = enrolled_at_ms;
                existing.node_id.clone()
            }
            None => {
                let node_id = format!("edge-{}", random_hex(8));
                registry.push(EdgeDeviceRecord {
                    node_id: node_id.clone(),
                    device_pubkey_b64: pubkey.to_string(),
                    device_name: device_name.to_string(),
                    platform: request.platform.trim().to_string(),
                    edge_token: edge_token.clone(),
                    enrolled_at_ms,
                });
                node_id
            }
        };
        persist_registry(&self.registry_path, &registry).map_err(EnrollError::Storage)?;
        drop(registry);

        Ok(EnrollmentResponse {
            node_id,
            edge_token,
        })
    }

    /// True when the bad-invite budget for the current window is exhausted.
    fn enroll_throttled(&self) -> bool {
        let mut failures = self.enroll_failures.lock().expect("enroll failures lock");
        if let Some(cutoff) = Instant::now().checked_sub(ENROLL_FAILURE_WINDOW) {
            while failures.front().is_some_and(|at| *at < cutoff) {
                failures.pop_front();
            }
        }
        failures.len() >= MAX_ENROLL_FAILURES
    }

    fn record_enroll_failure(&self) {
        self.enroll_failures
            .lock()
            .expect("enroll failures lock")
            .push_back(Instant::now());
    }

    pub(crate) fn is_enrolled(&self, node_id: &str) -> bool {
        self.registry
            .lock()
            .expect("edge registry lock")
            .iter()
            .any(|record| record.node_id == node_id)
    }

    /// Resolve a presented bearer token to the enrolled device's node id.
    /// Each candidate comparison is constant-time.
    pub(crate) fn device_node_for_token(&self, presented: &str) -> Option<String> {
        self.registry
            .lock()
            .expect("edge registry lock")
            .iter()
            .find(|record| {
                super::constant_time_eq(presented.as_bytes(), record.edge_token.as_bytes())
            })
            .map(|record| record.node_id.clone())
    }

    /// Mint the next outbound envelope for `node_id`, serialize it, and (when
    /// `retain` is true) keep it in the replay ring and fan it out on the
    /// node's live-delivery channel. Control frames pass `retain: false`:
    /// they consume a seq but are not replayed.
    pub(crate) fn outbound_envelope(
        &self,
        node_id: &str,
        ack: Option<u64>,
        msg: EdgeMessage,
        retain: bool,
    ) -> (u64, String) {
        let mut rings = self.rings.lock().expect("edge rings lock");
        let ring = rings.entry(node_id.to_string()).or_default();
        ring.next_seq += 1;
        let seq = ring.next_seq;
        let frame = serde_json::to_string(&EdgeEnvelope::new(seq, ack, msg))
            .expect("edge envelope serialization is infallible");
        if retain {
            ring.entries.push_back((seq, frame.clone()));
            while ring.entries.len() > EDGE_RING_CAPACITY {
                ring.entries.pop_front();
            }
            // Still under the rings lock, so delivery order == seq order.
            // No live session subscribed is fine: the frame stays retained.
            let _ = ring.delivery.send((seq, frame.clone()));
        }
        (seq, frame)
    }

    /// Retained `(seq, frame)` pairs with seq strictly greater than `cursor`,
    /// oldest first.
    pub(crate) fn replay_after(&self, node_id: &str, cursor: u64) -> Vec<(u64, String)> {
        let rings = self.rings.lock().expect("edge rings lock");
        let Some(ring) = rings.get(node_id) else {
            return Vec::new();
        };
        ring.entries
            .iter()
            .filter(|(seq, _)| *seq > cursor)
            .cloned()
            .collect()
    }

    /// Subscribe to the node's live-delivery channel (retained frames only,
    /// in seq order).
    pub(crate) fn subscribe_delivery(&self, node_id: &str) -> broadcast::Receiver<(u64, String)> {
        let mut rings = self.rings.lock().expect("edge rings lock");
        rings
            .entry(node_id.to_string())
            .or_default()
            .delivery
            .subscribe()
    }

    /// Register the calling session as the node's single live session,
    /// kicking (notifying) any previous one. The returned handle fires when
    /// a newer session takes over.
    pub(crate) fn begin_session(&self, node_id: &str) -> Arc<Notify> {
        let notify = Arc::new(Notify::new());
        let mut sessions = self.sessions.lock().expect("edge sessions lock");
        if let Some(previous) = sessions.insert(node_id.to_string(), notify.clone()) {
            previous.notify_one();
        }
        notify
    }

    /// Drop retained frames the client has acknowledged as durably received.
    pub(crate) fn acknowledge_up_to(&self, node_id: &str, seq: u64) {
        let mut rings = self.rings.lock().expect("edge rings lock");
        if let Some(ring) = rings.get_mut(node_id) {
            while ring.entries.front().is_some_and(|(s, _)| *s <= seq) {
                ring.entries.pop_front();
            }
        }
    }
}

fn random_hex(len: usize) -> String {
    let mut bytes = vec![0u8; len];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Atomic-ish persist: write a temp file next to the store, chmod 0600, rename.
fn persist_registry(path: &PathBuf, devices: &[EdgeDeviceRecord]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let tmp = path.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(&EdgeRegistryFile {
        devices: devices.to_vec(),
    })?;
    std::fs::write(&tmp, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ── Bearer identity ───────────────────────────────────────────────────────────

/// Resolve the presented bearer to an edge identity: the shared edge token
/// (checked first) or a per-device enrollment token. All comparisons are
/// constant-time.
pub(crate) fn edge_bearer_identity(
    headers: &HeaderMap,
    state: &AppState,
) -> Option<EdgeBearerIdentity> {
    let presented = super::header_bearer_token(headers)?.trim();
    if let Some(expected) = state.edge_token.as_deref() {
        if super::constant_time_eq(presented.as_bytes(), expected.as_bytes()) {
            return Some(EdgeBearerIdentity::Shared);
        }
    }
    state
        .edge
        .device_node_for_token(presented)
        .map(EdgeBearerIdentity::Device)
}

// ── POST /api/edge/enroll ─────────────────────────────────────────────────────

pub(crate) async fn handle_edge_enroll(
    State(state): State<AppState>,
    Json(request): Json<EnrollmentRequest>,
) -> Response {
    match state.edge.enroll(&request) {
        Ok(response) => {
            println!(
                "philotic-web: enrolled edge device {} ({} / {})",
                response.node_id,
                request.device_name.trim(),
                request.platform.trim()
            );
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(EnrollError::Disabled) => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": "edge enrollment is not configured on this hotel (set PHILOTIC_WEB_EDGE_INVITE or config web_edge_invite)"})),
        )
            .into_response(),
        Err(EnrollError::BadInvite) => (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "invalid invite code"})),
        )
            .into_response(),
        Err(EnrollError::Throttled) => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error": "too many failed enrollment attempts — try again later"})),
        )
            .into_response(),
        Err(EnrollError::InvalidRequest(message)) => {
            (StatusCode::BAD_REQUEST, Json(json!({"error": message}))).into_response()
        }
        Err(EnrollError::Storage(err)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": format!("failed to persist edge device registry: {err}")})),
        )
            .into_response(),
    }
}

// ── GET /api/edge/ws ──────────────────────────────────────────────────────────

pub(crate) async fn handle_edge_ws(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    // Deliberately bearer-only: operator UI cookies do not open the edge mux.
    let Some(auth) = edge_bearer_identity(&headers, &state) else {
        return super::unauthorized();
    };
    ws.on_upgrade(move |socket| edge_ws_session(socket, state, auth))
}

/// Outcome of a successful handshake ([`process_hello`]).
#[derive(Debug, PartialEq)]
struct HandshakeAccept {
    node_id: String,
    session_id: String,
    /// The client's hello envelope seq — seeds ack tracking.
    client_seq: u64,
    /// Echo of the client's cursor when a replay will occur.
    replay_from: Option<String>,
    /// Parsed ring position the replay starts after.
    replay_from_seq: Option<u64>,
}

/// Handshake rejection — always fatal, connection closes after the Error frame.
#[derive(Debug, PartialEq)]
struct HandshakeReject {
    code: &'static str,
    message: String,
}

/// Validate the first client frame: it must be a version-1 `Hello` envelope
/// from an enrolled node; a device-token bearer must match the hello node.
fn process_hello(
    edge: &EdgeState,
    auth: &EdgeBearerIdentity,
    raw: &str,
) -> Result<HandshakeAccept, HandshakeReject> {
    let envelope: EdgeEnvelope = serde_json::from_str(raw).map_err(|err| HandshakeReject {
        code: "bad_frame",
        message: format!("undecodable hello envelope: {err}"),
    })?;
    if envelope.v != PROTOCOL_VERSION {
        return Err(HandshakeReject {
            code: "bad_version",
            message: format!(
                "unsupported protocol version {} (server speaks {PROTOCOL_VERSION})",
                envelope.v
            ),
        });
    }
    let EdgeMessage::Hello(hello) = envelope.msg else {
        return Err(HandshakeReject {
            code: "protocol",
            message: "first frame must be a hello message".into(),
        });
    };
    if !edge.is_enrolled(&hello.node_id) {
        return Err(HandshakeReject {
            code: "not_enrolled",
            message: format!("node [{}] is not enrolled on this hotel", hello.node_id),
        });
    }
    if let EdgeBearerIdentity::Device(token_node) = auth {
        if token_node != &hello.node_id {
            return Err(HandshakeReject {
                code: "node_mismatch",
                message: "bearer token is bound to a different edge node".into(),
            });
        }
    }
    // v0 cursor: decimal seq string (see module docs). An unparseable cursor
    // is treated as absent — no replay rather than a wrong one.
    let replay_from_seq = hello
        .cursor
        .as_deref()
        .and_then(|cursor| cursor.trim().parse::<u64>().ok());
    let replay_from = replay_from_seq.map(|seq| seq.to_string());
    Ok(HandshakeAccept {
        node_id: hello.node_id,
        session_id: format!("edge-sess-{}", random_hex(8)),
        client_seq: envelope.seq,
        replay_from,
        replay_from_seq,
    })
}

/// `operator_session_id` marker used for turns submitted by this edge node;
/// broadcast events tagged with it are routed back to the node's sessions.
fn edge_session_marker(node_id: &str) -> String {
    format!("edge:{node_id}")
}

async fn send_text(socket: &mut WebSocket, frame: String) -> bool {
    socket.send(Message::Text(frame)).await.is_ok()
}

/// Fatal error before a node identity is established (no ring): ad-hoc seq 1.
async fn send_prelude_fatal(socket: &mut WebSocket, code: &str, message: &str) {
    let envelope = EdgeEnvelope::new(
        1,
        None,
        EdgeMessage::Error {
            code: code.to_string(),
            message: message.to_string(),
            fatal: true,
        },
    );
    if let Ok(frame) = serde_json::to_string(&envelope) {
        let _ = socket.send(Message::Text(frame)).await;
    }
    let _ = socket.send(Message::Close(None)).await;
}

async fn edge_ws_session(mut socket: WebSocket, state: AppState, auth: EdgeBearerIdentity) {
    // ── Handshake: first frame must be Hello ────────────────────────────────
    let first = match tokio::time::timeout(HANDSHAKE_TIMEOUT, socket.recv()).await {
        Ok(Some(Ok(Message::Text(text)))) => text,
        Ok(Some(Ok(Message::Close(_)))) | Ok(None) | Ok(Some(Err(_))) => return,
        Ok(Some(Ok(_non_text))) => {
            send_prelude_fatal(
                &mut socket,
                "protocol",
                "first frame must be a text hello envelope",
            )
            .await;
            return;
        }
        Err(_elapsed) => {
            send_prelude_fatal(&mut socket, "handshake_timeout", "no hello frame received").await;
            return;
        }
    };
    let accept = match process_hello(&state.edge, &auth, &first) {
        Ok(accept) => accept,
        Err(reject) => {
            send_prelude_fatal(&mut socket, reject.code, &reject.message).await;
            return;
        }
    };
    let node_id = accept.node_id.clone();
    let marker = edge_session_marker(&node_id);
    let mut client_seq = accept.client_seq;

    // Subscribe to the node's delivery channel before acking so no retained
    // frame minted mid-setup is missed (the replay dedupe below handles any
    // frame that lands in both the replay snapshot and the channel), and
    // kick any previous live session for this node (half-open reconnects).
    let mut delivery = state.edge.subscribe_delivery(&node_id);
    let kick = state.edge.begin_session(&node_id);

    let (_, hello_ack) = state.edge.outbound_envelope(
        &node_id,
        Some(client_seq),
        EdgeMessage::HelloAck {
            session_id: accept.session_id.clone(),
            replay_from: accept.replay_from.clone(),
        },
        false,
    );
    if !send_text(&mut socket, hello_ack).await {
        return;
    }

    // Highest retained seq already sent on this session — dedupes the replay
    // against the live delivery channel.
    let mut last_sent_seq = accept.replay_from_seq.unwrap_or(0);

    // ── Cursor replay (v0 in-memory ring — see module docs) ─────────────────
    if let Some(cursor) = accept.replay_from_seq {
        for (seq, frame) in state.edge.replay_after(&node_id, cursor) {
            if !send_text(&mut socket, frame).await {
                return;
            }
            last_sent_seq = last_sent_seq.max(seq);
        }
    }

    // ── Session mux ─────────────────────────────────────────────────────────
    loop {
        tokio::select! {
            delivered = delivery.recv() => {
                match delivered {
                    Ok((seq, frame)) => {
                        if seq > last_sent_seq {
                            if !send_text(&mut socket, frame).await {
                                break;
                            }
                            last_sent_seq = seq;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        eprintln!(
                            "philotic-web: edge session for [{node_id}] lagged {skipped} delivery frames — resyncing from ring"
                        );
                        let mut send_failed = false;
                        for (seq, frame) in state.edge.replay_after(&node_id, last_sent_seq) {
                            if !send_text(&mut socket, frame).await {
                                send_failed = true;
                                break;
                            }
                            last_sent_seq = last_sent_seq.max(seq);
                        }
                        if send_failed {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            _ = kick.notified() => {
                eprintln!(
                    "philotic-web: edge session for [{node_id}] superseded by a newer connection"
                );
                break;
            }
            inbound = socket.recv() => {
                let Some(Ok(message)) = inbound else { break };
                match message {
                    Message::Close(_) => break,
                    Message::Ping(data) => {
                        if socket.send(Message::Pong(data)).await.is_err() {
                            break;
                        }
                    }
                    Message::Pong(_) => {}
                    Message::Binary(_) => {
                        let (_, frame) = state.edge.outbound_envelope(
                            &node_id,
                            Some(client_seq),
                            edge_error("bad_frame", "binary frames are not part of the edge protocol", false),
                            false,
                        );
                        if !send_text(&mut socket, frame).await {
                            break;
                        }
                    }
                    Message::Text(text) => {
                        match handle_edge_frame(&state, &node_id, &marker, &mut client_seq, &text)
                            .await
                        {
                            FrameOutcome::Continue => {}
                            FrameOutcome::Reply { msg, retain } => {
                                if retain {
                                    // Retained replies flow through the ring +
                                    // delivery channel so they are replayable
                                    // and stay in seq order with retainer
                                    // frames; the delivery arm sends them.
                                    let _ = state
                                        .edge
                                        .outbound_envelope(&node_id, Some(client_seq), msg, true);
                                } else {
                                    let (_, frame) = state
                                        .edge
                                        .outbound_envelope(&node_id, Some(client_seq), msg, false);
                                    if !send_text(&mut socket, frame).await {
                                        break;
                                    }
                                }
                            }
                            FrameOutcome::Fatal { code, message } => {
                                let (_, frame) = state.edge.outbound_envelope(
                                    &node_id,
                                    Some(client_seq),
                                    edge_error(code, &message, true),
                                    false,
                                );
                                let _ = send_text(&mut socket, frame).await;
                                break;
                            }
                            FrameOutcome::Close => break,
                        }
                    }
                }
            }
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}

/// Spawn the process-wide retention task: subscribes to the broadcast bus and
/// mints every operator-chat event tagged with an `edge:{node_id}` marker for
/// an enrolled node into that node's replay ring — whether or not the node
/// currently has a live session. This is what makes store-and-forward hold
/// across the disconnect window (the live session only forwards frames from
/// the delivery channel).
pub(crate) fn spawn_edge_retainer(
    edge: EdgeState,
    tx: broadcast::Sender<String>,
) -> tokio::task::JoinHandle<()> {
    let mut rx = tx.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(raw) => {
                    if let Some((node_id, msg)) = translate_edge_broadcast(&raw) {
                        if edge.is_enrolled(&node_id) {
                            let _ = edge.outbound_envelope(&node_id, None, msg, true);
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    eprintln!(
                        "philotic-web: edge retainer lagged {skipped} broadcast events — some edge frames were not retained"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

fn edge_error(code: &str, message: &str, fatal: bool) -> EdgeMessage {
    EdgeMessage::Error {
        code: code.to_string(),
        message: message.to_string(),
        fatal,
    }
}

enum FrameOutcome {
    Continue,
    Reply { msg: EdgeMessage, retain: bool },
    Fatal { code: &'static str, message: String },
    Close,
}

/// Handle one post-handshake client frame. Fatal outcomes are protocol
/// violations; undecodable frames only earn a non-fatal `Error` (a newer
/// client may legitimately speak message types this server does not know).
async fn handle_edge_frame(
    state: &AppState,
    node_id: &str,
    marker: &str,
    client_seq: &mut u64,
    raw: &str,
) -> FrameOutcome {
    let envelope: EdgeEnvelope = match serde_json::from_str(raw) {
        Ok(envelope) => envelope,
        Err(err) => {
            return FrameOutcome::Reply {
                msg: edge_error("bad_frame", &format!("undecodable envelope: {err}"), false),
                retain: false,
            };
        }
    };
    if envelope.v != PROTOCOL_VERSION {
        return FrameOutcome::Fatal {
            code: "bad_version",
            message: format!(
                "unsupported protocol version {} (server speaks {PROTOCOL_VERSION})",
                envelope.v
            ),
        };
    }
    if envelope.seq > *client_seq {
        *client_seq = envelope.seq;
    }
    if let Some(ack) = envelope.ack {
        state.edge.acknowledge_up_to(node_id, ack);
    }

    match envelope.msg {
        EdgeMessage::Hello(_) => FrameOutcome::Fatal {
            code: "protocol",
            message: "duplicate hello: the session is already open".into(),
        },
        EdgeMessage::Ping { nonce } => FrameOutcome::Reply {
            msg: EdgeMessage::Pong { nonce },
            retain: false,
        },
        EdgeMessage::Pong { .. } => FrameOutcome::Continue,
        EdgeMessage::TurnSubmit {
            target_node_id,
            target_agent_id,
            conversation_id,
            content,
            blob_refs,
        } => {
            if !blob_refs.is_empty() {
                eprintln!(
                    "philotic-web: edge turn from [{node_id}] carried {} blob ref(s) — ignored in v0",
                    blob_refs.len()
                );
            }
            let conversation_id = conversation_id
                .unwrap_or_else(|| format!("operator-chat:{marker}:{target_agent_id}"));
            match super::submit_operator_chat_turn(
                state,
                &target_node_id,
                &target_agent_id,
                marker,
                conversation_id.clone(),
                content,
            )
            .await
            {
                Ok(accepted) => FrameOutcome::Reply {
                    msg: EdgeMessage::TurnEvent {
                        conversation_id: accepted.conversation_id,
                        event_kind: TurnEventKind::Status,
                        content: "accepted".into(),
                        turn_id: Some(accepted.turn_id),
                    },
                    retain: true,
                },
                Err(err) => FrameOutcome::Reply {
                    msg: EdgeMessage::TurnEvent {
                        conversation_id,
                        event_kind: TurnEventKind::Error,
                        content: err.message,
                        turn_id: None,
                    },
                    retain: true,
                },
            }
        }
        // Accepted and logged only — approval/tool routing lands in a later slice.
        EdgeMessage::ApprovalResolve {
            approval_id,
            approved,
            note,
        } => {
            println!(
                "philotic-web: edge [{node_id}] approval_resolve {approval_id} approved={approved} note={note:?} (routing lands in a later slice)"
            );
            FrameOutcome::Continue
        }
        EdgeMessage::ToolResult {
            invocation_id, ok, ..
        } => {
            println!(
                "philotic-web: edge [{node_id}] tool_result {invocation_id} ok={ok} (routing lands in a later slice)"
            );
            FrameOutcome::Continue
        }
        EdgeMessage::CapabilitiesUpdate { capabilities } => {
            println!(
                "philotic-web: edge [{node_id}] capabilities update: {} tool(s), {} model(s) (not persisted in v0)",
                capabilities.tools.len(),
                capabilities.models.len()
            );
            FrameOutcome::Continue
        }
        EdgeMessage::Error {
            code,
            message,
            fatal,
        } => {
            eprintln!("philotic-web: edge [{node_id}] reported error {code}: {message}");
            if fatal {
                FrameOutcome::Close
            } else {
                FrameOutcome::Continue
            }
        }
        // Server-to-client messages arriving from a client are protocol violations.
        EdgeMessage::HelloAck { .. }
        | EdgeMessage::TurnEvent { .. }
        | EdgeMessage::ApprovalRequest { .. }
        | EdgeMessage::LifeGraphChange { .. }
        | EdgeMessage::VoiceBlob { .. }
        | EdgeMessage::ToolInvoke { .. } => FrameOutcome::Fatal {
            code: "protocol",
            message: "clients must not send server-to-client message types".into(),
        },
    }
}

/// Translate an operator-chat broadcast event addressed to ANY edge node
/// (marker `edge:{node_id}`) into `(node_id, TurnEvent)`. Used by the
/// process-wide retainer, which does not know a marker up front.
fn translate_edge_broadcast(raw: &str) -> Option<(String, EdgeMessage)> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let marker = value
        .get("payload")?
        .get("operator_session_id")?
        .as_str()?
        .to_string();
    let node_id = marker.strip_prefix("edge:")?.to_string();
    if node_id.is_empty() {
        return None;
    }
    let msg = translate_operator_broadcast_value(&marker, &value)?;
    Some((node_id, msg))
}

/// Translate an operator-chat broadcast event (as fanned out by
/// `stream_operator_chat_turn`) into a `TurnEvent` frame for the edge node
/// whose session marker is `marker`. Returns `None` for events that belong
/// to other sessions or other types.
fn translate_operator_broadcast_value(marker: &str, value: &Value) -> Option<EdgeMessage> {
    let kind = value.get("type")?.as_str()?;
    let payload = value.get("payload")?;
    if payload.get("operator_session_id").and_then(Value::as_str) != Some(marker) {
        return None;
    }
    let conversation_id = payload
        .get("conversation_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let turn_id = payload
        .get("turn_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let str_field = |key: &str, fallback: &str| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(fallback)
            .to_string()
    };
    let (event_kind, content) = match kind {
        "operator_chat:turn_event" => (TurnEventKind::Status, str_field("event", "unknown")),
        "operator_chat:partial_reply" => (TurnEventKind::Token, str_field("content", "")),
        "operator_chat:reply" => (TurnEventKind::Final, str_field("content", "")),
        "operator_chat:error" => (TurnEventKind::Error, str_field("message", "turn failed")),
        _ => return None,
    };
    Some(EdgeMessage::TurnEvent {
        conversation_id,
        event_kind,
        content,
        turn_id,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use philotic_edge_protocol::{EdgeCapabilities, EdgeHello};

    fn temp_registry_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "philotic-web-edge-{name}-{}.json",
            uuid::Uuid::new_v4()
        ))
    }

    fn edge_state(path: &PathBuf, invite: Option<&str>) -> EdgeState {
        EdgeState::load(path.clone(), invite.map(str::to_string))
    }

    fn enroll_request(pubkey: &str) -> EnrollmentRequest {
        EnrollmentRequest {
            invite_code: "INV-1".into(),
            device_pubkey_b64: pubkey.into(),
            device_name: "Jared's iPhone".into(),
            platform: "ios".into(),
        }
    }

    fn caps() -> EdgeCapabilities {
        EdgeCapabilities {
            device_name: "Jared's iPhone".into(),
            platform: "ios".into(),
            roles: vec![],
            tools: vec![],
            models: vec![],
        }
    }

    fn hello_frame(node_id: &str, cursor: Option<&str>) -> String {
        serde_json::to_string(&EdgeEnvelope::new(
            1,
            None,
            EdgeMessage::Hello(EdgeHello {
                node_id: node_id.into(),
                capabilities: caps(),
                cursor: cursor.map(str::to_string),
            }),
        ))
        .unwrap()
    }

    // ── Enrollment ──────────────────────────────────────────────────────────

    #[test]
    fn enrollment_round_trip_persists_and_authorizes() {
        let path = temp_registry_path("round-trip");
        let state = edge_state(&path, Some("INV-1"));

        let response = state.enroll(&enroll_request("cHVia2V5")).unwrap();
        assert!(response.node_id.starts_with("edge-"));
        assert!(response.edge_token.starts_with("edge-tok-"));
        assert!(state.is_enrolled(&response.node_id));
        assert_eq!(
            state.device_node_for_token(&response.edge_token),
            Some(response.node_id.clone())
        );

        // Registry survives a reload from disk.
        let reloaded = edge_state(&path, Some("INV-1"));
        assert!(reloaded.is_enrolled(&response.node_id));
        assert_eq!(
            reloaded.device_node_for_token(&response.edge_token),
            Some(response.node_id.clone())
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn enroll_rejects_bad_invite_and_disabled_enrollment() {
        let path = temp_registry_path("bad-invite");
        let state = edge_state(&path, Some("INV-1"));
        let mut request = enroll_request("cHVia2V5");
        request.invite_code = "WRONG".into();
        assert!(matches!(
            state.enroll(&request),
            Err(EnrollError::BadInvite)
        ));

        let disabled = edge_state(&path, None);
        assert!(matches!(
            disabled.enroll(&enroll_request("cHVia2V5")),
            Err(EnrollError::Disabled)
        ));
        assert!(!disabled.enrollment_enabled());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn enroll_rejects_empty_fields() {
        let path = temp_registry_path("empty-fields");
        let state = edge_state(&path, Some("INV-1"));
        let mut request = enroll_request("  ");
        assert!(matches!(
            state.enroll(&request),
            Err(EnrollError::InvalidRequest(_))
        ));
        request.device_pubkey_b64 = "cHVia2V5".into();
        request.device_name = "  ".into();
        assert!(matches!(
            state.enroll(&request),
            Err(EnrollError::InvalidRequest(_))
        ));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn re_enroll_same_pubkey_keeps_node_id_and_rotates_token() {
        let path = temp_registry_path("re-enroll");
        let state = edge_state(&path, Some("INV-1"));
        let first = state.enroll(&enroll_request("cHVia2V5")).unwrap();
        let second = state.enroll(&enroll_request("cHVia2V5")).unwrap();
        assert_eq!(first.node_id, second.node_id);
        assert_ne!(first.edge_token, second.edge_token);
        assert_eq!(state.device_node_for_token(&first.edge_token), None);
        assert_eq!(
            state.device_node_for_token(&second.edge_token),
            Some(second.node_id.clone())
        );
        assert_eq!(state.device_count(), 1);
        let _ = std::fs::remove_file(&path);
    }

    // ── Handshake ───────────────────────────────────────────────────────────

    #[test]
    fn handshake_requires_hello_first() {
        let path = temp_registry_path("hello-first");
        let state = edge_state(&path, Some("INV-1"));
        let ping =
            serde_json::to_string(&EdgeEnvelope::new(1, None, EdgeMessage::Ping { nonce: 7 }))
                .unwrap();
        let reject = process_hello(&state, &EdgeBearerIdentity::Shared, &ping).unwrap_err();
        assert_eq!(reject.code, "protocol");

        let garbage = process_hello(&state, &EdgeBearerIdentity::Shared, "not json").unwrap_err();
        assert_eq!(garbage.code, "bad_frame");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn handshake_rejects_wrong_protocol_version() {
        let path = temp_registry_path("bad-version");
        let state = edge_state(&path, Some("INV-1"));
        let enrolled = state.enroll(&enroll_request("cHVia2V5")).unwrap();
        let frame = hello_frame(&enrolled.node_id, None).replace("\"v\":1", "\"v\":99");
        let reject = process_hello(&state, &EdgeBearerIdentity::Shared, &frame).unwrap_err();
        assert_eq!(reject.code, "bad_version");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn handshake_rejects_unenrolled_node() {
        let path = temp_registry_path("unenrolled");
        let state = edge_state(&path, Some("INV-1"));
        let reject = process_hello(
            &state,
            &EdgeBearerIdentity::Shared,
            &hello_frame("edge-ghost", None),
        )
        .unwrap_err();
        assert_eq!(reject.code, "not_enrolled");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn handshake_binds_device_token_to_its_node() {
        let path = temp_registry_path("node-binding");
        let state = edge_state(&path, Some("INV-1"));
        let a = state.enroll(&enroll_request("device-a")).unwrap();
        let b = state.enroll(&enroll_request("device-b")).unwrap();

        // Device token for B presenting hello as A → rejected.
        let reject = process_hello(
            &state,
            &EdgeBearerIdentity::Device(b.node_id.clone()),
            &hello_frame(&a.node_id, None),
        )
        .unwrap_err();
        assert_eq!(reject.code, "node_mismatch");

        // Matching device token and the shared token both pass.
        let accept = process_hello(
            &state,
            &EdgeBearerIdentity::Device(a.node_id.clone()),
            &hello_frame(&a.node_id, None),
        )
        .unwrap();
        assert_eq!(accept.node_id, a.node_id);
        assert!(accept.session_id.starts_with("edge-sess-"));
        assert_eq!(accept.client_seq, 1);
        assert_eq!(accept.replay_from, None);

        let shared = process_hello(
            &state,
            &EdgeBearerIdentity::Shared,
            &hello_frame(&b.node_id, None),
        )
        .unwrap();
        assert_eq!(shared.node_id, b.node_id);
        assert_ne!(shared.session_id, accept.session_id);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn handshake_cursor_parses_or_is_ignored() {
        let path = temp_registry_path("cursor");
        let state = edge_state(&path, Some("INV-1"));
        let enrolled = state.enroll(&enroll_request("cHVia2V5")).unwrap();

        let with_cursor = process_hello(
            &state,
            &EdgeBearerIdentity::Shared,
            &hello_frame(&enrolled.node_id, Some("5")),
        )
        .unwrap();
        assert_eq!(with_cursor.replay_from_seq, Some(5));
        assert_eq!(with_cursor.replay_from.as_deref(), Some("5"));

        let bad_cursor = process_hello(
            &state,
            &EdgeBearerIdentity::Shared,
            &hello_frame(&enrolled.node_id, Some("not-a-seq")),
        )
        .unwrap();
        assert_eq!(bad_cursor.replay_from_seq, None);
        assert_eq!(bad_cursor.replay_from, None);
        let _ = std::fs::remove_file(&path);
    }

    // ── Ring buffer / replay ────────────────────────────────────────────────

    #[test]
    fn ring_assigns_monotonic_seqs_and_replays_after_cursor() {
        let path = temp_registry_path("ring");
        let state = edge_state(&path, Some("INV-1"));
        for i in 1..=5u64 {
            let (seq, frame) = state.edge_push_for_test(i);
            assert_eq!(seq, i);
            // Every outbound frame decodes through the protocol structs.
            let envelope: EdgeEnvelope = serde_json::from_str(&frame).unwrap();
            assert_eq!(envelope.seq, i);
            assert_eq!(envelope.v, PROTOCOL_VERSION);
        }

        let replayed = state.replay_after("edge-ring", 2);
        let seqs: Vec<u64> = replayed
            .iter()
            .map(|(seq, frame)| {
                let envelope = serde_json::from_str::<EdgeEnvelope>(frame).unwrap();
                assert_eq!(envelope.seq, *seq, "pair seq must match frame seq");
                envelope.seq
            })
            .collect();
        assert_eq!(seqs, vec![3, 4, 5]);

        assert!(state.replay_after("edge-ring", 5).is_empty());
        assert!(state.replay_after("edge-other", 0).is_empty());
        let _ = std::fs::remove_file(&path);
    }

    impl EdgeState {
        /// Test helper: push a retained TurnEvent token frame for node "edge-ring".
        fn edge_push_for_test(&self, n: u64) -> (u64, String) {
            self.outbound_envelope(
                "edge-ring",
                None,
                EdgeMessage::TurnEvent {
                    conversation_id: "conv".into(),
                    event_kind: TurnEventKind::Token,
                    content: format!("tok-{n}"),
                    turn_id: None,
                },
                true,
            )
        }
    }

    #[test]
    fn ring_acknowledge_prunes_and_control_frames_are_not_retained() {
        let path = temp_registry_path("ring-ack");
        let state = edge_state(&path, Some("INV-1"));
        for i in 1..=4u64 {
            state.edge_push_for_test(i);
        }
        state.acknowledge_up_to("edge-ring", 3);
        let seqs: Vec<u64> = state
            .replay_after("edge-ring", 0)
            .iter()
            .map(|(seq, _)| *seq)
            .collect();
        assert_eq!(seqs, vec![4]);

        // Control frame: consumes seq 5 but is not replayable.
        let (seq, _) =
            state.outbound_envelope("edge-ring", None, EdgeMessage::Pong { nonce: 1 }, false);
        assert_eq!(seq, 5);
        let seqs: Vec<u64> = state
            .replay_after("edge-ring", 0)
            .iter()
            .map(|(seq, _)| *seq)
            .collect();
        assert_eq!(seqs, vec![4]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ring_evicts_oldest_beyond_capacity() {
        let path = temp_registry_path("ring-cap");
        let state = edge_state(&path, Some("INV-1"));
        let total = (EDGE_RING_CAPACITY + 10) as u64;
        for i in 1..=total {
            state.edge_push_for_test(i);
        }
        let replayed = state.replay_after("edge-ring", 0);
        assert_eq!(replayed.len(), EDGE_RING_CAPACITY);
        assert_eq!(replayed[0].0, total - EDGE_RING_CAPACITY as u64 + 1);
        let _ = std::fs::remove_file(&path);
    }

    // ── Enrollment throttle ─────────────────────────────────────────────────

    #[test]
    fn enroll_throttles_after_repeated_bad_invites() {
        let path = temp_registry_path("throttle");
        let state = edge_state(&path, Some("INV-1"));
        let mut bad = enroll_request("cHVia2V5");
        bad.invite_code = "WRONG".into();

        for _ in 0..MAX_ENROLL_FAILURES {
            assert!(matches!(state.enroll(&bad), Err(EnrollError::BadInvite)));
        }
        // Budget exhausted: even a CORRECT invite is throttled until the
        // window drains (denies brute-force a free oracle).
        assert!(matches!(
            state.enroll(&enroll_request("cHVia2V5")),
            Err(EnrollError::Throttled)
        ));
        assert!(matches!(state.enroll(&bad), Err(EnrollError::Throttled)));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn enroll_succeeds_below_throttle_budget() {
        let path = temp_registry_path("throttle-ok");
        let state = edge_state(&path, Some("INV-1"));
        let mut bad = enroll_request("cHVia2V5");
        bad.invite_code = "WRONG".into();
        for _ in 0..(MAX_ENROLL_FAILURES - 1) {
            assert!(matches!(state.enroll(&bad), Err(EnrollError::BadInvite)));
        }
        assert!(state.enroll(&enroll_request("cHVia2V5")).is_ok());
        let _ = std::fs::remove_file(&path);
    }

    // ── Single live session per node ────────────────────────────────────────

    #[tokio::test]
    async fn begin_session_kicks_previous_session() {
        let path = temp_registry_path("kick");
        let state = edge_state(&path, Some("INV-1"));
        let first = state.begin_session("edge-abc");
        let second = state.begin_session("edge-abc");

        // The first session's handle fires; the second stays pending.
        tokio::time::timeout(Duration::from_secs(1), first.notified())
            .await
            .expect("first session should be kicked by the second hello");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), second.notified())
                .await
                .is_err(),
            "the newest session must not be kicked"
        );

        // A different node's session is unaffected.
        let other = state.begin_session("edge-other");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), other.notified())
                .await
                .is_err()
        );
        let _ = std::fs::remove_file(&path);
    }

    // ── Delivery channel + retainer ─────────────────────────────────────────

    #[tokio::test]
    async fn retained_frames_fan_out_on_delivery_channel() {
        let path = temp_registry_path("delivery");
        let state = edge_state(&path, Some("INV-1"));
        let mut delivery = state.subscribe_delivery("edge-ring");

        let (seq, frame) = state.edge_push_for_test(1);
        let (got_seq, got_frame) = delivery.recv().await.expect("delivery frame");
        assert_eq!((got_seq, got_frame.as_str()), (seq, frame.as_str()));

        // Control frames are not delivered on the retained channel.
        state.outbound_envelope("edge-ring", None, EdgeMessage::Pong { nonce: 1 }, false);
        let (seq3, _) = state.edge_push_for_test(3);
        let (got_seq, _) = delivery.recv().await.expect("delivery frame");
        assert_eq!(
            got_seq, seq3,
            "pong must not appear on the delivery channel"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// The core store-and-forward guarantee: an operator-chat broadcast for an
    /// enrolled edge node is retained in the replay ring even when the node
    /// has NO live session subscribed anywhere.
    #[tokio::test]
    async fn retainer_retains_broadcasts_without_a_live_session() {
        let path = temp_registry_path("retainer");
        let state = edge_state(&path, Some("INV-1"));
        let enrolled = state.enroll(&enroll_request("cHVia2V5")).unwrap();
        let marker = edge_session_marker(&enrolled.node_id);

        let (tx, _) = broadcast::channel::<String>(16);
        let retainer = spawn_edge_retainer(state.clone(), tx.clone());

        // No edge session exists — only the retainer is subscribed.
        tx.send(broadcast_event(
            "operator_chat:reply",
            &marker,
            &[("content", "answer while offline")],
        ))
        .expect("broadcast");
        // A foreign-session event and an un-enrolled edge marker are ignored.
        tx.send(broadcast_event(
            "operator_chat:reply",
            "desktop-membrane",
            &[],
        ))
        .expect("broadcast");
        tx.send(broadcast_event(
            "operator_chat:reply",
            "edge:edge-ghost",
            &[],
        ))
        .expect("broadcast");

        // Wait for the retainer to drain the bus.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if !state.replay_after(&enrolled.node_id, 0).is_empty() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "retainer never retained the frame"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let retained = state.replay_after(&enrolled.node_id, 0);
        assert_eq!(retained.len(), 1, "exactly one frame should be retained");
        let envelope: EdgeEnvelope = serde_json::from_str(&retained[0].1).unwrap();
        assert!(matches!(
            envelope.msg,
            EdgeMessage::TurnEvent { event_kind: TurnEventKind::Final, ref content, .. }
                if content == "answer while offline"
        ));
        assert!(state.replay_after("edge-ghost", 0).is_empty());

        retainer.abort();
        let _ = std::fs::remove_file(&path);
    }

    // ── Broadcast translation ───────────────────────────────────────────────

    /// Test shim over [`translate_operator_broadcast_value`] keeping the
    /// original string-based call sites readable.
    fn translate_operator_broadcast(marker: &str, raw: &str) -> Option<EdgeMessage> {
        let value: Value = serde_json::from_str(raw).ok()?;
        translate_operator_broadcast_value(marker, &value)
    }

    fn broadcast_event(kind: &str, marker: &str, extra: &[(&str, &str)]) -> String {
        let mut payload = json!({
            "target_node_id": "mbp-jane",
            "target_agent_id": "jane",
            "operator_session_id": marker,
            "conversation_id": "conv-1",
            "session_id": "conv-1",
            "turn_id": "turn-1",
        });
        for (key, value) in extra {
            payload[*key] = json!(value);
        }
        json!({"type": kind, "payload": payload}).to_string()
    }

    #[test]
    fn translate_operator_broadcast_maps_event_kinds() {
        let marker = "edge:edge-abc";
        let token = translate_operator_broadcast(
            marker,
            &broadcast_event("operator_chat:partial_reply", marker, &[("content", "Hel")]),
        )
        .unwrap();
        assert_eq!(
            token,
            EdgeMessage::TurnEvent {
                conversation_id: "conv-1".into(),
                event_kind: TurnEventKind::Token,
                content: "Hel".into(),
                turn_id: Some("turn-1".into()),
            }
        );

        let final_reply = translate_operator_broadcast(
            marker,
            &broadcast_event("operator_chat:reply", marker, &[("content", "Hello!")]),
        )
        .unwrap();
        assert!(matches!(
            final_reply,
            EdgeMessage::TurnEvent { event_kind: TurnEventKind::Final, ref content, .. }
                if content == "Hello!"
        ));

        let status = translate_operator_broadcast(
            marker,
            &broadcast_event("operator_chat:turn_event", marker, &[("event", "thinking")]),
        )
        .unwrap();
        assert!(matches!(
            status,
            EdgeMessage::TurnEvent { event_kind: TurnEventKind::Status, ref content, .. }
                if content == "thinking"
        ));

        let error = translate_operator_broadcast(
            marker,
            &broadcast_event("operator_chat:error", marker, &[("message", "boom")]),
        )
        .unwrap();
        assert!(matches!(
            error,
            EdgeMessage::TurnEvent { event_kind: TurnEventKind::Error, ref content, .. }
                if content == "boom"
        ));
    }

    #[test]
    fn translate_operator_broadcast_filters_foreign_sessions_and_types() {
        let marker = "edge:edge-abc";
        // Different operator session → not ours.
        assert!(translate_operator_broadcast(
            marker,
            &broadcast_event("operator_chat:reply", "desktop-membrane", &[]),
        )
        .is_none());
        // Non-chat broadcast types are ignored.
        assert!(translate_operator_broadcast(
            marker,
            &json!({"type": "connected", "payload": {"server": "philotic-web"}}).to_string(),
        )
        .is_none());
        // Garbage is ignored.
        assert!(translate_operator_broadcast(marker, "not json").is_none());
    }
}
