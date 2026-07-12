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
//! - `GET /api/edge/sessions` (+ `/:session_id/turns`) — edge-bearer REST
//!   bridge over the hotel's `ListOperatorSessions` / `ListSessionTurns` IPC,
//!   making the hotel the canonical conversation-history authority for edge
//!   devices (the app's local store is a cache, not truth).
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
        Path, Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
};
use base64::Engine as _;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse};
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
use tokio::sync::{broadcast, mpsc, Notify};

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

    /// Seq-number a frame and send it on the node's live-delivery channel
    /// WITHOUT retaining it in the replay ring. For deliver-once payloads
    /// minted by the retainer (e.g. inline voice audio): played live when a
    /// session is up, silently dropped otherwise — the retained Final
    /// `TurnEvent` still carries the transcript text for replay. Direct
    /// session replies (Pong/Error) must NOT use this: the session writes
    /// those to its own socket and the delivery channel would duplicate them.
    pub(crate) fn outbound_delivery_only(&self, node_id: &str, msg: EdgeMessage) -> u64 {
        let mut rings = self.rings.lock().expect("edge rings lock");
        let ring = rings.entry(node_id.to_string()).or_default();
        ring.next_seq += 1;
        let seq = ring.next_seq;
        let frame = serde_json::to_string(&EdgeEnvelope::new(seq, None, msg))
            .expect("edge envelope serialization is infallible");
        let _ = ring.delivery.send((seq, frame));
        seq
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

/// Resolve a client-supplied chat target to a registry node id. Edge clients
/// commonly know hotels by name (`mbp-jane`) while the registry keys targets
/// by node id (`mbp-jane-aiua-01`); the operator target view carries both, so
/// hotel names map through it. Unknown values pass through untouched so aiua
/// stays the authority on the final error.
pub(crate) async fn resolve_target_node_id(state: &AppState, requested: &str) -> String {
    match super::ipc_desktop_membrane_targets(&state.socket).await {
        Ok(targets) => {
            if targets.iter().any(|t| t.target_node_id == requested) {
                return requested.to_string();
            }
            if let Some(t) = targets.iter().find(|t| t.target_hotel == requested) {
                return t.target_node_id.clone();
            }
            requested.to_string()
        }
        Err(err) => {
            eprintln!(
                "philotic-web: edge target resolution unavailable ({err}); passing [{requested}] through"
            );
            requested.to_string()
        }
    }
}

/// Base URL of the hotel blob store the edge blob proxy forwards to:
/// `PHILOTIC_WEB_BLOB_BASE` env, else the conventional default. TODO
/// (apple-edge-client/edge-blob-discovery): resolve from the hotel record's
/// advertised `blob_port` over IPC instead of configuration.
fn edge_blob_base() -> String {
    std::env::var("PHILOTIC_WEB_BLOB_BASE")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "http://127.0.0.1:9001".to_string())
}

/// `POST /api/edge/blob` — edge-bearer-scoped blob upload proxy. Accepts raw
/// bytes (Content-Type = the media MIME), forwards them to the hotel blob
/// store's multipart `/upload`, and returns `{blob_id, download_url, mime}`.
/// The returned `download_url` is HOTEL-LOCAL (loopback): it is meant to ride
/// a `TurnSubmit.blob_refs` entry so hotel-side consumers (transcription
/// providers) can fetch it — remote devices should not expect to dereference
/// it themselves.
pub(crate) async fn handle_edge_blob(
    headers: HeaderMap,
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Response {
    if edge_bearer_identity(&headers, &state).is_none() {
        return super::unauthorized();
    }
    if body.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "empty blob body"})),
        )
            .into_response();
    }
    let mime = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    match upload_to_blob_store(body.to_vec(), &mime).await {
        Ok((blob_id, download_url)) => Json(json!({
            "blob_id": blob_id,
            "download_url": download_url,
            "mime": mime,
        }))
        .into_response(),
        Err(err) => (StatusCode::BAD_GATEWAY, Json(json!({"error": err}))).into_response(),
    }
}

/// Upload bytes to the hotel blob store's multipart `/upload`. Returns
/// `(blob_id, hotel-local download_url)`. Shared by the HTTP blob proxy and
/// the streamed-audio assembly path.
async fn upload_to_blob_store(bytes: Vec<u8>, mime: &str) -> Result<(String, String), String> {
    let blob_base = edge_blob_base();
    let part = match reqwest::multipart::Part::bytes(bytes.clone())
        .file_name("edge-upload")
        .mime_str(mime)
    {
        Ok(part) => part,
        Err(_) => reqwest::multipart::Part::bytes(bytes).file_name("edge-upload"),
    };
    let form = reqwest::multipart::Form::new().part("file", part);
    let response = reqwest::Client::new()
        .post(format!("{blob_base}/upload"))
        .multipart(form)
        .send()
        .await
        .map_err(|err| format!("blob store unreachable at {blob_base}: {err}"))?;
    if !response.status().is_success() {
        return Err(format!("blob store rejected upload: {}", response.status()));
    }
    let parsed: Value = response
        .json()
        .await
        .map_err(|err| format!("bad blob store response: {err}"))?;
    let blob_id = parsed
        .get("blob_ids")
        .and_then(Value::as_array)
        .and_then(|ids| ids.first())
        .and_then(Value::as_str)
        .ok_or_else(|| "blob store returned no blob id".to_string())?
        .to_string();
    let download_url = format!("{blob_base}/download/{blob_id}");
    Ok((blob_id, download_url))
}

#[derive(Serialize)]
struct EdgeAgentEntry {
    target_node_id: String,
    agent_id: String,
    display_name: String,
    authority_hotel: String,
}

/// `GET /api/edge/agents` — edge-bearer-scoped agent directory. Returns each
/// mesh agent with its authority hotel already resolved to a registry node id
/// suitable for `TurnSubmit.target_node_id`, so clients never hardcode a
/// catalog or guess node ids.
pub(crate) async fn handle_edge_agents(
    headers: HeaderMap,
    State(state): State<AppState>,
) -> Response {
    if edge_bearer_identity(&headers, &state).is_none() {
        return super::unauthorized();
    }
    let agents = match super::ipc_desktop_membrane_agents(&state.socket).await {
        Ok(agents) => agents,
        Err(err) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": format!("hotel agents unavailable: {err}")})),
            )
                .into_response();
        }
    };
    let targets = super::ipc_desktop_membrane_targets(&state.socket)
        .await
        .unwrap_or_default();
    let mut entries: Vec<EdgeAgentEntry> = agents
        .iter()
        .map(|a| {
            let target_node_id = targets
                .iter()
                .find(|t| t.target_hotel == a.authority_hotel)
                .map(|t| t.target_node_id.clone())
                .unwrap_or_else(|| a.authority_hotel.clone());
            EdgeAgentEntry {
                target_node_id,
                agent_id: a.agent_id.clone(),
                display_name: a.persona_name.clone(),
                authority_hotel: a.authority_hotel.clone(),
            }
        })
        .collect();
    // Union in remote targets' agents so the directory covers the whole
    // reachable mesh, not just this hotel's authority. Remote queries are
    // best-effort: a slow or dark peer must not break the local list.
    for target in &targets {
        if target.is_local {
            continue;
        }
        let inventory = tokio::time::timeout(
            Duration::from_secs(4),
            super::ipc_desktop_membrane_target_agents(&state.socket, &target.target_node_id),
        )
        .await;
        let Ok(Ok(inventory)) = inventory else {
            continue;
        };
        for a in &inventory.agents {
            if entries
                .iter()
                .any(|e| e.target_node_id == target.target_node_id && e.agent_id == a.agent_id)
            {
                continue;
            }
            entries.push(EdgeAgentEntry {
                target_node_id: target.target_node_id.clone(),
                agent_id: a.agent_id.clone(),
                display_name: a.persona_name.clone(),
                authority_hotel: a.authority_hotel.clone(),
            });
        }
    }
    Json(json!({ "agents": entries })).into_response()
}

#[derive(Deserialize)]
pub(crate) struct EdgeSessionsQuery {
    /// Restrict to sessions whose primary agent matches (e.g. "jane").
    agent_id: Option<String>,
    limit: Option<u32>,
}

/// `GET /api/edge/sessions` — edge-bearer-scoped conversation history: the
/// hotel's recorded operator sessions (most recent activity first), served
/// from the context graph via `ListOperatorSessions`. This makes the hotel —
/// not the device's local store — the canonical history authority, so a
/// second device (or a reinstall) can list and resume prior conversations.
pub(crate) async fn handle_edge_sessions(
    headers: HeaderMap,
    State(state): State<AppState>,
    Query(query): Query<EdgeSessionsQuery>,
) -> Response {
    if edge_bearer_identity(&headers, &state).is_none() {
        return super::unauthorized();
    }
    match super::ipc_list_operator_sessions(&state.socket, query.agent_id, query.limit).await {
        Ok(sessions) => Json(json!({ "sessions": sessions })).into_response(),
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": format!("hotel sessions unavailable: {err}")})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub(crate) struct EdgeSessionTurnsQuery {
    limit: Option<u32>,
    /// Pagination cursor: only turns strictly older than this turn_id.
    before_turn_id: Option<String>,
}

/// `GET /api/edge/sessions/:session_id/turns` — expanded operator/agent
/// messages for one session, oldest first, via `ListSessionTurns`. One stored
/// turn record expands to two items sharing a `turn_id` (operator + agent),
/// mirroring the IPC contract — clients fold the role into their row identity.
pub(crate) async fn handle_edge_session_turns(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    Query(query): Query<EdgeSessionTurnsQuery>,
) -> Response {
    if edge_bearer_identity(&headers, &state).is_none() {
        return super::unauthorized();
    }
    match super::ipc_list_session_turns(
        &state.socket,
        session_id.clone(),
        query.limit,
        query.before_turn_id,
    )
    .await
    {
        Ok(turns) => Json(json!({ "session_id": session_id, "turns": turns })).into_response(),
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": format!("hotel session turns unavailable: {err}")})),
        )
            .into_response(),
    }
}

/// Named `life.recall` retrieval strategies a device may request as a lens.
/// Mirrors `NamedRecallStrategy` in `data-memorygraphrag` — validated here so
/// arbitrary device strings never reach the datasource task envelope.
const LIFEGRAPH_LENSES: &[&str] = &[
    "semantic_pivot",
    "open_loops_by_context",
    "goals_and_next_actions",
    "commitments_approaching",
    "re_entry_context",
    "cross_domain_entanglement",
    "current_prompt_semantic",
];

fn life_graph_unavailable(err: anyhow::Error) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({"error": format!("life-graph unavailable: {err}")})),
    )
        .into_response()
}

#[derive(Deserialize)]
pub(crate) struct EdgeLifegraphLensQuery {
    /// Optional operator context ("where was I on the porch project?") —
    /// becomes the recall `query_text`; defaults to the lens name.
    context: Option<String>,
    limit: Option<u32>,
}

/// `GET /api/edge/lifegraph/lens/:lens` — edge-bearer LifeGraph lens read:
/// one named `life.recall` retrieval strategy (open loops, commitments
/// approaching, re-entry, …) served as a render-ready context packet. The
/// device never sends Cypher — only a validated lens name plus free-text
/// context; zoning and validation-state policy stay server-side in the
/// life-graph-runner.
pub(crate) async fn handle_edge_lifegraph_lens(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(lens): Path<String>,
    Query(query): Query<EdgeLifegraphLensQuery>,
) -> Response {
    if edge_bearer_identity(&headers, &state).is_none() {
        return super::unauthorized();
    }
    if !LIFEGRAPH_LENSES.contains(&lens.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!("unknown lens '{lens}'"),
                "lenses": LIFEGRAPH_LENSES,
            })),
        )
            .into_response();
    }
    let context = query
        .context
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .unwrap_or(lens.as_str())
        .to_string();
    let arguments = json!({
        "query_id": format!("edge-lens-{lens}"),
        "query_text": context,
        "named_strategy": lens,
        "operator_intent": lens,
        "max_context_packets": query.limit.unwrap_or(10).min(50),
    });
    match super::ipc_life_graph_datasource_call(&state.socket, "life.recall", arguments).await {
        Ok(data) => Json(json!({ "lens": lens, "data": data })).into_response(),
        Err(err) => life_graph_unavailable(err),
    }
}

#[derive(Deserialize)]
pub(crate) struct EdgeLifegraphNodeQuery {
    edge_limit: Option<u32>,
}

/// `GET /api/edge/lifegraph/node/:node_id` — edge-bearer node detail via the
/// read-only `life.view.node` datasource tool: the node (provenance envelope
/// in its properties) plus bounded typed edges to non-retired neighbours.
pub(crate) async fn handle_edge_lifegraph_node(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(node_id): Path<String>,
    Query(query): Query<EdgeLifegraphNodeQuery>,
) -> Response {
    if edge_bearer_identity(&headers, &state).is_none() {
        return super::unauthorized();
    }
    let mut arguments = json!({ "id": node_id });
    if let Some(edge_limit) = query.edge_limit {
        arguments["edge_limit"] = json!(edge_limit);
    }
    match super::ipc_life_graph_datasource_call(&state.socket, "life.view.node", arguments).await {
        Ok(data) => Json(data).into_response(),
        Err(err) => life_graph_unavailable(err),
    }
}

#[derive(Deserialize)]
pub(crate) struct EdgeLifegraphNeighborhoodQuery {
    depth: Option<u32>,
    max_nodes: Option<u32>,
}

/// `GET /api/edge/lifegraph/neighborhood/:node_id` — edge-bearer bounded
/// living-cycle expansion via the read-only `life.view.neighborhood`
/// datasource tool, for the canvas view (depth ≤ 2, node budget ≤ 150 —
/// clamped again runner-side).
pub(crate) async fn handle_edge_lifegraph_neighborhood(
    headers: HeaderMap,
    State(state): State<AppState>,
    Path(node_id): Path<String>,
    Query(query): Query<EdgeLifegraphNeighborhoodQuery>,
) -> Response {
    if edge_bearer_identity(&headers, &state).is_none() {
        return super::unauthorized();
    }
    let mut arguments = json!({ "id": node_id });
    if let Some(depth) = query.depth {
        arguments["depth"] = json!(depth);
    }
    if let Some(max_nodes) = query.max_nodes {
        arguments["max_nodes"] = json!(max_nodes);
    }
    match super::ipc_life_graph_datasource_call(&state.socket, "life.view.neighborhood", arguments)
        .await
    {
        Ok(data) => Json(data).into_response(),
        Err(err) => life_graph_unavailable(err),
    }
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
    // Open uplink audio streams for this session (streaming voice capture).
    // Session-scoped on purpose: a dropped connection discards partial audio
    // — the client re-records rather than resuming a half-stream.
    let mut audio_streams: HashMap<String, AudioStreamState> = HashMap::new();

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
                        match handle_edge_frame(
                            &state,
                            &node_id,
                            &marker,
                            &mut client_seq,
                            &mut audio_streams,
                            &text,
                        )
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
                    if let Some((node_id, msgs)) = translate_edge_broadcast(&raw) {
                        if edge.is_enrolled(&node_id) {
                            for (msg, retain) in msgs {
                                if retain {
                                    let _ = edge.outbound_envelope(&node_id, None, msg, true);
                                } else {
                                    let _ = edge.outbound_delivery_only(&node_id, msg);
                                }
                            }
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

/// One in-flight uplink audio stream: chunks accumulate here until
/// `AudioStreamEnd` seals the stream and the assembled audio becomes a
/// blob-backed voice turn.
struct AudioAssembly {
    target_node_id: String,
    target_agent_id: String,
    conversation_id: Option<String>,
    mime_type: String,
    next_chunk: u64,
    bytes: Vec<u8>,
}

/// An open uplink audio stream is either batch-assembled (container audio ->
/// blob -> attachment turn) or relayed live into a realtime STT session
/// (raw PCM -> model.{provider} -> partial transcripts -> text turn).
enum AudioStreamState {
    Assemble(AudioAssembly),
    Stt {
        cmd_tx: mpsc::Sender<SttCmd>,
        next_chunk: u64,
    },
}

/// Commands from the WS session loop to a streaming-STT relay task.
enum SttCmd {
    Chunk(Vec<u8>),
    End,
    Cancel,
}

/// Realtime STT provider for PCM uplink streams: `PHILOTIC_WEB_STREAMING_STT`
/// env (e.g. "elevenlabs"); unset/empty/"off" disables (batch assembly path).
fn edge_streaming_stt_provider() -> Option<String> {
    std::env::var("PHILOTIC_WEB_STREAMING_STT")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty() && v != "off")
}

/// Reply role the STT relay subscribes on for transcribe_partial tasks.
const EDGE_STT_REPLY_ROLE: &str = "management.edge_stt.reply";
/// Relay inactivity ceiling: no chunk, command, or provider reply for this
/// long tears the session down with a terminal error frame.
const EDGE_STT_IDLE_SECS: u64 = 120;

/// Per-stream assembled-audio ceiling (matches the blob proxy body limit).
const AUDIO_STREAM_MAX_BYTES: usize = 25 * 1024 * 1024;
/// Concurrent open streams per session — one live recording plus headroom
/// for a stop/start race.
const AUDIO_STREAM_MAX_OPEN: usize = 2;

/// Handle one post-handshake client frame. Fatal outcomes are protocol
/// violations; undecodable frames only earn a non-fatal `Error` (a newer
/// client may legitimately speak message types this server does not know).
async fn handle_edge_frame(
    state: &AppState,
    node_id: &str,
    marker: &str,
    client_seq: &mut u64,
    audio_streams: &mut HashMap<String, AudioStreamState>,
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
            message_kind,
        } => {
            // Blob refs become hotel-side attachments matching philote's
            // TransportAttachment shape (crates/philote/src/protocol.rs) —
            // `kind` and `file_id` are REQUIRED (missing fields make philote
            // drop the whole task payload with only a warn), and the MIME
            // key is `mime_type`. `file_id` has no transport meaning here,
            // so the content-addressed blob id doubles as it.
            let attachments: Vec<Value> = blob_refs
                .iter()
                .map(|b| {
                    let kind = match message_kind.as_deref() {
                        Some(k @ ("voice" | "audio")) => k,
                        _ => "file",
                    };
                    json!({
                        "kind": kind,
                        "file_id": b.blob_id,
                        "blob_id": b.blob_id,
                        "blob_download_url": b.download_url,
                        "mime_type": b.mime,
                    })
                })
                .collect();
            let conversation_id = conversation_id
                .unwrap_or_else(|| format!("operator-chat:{marker}:{target_agent_id}"));
            let target_node_id = resolve_target_node_id(state, &target_node_id).await;
            match super::submit_operator_chat_turn(
                state,
                &target_node_id,
                &target_agent_id,
                marker,
                conversation_id.clone(),
                content,
                message_kind,
                attachments,
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
        EdgeMessage::AudioStreamStart {
            stream_id,
            target_node_id,
            target_agent_id,
            conversation_id,
            mime_type,
        } => {
            if audio_streams.len() >= AUDIO_STREAM_MAX_OPEN {
                return FrameOutcome::Reply {
                    msg: edge_error("audio_stream", "too many open audio streams", false),
                    retain: false,
                };
            }
            if audio_streams.contains_key(&stream_id) {
                return FrameOutcome::Reply {
                    msg: edge_error("audio_stream", "duplicate stream id", false),
                    retain: false,
                };
            }
            if let Some(provider) =
                edge_streaming_stt_provider().filter(|_| mime_type.starts_with("audio/pcm"))
            {
                let (cmd_tx, cmd_rx) = mpsc::channel::<SttCmd>(128);
                audio_streams.insert(
                    stream_id.clone(),
                    AudioStreamState::Stt {
                        cmd_tx,
                        next_chunk: 0,
                    },
                );
                tokio::spawn(run_edge_stt_relay(
                    state.clone(),
                    node_id.to_string(),
                    marker.to_string(),
                    provider,
                    stream_id,
                    target_node_id,
                    target_agent_id,
                    conversation_id,
                    mime_type,
                    cmd_rx,
                ));
                return FrameOutcome::Continue;
            }
            audio_streams.insert(
                stream_id,
                AudioStreamState::Assemble(AudioAssembly {
                    target_node_id,
                    target_agent_id,
                    conversation_id,
                    mime_type,
                    next_chunk: 0,
                    bytes: Vec::new(),
                }),
            );
            FrameOutcome::Continue
        }
        EdgeMessage::AudioChunk {
            stream_id,
            chunk_seq,
            data_base64,
        } => {
            let Some(stream) = audio_streams.get_mut(&stream_id) else {
                return FrameOutcome::Reply {
                    msg: edge_error("audio_stream", "chunk for unknown stream", false),
                    retain: false,
                };
            };
            let expected = match stream {
                AudioStreamState::Assemble(a) => a.next_chunk,
                AudioStreamState::Stt { next_chunk, .. } => *next_chunk,
            };
            if chunk_seq != expected {
                audio_streams.remove(&stream_id);
                return FrameOutcome::Reply {
                    msg: edge_error(
                        "audio_stream",
                        "out-of-order chunk; stream discarded",
                        false,
                    ),
                    retain: false,
                };
            }
            let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(&data_base64) else {
                audio_streams.remove(&stream_id);
                return FrameOutcome::Reply {
                    msg: edge_error("audio_stream", "undecodable chunk; stream discarded", false),
                    retain: false,
                };
            };
            match stream {
                AudioStreamState::Assemble(assembly) => {
                    if assembly.bytes.len() + bytes.len() > AUDIO_STREAM_MAX_BYTES {
                        audio_streams.remove(&stream_id);
                        return FrameOutcome::Reply {
                            msg: edge_error(
                                "audio_stream",
                                "stream exceeds size ceiling; discarded",
                                false,
                            ),
                            retain: false,
                        };
                    }
                    assembly.next_chunk += 1;
                    assembly.bytes.extend_from_slice(&bytes);
                }
                AudioStreamState::Stt { cmd_tx, next_chunk } => {
                    if cmd_tx.try_send(SttCmd::Chunk(bytes)).is_err() {
                        audio_streams.remove(&stream_id);
                        return FrameOutcome::Reply {
                            msg: edge_error(
                                "audio_stream",
                                "stt relay unavailable; stream discarded",
                                false,
                            ),
                            retain: false,
                        };
                    }
                    *next_chunk += 1;
                }
            }
            FrameOutcome::Continue
        }
        EdgeMessage::AudioStreamEnd { stream_id, cancel } => {
            let Some(stream) = audio_streams.remove(&stream_id) else {
                return FrameOutcome::Reply {
                    msg: edge_error("audio_stream", "end for unknown stream", false),
                    retain: false,
                };
            };
            let assembly = match stream {
                AudioStreamState::Stt { cmd_tx, .. } => {
                    // The relay owns everything from here: final transcript,
                    // turn submission, and status/error frames.
                    let _ = cmd_tx.try_send(if cancel { SttCmd::Cancel } else { SttCmd::End });
                    return FrameOutcome::Continue;
                }
                AudioStreamState::Assemble(assembly) => assembly,
            };
            if cancel {
                return FrameOutcome::Continue;
            }
            if assembly.bytes.is_empty() {
                return FrameOutcome::Reply {
                    msg: edge_error("audio_stream", "stream sealed with no audio", false),
                    retain: false,
                };
            }
            let conversation_id = assembly
                .conversation_id
                .clone()
                .unwrap_or_else(|| format!("operator-chat:{marker}:{}", assembly.target_agent_id));
            let (blob_id, download_url) =
                match upload_to_blob_store(assembly.bytes, &assembly.mime_type).await {
                    Ok(ok) => ok,
                    Err(err) => {
                        return FrameOutcome::Reply {
                            msg: EdgeMessage::TurnEvent {
                                conversation_id,
                                event_kind: TurnEventKind::Error,
                                content: format!("voice stream store failed: {err}"),
                                turn_id: None,
                            },
                            retain: true,
                        };
                    }
                };
            let attachments = vec![json!({
                "kind": "voice",
                "file_id": blob_id,
                "blob_id": blob_id,
                "blob_download_url": download_url,
                "mime_type": assembly.mime_type,
            })];
            let target_node_id = resolve_target_node_id(state, &assembly.target_node_id).await;
            match super::submit_operator_chat_turn(
                state,
                &target_node_id,
                &assembly.target_agent_id,
                marker,
                conversation_id.clone(),
                String::new(),
                Some("voice".to_string()),
                attachments,
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
        | EdgeMessage::VoiceReply { .. }
        | EdgeMessage::TranscriptPartial { .. }
        | EdgeMessage::ToolInvoke { .. } => FrameOutcome::Fatal {
            code: "protocol",
            message: "clients must not send server-to-client message types".into(),
        },
    }
}

/// Translate an operator-chat broadcast event addressed to ANY edge node
/// (marker `edge:{node_id}`) into `(node_id, [(frame, retain)])`. Used by the
/// process-wide retainer, which does not know a marker up front.
fn translate_edge_broadcast(raw: &str) -> Option<(String, Vec<(EdgeMessage, bool)>)> {
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
    let msgs = translate_operator_broadcast_value(&marker, &value);
    if msgs.is_empty() {
        return None;
    }
    Some((node_id, msgs))
}

/// Translate an operator-chat broadcast event (as fanned out by
/// `stream_operator_chat_turn`) into edge frames for the node whose session
/// marker is `marker`, each paired with a retain flag (replayable or
/// ephemeral). Empty for events that belong to other sessions/types. A final
/// reply with an inline synthesized-audio artifact yields the retained Final
/// `TurnEvent` plus an ephemeral `VoiceReply`.
fn translate_operator_broadcast_value(marker: &str, value: &Value) -> Vec<(EdgeMessage, bool)> {
    let Some(kind) = value.get("type").and_then(Value::as_str) else {
        return Vec::new();
    };
    let Some(payload) = value.get("payload") else {
        return Vec::new();
    };
    if payload.get("operator_session_id").and_then(Value::as_str) != Some(marker) {
        return Vec::new();
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
    if kind == "operator_chat:voice_chunk" {
        return voice_reply_from_payload(
            payload,
            &conversation_id,
            turn_id,
            &str_field("content", ""),
        )
        .map(|voice| vec![(voice, false)])
        .unwrap_or_default();
    }
    let (event_kind, content) = match kind {
        "operator_chat:turn_event" => (TurnEventKind::Status, str_field("event", "unknown")),
        "operator_chat:partial_reply" => (TurnEventKind::Token, str_field("content", "")),
        "operator_chat:reply" => (TurnEventKind::Final, str_field("content", "")),
        "operator_chat:error" => (TurnEventKind::Error, str_field("message", "turn failed")),
        _ => return Vec::new(),
    };
    let mut out = vec![(
        EdgeMessage::TurnEvent {
            conversation_id: conversation_id.clone(),
            event_kind,
            content: content.clone(),
            turn_id: turn_id.clone(),
        },
        true,
    )];
    if kind == "operator_chat:reply" {
        if let Some(voice) = voice_reply_from_payload(payload, &conversation_id, turn_id, &content)
        {
            out.push((voice, false));
        }
    }
    out
}

/// Streaming-STT relay: bridges one uplink PCM audio stream into a realtime
/// transcription session on `model.{provider}` and turns the final transcript
/// into a TEXT operator-chat turn (tagged voice-modality so the persona voice
/// replies). Partial transcripts stream to the device as ephemeral
/// `TranscriptPartial` frames. The reasoning model never sees audio — this is
/// the decoupling that lets any text model serve voice conversations.
#[allow(clippy::too_many_arguments)]
async fn run_edge_stt_relay(
    state: AppState,
    node_id: String,
    marker: String,
    provider: String,
    stream_id: String,
    target_node_id: String,
    target_agent_id: String,
    conversation_id: Option<String>,
    audio_mime: String,
    mut cmd_rx: mpsc::Receiver<SttCmd>,
) {
    let conversation_id =
        conversation_id.unwrap_or_else(|| format!("operator-chat:{marker}:{target_agent_id}"));
    let deliver_error = |message: &str| {
        let _ = state
            .edge
            .outbound_delivery_only(&node_id, edge_error("stt_stream", message, false));
    };

    // Reply client: a dedicated ephemeral guest subscribed for
    // transcribe_partial tasks from the provider controller.
    let reply_guest_id = format!("edge-stt-{}", random_hex(8));
    let identity = GuestIdentity {
        guest_id: reply_guest_id.clone(),
        role: EDGE_STT_REPLY_ROLE.into(),
        supported_tools: vec![],
    };
    let mut client = match super::connect_client_with_identity(&state.socket, identity).await {
        Ok(client) => client,
        Err(err) => {
            deliver_error(&format!("stt relay could not reach the hotel: {err}"));
            return;
        }
    };
    match client
        .send_request(IpcRequest::SubscribeInbox {
            role: EDGE_STT_REPLY_ROLE.into(),
        })
        .await
    {
        Ok(IpcResponse::Standard { ok: true, .. }) => {}
        other => {
            deliver_error(&format!("stt relay subscribe failed: {other:?}"));
            return;
        }
    }

    // Local node id for reply routing (the controller EmitTasks back to us).
    let local_node = match super::ipc_desktop_membrane_targets(&state.socket)
        .await
        .ok()
        .and_then(|ts| ts.into_iter().find(|t| t.is_local))
    {
        Some(t) => t.target_node_id,
        None => {
            deliver_error("stt relay could not resolve the local node");
            return;
        }
    };

    // Parse rate/channels from the mime (audio/pcm;rate=16000;channels=1).
    let param = |key: &str, default: u32| {
        audio_mime
            .split(';')
            .filter_map(|part| part.trim().strip_prefix(&format!("{key}=")))
            .filter_map(|v| v.parse::<u32>().ok())
            .next()
            .unwrap_or(default)
    };
    let stt_role = format!("model.{provider}");
    let session_id = format!("edge-stt:{stream_id}");
    let open = json!({
        "kind": "voice.transcribe.stream",
        "stream_op": "open",
        "stream_session_id": session_id,
        "audio_format": {
            "encoding": "pcm_s16le",
            "sample_rate": param("rate", 16_000),
            "channels": param("channels", 1),
        },
        "reply_to": local_node,
        "reply_role": EDGE_STT_REPLY_ROLE,
        "reply_guest_id": reply_guest_id,
    });
    let emit = |task: Value| IpcRequest::EmitTask {
        target_node: local_node.clone(),
        target_role: stt_role.clone(),
        target_guest_id: None,
        task_json: task.to_string(),
    };
    match client.send_request(emit(open)).await {
        Ok(IpcResponse::Standard { ok: true, .. }) => {}
        other => {
            deliver_error(&format!(
                "streaming STT unavailable ({stt_role}): {other:?}"
            ));
            return;
        }
    }

    // Poll loop: drain session commands, then wait briefly for provider
    // replies. A 200ms recv window bounds both chunk-forwarding latency and
    // partial-transcript latency without wrestling two &mut borrows.
    let mut ended = false;
    let mut final_text: Option<String> = None;
    let mut last_activity = Instant::now();
    'relay: loop {
        if last_activity.elapsed() > Duration::from_secs(EDGE_STT_IDLE_SECS) {
            deliver_error("stt relay idle timeout");
            let _ = client
                .send_request(emit(json!({
                    "kind": "voice.transcribe.stream",
                    "stream_op": "end",
                    "stream_session_id": session_id,
                })))
                .await;
            return;
        }
        while let Ok(cmd) = cmd_rx.try_recv() {
            last_activity = Instant::now();
            match cmd {
                SttCmd::Chunk(bytes) => {
                    let chunk = json!({
                        "kind": "voice.transcribe.stream",
                        "stream_op": "chunk",
                        "stream_session_id": session_id,
                        "audio_base64":
                            base64::engine::general_purpose::STANDARD.encode(&bytes),
                    });
                    if let Err(err) = client.send_request(emit(chunk)).await {
                        deliver_error(&format!("stt chunk forward failed: {err}"));
                        return;
                    }
                }
                SttCmd::End => {
                    ended = true;
                    let end = json!({
                        "kind": "voice.transcribe.stream",
                        "stream_op": "end",
                        "stream_session_id": session_id,
                    });
                    if let Err(err) = client.send_request(emit(end)).await {
                        deliver_error(&format!("stt end forward failed: {err}"));
                        return;
                    }
                }
                SttCmd::Cancel => {
                    let _ = client
                        .send_request(emit(json!({
                            "kind": "voice.transcribe.stream",
                            "stream_op": "end",
                            "stream_session_id": session_id,
                        })))
                        .await;
                    return;
                }
            }
        }
        match tokio::time::timeout(Duration::from_millis(200), client.recv_task()).await {
            Err(_) => {} // window elapsed — loop back to command draining
            Ok(Err(err)) => {
                deliver_error(&format!("stt relay lost the hotel: {err}"));
                return;
            }
            Ok(Ok(IpcResponse::InboundTask { task_json, .. })) => {
                last_activity = Instant::now();
                let Ok(payload) = serde_json::from_str::<Value>(&task_json) else {
                    continue;
                };
                if payload.get("action").and_then(Value::as_str) != Some("transcribe_partial")
                    || payload.get("stream_session_id").and_then(Value::as_str)
                        != Some(session_id.as_str())
                {
                    continue;
                }
                let text = payload
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let is_final = payload
                    .get("is_final")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if let Some(err) = payload.get("error").and_then(Value::as_str) {
                    deliver_error(&format!("streaming transcription failed: {err}"));
                    return;
                }
                let _ = state.edge.outbound_delivery_only(
                    &node_id,
                    EdgeMessage::TranscriptPartial {
                        stream_id: stream_id.clone(),
                        conversation_id: Some(conversation_id.clone()),
                        text: text.clone(),
                        is_final,
                    },
                );
                if is_final {
                    final_text = Some(text);
                    if ended {
                        break 'relay;
                    }
                }
            }
            Ok(Ok(_)) => {} // unsolicited pushes (MuninnStatus etc.) — skip
        }
        if ended && final_text.is_some() {
            break 'relay;
        }
    }

    let transcript = final_text.unwrap_or_default();
    if transcript.trim().is_empty() {
        deliver_error("streaming transcription produced no text");
        return;
    }
    // Submit the transcript as a TEXT turn tagged voice-modality: the persona
    // voice reply fires, but the reasoning model receives plain text — any
    // downstream model works.
    let resolved_target = resolve_target_node_id(&state, &target_node_id).await;
    match super::submit_operator_chat_turn(
        &state,
        &resolved_target,
        &target_agent_id,
        &marker,
        conversation_id.clone(),
        transcript,
        Some("voice".to_string()),
        Vec::new(),
    )
    .await
    {
        Ok(accepted) => {
            let _ = state.edge.outbound_envelope(
                &node_id,
                None,
                EdgeMessage::TurnEvent {
                    conversation_id: accepted.conversation_id,
                    event_kind: TurnEventKind::Status,
                    content: "accepted".into(),
                    turn_id: Some(accepted.turn_id),
                },
                true,
            );
        }
        Err(err) => {
            let _ = state.edge.outbound_envelope(
                &node_id,
                None,
                EdgeMessage::TurnEvent {
                    conversation_id,
                    event_kind: TurnEventKind::Error,
                    content: err.message,
                    turn_id: None,
                },
                true,
            );
        }
    }
}

/// Extract an inline persona-voice reply from a `send_reply` broadcast
/// payload. The philote attaches TTS output as `audio_artifact` — a
/// serialized [`AudioArtifactEnvelope`] JSON string (or already-inline
/// object) with `audio_base64` at the top level (Synthesized path) or nested
/// under `payload` (NativeAudio path).
fn voice_reply_from_payload(
    payload: &Value,
    conversation_id: &str,
    turn_id: Option<String>,
    content: &str,
) -> Option<EdgeMessage> {
    let chunk_seq = payload.get("chunk_seq").and_then(Value::as_u64);
    let is_final = payload.get("is_final").and_then(Value::as_bool);
    let raw = payload.get("audio_artifact")?;
    let artifact: Value = match raw {
        Value::String(s) => serde_json::from_str(s).ok()?,
        Value::Object(_) => raw.clone(),
        _ => return None,
    };
    let nested = artifact.get("payload");
    let audio_base64 = artifact
        .get("audio_base64")
        .and_then(Value::as_str)
        .or_else(|| {
            nested
                .and_then(|p| p.get("audio_base64"))
                .and_then(Value::as_str)
        })?
        .to_string();
    if audio_base64.is_empty() {
        return None;
    }
    let mime_type = artifact
        .get("mime_type")
        .and_then(Value::as_str)
        .or_else(|| {
            nested
                .and_then(|p| p.get("mime_type"))
                .and_then(Value::as_str)
        })
        .unwrap_or("audio/mpeg")
        .to_string();
    Some(EdgeMessage::VoiceReply {
        conversation_id: conversation_id.to_string(),
        turn_id,
        audio_base64,
        mime_type,
        transcript: if content.is_empty() {
            None
        } else {
            Some(content.to_string())
        },
        chunk_seq,
        is_final,
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
    /// original string-based call sites readable (first frame only).
    fn translate_operator_broadcast(marker: &str, raw: &str) -> Option<EdgeMessage> {
        let value: Value = serde_json::from_str(raw).ok()?;
        translate_operator_broadcast_value(marker, &value)
            .into_iter()
            .next()
            .map(|(msg, _)| msg)
    }

    /// All frames (with retain flags) for a raw broadcast string.
    fn translate_operator_broadcast_all(marker: &str, raw: &str) -> Vec<(EdgeMessage, bool)> {
        let value: Value = serde_json::from_str(raw).ok().unwrap_or(Value::Null);
        translate_operator_broadcast_value(marker, &value)
    }

    #[test]
    fn reply_with_audio_artifact_yields_ephemeral_voice_reply() {
        let marker = "edge:edge-node-1";
        let artifact = json!({
            "kind": "audio_artifact",
            "provider": "elevenlabs",
            "mime_type": "audio/mpeg",
            "audio_base64": "bW9jay1hdWRpbw=="
        })
        .to_string();
        let raw = broadcast_event(
            "operator_chat:reply",
            marker,
            &[("content", "Hello!"), ("audio_artifact", artifact.as_str())],
        );
        let frames = translate_operator_broadcast_all(marker, &raw);
        assert_eq!(
            frames.len(),
            2,
            "expected Final + VoiceReply, got {frames:?}"
        );
        assert!(matches!(
            frames[0],
            (
                EdgeMessage::TurnEvent {
                    event_kind: TurnEventKind::Final,
                    ..
                },
                true
            )
        ));
        match &frames[1] {
            (
                EdgeMessage::VoiceReply {
                    audio_base64,
                    mime_type,
                    transcript,
                    ..
                },
                retain,
            ) => {
                assert_eq!(audio_base64, "bW9jay1hdWRpbw==");
                assert_eq!(mime_type, "audio/mpeg");
                assert_eq!(transcript.as_deref(), Some("Hello!"));
                assert!(!retain, "voice replies must be ephemeral (not replayed)");
            }
            other => panic!("expected VoiceReply, got {other:?}"),
        }
    }

    #[test]
    fn reply_without_audio_yields_single_retained_final() {
        let marker = "edge:edge-node-1";
        let raw = broadcast_event("operator_chat:reply", marker, &[("content", "Hello!")]);
        let frames = translate_operator_broadcast_all(marker, &raw);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].1, "final turn events must stay replayable");
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
