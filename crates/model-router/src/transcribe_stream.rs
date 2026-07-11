//! Streaming transcription sessions backed by ElevenLabs' realtime
//! speech-to-text WebSocket API (Scribe v2 Realtime).
//!
//! Wire contract (hotel side is built against exactly this): the controller
//! receives `EmitTask` frames with `kind == "voice.transcribe.stream"` and a
//! `stream_op` of `open` / `chunk` / `end` on role `model.elevenlabs`. Replies
//! are `EmitTask` frames back to the OPEN frame's `reply_to`/`reply_role`/
//! `reply_guest_id` with `action == "transcribe_partial"`:
//!
//! ```json
//! {"action":"transcribe_partial","stream_session_id":"<id>","text":"…","is_final":false}
//! {"action":"transcribe_partial","stream_session_id":"<id>","text":"…","is_final":true}
//! {"action":"transcribe_partial","stream_session_id":"<id>","text":"","is_final":true,"error":"…"}
//! ```
//!
//! Errors ALWAYS produce a terminal `is_final: true` reply so the consumer
//! never hangs.
//!
//! ElevenLabs API facts this module relies on (docs:
//! <https://elevenlabs.io/docs/api-reference/speech-to-text/v-1-speech-to-text-realtime>):
//! - Endpoint: `wss://api.elevenlabs.io/v1/speech-to-text/realtime` with query
//!   params `model_id`, `audio_format` (`pcm_16000` = 16-bit signed LE mono
//!   PCM at 16 kHz — matches our `pcm_s16le`/16000/1 input), `commit_strategy`
//!   (`manual` | `vad`), optional `language_code`.
//! - Auth: `xi-api-key` header.
//! - Realtime model id: `scribe_v2_realtime`.
//! - Client → server: `{"message_type":"input_audio_chunk","audio_base_64":…,
//!   "commit":bool,"sample_rate":int}` (all four fields required). Sending an
//!   empty `audio_base_64` with `commit: true` forces a commit — that is the
//!   end-of-stream flush.
//! - Server → client: `session_started`, `partial_transcript`,
//!   `committed_transcript` (+ `_with_timestamps` variant), and a family of
//!   error message types (`error`, `auth_error`, `quota_exceeded`, …) that all
//!   carry an `"error"` field.

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use philotic_client::{GuestIdentity, IpcRequest, PhiloticClient};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tracing::{info, warn};
use ulid::Ulid;

pub const STREAM_TASK_KIND: &str = "voice.transcribe.stream";
/// Cap on concurrently open streaming sessions per controller process.
pub const DEFAULT_MAX_SESSIONS: usize = 4;
/// A session with no inbound `chunk`/`end` frame for this long is torn down
/// with a terminal error reply.
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 120;
/// ElevenLabs realtime STT model id.
pub const ELEVENLABS_REALTIME_MODEL_ID: &str = "scribe_v2_realtime";
/// How long the finalize path waits for ElevenLabs to answer the forced
/// commit with a `committed_transcript` before falling back to the best
/// transcript accumulated so far.
const FINAL_COMMIT_TIMEOUT_SECS: u64 = 10;
/// Sample rates ElevenLabs accepts for PCM input (`pcm_<rate>` formats).
const SUPPORTED_PCM_SAMPLE_RATES: [u32; 6] = [8000, 16000, 22050, 24000, 44100, 48000];

// ── Inbound frames ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyAddress {
    pub node: String,
    pub role: String,
    pub guest_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRequest {
    pub stream_session_id: String,
    pub sample_rate: u32,
    pub language_code: Option<String>,
    pub reply: ReplyAddress,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamFrame {
    Open(OpenRequest),
    Chunk {
        stream_session_id: String,
        audio_base64: String,
    },
    End {
        stream_session_id: String,
    },
}

/// True when the task JSON is a streaming-transcription frame that must be
/// handled by the session manager instead of normal provider dispatch.
pub fn is_stream_task(task: &Value) -> bool {
    task.get("kind").and_then(Value::as_str) == Some(STREAM_TASK_KIND)
}

/// Best-effort extraction of the session id from any stream frame (used to
/// address terminal error replies for frames that fail full parsing).
pub fn stream_session_id(task: &Value) -> Option<&str> {
    task.get("stream_session_id").and_then(Value::as_str)
}

/// Best-effort extraction of the reply address (only OPEN frames carry one).
pub fn reply_address(task: &Value) -> Option<ReplyAddress> {
    Some(ReplyAddress {
        node: task.get("reply_to").and_then(Value::as_str)?.to_string(),
        role: task.get("reply_role").and_then(Value::as_str)?.to_string(),
        guest_id: task
            .get("reply_guest_id")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

pub fn parse_stream_frame(task: &Value) -> Result<StreamFrame> {
    if !is_stream_task(task) {
        anyhow::bail!("not a {STREAM_TASK_KIND} frame");
    }
    let stream_op = task
        .get("stream_op")
        .and_then(Value::as_str)
        .context("stream frame missing stream_op")?;
    let stream_session_id = stream_session_id(task)
        .context("stream frame missing stream_session_id")?
        .to_string();

    match stream_op {
        "open" => {
            let audio_format = task.get("audio_format").cloned().unwrap_or_else(
                // Default to the contract's canonical format when omitted.
                || json!({"encoding": "pcm_s16le", "sample_rate": 16000, "channels": 1}),
            );
            let encoding = audio_format
                .get("encoding")
                .and_then(Value::as_str)
                .unwrap_or("pcm_s16le");
            if encoding != "pcm_s16le" {
                anyhow::bail!(
                    "unsupported audio encoding [{encoding}]: ElevenLabs realtime STT accepts raw 16-bit signed little-endian PCM (pcm_s16le)"
                );
            }
            let channels = audio_format
                .get("channels")
                .and_then(Value::as_u64)
                .unwrap_or(1);
            if channels != 1 {
                anyhow::bail!("unsupported channel count [{channels}]: audio must be mono");
            }
            let sample_rate = audio_format
                .get("sample_rate")
                .and_then(Value::as_u64)
                .unwrap_or(16000) as u32;
            if !SUPPORTED_PCM_SAMPLE_RATES.contains(&sample_rate) {
                anyhow::bail!(
                    "unsupported sample_rate [{sample_rate}]: ElevenLabs realtime STT accepts {SUPPORTED_PCM_SAMPLE_RATES:?}"
                );
            }
            let reply =
                reply_address(task).context("stream open frame missing reply_to/reply_role")?;
            Ok(StreamFrame::Open(OpenRequest {
                stream_session_id,
                sample_rate,
                language_code: task
                    .get("language_code")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                reply,
            }))
        }
        "chunk" => Ok(StreamFrame::Chunk {
            stream_session_id,
            audio_base64: task
                .get("audio_base64")
                .and_then(Value::as_str)
                .context("stream chunk frame missing audio_base64")?
                .to_string(),
        }),
        "end" => Ok(StreamFrame::End { stream_session_id }),
        other => anyhow::bail!("unknown stream_op [{other}]"),
    }
}

// ── Provider-side abstraction (mocked in tests) ───────────────────────────────

/// Events surfaced by the realtime STT connection reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SttEvent {
    Partial(String),
    Committed(String),
    /// Terminal provider-side error (auth, quota, transcriber failure, …).
    Error(String),
    /// The provider closed the connection.
    Closed,
}

/// Write half of a realtime STT connection.
#[async_trait]
pub trait SttAudioSink: Send {
    /// Forward a base64 PCM chunk. `commit: true` with empty audio forces the
    /// provider to finalize the pending segment (end-of-stream flush).
    async fn send_audio(&mut self, audio_base64: &str, commit: bool) -> Result<()>;
    async fn close(&mut self);
}

/// A connected realtime STT stream: write half + reader events.
pub struct SttStream {
    pub sink: Box<dyn SttAudioSink>,
    pub events: mpsc::Receiver<SttEvent>,
}

#[async_trait]
pub trait SttConnector: Send + Sync {
    async fn connect(&self, open: &OpenRequest) -> Result<SttStream>;
}

/// Where session replies (`transcribe_partial` frames) are delivered.
#[async_trait]
pub trait StreamReplySink: Send {
    async fn send(
        &mut self,
        stream_session_id: &str,
        text: &str,
        is_final: bool,
        error: Option<&str>,
    ) -> Result<()>;
}

/// The exact reply payload contract. Kept as a helper so tests can pin it.
pub fn transcribe_partial_json(
    stream_session_id: &str,
    text: &str,
    is_final: bool,
    error: Option<&str>,
) -> Value {
    let mut payload = json!({
        "action": "transcribe_partial",
        "stream_session_id": stream_session_id,
        "text": text,
        "is_final": is_final,
    });
    if let Some(error) = error {
        payload["error"] = Value::String(error.to_string());
    }
    payload
}

// ── Session manager ───────────────────────────────────────────────────────────

enum SessionCmd {
    Chunk(String),
    End,
}

struct SessionHandle {
    cmd_tx: mpsc::Sender<SessionCmd>,
    task: tokio::task::JoinHandle<()>,
}

pub struct SttSessionManager {
    sessions: HashMap<String, SessionHandle>,
    max_sessions: usize,
    idle_timeout: Duration,
}

impl SttSessionManager {
    pub fn new(max_sessions: usize, idle_timeout: Duration) -> Self {
        Self {
            sessions: HashMap::new(),
            max_sessions,
            idle_timeout,
        }
    }

    pub fn active_sessions(&self) -> usize {
        self.sessions.len()
    }

    /// Drop map entries whose session task has already exited (idle timeout,
    /// provider error, finalize complete).
    fn reap(&mut self) {
        self.sessions.retain(|_, handle| !handle.task.is_finished());
    }

    /// Open a new streaming session. All failure paths deliver a terminal
    /// `is_final: true` error reply through `reply` so the consumer never
    /// hangs.
    pub async fn open(
        &mut self,
        open: OpenRequest,
        connector: Arc<dyn SttConnector>,
        mut reply: Box<dyn StreamReplySink>,
    ) {
        self.reap();
        let session_id = open.stream_session_id.clone();

        if self.sessions.contains_key(&session_id) {
            send_terminal_error(
                reply.as_mut(),
                &session_id,
                "duplicate stream_session_id: a session with this id is already open",
            )
            .await;
            return;
        }
        if self.sessions.len() >= self.max_sessions {
            send_terminal_error(
                reply.as_mut(),
                &session_id,
                &format!(
                    "session limit reached ({} concurrent streaming transcription sessions)",
                    self.max_sessions
                ),
            )
            .await;
            return;
        }

        let stream = match connector.connect(&open).await {
            Ok(stream) => stream,
            Err(err) => {
                send_terminal_error(
                    reply.as_mut(),
                    &session_id,
                    &format!("failed to open ElevenLabs realtime STT stream: {err:#}"),
                )
                .await;
                return;
            }
        };

        let (cmd_tx, cmd_rx) = mpsc::channel::<SessionCmd>(64);
        let idle_timeout = self.idle_timeout;
        let task_session_id = session_id.clone();
        let task = tokio::spawn(async move {
            session_task(task_session_id, stream, cmd_rx, reply, idle_timeout).await;
        });
        info!(
            session = %session_id,
            active = self.sessions.len() + 1,
            "transcribe-stream session opened"
        );
        self.sessions
            .insert(session_id, SessionHandle { cmd_tx, task });
    }

    /// Forward an audio chunk into a session. Returns false when the session
    /// is unknown (never opened, already ended, or torn down).
    pub async fn chunk(&mut self, stream_session_id: &str, audio_base64: String) -> bool {
        let Some(handle) = self.sessions.get(stream_session_id) else {
            return false;
        };
        if handle
            .cmd_tx
            .send(SessionCmd::Chunk(audio_base64))
            .await
            .is_err()
        {
            // Session task already exited (idle timeout / provider error).
            self.sessions.remove(stream_session_id);
            return false;
        }
        true
    }

    /// Signal end-of-audio. The session task flushes the provider, emits the
    /// terminal `is_final: true` reply, and exits. Returns false when the
    /// session is unknown.
    pub async fn end(&mut self, stream_session_id: &str) -> bool {
        let Some(handle) = self.sessions.remove(stream_session_id) else {
            return false;
        };
        if handle.cmd_tx.send(SessionCmd::End).await.is_err() {
            // Task already exited; it emitted its own terminal reply.
            return false;
        }
        true
    }

    /// Clean shutdown: ask every live session to finalize and wait for the
    /// session tasks to emit their terminal replies and close their sockets.
    pub async fn shutdown(&mut self) {
        let sessions = std::mem::take(&mut self.sessions);
        for (session_id, handle) in sessions {
            let _ = handle.cmd_tx.send(SessionCmd::End).await;
            if tokio::time::timeout(
                Duration::from_secs(FINAL_COMMIT_TIMEOUT_SECS + 2),
                handle.task,
            )
            .await
            .is_err()
            {
                warn!(session = %session_id, "transcribe-stream session did not shut down in time");
            }
        }
    }
}

async fn send_terminal_error(reply: &mut dyn StreamReplySink, session_id: &str, error: &str) {
    warn!(session = %session_id, "transcribe-stream terminal error: {error}");
    if let Err(err) = reply.send(session_id, "", true, Some(error)).await {
        warn!(session = %session_id, "failed to deliver terminal stream error reply: {err:#}");
    }
}

// ── Per-session task ──────────────────────────────────────────────────────────

/// Accumulated transcript state for one session.
#[derive(Default)]
struct TranscriptState {
    committed: Vec<String>,
    partial: String,
}

impl TranscriptState {
    fn full_text(&self) -> String {
        let mut parts: Vec<&str> = self
            .committed
            .iter()
            .map(String::as_str)
            .filter(|s| !s.trim().is_empty())
            .collect();
        if !self.partial.trim().is_empty() {
            parts.push(self.partial.as_str());
        }
        parts.join(" ")
    }

    fn apply(&mut self, event: &SttEvent) {
        match event {
            SttEvent::Partial(text) => self.partial = text.clone(),
            SttEvent::Committed(text) => {
                self.committed.push(text.clone());
                self.partial.clear();
            }
            SttEvent::Error(_) | SttEvent::Closed => {}
        }
    }
}

async fn session_task(
    session_id: String,
    mut stream: SttStream,
    mut cmd_rx: mpsc::Receiver<SessionCmd>,
    mut reply: Box<dyn StreamReplySink>,
    idle_timeout: Duration,
) {
    let mut transcript = TranscriptState::default();
    let mut last_sent = String::new();
    let mut idle_deadline = Instant::now() + idle_timeout;

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                idle_deadline = Instant::now() + idle_timeout;
                match cmd {
                    Some(SessionCmd::Chunk(audio_base64)) => {
                        if let Err(err) = stream.sink.send_audio(&audio_base64, false).await {
                            send_terminal_error(
                                reply.as_mut(),
                                &session_id,
                                &format!("failed to forward audio to ElevenLabs: {err:#}"),
                            )
                            .await;
                            stream.sink.close().await;
                            return;
                        }
                    }
                    // Explicit END, or the manager was dropped/shut down —
                    // both finalize the session.
                    Some(SessionCmd::End) | None => {
                        finalize_session(&session_id, &mut stream, &mut transcript, reply.as_mut())
                            .await;
                        stream.sink.close().await;
                        return;
                    }
                }
            }
            event = stream.events.recv() => {
                match event {
                    Some(SttEvent::Error(err)) => {
                        send_terminal_error(
                            reply.as_mut(),
                            &session_id,
                            &format!("ElevenLabs realtime STT error: {err}"),
                        )
                        .await;
                        stream.sink.close().await;
                        return;
                    }
                    Some(SttEvent::Closed) | None => {
                        send_terminal_error(
                            reply.as_mut(),
                            &session_id,
                            "ElevenLabs realtime STT connection closed unexpectedly",
                        )
                        .await;
                        return;
                    }
                    Some(event) => {
                        transcript.apply(&event);
                        let text = transcript.full_text();
                        if text != last_sent && !text.is_empty() {
                            last_sent = text.clone();
                            if let Err(err) = reply.send(&session_id, &text, false, None).await {
                                warn!(
                                    session = %session_id,
                                    "failed to deliver partial transcript reply: {err:#}"
                                );
                            }
                        }
                    }
                }
            }
            _ = tokio::time::sleep_until(idle_deadline) => {
                send_terminal_error(
                    reply.as_mut(),
                    &session_id,
                    &format!(
                        "idle timeout: no audio received for {}s",
                        idle_timeout.as_secs()
                    ),
                )
                .await;
                stream.sink.close().await;
                return;
            }
        }
    }
}

/// End-of-stream flush: force a commit (empty audio + `commit: true`), drain
/// events until the provider answers with a `committed_transcript` (or the
/// flush window lapses), and emit the terminal `is_final: true` reply.
async fn finalize_session(
    session_id: &str,
    stream: &mut SttStream,
    transcript: &mut TranscriptState,
    reply: &mut dyn StreamReplySink,
) {
    let commit_sent = stream.sink.send_audio("", true).await;
    if let Err(err) = &commit_sent {
        warn!(session = %session_id, "final commit send failed: {err:#}");
    }

    if commit_sent.is_ok() {
        let deadline = Instant::now() + Duration::from_secs(FINAL_COMMIT_TIMEOUT_SECS);
        loop {
            let event = match tokio::time::timeout_at(deadline, stream.events.recv()).await {
                Ok(Some(event)) => event,
                // Channel closed or flush window lapsed — settle with what we have.
                Ok(None) | Err(_) => break,
            };
            match event {
                SttEvent::Committed(_) => {
                    transcript.apply(&event);
                    break;
                }
                SttEvent::Partial(_) => transcript.apply(&event),
                SttEvent::Error(err) => {
                    // A flush against a session with no pending speech can
                    // surface e.g. `insufficient_audio_activity`. If we hold
                    // any transcript, finish gracefully with it; a session
                    // that produced nothing surfaces the error.
                    if transcript.full_text().is_empty() {
                        send_terminal_error(
                            reply,
                            session_id,
                            &format!("ElevenLabs realtime STT error during finalize: {err}"),
                        )
                        .await;
                        return;
                    }
                    break;
                }
                SttEvent::Closed => break,
            }
        }
    }

    let final_text = transcript.full_text();
    info!(
        session = %session_id,
        chars = final_text.len(),
        "transcribe-stream session finalized"
    );
    if let Err(err) = reply.send(session_id, &final_text, true, None).await {
        warn!(session = %session_id, "failed to deliver final transcript reply: {err:#}");
    }
}

// ── ElevenLabs realtime connector (real implementation) ──────────────────────

pub struct ElevenLabsRealtimeConnector {
    api_key: String,
    model_id: String,
    base_url: String,
}

impl ElevenLabsRealtimeConnector {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            model_id: ELEVENLABS_REALTIME_MODEL_ID.to_string(),
            base_url: "wss://api.elevenlabs.io".to_string(),
        }
    }

    fn websocket_url(&self, open: &OpenRequest) -> String {
        let mut url = format!(
            "{}/v1/speech-to-text/realtime?model_id={}&audio_format=pcm_{}&commit_strategy=manual",
            self.base_url, self.model_id, open.sample_rate
        );
        if let Some(language_code) = open.language_code.as_deref() {
            url.push_str("&language_code=");
            url.push_str(language_code);
        }
        url
    }
}

/// Build the exact ElevenLabs `input_audio_chunk` client message.
fn input_audio_chunk_json(audio_base64: &str, commit: bool, sample_rate: u32) -> Value {
    json!({
        "message_type": "input_audio_chunk",
        "audio_base_64": audio_base64,
        "commit": commit,
        "sample_rate": sample_rate,
    })
}

/// Map an ElevenLabs server text frame to a session event. `session_started`
/// (and anything unrecognized without an `error` field) is ignored.
fn parse_el_server_message(raw: &str) -> Option<SttEvent> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let message_type = value.get("message_type").and_then(Value::as_str)?;
    let text = || {
        value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    match message_type {
        "partial_transcript" => Some(SttEvent::Partial(text())),
        "committed_transcript" | "committed_transcript_with_timestamps" => {
            Some(SttEvent::Committed(text()))
        }
        "session_started" => None,
        other => {
            // Every documented error message type carries an `error` field
            // (error, auth_error, quota_exceeded, rate_limited, input_error,
            // transcriber_error, session_time_limit_exceeded, …).
            let error = value.get("error").and_then(Value::as_str)?;
            Some(SttEvent::Error(format!("{other}: {error}")))
        }
    }
}

struct ElevenLabsWsSink {
    write: futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        WsMessage,
    >,
    sample_rate: u32,
}

#[async_trait]
impl SttAudioSink for ElevenLabsWsSink {
    async fn send_audio(&mut self, audio_base64: &str, commit: bool) -> Result<()> {
        let payload = input_audio_chunk_json(audio_base64, commit, self.sample_rate).to_string();
        self.write
            .send(WsMessage::Text(payload))
            .await
            .map_err(|err| anyhow!("elevenlabs realtime ws send failed: {err}"))
    }

    async fn close(&mut self) {
        let _ = self.write.send(WsMessage::Close(None)).await;
    }
}

#[async_trait]
impl SttConnector for ElevenLabsRealtimeConnector {
    async fn connect(&self, open: &OpenRequest) -> Result<SttStream> {
        let url = self.websocket_url(open);
        let mut request = url
            .clone()
            .into_client_request()
            .context("invalid ElevenLabs realtime STT url")?;
        request.headers_mut().insert(
            "xi-api-key",
            self.api_key
                .parse()
                .context("ElevenLabs API key is not a valid header value")?,
        );

        let (ws, _) = tokio::time::timeout(Duration::from_secs(15), connect_async(request))
            .await
            .map_err(|_| anyhow!("timed out connecting to ElevenLabs realtime STT"))?
            .context("ElevenLabs realtime STT websocket connect failed")?;

        let (write, mut read) = ws.split();
        let (event_tx, event_rx) = mpsc::channel::<SttEvent>(256);
        let session_id = open.stream_session_id.clone();
        tokio::spawn(async move {
            while let Some(message) = read.next().await {
                match message {
                    Ok(WsMessage::Text(text)) => {
                        if let Some(event) = parse_el_server_message(&text) {
                            let terminal = matches!(event, SttEvent::Error(_));
                            if event_tx.send(event).await.is_err() || terminal {
                                break;
                            }
                        }
                    }
                    Ok(WsMessage::Close(_)) => {
                        let _ = event_tx.send(SttEvent::Closed).await;
                        break;
                    }
                    Ok(_) => {}
                    Err(err) => {
                        let _ = event_tx
                            .send(SttEvent::Error(format!("websocket read failed: {err}")))
                            .await;
                        break;
                    }
                }
            }
            info!(session = %session_id, "elevenlabs realtime ws reader exited");
        });

        Ok(SttStream {
            sink: Box::new(ElevenLabsWsSink {
                write,
                sample_rate: open.sample_rate,
            }),
            events: event_rx,
        })
    }
}

// ── IPC reply sink (real implementation) ─────────────────────────────────────

/// Delivers `transcribe_partial` frames back to the hotel over a dedicated
/// IPC connection (session replies are produced from spawned session tasks,
/// so they cannot share the controller's main IPC client).
pub struct IpcStreamReplySink {
    ipc: PhiloticClient,
    reply: ReplyAddress,
}

impl IpcStreamReplySink {
    pub async fn connect(controller_guest_id: &str, reply: ReplyAddress) -> Result<Self> {
        let identity = GuestIdentity {
            guest_id: format!("stt-stream-{}", Ulid::new()),
            role: controller_guest_id.to_string(),
            supported_tools: Vec::new(),
        };
        let ipc = tokio::time::timeout(Duration::from_secs(5), PhiloticClient::connect(identity))
            .await
            .map_err(|_| anyhow!("timed out connecting stream reply IPC"))??;
        Ok(Self { ipc, reply })
    }
}

#[async_trait]
impl StreamReplySink for IpcStreamReplySink {
    async fn send(
        &mut self,
        stream_session_id: &str,
        text: &str,
        is_final: bool,
        error: Option<&str>,
    ) -> Result<()> {
        let task_json =
            transcribe_partial_json(stream_session_id, text, is_final, error).to_string();
        tokio::time::timeout(
            Duration::from_secs(10),
            self.ipc.send_request(IpcRequest::EmitTask {
                target_node: self.reply.node.clone(),
                target_role: self.reply.role.clone(),
                target_guest_id: self.reply.guest_id.clone(),
                task_json,
            }),
        )
        .await
        .map_err(|_| anyhow!("transcribe_partial emit: ipc ack timeout after 10s"))??;
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // ── Mocks ────────────────────────────────────────────────────────────────

    #[derive(Clone, Default)]
    struct SinkLog {
        sent: Arc<Mutex<Vec<(String, bool)>>>,
        closed: Arc<Mutex<bool>>,
        fail_sends: Arc<Mutex<bool>>,
    }

    struct MockAudioSink {
        log: SinkLog,
    }

    #[async_trait]
    impl SttAudioSink for MockAudioSink {
        async fn send_audio(&mut self, audio_base64: &str, commit: bool) -> Result<()> {
            if *self.log.fail_sends.lock().unwrap() {
                anyhow::bail!("mock send failure");
            }
            self.log
                .sent
                .lock()
                .unwrap()
                .push((audio_base64.to_string(), commit));
            Ok(())
        }

        async fn close(&mut self) {
            *self.log.closed.lock().unwrap() = true;
        }
    }

    /// Connector handing out pre-built mock streams; the test keeps the event
    /// sender halves to script provider behavior.
    struct MockConnector {
        streams: Mutex<Vec<SttStream>>,
    }

    #[async_trait]
    impl SttConnector for MockConnector {
        async fn connect(&self, _open: &OpenRequest) -> Result<SttStream> {
            self.streams
                .lock()
                .unwrap()
                .pop()
                .context("mock connector out of streams")
        }
    }

    struct FailingConnector;

    #[async_trait]
    impl SttConnector for FailingConnector {
        async fn connect(&self, _open: &OpenRequest) -> Result<SttStream> {
            anyhow::bail!("mock connect refused")
        }
    }

    #[derive(Debug, Clone, PartialEq)]
    struct Reply {
        session: String,
        text: String,
        is_final: bool,
        error: Option<String>,
    }

    #[derive(Clone, Default)]
    struct MockReplySink {
        replies: Arc<Mutex<Vec<Reply>>>,
    }

    #[async_trait]
    impl StreamReplySink for MockReplySink {
        async fn send(
            &mut self,
            stream_session_id: &str,
            text: &str,
            is_final: bool,
            error: Option<&str>,
        ) -> Result<()> {
            self.replies.lock().unwrap().push(Reply {
                session: stream_session_id.to_string(),
                text: text.to_string(),
                is_final,
                error: error.map(str::to_string),
            });
            Ok(())
        }
    }

    fn mock_stream(log: &SinkLog) -> (SttStream, mpsc::Sender<SttEvent>) {
        let (event_tx, events) = mpsc::channel(64);
        (
            SttStream {
                sink: Box::new(MockAudioSink { log: log.clone() }),
                events,
            },
            event_tx,
        )
    }

    fn open_request(id: &str) -> OpenRequest {
        OpenRequest {
            stream_session_id: id.to_string(),
            sample_rate: 16000,
            language_code: None,
            reply: ReplyAddress {
                node: "hotel-a".into(),
                role: "edge-mesh".into(),
                guest_id: Some("edge-guest".into()),
            },
        }
    }

    async fn wait_for<F: Fn() -> bool>(cond: F) {
        for _ in 0..200 {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("condition not reached within 1s");
    }

    // ── Frame parsing ────────────────────────────────────────────────────────

    #[test]
    fn parses_open_chunk_end_frames() {
        let open = json!({
            "kind": "voice.transcribe.stream",
            "stream_op": "open",
            "stream_session_id": "s-1",
            "audio_format": {"encoding": "pcm_s16le", "sample_rate": 16000, "channels": 1},
            "reply_to": "hotel-a",
            "reply_role": "edge-mesh",
            "reply_guest_id": "edge-guest",
        });
        assert!(is_stream_task(&open));
        assert_eq!(
            parse_stream_frame(&open).unwrap(),
            StreamFrame::Open(open_request("s-1"))
        );

        let chunk = json!({
            "kind": "voice.transcribe.stream",
            "stream_op": "chunk",
            "stream_session_id": "s-1",
            "audio_base64": "AAAA",
        });
        assert_eq!(
            parse_stream_frame(&chunk).unwrap(),
            StreamFrame::Chunk {
                stream_session_id: "s-1".into(),
                audio_base64: "AAAA".into()
            }
        );

        let end = json!({
            "kind": "voice.transcribe.stream",
            "stream_op": "end",
            "stream_session_id": "s-1",
        });
        assert_eq!(
            parse_stream_frame(&end).unwrap(),
            StreamFrame::End {
                stream_session_id: "s-1".into()
            }
        );
    }

    #[test]
    fn open_rejects_non_pcm_s16le_and_stereo() {
        let bad_encoding = json!({
            "kind": "voice.transcribe.stream",
            "stream_op": "open",
            "stream_session_id": "s-1",
            "audio_format": {"encoding": "opus", "sample_rate": 16000, "channels": 1},
            "reply_to": "hotel-a",
            "reply_role": "edge-mesh",
        });
        assert!(parse_stream_frame(&bad_encoding).is_err());

        let stereo = json!({
            "kind": "voice.transcribe.stream",
            "stream_op": "open",
            "stream_session_id": "s-1",
            "audio_format": {"encoding": "pcm_s16le", "sample_rate": 16000, "channels": 2},
            "reply_to": "hotel-a",
            "reply_role": "edge-mesh",
        });
        assert!(parse_stream_frame(&stereo).is_err());

        let bad_rate = json!({
            "kind": "voice.transcribe.stream",
            "stream_op": "open",
            "stream_session_id": "s-1",
            "audio_format": {"encoding": "pcm_s16le", "sample_rate": 11025, "channels": 1},
            "reply_to": "hotel-a",
            "reply_role": "edge-mesh",
        });
        assert!(parse_stream_frame(&bad_rate).is_err());
    }

    // ── Wire-format pins ─────────────────────────────────────────────────────

    /// Pins the exact ElevenLabs client message (docs: realtime STT API
    /// reference — `message_type`, `audio_base_64`, `commit`, `sample_rate`
    /// are all required).
    #[test]
    fn input_audio_chunk_message_matches_elevenlabs_schema() {
        assert_eq!(
            input_audio_chunk_json("cGNtYXVkaW8=", false, 16000),
            json!({
                "message_type": "input_audio_chunk",
                "audio_base_64": "cGNtYXVkaW8=",
                "commit": false,
                "sample_rate": 16000,
            })
        );
        // End-of-stream flush: empty audio, commit=true.
        assert_eq!(
            input_audio_chunk_json("", true, 16000),
            json!({
                "message_type": "input_audio_chunk",
                "audio_base_64": "",
                "commit": true,
                "sample_rate": 16000,
            })
        );
    }

    #[test]
    fn parses_elevenlabs_server_messages() {
        assert_eq!(
            parse_el_server_message(r#"{"message_type":"partial_transcript","text":"hel"}"#),
            Some(SttEvent::Partial("hel".into()))
        );
        assert_eq!(
            parse_el_server_message(r#"{"message_type":"committed_transcript","text":"hello"}"#),
            Some(SttEvent::Committed("hello".into()))
        );
        assert_eq!(
            parse_el_server_message(
                r#"{"message_type":"committed_transcript_with_timestamps","text":"hello","words":[]}"#
            ),
            Some(SttEvent::Committed("hello".into()))
        );
        // session_started is ignored.
        assert_eq!(
            parse_el_server_message(r#"{"message_type":"session_started","session_id":"x"}"#),
            None
        );
        // All documented error message types carry an `error` field.
        assert_eq!(
            parse_el_server_message(r#"{"message_type":"auth_error","error":"bad key"}"#),
            Some(SttEvent::Error("auth_error: bad key".into()))
        );
        assert_eq!(
            parse_el_server_message(r#"{"message_type":"quota_exceeded","error":"quota"}"#),
            Some(SttEvent::Error("quota_exceeded: quota".into()))
        );
    }

    #[test]
    fn transcribe_partial_reply_matches_contract() {
        assert_eq!(
            transcribe_partial_json("s-1", "so far", false, None),
            json!({
                "action": "transcribe_partial",
                "stream_session_id": "s-1",
                "text": "so far",
                "is_final": false,
            })
        );
        assert_eq!(
            transcribe_partial_json("s-1", "", true, Some("boom")),
            json!({
                "action": "transcribe_partial",
                "stream_session_id": "s-1",
                "text": "",
                "is_final": true,
                "error": "boom",
            })
        );
    }

    #[test]
    fn realtime_websocket_url_pins_endpoint_model_and_format() {
        let connector = ElevenLabsRealtimeConnector::new("key".into());
        assert_eq!(
            connector.websocket_url(&open_request("s-1")),
            "wss://api.elevenlabs.io/v1/speech-to-text/realtime?model_id=scribe_v2_realtime&audio_format=pcm_16000&commit_strategy=manual"
        );
        let mut with_lang = open_request("s-1");
        with_lang.language_code = Some("en".into());
        assert!(
            connector
                .websocket_url(&with_lang)
                .ends_with("&language_code=en")
        );
    }

    // ── Session lifecycle ────────────────────────────────────────────────────

    #[tokio::test]
    async fn open_chunk_partial_end_final_lifecycle() {
        let mut manager = SttSessionManager::new(4, Duration::from_secs(120));
        let log = SinkLog::default();
        let (stream, event_tx) = mock_stream(&log);
        let connector = Arc::new(MockConnector {
            streams: Mutex::new(vec![stream]),
        });
        let replies = MockReplySink::default();

        manager
            .open(open_request("s-1"), connector, Box::new(replies.clone()))
            .await;
        assert_eq!(manager.active_sessions(), 1);

        // CHUNK forwards audio with commit=false.
        assert!(manager.chunk("s-1", "QUJD".into()).await);
        wait_for(|| log.sent.lock().unwrap().len() == 1).await;
        assert_eq!(log.sent.lock().unwrap()[0], ("QUJD".to_string(), false));

        // Provider partials fan out as non-final replies.
        event_tx
            .send(SttEvent::Partial("hel".into()))
            .await
            .unwrap();
        event_tx
            .send(SttEvent::Partial("hello wor".into()))
            .await
            .unwrap();
        wait_for(|| replies.replies.lock().unwrap().len() == 2).await;
        {
            let replies = replies.replies.lock().unwrap();
            assert_eq!(replies[0].text, "hel");
            assert!(!replies[0].is_final);
            assert_eq!(replies[1].text, "hello wor");
            assert!(!replies[1].is_final);
        }

        // END flushes with an empty commit chunk, collects the committed
        // transcript, emits the terminal reply, closes the stream.
        assert!(manager.end("s-1").await);
        wait_for(|| log.sent.lock().unwrap().len() == 2).await;
        assert_eq!(log.sent.lock().unwrap()[1], (String::new(), true));
        event_tx
            .send(SttEvent::Committed("hello world".into()))
            .await
            .unwrap();
        wait_for(|| replies.replies.lock().unwrap().last().map(|r| r.is_final) == Some(true)).await;
        {
            let replies = replies.replies.lock().unwrap();
            let last = replies.last().unwrap();
            assert_eq!(last.text, "hello world");
            assert!(last.is_final);
            assert_eq!(last.error, None);
        }
        wait_for(|| *log.closed.lock().unwrap()).await;
        assert_eq!(manager.active_sessions(), 0);
    }

    #[tokio::test]
    async fn chunk_and_end_to_unknown_session_return_false() {
        let mut manager = SttSessionManager::new(4, Duration::from_secs(120));
        assert!(!manager.chunk("nope", "QUJD".into()).await);
        assert!(!manager.end("nope").await);
    }

    #[tokio::test]
    async fn session_cap_rejects_fifth_session_with_terminal_error() {
        let mut manager = SttSessionManager::new(4, Duration::from_secs(120));
        let log = SinkLog::default();
        let mut streams = Vec::new();
        let mut event_txs = Vec::new();
        for _ in 0..4 {
            let (stream, event_tx) = mock_stream(&log);
            streams.push(stream);
            event_txs.push(event_tx);
        }
        let connector = Arc::new(MockConnector {
            streams: Mutex::new(streams),
        });
        let replies = MockReplySink::default();

        for i in 0..4 {
            manager
                .open(
                    open_request(&format!("s-{i}")),
                    connector.clone(),
                    Box::new(replies.clone()),
                )
                .await;
        }
        assert_eq!(manager.active_sessions(), 4);

        manager
            .open(
                open_request("s-overflow"),
                connector,
                Box::new(replies.clone()),
            )
            .await;
        let replies = replies.replies.lock().unwrap();
        let overflow: Vec<_> = replies
            .iter()
            .filter(|r| r.session == "s-overflow")
            .collect();
        assert_eq!(overflow.len(), 1);
        assert!(overflow[0].is_final);
        assert!(
            overflow[0]
                .error
                .as_deref()
                .unwrap()
                .contains("session limit")
        );
    }

    #[tokio::test]
    async fn duplicate_open_gets_terminal_error_without_touching_existing_session() {
        let mut manager = SttSessionManager::new(4, Duration::from_secs(120));
        let log = SinkLog::default();
        let (stream, _event_tx) = mock_stream(&log);
        let connector = Arc::new(MockConnector {
            streams: Mutex::new(vec![stream]),
        });
        let replies = MockReplySink::default();

        manager
            .open(
                open_request("s-1"),
                connector.clone(),
                Box::new(replies.clone()),
            )
            .await;
        manager
            .open(open_request("s-1"), connector, Box::new(replies.clone()))
            .await;

        let replies = replies.replies.lock().unwrap();
        assert_eq!(replies.len(), 1);
        assert!(replies[0].is_final);
        assert!(
            replies[0]
                .error
                .as_deref()
                .unwrap()
                .contains("duplicate stream_session_id")
        );
        assert_eq!(manager.active_sessions(), 1);
    }

    #[tokio::test]
    async fn connect_failure_delivers_terminal_error() {
        let mut manager = SttSessionManager::new(4, Duration::from_secs(120));
        let replies = MockReplySink::default();
        manager
            .open(
                open_request("s-1"),
                Arc::new(FailingConnector),
                Box::new(replies.clone()),
            )
            .await;
        let replies = replies.replies.lock().unwrap();
        assert_eq!(replies.len(), 1);
        assert!(replies[0].is_final);
        assert!(
            replies[0]
                .error
                .as_deref()
                .unwrap()
                .contains("mock connect refused")
        );
        assert_eq!(manager.active_sessions(), 0);
    }

    #[tokio::test]
    async fn idle_timeout_tears_down_session_with_terminal_error() {
        let mut manager = SttSessionManager::new(4, Duration::from_millis(80));
        let log = SinkLog::default();
        let (stream, _event_tx) = mock_stream(&log);
        let connector = Arc::new(MockConnector {
            streams: Mutex::new(vec![stream]),
        });
        let replies = MockReplySink::default();

        manager
            .open(open_request("s-1"), connector, Box::new(replies.clone()))
            .await;
        wait_for(|| {
            replies
                .replies
                .lock()
                .unwrap()
                .last()
                .map(|r| r.is_final)
                .unwrap_or(false)
        })
        .await;
        {
            let replies = replies.replies.lock().unwrap();
            let last = replies.last().unwrap();
            assert!(last.error.as_deref().unwrap().contains("idle timeout"));
        }
        wait_for(|| *log.closed.lock().unwrap()).await;
        // The dead session no longer accepts chunks and is reaped.
        assert!(!manager.chunk("s-1", "QUJD".into()).await);
        assert_eq!(manager.active_sessions(), 0);
    }

    #[tokio::test]
    async fn provider_error_mid_stream_delivers_terminal_error() {
        let mut manager = SttSessionManager::new(4, Duration::from_secs(120));
        let log = SinkLog::default();
        let (stream, event_tx) = mock_stream(&log);
        let connector = Arc::new(MockConnector {
            streams: Mutex::new(vec![stream]),
        });
        let replies = MockReplySink::default();

        manager
            .open(open_request("s-1"), connector, Box::new(replies.clone()))
            .await;
        event_tx
            .send(SttEvent::Error("quota_exceeded: out of credits".into()))
            .await
            .unwrap();
        wait_for(|| {
            replies
                .replies
                .lock()
                .unwrap()
                .last()
                .map(|r| r.is_final)
                .unwrap_or(false)
        })
        .await;
        let replies = replies.replies.lock().unwrap();
        let last = replies.last().unwrap();
        assert!(last.error.as_deref().unwrap().contains("quota_exceeded"));
        assert_eq!(last.text, "");
    }

    #[tokio::test]
    async fn end_without_committed_reply_falls_back_to_last_partial() {
        let mut manager = SttSessionManager::new(4, Duration::from_secs(120));
        let log = SinkLog::default();
        let (stream, event_tx) = mock_stream(&log);
        let connector = Arc::new(MockConnector {
            streams: Mutex::new(vec![stream]),
        });
        let replies = MockReplySink::default();

        manager
            .open(open_request("s-1"), connector, Box::new(replies.clone()))
            .await;
        event_tx
            .send(SttEvent::Partial("almost done".into()))
            .await
            .unwrap();
        wait_for(|| !replies.replies.lock().unwrap().is_empty()).await;

        assert!(manager.end("s-1").await);
        // Provider closes without ever committing.
        wait_for(|| log.sent.lock().unwrap().iter().any(|(_, commit)| *commit)).await;
        event_tx.send(SttEvent::Closed).await.unwrap();
        wait_for(|| {
            replies
                .replies
                .lock()
                .unwrap()
                .last()
                .map(|r| r.is_final)
                .unwrap_or(false)
        })
        .await;
        let replies = replies.replies.lock().unwrap();
        let last = replies.last().unwrap();
        assert_eq!(last.text, "almost done");
        assert_eq!(last.error, None);
    }

    #[tokio::test]
    async fn shutdown_finalizes_all_sessions() {
        let mut manager = SttSessionManager::new(4, Duration::from_secs(120));
        let log_a = SinkLog::default();
        let log_b = SinkLog::default();
        let (stream_a, event_tx_a) = mock_stream(&log_a);
        let (stream_b, event_tx_b) = mock_stream(&log_b);
        let connector = Arc::new(MockConnector {
            streams: Mutex::new(vec![stream_b, stream_a]),
        });
        let replies = MockReplySink::default();

        manager
            .open(
                open_request("s-a"),
                connector.clone(),
                Box::new(replies.clone()),
            )
            .await;
        manager
            .open(open_request("s-b"), connector, Box::new(replies.clone()))
            .await;

        // Answer each session's finalize commit so shutdown resolves quickly.
        for (log, event_tx, text) in [
            (log_a.clone(), event_tx_a, "bye a"),
            (log_b.clone(), event_tx_b, "bye b"),
        ] {
            tokio::spawn(async move {
                loop {
                    if log.sent.lock().unwrap().iter().any(|(_, commit)| *commit) {
                        let _ = event_tx.send(SttEvent::Committed(text.into())).await;
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            });
        }

        manager.shutdown().await;
        assert_eq!(manager.active_sessions(), 0);
        let replies = replies.replies.lock().unwrap();
        let finals: Vec<_> = replies.iter().filter(|r| r.is_final).collect();
        assert_eq!(finals.len(), 2);
        assert!(*log_a.closed.lock().unwrap());
        assert!(*log_b.closed.lock().unwrap());
    }
}
