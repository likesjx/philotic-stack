use anyhow::Result;
// Telegram gateway guest built on the `membrane` SDK: `MembraneRuntime` owns
// the IPC lifecycle (registration, reconnect, renew tick, inbound dispatch)
// and `TelegramSeatGuest` provides the Telegram-specific behaviour. One
// runtime instance runs per seat, preserving the one-process/N-seats model.
use async_trait::async_trait;
use clap::Parser;
use membrane::{
    InboundEnvelope, LeaseAcquireOutcome, LeaseBackend, LeaseDriver, LeaseDriverConfig, LeaseEvent,
    LeaseRenewResult, MembraneGuest, MembraneRuntime, OutboundReply, SenderInfo,
};
use philotic_client::{CommandManifestEntry, IpcRequest, IpcResponse, PhiloticClient};
use pulldown_cmark::{
    CodeBlockKind, Event, LinkType, Options, Parser as MarkdownParser, Tag, TagEnd,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

const TELEGRAM_POLL_TIMEOUT_SECS: u64 = 10;
const MEMBRANE_ERROR_BACKOFF_INITIAL_SECS: u64 = 1;
const MEMBRANE_ERROR_BACKOFF_MAX_SECS: u64 = 600;

/// Shared map of in-flight turns, keyed by session_id. Written by the seat's
/// poll task (turn start) and by `handle_push` (turn lifecycle + final reply).
/// Guarded by a std mutex — never held across an await.
type ActiveTurns = Arc<StdMutex<HashMap<String, ActiveTurn>>>;

fn next_error_backoff_secs(current_secs: u64) -> u64 {
    current_secs.saturating_mul(2).clamp(
        MEMBRANE_ERROR_BACKOFF_INITIAL_SECS,
        MEMBRANE_ERROR_BACKOFF_MAX_SECS,
    )
}

fn local_node_id() -> String {
    std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string())
}

fn local_guest_id() -> String {
    std::env::var("PHILOTIC_GUEST_ID").unwrap_or_else(|_| "membrane-telegram-01".to_string())
}

fn hotel_socket_path() -> String {
    std::env::var("PHILOTIC_HOTEL_SOCKET").unwrap_or_else(|_| "/tmp/philotic-aiua.sock".to_string())
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Port of the local Ansible daemon Hotel Manager (IPC port)
    #[arg(short, long, default_value_t = 9000)]
    ansible_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TelegramMessageEnvelope {
    session_id: String,
    turn_id: String,
    chat_id: String,
    thread_id: Option<String>,
    sender_id: Option<String>,
    sender_username: Option<String>,
    /// The chat type: "private", "group", "supergroup", or "channel".
    chat_type: Option<String>,
    /// The sender's first name (display name, always present for real users).
    sender_first_name: Option<String>,
    message_kind: &'static str,
    content: String,
    attachments: Vec<Value>,
    command: Option<String>,
    callback_data: Option<String>,
    raw_transport_event: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TelegramFormattedText {
    text: String,
    parse_mode: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TelegramBotCommand {
    command: &'static str,
    description: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TelegramFileRef {
    file_path: String,
    file_size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BlockKind {
    Paragraph,
    Heading,
    BlockQuote,
    CodeBlock(Option<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ListKind {
    Bullet,
    Ordered(u64),
}

#[derive(Debug, Default)]
struct TelegramHtmlRenderer {
    output: String,
    block_stack: Vec<BlockKind>,
    list_stack: Vec<ListKind>,
    list_item_stack: Vec<usize>,
    pending_link: Option<String>,
}

impl TelegramHtmlRenderer {
    fn render(markdown: &str) -> TelegramFormattedText {
        let parser = MarkdownParser::new_ext(markdown, Options::all());
        let mut renderer = Self::default();
        for event in parser {
            renderer.push_event(event);
        }
        let text = renderer.finish();
        if text.is_empty() {
            return TelegramFormattedText {
                text: String::new(),
                parse_mode: "HTML",
            };
        }
        TelegramFormattedText {
            text,
            parse_mode: "HTML",
        }
    }

    fn push_event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => self.push_text(&text),
            Event::Code(text) => {
                self.output.push_str("<code>");
                self.output.push_str(&escape_html(&text));
                self.output.push_str("</code>");
            }
            Event::Html(text) | Event::InlineHtml(text) => self.push_text(&text),
            Event::SoftBreak | Event::HardBreak => {
                self.output.push('\n');
                if self.in_blockquote() {
                    self.output.push_str("&gt; ");
                }
            }
            Event::Rule => {
                self.ensure_block_spacing();
                self.output.push_str("----------\n");
            }
            Event::FootnoteReference(text) => {
                self.output.push('[');
                self.output.push_str(&escape_html(&text));
                self.output.push(']');
            }
            Event::TaskListMarker(checked) => {
                self.output.push_str(if checked { "[x] " } else { "[ ] " });
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => self.push_text(&text),
        }
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                if !self.in_blockquote() {
                    self.ensure_block_spacing();
                }
                self.block_stack.push(BlockKind::Paragraph);
            }
            Tag::Heading { .. } => {
                self.ensure_block_spacing();
                self.block_stack.push(BlockKind::Heading);
                self.output.push_str("<b>");
            }
            Tag::BlockQuote(_) => {
                self.ensure_block_spacing();
                self.block_stack.push(BlockKind::BlockQuote);
                self.output.push_str("&gt; ");
            }
            Tag::CodeBlock(kind) => {
                self.ensure_block_spacing();
                let language = match kind {
                    CodeBlockKind::Fenced(language) => {
                        let language = language.trim();
                        (!language.is_empty()).then(|| language.to_string())
                    }
                    CodeBlockKind::Indented => None,
                };
                self.block_stack
                    .push(BlockKind::CodeBlock(language.clone()));
                self.output.push_str("<pre><code");
                if let Some(language) = language {
                    self.output.push_str(" class=\"language-");
                    self.output.push_str(&escape_html_attribute(&language));
                    self.output.push('"');
                }
                self.output.push('>');
            }
            Tag::List(start) => {
                let kind = match start {
                    Some(start) => ListKind::Ordered(start),
                    None => ListKind::Bullet,
                };
                self.ensure_block_spacing();
                self.list_stack.push(kind);
            }
            Tag::Item => {
                self.ensure_line_start();
                let indent_level = self.list_stack.len().saturating_sub(1);
                self.output.push_str(&"  ".repeat(indent_level));
                match self.list_stack.last_mut() {
                    Some(ListKind::Bullet) => self.output.push_str("- "),
                    Some(ListKind::Ordered(next)) => {
                        let current = *next;
                        *next += 1;
                        self.output.push_str(&format!("{current}. "));
                    }
                    None => self.output.push_str("- "),
                }
                self.list_item_stack.push(indent_level);
            }
            Tag::Emphasis => self.output.push_str("<i>"),
            Tag::Strong => self.output.push_str("<b>"),
            Tag::Strikethrough => self.output.push_str("<s>"),
            Tag::Link {
                link_type,
                dest_url,
                ..
            } => {
                if matches!(
                    link_type,
                    LinkType::Inline | LinkType::Autolink | LinkType::Email
                ) {
                    self.output.push_str("<a href=\"");
                    self.output
                        .push_str(&escape_html_attribute(dest_url.as_ref()));
                    self.output.push_str("\">");
                    self.pending_link = Some(dest_url.to_string());
                }
            }
            Tag::Image {
                dest_url, title, ..
            } => {
                let mut label = String::from("image");
                if !title.is_empty() {
                    label.push_str(": ");
                    label.push_str(title.as_ref());
                }
                self.output.push_str("<a href=\"");
                self.output
                    .push_str(&escape_html_attribute(dest_url.as_ref()));
                self.output.push_str("\">");
                self.output.push_str(&escape_html(&label));
                self.output.push_str("</a>");
            }
            _ => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.block_stack.pop();
                if !self.in_blockquote() {
                    self.output.push('\n');
                }
            }
            TagEnd::Heading(_) => {
                self.block_stack.pop();
                self.output.push_str("</b>\n");
            }
            TagEnd::BlockQuote(_) => {
                self.block_stack.pop();
                self.output.push('\n');
            }
            TagEnd::CodeBlock => {
                self.block_stack.pop();
                self.output.push_str("</code></pre>\n");
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                self.output.push('\n');
            }
            TagEnd::Item => {
                self.list_item_stack.pop();
                self.output.push('\n');
            }
            TagEnd::Emphasis => self.output.push_str("</i>"),
            TagEnd::Strong => self.output.push_str("</b>"),
            TagEnd::Strikethrough => self.output.push_str("</s>"),
            TagEnd::Link => {
                self.pending_link = None;
                self.output.push_str("</a>");
            }
            _ => {}
        }
    }

    fn push_text(&mut self, text: &str) {
        if matches!(self.block_stack.last(), Some(BlockKind::BlockQuote)) {
            let mut first = true;
            for line in text.lines() {
                if !first {
                    self.output.push('\n');
                    self.output.push_str("&gt; ");
                }
                self.output.push_str(&escape_html(line));
                first = false;
            }
            if text.ends_with('\n') {
                self.output.push('\n');
                self.output.push_str("&gt; ");
            }
            return;
        }
        self.output.push_str(&escape_html(text));
    }

    fn ensure_block_spacing(&mut self) {
        if !self.output.is_empty() && !self.output.ends_with("\n\n") {
            if !self.output.ends_with('\n') {
                self.output.push('\n');
            }
            self.output.push('\n');
        }
    }

    fn ensure_line_start(&mut self) {
        if !self.output.is_empty() && !self.output.ends_with('\n') {
            self.output.push('\n');
        }
    }

    fn in_blockquote(&self) -> bool {
        self.block_stack
            .iter()
            .any(|kind| matches!(kind, BlockKind::BlockQuote))
    }

    fn finish(mut self) -> String {
        while self.output.ends_with('\n') {
            self.output.pop();
        }
        self.output
    }
}

fn telegram_format_text(markdown: &str) -> TelegramFormattedText {
    TelegramHtmlRenderer::render(markdown)
}

fn escape_html(text: &str) -> String {
    text.chars()
        .map(|ch| match ch {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            _ => ch.to_string(),
        })
        .collect()
}

fn escape_html_attribute(text: &str) -> String {
    escape_html(text).replace('"', "&quot;")
}

async fn hydrate_telegram_attachments(
    http_client: &reqwest::Client,
    tg_base: &str,
    tg_file_base: &str,
    blob_base: &str,
    attachments: Vec<Value>,
) -> Vec<Value> {
    info!(
        "Hydrating {} Telegram attachment(s) using blob base [{}]",
        attachments.len(),
        blob_base
    );
    let mut hydrated = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        hydrated.push(
            hydrate_single_telegram_attachment(
                http_client,
                tg_base,
                tg_file_base,
                blob_base,
                attachment,
            )
            .await,
        );
    }
    hydrated
}

async fn hydrate_single_telegram_attachment(
    http_client: &reqwest::Client,
    tg_base: &str,
    tg_file_base: &str,
    blob_base: &str,
    attachment: Value,
) -> Value {
    let Some(file_id) = attachment
        .get("file_id")
        .and_then(Value::as_str)
        .filter(|file_id| !file_id.is_empty())
        .map(str::to_string)
    else {
        return attachment;
    };
    let kind = attachment
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("attachment");
    info!(
        "Starting Telegram attachment hydration for kind [{}] file_id [{}]",
        kind, file_id
    );

    match fetch_telegram_file_ref(http_client, tg_base, &file_id).await {
        Ok(file_ref) => {
            info!(
                "Telegram getFile resolved file_id [{}] to path [{}] size {:?}",
                file_id, file_ref.file_path, file_ref.file_size
            );
            let file_url = format!("{tg_file_base}{}", file_ref.file_path);
            match download_telegram_file(http_client, &file_url).await {
                Ok(bytes) => {
                    let file_name = attachment
                        .get("file_name")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| default_attachment_name(&attachment));
                    let mime_type = attachment
                        .get("mime_type")
                        .and_then(Value::as_str)
                        .filter(|mime| !mime.is_empty())
                        .unwrap_or("application/octet-stream");
                    info!(
                        "Downloaded Telegram attachment [{}] as [{}] mime [{}] with {} bytes; uploading to [{}]",
                        file_id,
                        file_name,
                        mime_type,
                        bytes.len(),
                        blob_base
                    );

                    match upload_blob(http_client, blob_base, &file_name, mime_type, bytes).await {
                        Ok(blob_id) => {
                            info!(
                                "Uploaded Telegram attachment [{}] to blob [{}]",
                                file_id, blob_id
                            );
                            return enrich_attachment_with_transport(
                                attachment,
                                Some(&file_ref),
                                Some(&blob_id),
                                blob_base,
                                None,
                            );
                        }
                        Err(err) => {
                            warn!(
                                "Failed to upload Telegram attachment {} to blob service: {}",
                                file_id, err
                            );
                            return enrich_attachment_with_transport(
                                attachment,
                                Some(&file_ref),
                                None,
                                blob_base,
                                Some(&format!("blob_upload_failed:{err}")),
                            );
                        }
                    }
                }
                Err(err) => {
                    warn!(
                        "Failed to download Telegram attachment {}: {}",
                        file_id, err
                    );
                    return enrich_attachment_with_transport(
                        attachment,
                        Some(&file_ref),
                        None,
                        blob_base,
                        Some(&format!("telegram_download_failed:{err}")),
                    );
                }
            }
        }
        Err(err) => {
            warn!(
                "Failed to resolve Telegram attachment {} via getFile: {}",
                file_id, err
            );
            return enrich_attachment_with_transport(
                attachment,
                None,
                None,
                blob_base,
                Some(&format!("telegram_get_file_failed:{err}")),
            );
        }
    }
}

async fn fetch_telegram_file_ref(
    http_client: &reqwest::Client,
    tg_base: &str,
    file_id: &str,
) -> Result<TelegramFileRef> {
    let response = http_client
        .get(format!("{tg_base}getFile"))
        .query(&[("file_id", file_id)])
        .send()
        .await?;
    let status = response.status();
    let payload: Value = response.json().await?;
    if !status.is_success() {
        anyhow::bail!(
            "Telegram getFile returned HTTP {} with payload {}",
            status,
            payload
        );
    }
    if payload.get("ok").and_then(Value::as_bool) != Some(true) {
        anyhow::bail!(
            "{}",
            payload
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("Telegram getFile failed")
        );
    }

    let result = payload
        .get("result")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("Telegram getFile response missing result"))?;
    let file_path = result
        .get("file_path")
        .and_then(Value::as_str)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Telegram getFile response missing file_path"))?;
    let file_size = result.get("file_size").and_then(Value::as_u64);

    Ok(TelegramFileRef {
        file_path: file_path.to_string(),
        file_size,
    })
}

async fn download_telegram_file(http_client: &reqwest::Client, file_url: &str) -> Result<Vec<u8>> {
    let response = http_client.get(file_url).send().await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!(
            "Telegram file download returned HTTP {} for {} with body {}",
            status,
            file_url,
            body
        );
    }
    let bytes = response.bytes().await?;
    Ok(bytes.to_vec())
}

/// Transcode audio bytes (e.g. ElevenLabs MP3) into OGG/OPUS so Telegram accepts
/// them as a proper voice note via `sendVoice` (the round bubble that plays
/// inline) instead of an `sendAudio` music-file card. Returns `None` if ffmpeg
/// is missing or the transcode fails, so the caller can fall back to `sendAudio`.
/// Uses temp files rather than stdio pipes to avoid a pipe-buffer deadlock on
/// multi-MB inputs.
async fn transcode_to_voice_ogg(input: &[u8]) -> Option<Vec<u8>> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir();
    let stem = format!("phil-tts-{}-{}", std::process::id(), nanos);
    let in_path = dir.join(format!("{stem}.in"));
    let out_path = dir.join(format!("{stem}.ogg"));

    if tokio::fs::write(&in_path, input).await.is_err() {
        return None;
    }
    let status = tokio::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-i"])
        .arg(&in_path)
        .args(["-ac", "1", "-c:a", "libopus", "-b:a", "48k", "-f", "ogg"])
        .arg(&out_path)
        .status()
        .await;
    let ogg = match status {
        Ok(s) if s.success() => tokio::fs::read(&out_path).await.ok(),
        Ok(s) => {
            warn!("ffmpeg voice transcode exited non-zero: {:?}", s.code());
            None
        }
        Err(e) => {
            warn!("ffmpeg unavailable for voice transcode: {}", e);
            None
        }
    };
    let _ = tokio::fs::remove_file(&in_path).await;
    let _ = tokio::fs::remove_file(&out_path).await;
    ogg.filter(|b| !b.is_empty())
}

async fn upload_blob(
    http_client: &reqwest::Client,
    blob_base: &str,
    file_name: &str,
    mime_type: &str,
    bytes: Vec<u8>,
) -> Result<String> {
    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(file_name.to_string())
        .mime_str(mime_type)?;
    let form = reqwest::multipart::Form::new().part("file", part);
    let response = http_client
        .post(format!("{blob_base}/upload"))
        .multipart(form)
        .send()
        .await?;
    let status = response.status();
    let payload: Value = response.json().await?;
    if !status.is_success() {
        anyhow::bail!(
            "Blob upload returned HTTP {} with payload {}",
            status,
            payload
        );
    }
    let blob_id = payload
        .get("blob_ids")
        .and_then(Value::as_array)
        .and_then(|blob_ids| blob_ids.first())
        .and_then(Value::as_str)
        .filter(|blob_id| !blob_id.is_empty())
        .ok_or_else(|| anyhow::anyhow!("blob upload response missing blob_id"))?;
    Ok(blob_id.to_string())
}

fn default_attachment_name(attachment: &Value) -> String {
    let kind = attachment
        .get("kind")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty())
        .unwrap_or("attachment");
    let file_id = attachment
        .get("file_id")
        .and_then(Value::as_str)
        .filter(|file_id| !file_id.is_empty())
        .unwrap_or("unknown");
    format!("{kind}-{file_id}")
}

fn enrich_attachment_with_transport(
    mut attachment: Value,
    file_ref: Option<&TelegramFileRef>,
    blob_id: Option<&str>,
    blob_base: &str,
    transport_error: Option<&str>,
) -> Value {
    if let Some(file_ref) = file_ref {
        attachment["telegram_file_path"] = Value::String(file_ref.file_path.clone());
        if let Some(file_size) = file_ref.file_size {
            attachment["file_size"] = Value::Number(serde_json::Number::from(file_size));
        }
    }

    if let Some(blob_id) = blob_id {
        attachment["blob_id"] = Value::String(blob_id.to_string());
        attachment["blob_download_url"] = Value::String(format!("{blob_base}/download/{blob_id}"));
    }

    if let Some(transport_error) = transport_error.filter(|error| !error.is_empty()) {
        attachment["transport_error"] = Value::String(transport_error.to_string());
    }

    let kind = attachment
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("attachment");
    let file_id = attachment
        .get("file_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    info!(
        "Telegram attachment transport result kind [{}] file_id [{}] path {:?} blob {:?} error {:?}",
        kind,
        file_id,
        attachment
            .get("telegram_file_path")
            .and_then(|value| value.as_str()),
        attachment.get("blob_id").and_then(|value| value.as_str()),
        attachment
            .get("transport_error")
            .and_then(|value| value.as_str())
    );

    attachment
}

/// Tracks an in-flight agent turn so membrane can maintain delivery UX (typing indicator,
/// progressive delivery) independent of when the final reply arrives.
struct ActiveTurn {
    /// Sending on this channel cancels the typing heartbeat task.
    cancel_typing: oneshot::Sender<()>,
    /// `message_id` of the first message sent for this turn, used for edit-based streaming.
    draft_message_id: Option<i64>,
    /// Ephemeral status message (e.g. "Running command...") shown during tool calls.
    /// Deleted when the final reply is delivered.
    status_message_id: Option<i64>,
    /// Telegram thread the turn belongs to, if any.
    thread_id: Option<String>,
}

impl ActiveTurn {
    fn new(cancel_typing: oneshot::Sender<()>, thread_id: Option<String>) -> Self {
        Self {
            cancel_typing,
            draft_message_id: None,
            status_message_id: None,
            thread_id,
        }
    }

    fn cancel(self) {
        let _ = self.cancel_typing.send(());
    }
}

/// Start a `sendChatAction(typing)` heartbeat that refreshes every 4 seconds until cancelled.
fn spawn_typing_heartbeat(
    http_client: reqwest::Client,
    tg_base: String,
    chat_id: String,
) -> (oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let (tx, mut rx) = oneshot::channel::<()>();
    let handle = tokio::spawn(async move {
        loop {
            let url = format!("{}sendChatAction", tg_base);
            let payload = json!({ "chat_id": chat_id, "action": "typing" });
            if let Err(e) = http_client.post(&url).json(&payload).send().await {
                warn!("sendChatAction failed: {}", e);
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(4)) => {}
                _ = &mut rx => break,
            }
        }
    });
    (tx, handle)
}

/// Strip the `@agent:<role_name>` attribution tag from the end of outbound
/// content. Returns (clean_content, Option<role_name>).
/// Format contract: tag is on its own line at the very end, e.g.
///   "Some answer.\n\n@agent:theoretician"
fn strip_attribution_tag(content: &str) -> (String, Option<String>) {
    let trimmed = content.trim_end();
    if let Some(tag_start) = trimmed.rfind("\n@agent:") {
        let role = trimmed[tag_start + "\n@agent:".len()..].trim().to_string();
        if !role.is_empty()
            && role
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        {
            let clean = trimmed[..tag_start].trim_end().to_string();
            return (clean, Some(role));
        }
    }
    (content.to_string(), None)
}

/// Build a Telegram `reply_markup` with a single inline button to switch to
/// the named specialist role.
fn role_switch_button(role_name: &str) -> Value {
    let label = format!("🎭 {}", role_name);
    let callback = format!("/role {}", role_name);
    json!({
        "inline_keyboard": [[{
            "text": label,
            "callback_data": callback
        }]]
    })
}

/// Send a plain text reply to a Telegram chat, formatted as HTML.
/// Returns the Telegram `message_id` if the send succeeded.
async fn send_telegram_text(
    http_client: &reqwest::Client,
    tg_base: &str,
    chat_id: &str,
    thread_id: Option<&str>,
    text: &str,
    reply_markup: Option<Value>,
) -> Option<i64> {
    let formatted = telegram_format_text(text);
    let send_url = format!("{tg_base}sendMessage");
    let mut payload = json!({
        "chat_id": chat_id,
        "text": formatted.text,
        "parse_mode": formatted.parse_mode,
        "disable_web_page_preview": true
    });
    if let Some(tid) = thread_id {
        payload["message_thread_id"] = Value::String(tid.to_string());
    }
    if let Some(markup) = reply_markup {
        payload["reply_markup"] = markup;
    }
    match http_client.post(&send_url).json(&payload).send().await {
        Ok(res) => {
            if let Ok(body) = res.json::<Value>().await {
                body.get("result")
                    .and_then(|r| r.get("message_id"))
                    .and_then(Value::as_i64)
            } else {
                None
            }
        }
        Err(e) => {
            error!("sendMessage failed: {}", e);
            None
        }
    }
}

async fn edit_telegram_text(
    http_client: &reqwest::Client,
    tg_base: &str,
    chat_id: &str,
    message_id: i64,
    text: &str,
) -> bool {
    let formatted = telegram_format_text(text);
    let edit_url = format!("{tg_base}editMessageText");
    let payload = json!({
        "chat_id": chat_id,
        "message_id": message_id,
        "text": formatted.text,
        "parse_mode": formatted.parse_mode,
        "disable_web_page_preview": true
    });

    match http_client.post(&edit_url).json(&payload).send().await {
        Ok(res) => res.status().is_success(),
        Err(e) => {
            error!("editMessageText failed: {}", e);
            false
        }
    }
}

async fn delete_telegram_message(
    http_client: &reqwest::Client,
    tg_base: &str,
    chat_id: &str,
    message_id: i64,
) {
    let url = format!("{tg_base}deleteMessage");
    let payload = json!({ "chat_id": chat_id, "message_id": message_id });
    if let Err(e) = http_client.post(&url).json(&payload).send().await {
        warn!("deleteMessage failed: {}", e);
    }
}

/// Splits `text` at `\n\n` paragraph boundaries so each chunk fits within `limit` bytes.
/// Falls back to `\n` line breaks, then hard-splits at `limit` if no breaks are found.
fn split_at_paragraph_boundary(text: &str, limit: usize) -> Vec<String> {
    if text.len() <= limit {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut remaining = text;
    while remaining.len() > limit {
        let split_at = if let Some(pos) = remaining[..limit].rfind("\n\n") {
            pos + 2
        } else if let Some(pos) = remaining[..limit].rfind('\n') {
            pos + 1
        } else {
            limit
        };
        chunks.push(remaining[..split_at].to_string());
        remaining = &remaining[split_at..];
    }
    if !remaining.is_empty() {
        chunks.push(remaining.to_string());
    }
    chunks
}

async fn upsert_formatted_text(
    http_client: &reqwest::Client,
    tg_base: &str,
    chat_id: &str,
    thread_id: Option<&str>,
    existing_message_id: Option<i64>,
    text: &str,
    reply_markup: Option<Value>,
) -> Option<i64> {
    let chunks = split_at_paragraph_boundary(text, 4096);
    let Some(first_chunk) = chunks.first() else {
        return existing_message_id;
    };
    let last_idx = chunks.len() - 1;

    let first_message_id = match existing_message_id {
        Some(message_id)
            if edit_telegram_text(http_client, tg_base, chat_id, message_id, first_chunk).await =>
        {
            Some(message_id)
        }
        _ => {
            let markup = if last_idx == 0 {
                reply_markup.clone()
            } else {
                None
            };
            send_telegram_text(
                http_client,
                tg_base,
                chat_id,
                thread_id,
                first_chunk,
                markup,
            )
            .await
        }
    };

    for (i, chunk) in chunks.iter().enumerate().skip(1) {
        let markup = if i == last_idx {
            reply_markup.clone()
        } else {
            None
        };
        send_telegram_text(http_client, tg_base, chat_id, thread_id, chunk, markup).await;
    }

    first_message_id
}

/// Commands handled entirely within membrane, before the envelope reaches agent-core.
/// Returns `true` if the command was handled locally (caller should skip the EmitTask).
async fn handle_membrane_command(
    http_client: &reqwest::Client,
    tg_base: &str,
    envelope: &TelegramMessageEnvelope,
    session_id_overrides: &mut HashMap<String, String>,
    agent_id: &str,
    agent_cmds: &[CommandManifestEntry],
) -> bool {
    let Some(command) = envelope.command.as_deref() else {
        return false;
    };
    match command {
        "/help" | "/commands" => {
            info!(
                "Membrane handling {} for chat [{}]",
                command, envelope.chat_id
            );
            send_telegram_text(
                http_client,
                tg_base,
                &envelope.chat_id,
                envelope.thread_id.as_deref(),
                &telegram_help_text(agent_cmds),
                None,
            )
            .await;
            true
        }
        "/ping" => {
            info!("Membrane handling /ping for chat [{}]", envelope.chat_id);
            send_telegram_text(
                http_client,
                tg_base,
                &envelope.chat_id,
                envelope.thread_id.as_deref(),
                "pong",
                None,
            )
            .await;
            true
        }
        "/new" => {
            info!("Membrane handling /new for chat [{}]", envelope.chat_id);
            let epoch_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let new_session_id = match envelope.thread_id.as_deref() {
                Some(tid) => format!(
                    "telegram:{}:{}:{}:{}",
                    envelope.chat_id, tid, epoch_ms, agent_id
                ),
                None => format!("telegram:{}:{}:{}", envelope.chat_id, epoch_ms, agent_id),
            };
            let session_key = format!(
                "{}:{}",
                envelope.chat_id,
                envelope.thread_id.as_deref().unwrap_or("")
            );
            session_id_overrides.insert(session_key, new_session_id);
            send_telegram_text(
                http_client,
                tg_base,
                &envelope.chat_id,
                envelope.thread_id.as_deref(),
                "Started a new conversation.",
                None,
            )
            .await;
            true
        }
        _ => false,
    }
}

fn telegram_inbound_envelope(
    update: &Value,
    update_id: i64,
    agent_id: &str,
) -> Option<TelegramMessageEnvelope> {
    telegram_callback_envelope(update, update_id, agent_id)
        .or_else(|| telegram_message_envelope(update, update_id, agent_id))
}

fn telegram_message_envelope(
    update: &Value,
    update_id: i64,
    agent_id: &str,
) -> Option<TelegramMessageEnvelope> {
    let message = update.get("message")?;
    let chat_id = message
        .get("chat")
        .and_then(|chat| chat.get("id"))
        .and_then(value_to_id_string)?;
    let thread_id = message
        .get("message_thread_id")
        .and_then(value_to_id_string)
        .filter(|id| !id.is_empty());
    let sender_id = message
        .get("from")
        .and_then(|from| from.get("id"))
        .and_then(value_to_id_string)
        .filter(|id| !id.is_empty());
    let sender_username = message
        .get("from")
        .and_then(|from| from.get("username"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|name| !name.is_empty());
    let chat_type = message
        .get("chat")
        .and_then(|chat| chat.get("type"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let sender_first_name = message
        .get("from")
        .and_then(|from| from.get("first_name"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|name| !name.is_empty());
    let attachments = telegram_message_attachments(message);
    let message_kind = telegram_message_kind(message);
    let explicit_text = message
        .get("text")
        .or_else(|| message.get("caption"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let content = explicit_text
        .clone()
        .unwrap_or_else(|| telegram_message_summary(message, message_kind, &attachments));

    if content.trim().is_empty() && attachments.is_empty() {
        return None;
    }

    Some(TelegramMessageEnvelope {
        session_id: telegram_session_id(&chat_id, thread_id.as_deref(), agent_id),
        turn_id: format!("telegram-update-{update_id}"),
        chat_id,
        thread_id,
        sender_id,
        sender_username,
        chat_type,
        sender_first_name,
        message_kind,
        content: content.clone(),
        attachments,
        command: explicit_text.as_deref().and_then(telegram_command),
        callback_data: None,
        raw_transport_event: update.clone(),
    })
}

fn telegram_callback_envelope(
    update: &Value,
    update_id: i64,
    agent_id: &str,
) -> Option<TelegramMessageEnvelope> {
    let callback = update.get("callback_query")?;
    let callback_data = callback
        .get("data")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|data| !data.is_empty())
        .map(str::to_string)?;
    let message = callback.get("message")?;
    let chat_id = message
        .get("chat")
        .and_then(|chat| chat.get("id"))
        .and_then(value_to_id_string)?;
    let thread_id = message
        .get("message_thread_id")
        .and_then(value_to_id_string)
        .filter(|id| !id.is_empty());
    let sender_id = callback
        .get("from")
        .and_then(|from| from.get("id"))
        .and_then(value_to_id_string)
        .filter(|id| !id.is_empty());
    let sender_username = callback
        .get("from")
        .and_then(|from| from.get("username"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|name| !name.is_empty());
    let chat_type = message
        .get("chat")
        .and_then(|chat| chat.get("type"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let sender_first_name = callback
        .get("from")
        .and_then(|from| from.get("first_name"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|name| !name.is_empty());

    Some(TelegramMessageEnvelope {
        session_id: telegram_session_id(&chat_id, thread_id.as_deref(), agent_id),
        turn_id: format!("telegram-update-{update_id}"),
        chat_id,
        thread_id,
        sender_id,
        sender_username,
        chat_type,
        sender_first_name,
        message_kind: "callback",
        content: approval_callback_content(&callback_data),
        attachments: Vec::new(),
        command: None,
        callback_data: Some(callback_data),
        raw_transport_event: update.clone(),
    })
}

/// Translate approval inline-button callbacks into the slash commands the philote's
/// approval resolver recognizes.
///
/// The Approve/Deny/Trust buttons a parked turn renders emit bare `callback_data`
/// (`"approve"` / `"deny"` / `"trust"`, occasionally turn-suffixed like `"approve:turn-1"`).
/// Without this mapping the tap arrives as literal `"Telegram callback action: …"` text,
/// which `parse_slash_command` does not recognize, so it is dispatched as an ordinary chat
/// turn to the model and the parked approval turn is never resolved — it simply times out
/// under the turn watchdog. `trust` maps to `/approve`; callers preserve the original
/// `callback_data` so the runtime pre-approves the session before resolving the turn.
/// Non-approval callbacks pass through unchanged.
fn approval_callback_content(callback_data: &str) -> String {
    if callback_data == "approve"
        || callback_data.starts_with("approve:")
        || callback_data == "trust"
        || callback_data.starts_with("trust:")
    {
        "/approve".to_string()
    } else if callback_data == "deny" || callback_data.starts_with("deny:") {
        "/deny".to_string()
    } else {
        format!("Telegram callback action: {callback_data}")
    }
}

/// Group-chat detection for the operator gate: explicit chat type wins,
/// with the negative-chat-id convention as a fallback.
fn is_group_chat(chat_type: Option<&str>, chat_id: &str) -> bool {
    matches!(chat_type, Some("group") | Some("supergroup")) || chat_id.starts_with('-')
}

/// True when the sender's username is in the operator allowlist
/// (case-insensitive; the allowlist is stored lowercased).
fn is_operator_sender(sender_username: Option<&str>, operator_usernames: &HashSet<String>) -> bool {
    sender_username
        .map(|u| operator_usernames.contains(&u.to_lowercase()))
        .unwrap_or(false)
}

/// Derive the operator allowlist config key from the bot-token config key:
/// `telegram_bot_token_{agent_key}` → `telegram_allowed_users_{agent_key}`,
/// with the un-suffixed global fallback for single-seat mode.
fn telegram_allowed_users_key(telegram_token_key: &str) -> String {
    if let Some(suffix) = telegram_token_key.strip_prefix("telegram_bot_token_") {
        format!("telegram_allowed_users_{suffix}")
    } else {
        "telegram_allowed_users".to_string()
    }
}

fn telegram_session_id(chat_id: &str, thread_id: Option<&str>, agent_id: &str) -> String {
    match thread_id {
        Some(thread_id) => format!("telegram:{chat_id}:{thread_id}:{agent_id}"),
        None => format!("telegram:{chat_id}:{agent_id}"),
    }
}

fn telegram_message_kind(message: &Value) -> &'static str {
    if message.get("text").is_some() {
        "text"
    } else if message.get("voice").is_some() {
        "voice"
    } else if message.get("audio").is_some() {
        "audio"
    } else if message.get("photo").is_some() {
        "photo"
    } else if message.get("document").is_some() {
        "document"
    } else if message.get("video").is_some() {
        "video"
    } else if message.get("video_note").is_some() {
        "video_note"
    } else if message.get("animation").is_some() {
        "animation"
    } else if message.get("sticker").is_some() {
        "sticker"
    } else if message.get("location").is_some() {
        "location"
    } else if message.get("contact").is_some() {
        "contact"
    } else {
        "message"
    }
}

fn telegram_message_attachments(message: &Value) -> Vec<Value> {
    let mut attachments = Vec::new();

    if let Some(voice) = message.get("voice") {
        attachments.push(transport_attachment(
            "voice",
            voice.get("file_id"),
            voice.get("mime_type").and_then(Value::as_str),
            None,
        ));
    }

    if let Some(audio) = message.get("audio") {
        attachments.push(transport_attachment(
            "audio",
            audio.get("file_id"),
            audio.get("mime_type").and_then(Value::as_str),
            audio.get("file_name").and_then(Value::as_str),
        ));
    }

    if let Some(document) = message.get("document") {
        attachments.push(transport_attachment(
            "document",
            document.get("file_id"),
            document.get("mime_type").and_then(Value::as_str),
            document.get("file_name").and_then(Value::as_str),
        ));
    }

    if let Some(video) = message.get("video") {
        attachments.push(transport_attachment(
            "video",
            video.get("file_id"),
            video.get("mime_type").and_then(Value::as_str),
            video.get("file_name").and_then(Value::as_str),
        ));
    }

    if let Some(video_note) = message.get("video_note") {
        attachments.push(transport_attachment(
            "video_note",
            video_note.get("file_id"),
            video_note.get("mime_type").and_then(Value::as_str),
            None,
        ));
    }

    if let Some(animation) = message.get("animation") {
        attachments.push(transport_attachment(
            "animation",
            animation.get("file_id"),
            animation.get("mime_type").and_then(Value::as_str),
            animation.get("file_name").and_then(Value::as_str),
        ));
    }

    if let Some(sticker) = message.get("sticker") {
        attachments.push(transport_attachment(
            "sticker",
            sticker.get("file_id"),
            sticker.get("emoji").and_then(Value::as_str),
            None,
        ));
    }

    if let Some(photo_sizes) = message.get("photo").and_then(Value::as_array) {
        if let Some(photo) = photo_sizes.last() {
            attachments.push(transport_attachment(
                "photo",
                photo.get("file_id"),
                None,
                None,
            ));
        }
    }

    attachments
}

fn telegram_message_summary(message: &Value, message_kind: &str, attachments: &[Value]) -> String {
    match message_kind {
        "voice" => "User sent a Telegram voice message.".to_string(),
        "audio" => {
            let file_name = attachments
                .first()
                .and_then(|attachment| attachment.get("file_name"))
                .and_then(Value::as_str);
            match file_name {
                Some(file_name) => format!("User sent a Telegram audio file: {file_name}."),
                None => "User sent a Telegram audio file.".to_string(),
            }
        }
        "document" => {
            let file_name = attachments
                .first()
                .and_then(|attachment| attachment.get("file_name"))
                .and_then(Value::as_str);
            match file_name {
                Some(file_name) => format!("User sent a Telegram document: {file_name}."),
                None => "User sent a Telegram document.".to_string(),
            }
        }
        "photo" => "User sent a Telegram photo.".to_string(),
        "video" => "User sent a Telegram video.".to_string(),
        "video_note" => "User sent a Telegram video note.".to_string(),
        "animation" => "User sent a Telegram animation.".to_string(),
        "sticker" => "User sent a Telegram sticker.".to_string(),
        "location" => {
            let latitude = message
                .get("location")
                .and_then(|location| location.get("latitude"))
                .and_then(Value::as_f64);
            let longitude = message
                .get("location")
                .and_then(|location| location.get("longitude"))
                .and_then(Value::as_f64);
            match (latitude, longitude) {
                (Some(lat), Some(lon)) => {
                    format!("User shared a Telegram location: {lat:.5}, {lon:.5}.")
                }
                _ => "User shared a Telegram location.".to_string(),
            }
        }
        "contact" => {
            let first_name = message
                .get("contact")
                .and_then(|contact| contact.get("first_name"))
                .and_then(Value::as_str);
            let phone_number = message
                .get("contact")
                .and_then(|contact| contact.get("phone_number"))
                .and_then(Value::as_str);
            match (first_name, phone_number) {
                (Some(first_name), Some(phone_number)) => {
                    format!("User shared a Telegram contact: {first_name} ({phone_number}).")
                }
                _ => "User shared a Telegram contact.".to_string(),
            }
        }
        _ if !attachments.is_empty() => format!(
            "User sent a Telegram {message_kind} message with {} attachment(s).",
            attachments.len()
        ),
        _ => "User sent a Telegram message.".to_string(),
    }
}

fn transport_attachment(
    kind: &str,
    file_id: Option<&Value>,
    mime_type: Option<&str>,
    file_name: Option<&str>,
) -> Value {
    json!({
        "kind": kind,
        "file_id": file_id.and_then(value_to_id_string).unwrap_or_default(),
        "mime_type": mime_type,
        "file_name": file_name,
    })
}

fn telegram_command(text: &str) -> Option<String> {
    text.split_whitespace()
        .next()
        .filter(|token| token.starts_with('/'))
        .map(str::to_string)
}

/// Native membrane commands — these are always present regardless of what the agent advertises.
/// Agent-side commands are fetched at startup via `fetch_agent_command_manifest` and merged in.
const TELEGRAM_MENU_COMMANDS: &[TelegramBotCommand] = &[
    TelegramBotCommand {
        command: "help",
        description: "Show available commands.",
    },
    TelegramBotCommand {
        command: "ping",
        description: "Quick health check.",
    },
    TelegramBotCommand {
        command: "new",
        description: "Start a fresh conversation.",
    },
];

const TELEGRAM_MAX_COMMANDS: usize = 100;

fn telegram_help_text(agent_cmds: &[CommandManifestEntry]) -> String {
    let mut help = String::from("Available Telegram slash commands:\n\n");
    for command in TELEGRAM_MENU_COMMANDS {
        help.push_str(&format!(
            "/{:<9} {}\n",
            command.command, command.description
        ));
    }
    for entry in agent_cmds {
        if let Some(hint) = &entry.usage_hint {
            help.push_str(&format!(
                "/{:<9} {} ({})\n",
                entry.command, entry.description, hint
            ));
        } else {
            help.push_str(&format!("/{:<9} {}\n", entry.command, entry.description));
        }
    }
    help
}

fn normalize_telegram_menu_command_name(command: &str) -> Option<String> {
    let trimmed = command.trim().trim_start_matches('/');
    if trimmed.is_empty() {
        return None;
    }

    let normalized = trimmed
        .chars()
        .filter_map(|ch| match ch {
            'a'..='z' | '0'..='9' | '_' => Some(ch),
            'A'..='Z' => Some(ch.to_ascii_lowercase()),
            '-' => Some('_'),
            _ => None,
        })
        .collect::<String>();

    (!normalized.is_empty()).then_some(normalized)
}

#[allow(dead_code)]
fn build_telegram_menu_commands(commands: &[TelegramBotCommand]) -> Vec<Value> {
    let mut normalized_commands = Vec::new();
    let mut seen = HashSet::new();

    for command in commands {
        let Some(normalized_name) = normalize_telegram_menu_command_name(command.command) else {
            warn!(
                "Skipping Telegram command {:?}: normalization produced an empty command.",
                command.command
            );
            continue;
        };

        if !seen.insert(normalized_name.clone()) {
            warn!(
                "Skipping duplicate Telegram command after normalization: /{}",
                normalized_name
            );
            continue;
        }

        normalized_commands.push(json!({
            "command": normalized_name,
            "description": command.description
        }));
    }

    if normalized_commands.len() > TELEGRAM_MAX_COMMANDS {
        let overflow = normalized_commands.len() - TELEGRAM_MAX_COMMANDS;
        warn!(
            "Telegram command menu has {} entries; truncating {} overflow command(s) to respect Telegram's {} command limit.",
            normalized_commands.len(),
            overflow,
            TELEGRAM_MAX_COMMANDS
        );
        normalized_commands.truncate(TELEGRAM_MAX_COMMANDS);
    }

    normalized_commands
}

/// Build the Telegram bot command list from native membrane commands plus agent-published entries.
fn build_combined_telegram_commands(
    native: &[TelegramBotCommand],
    agent_cmds: &[CommandManifestEntry],
) -> Vec<Value> {
    let mut normalized_commands = Vec::new();
    let mut seen = HashSet::new();

    let mut push = |name: &str, description: &str| {
        let Some(normalized_name) = normalize_telegram_menu_command_name(name) else {
            return;
        };
        if !seen.insert(normalized_name.clone()) {
            return;
        }
        normalized_commands.push(json!({
            "command": normalized_name,
            "description": description
        }));
    };

    for cmd in native {
        push(cmd.command, cmd.description);
    }
    for entry in agent_cmds {
        push(&entry.command, &entry.description);
    }

    if normalized_commands.len() > TELEGRAM_MAX_COMMANDS {
        let overflow = normalized_commands.len() - TELEGRAM_MAX_COMMANDS;
        warn!(
            "Telegram command menu has {} entries; truncating {} overflow command(s) to respect Telegram's {} command limit.",
            normalized_commands.len(),
            overflow,
            TELEGRAM_MAX_COMMANDS
        );
        normalized_commands.truncate(TELEGRAM_MAX_COMMANDS);
    }

    normalized_commands
}

/// Fetch the command manifest that agent-core published to the hotel via `SyncApartment`.
/// Returns an empty list if the agent hasn't started yet or on any error.
async fn fetch_agent_command_manifest(
    ipc_client: &mut PhiloticClient,
    agent_id: &str,
) -> Vec<CommandManifestEntry> {
    let key = format!("__apartment__:{agent_id}:command_manifest");
    match ipc_client.send_request(IpcRequest::GetConfig { key }).await {
        Ok(IpcResponse::ConfigData {
            value_json: Some(json_str),
            ..
        }) => match serde_json::from_str::<Vec<CommandManifestEntry>>(&json_str) {
            Ok(entries) => {
                info!("Fetched {} agent command manifest entries.", entries.len());
                entries
            }
            Err(e) => {
                warn!("Failed to parse agent command manifest: {}", e);
                vec![]
            }
        },
        Ok(IpcResponse::ConfigData {
            value_json: None, ..
        }) => {
            info!("Agent command manifest not yet available; using native commands only.");
            vec![]
        }
        Ok(_) => {
            warn!("Unexpected IPC response when fetching agent command manifest.");
            vec![]
        }
        Err(e) => {
            warn!("Failed to fetch agent command manifest: {}", e);
            vec![]
        }
    }
}

async fn register_telegram_commands(
    http_client: &reqwest::Client,
    tg_base: &str,
    agent_cmds: &[CommandManifestEntry],
) {
    let delete_url = format!("{tg_base}deleteMyCommands");
    match http_client.post(&delete_url).json(&json!({})).send().await {
        Ok(response) => {
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                warn!("deleteMyCommands failed with status {}: {}", status, body);
            }
        }
        Err(err) => warn!("deleteMyCommands request failed: {}", err),
    }

    let commands = build_combined_telegram_commands(TELEGRAM_MENU_COMMANDS, agent_cmds);
    if commands.is_empty() {
        warn!(
            "Skipping setMyCommands because no Telegram-safe commands remained after normalization."
        );
        return;
    }

    let url = format!("{tg_base}setMyCommands");
    let payload = json!({
        "commands": commands
    });

    match http_client.post(&url).json(&payload).send().await {
        Ok(response) => {
            if response.status().is_success() {
                info!(
                    "Registered {} Telegram bot commands for menu UI ({} native, {} agent).",
                    commands.len(),
                    TELEGRAM_MENU_COMMANDS.len(),
                    agent_cmds.len(),
                );
            } else {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                warn!("setMyCommands failed with status {}: {}", status, body);
            }
        }
        Err(err) => warn!("setMyCommands request failed: {}", err),
    }
}

fn value_to_id_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn configured_target_agent_id() -> String {
    std::env::var("PHILOTIC_TARGET_AGENT_ID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "agent-bjork-01".to_string())
}

fn configured_telegram_token_key() -> String {
    std::env::var("PHILOTIC_TELEGRAM_BOT_TOKEN_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "telegram_bot_token".to_string())
}

fn telegram_poll_lease_key(token_key: &str, bot_token: &str) -> String {
    let fingerprint = format!("{:x}", Sha256::digest(bot_token.as_bytes()));
    format!("telegram:{token_key}:{}", &fingerprint[..16])
}

/// Short-lived [`LeaseBackend`] adapter driven by the SDK [`LeaseDriver`].
///
/// Maps the Telegram poll-lease IPC responses onto the driver's typed
/// outcomes. The mapping preserves the deployed re-acquire-in-place
/// semantics: a renew denial where nobody (or our own stale binding after an
/// IPC reconnect — the hotel fences renewals by connection id) holds the
/// lease is `NeedsReacquire`, not `Lost`; only a denial naming a *different*
/// live holder is `Lost`.
struct TelegramLeaseBackend<'a> {
    client: &'a mut PhiloticClient,
    lease_key: &'a str,
    /// Agent this seat polls for (`AcquireTelegramPollLease.agent_id`).
    agent_id: &'a str,
    /// Token config key, doubling as the transport resource ref.
    resource_ref: &'a str,
    /// This seat's IPC guest_id — used to recognise our own stale lease
    /// binding in a renew denial (conn-id fencing after IPC reconnect).
    seat_guest_id: &'a str,
}

#[async_trait]
impl LeaseBackend for TelegramLeaseBackend<'_> {
    async fn acquire(&mut self) -> Result<LeaseAcquireOutcome> {
        match self
            .client
            .send_request(IpcRequest::AcquireTelegramPollLease {
                lease_key: self.lease_key.to_string(),
                agent_id: self.agent_id.to_string(),
                resource_ref: Some(self.resource_ref.to_string()),
            })
            .await?
        {
            IpcResponse::TelegramPollLease {
                granted: true,
                lease: Some(lease),
            } => Ok(LeaseAcquireOutcome::Granted {
                epoch: lease.lease_epoch,
            }),
            IpcResponse::TelegramPollLease {
                granted: false,
                lease,
            } => Ok(LeaseAcquireOutcome::Held {
                owner: lease.as_ref().map(|entry| entry.owner_guest_id.clone()),
                epoch: lease.as_ref().map(|entry| entry.lease_epoch).unwrap_or(0),
            }),
            // The hotel answered but refused (unknown agent, foreign
            // authority, transport-home mismatch, …): a deterministic denial,
            // not a transient IPC failure — report it as held-by-nobody so the
            // driver goes terminal and the seat stands down instead of
            // hammering acquire forever.
            IpcResponse::Standard {
                ok: false, message, ..
            } => {
                warn!(
                    "Telegram poll lease [{}] acquire refused by hotel: {}",
                    self.lease_key, message
                );
                Ok(LeaseAcquireOutcome::Held {
                    owner: None,
                    epoch: 0,
                })
            }
            other => anyhow::bail!(
                "unexpected Telegram poll lease response for [{}]: {:?}",
                self.lease_key,
                other
            ),
        }
    }

    async fn renew(&mut self, epoch: u64) -> Result<LeaseRenewResult> {
        match self
            .client
            .send_request(IpcRequest::RenewTelegramPollLease {
                lease_key: self.lease_key.to_string(),
                agent_id: self.agent_id.to_string(),
                resource_ref: Some(self.resource_ref.to_string()),
                lease_epoch: epoch,
            })
            .await?
        {
            IpcResponse::TelegramPollLease {
                granted: true,
                lease: Some(lease),
            } => Ok(LeaseRenewResult::Ok {
                epoch: lease.lease_epoch,
            }),
            IpcResponse::TelegramPollLease {
                granted: false,
                lease: None,
            } => Ok(LeaseRenewResult::NeedsReacquire),
            IpcResponse::TelegramPollLease {
                granted: false,
                lease: Some(lease),
            } => {
                if lease.owner_guest_id == self.seat_guest_id {
                    // Our own binding with a mismatched epoch/connection —
                    // typical after an IPC reconnect. Re-acquire in place.
                    Ok(LeaseRenewResult::NeedsReacquire)
                } else {
                    Ok(LeaseRenewResult::Lost {
                        owner: Some(lease.owner_guest_id),
                    })
                }
            }
            // Hotel refused the renew outright (e.g. registration race after
            // reconnect): treat like a lapse and re-acquire in place — the
            // acquire decides grant vs. deny. Matches the deployed behaviour
            // where any renew failure fell through to an immediate re-acquire.
            IpcResponse::Standard {
                ok: false, message, ..
            } => {
                warn!(
                    "Telegram poll lease [{}] renew refused by hotel: {}. Re-acquiring in place.",
                    self.lease_key, message
                );
                Ok(LeaseRenewResult::NeedsReacquire)
            }
            other => anyhow::bail!(
                "unexpected Telegram poll lease renew response for [{}]: {:?}",
                self.lease_key,
                other
            ),
        }
    }

    async fn release(&mut self) -> Result<()> {
        match self
            .client
            .send_request(IpcRequest::ReleaseTelegramPollLease {
                lease_key: self.lease_key.to_string(),
            })
            .await?
        {
            IpcResponse::Standard { ok: true, .. } => Ok(()),
            IpcResponse::Standard { message, .. } => anyhow::bail!(message),
            other => anyhow::bail!(
                "unexpected Telegram poll lease release response for [{}]: {:?}",
                self.lease_key,
                other
            ),
        }
    }
}

/// Convert a normalized Telegram envelope into the SDK [`InboundEnvelope`]
/// the runtime dispatches to the hotel.
///
/// `extra` carries the Telegram-specific payload fields verbatim so the
/// dispatched task payload keeps the exact shape philote already consumes
/// from the deployed seat loop (`source`, `transport`, `chat_id`,
/// `thread_id`, `message_kind`, `callback_data` at the top level).
/// `target_node`/`target_guest_id` reproduce the deployed local `EmitTask`
/// routing, and `final_reply_guest_id` pins replies to this seat's inbox.
fn seat_inbound_envelope(
    envelope: &TelegramMessageEnvelope,
    node_id: &str,
    target_agent_id: &str,
    seat_guest_id: &str,
) -> InboundEnvelope {
    let mut extra = serde_json::Map::new();
    extra.insert("source".into(), Value::String("telegram".into()));
    extra.insert("transport".into(), Value::String("telegram".into()));
    extra.insert("chat_id".into(), Value::String(envelope.chat_id.clone()));
    extra.insert(
        "thread_id".into(),
        envelope
            .thread_id
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    extra.insert(
        "message_kind".into(),
        Value::String(envelope.message_kind.to_string()),
    );
    extra.insert(
        "callback_data".into(),
        envelope
            .callback_data
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    extra.insert(
        "chat_type".into(),
        envelope
            .chat_type
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    extra.insert(
        "sender_first_name".into(),
        envelope
            .sender_first_name
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );

    InboundEnvelope {
        session_id: envelope.session_id.clone(),
        turn_id: envelope.turn_id.clone(),
        sender: SenderInfo {
            id: envelope.sender_id.clone(),
            display_name: envelope.sender_first_name.clone(),
            username: envelope.sender_username.clone(),
            is_operator: false,
        },
        content: envelope.content.clone(),
        attachments: envelope.attachments.clone(),
        command: envelope.command.clone(),
        reply_to: None,
        raw_transport: envelope.raw_transport_event.clone(),
        requires_approval: false,
        final_reply_to: Some(node_id.to_string()),
        final_reply_role: Some("membrane".to_string()),
        final_reply_guest_id: Some(seat_guest_id.to_string()),
        target_node: Some(node_id.to_string()),
        target_guest_id: Some(target_agent_id.to_string()),
        extra,
    }
}

/// Everything the seat's Telegram long-polling task needs. Spawned by
/// [`TelegramSeatGuest::setup`]; aborted on IPC reconnect and teardown.
struct SeatPollContext {
    seat_guest_id: String,
    target_agent_id: String,
    node_id: String,
    http_client: reqwest::Client,
    tg_base: String,
    tg_file_base: String,
    blob_base: String,
    agent_cmds: Vec<CommandManifestEntry>,
    /// Operator usernames (lowercased) allowed to issue commands and
    /// approval callbacks in group chats. Empty set disables the gate.
    operator_usernames: HashSet<String>,
    active_turns: ActiveTurns,
    inbound_tx: mpsc::Sender<InboundEnvelope>,
    /// NetworkState from the hotel: polling is suppressed while `false`.
    online_rx: watch::Receiver<bool>,
}

/// Telegram long-polling loop for one seat.
///
/// Owns the getUpdates cycle, envelope normalization, attachment hydration,
/// membrane-local command handling, and the exponential poll-error backoff
/// (1s → 600s, reset on success). Normalized messages are handed to the
/// membrane runtime through `inbound_tx`; the runtime owns the IPC dispatch.
async fn seat_poll_loop(mut ctx: SeatPollContext) {
    let mut offset: i64 = 0;
    // (chat_id:thread_id) → overridden session_id, set by /new.
    let mut session_id_overrides: HashMap<String, String> = HashMap::new();

    info!(
        "Starting Telegram long-polling loop for seat [{}]...",
        ctx.seat_guest_id
    );

    // Poll task: spawned so that concurrent work never cancels the in-flight
    // getUpdates HTTP request. A cancelled request leaves a zombie session on
    // Telegram's server and causes immediate Conflict on the next retry.
    // There is always at most ONE request in flight — one JoinHandle = one connection.
    let mut poll_handle: Option<tokio::task::JoinHandle<Result<Value, reqwest::Error>>> = None;
    // When set, do not start a new poll until this instant.
    let mut poll_resume_at: Option<tokio::time::Instant> = None;
    // Exponential back-off state for poll errors and 409 Conflicts.
    // Reset to the initial value after any successful update batch.
    let mut poll_error_backoff_secs: u64 = MEMBRANE_ERROR_BACKOFF_INITIAL_SECS;

    loop {
        let network_online = *ctx.online_rx.borrow();

        // Start a new poll if none is in flight, not in back-off, and network is reachable.
        if poll_handle.is_none() && network_online {
            let ready = poll_resume_at.is_none_or(|t| tokio::time::Instant::now() >= t);
            if ready {
                poll_resume_at = None;
                let url = format!("{}getUpdates", ctx.tg_base);
                let off = offset;
                let client = ctx.http_client.clone();
                poll_handle = Some(tokio::spawn(async move {
                    let res = client
                        .get(&url)
                        .timeout(Duration::from_secs(TELEGRAM_POLL_TIMEOUT_SECS + 5))
                        .query(&[
                            ("offset", off.to_string()),
                            ("timeout", TELEGRAM_POLL_TIMEOUT_SECS.to_string()),
                            (
                                "allowed_updates",
                                "[\"message\",\"callback_query\"]".to_string(),
                            ),
                        ])
                        .send()
                        .await?;
                    res.json::<Value>().await
                }));
            }
        }

        tokio::select! {
            // Branch 1: Wait for Telegram Updates (Long Polling).
            //
            // NOTE: tokio::select! evaluates the future *expression* before checking
            // the guard, so `poll_handle.as_mut().unwrap()` would panic when None.
            // We use a safe inline future that pends when no poll is in flight.
            poll_result = async {
                match poll_handle.as_mut() {
                    Some(h) => h.await,
                    None => std::future::pending().await,
                }
            }, if poll_handle.is_some() => {
                poll_handle = None;
                let http_result: Result<Value, reqwest::Error> = match poll_result {
                    Ok(r) => r,
                    Err(e) => { warn!("Poll task panicked: {}", e); continue; }
                };
                match http_result {
                    Ok(json) => {
                        if let Some(result) = json.get("result").and_then(|r| r.as_array()) {
                            // Successful response — reset exponential back-off.
                            poll_error_backoff_secs = MEMBRANE_ERROR_BACKOFF_INITIAL_SECS;
                            for update in result {
                                if let Some(update_id) = update.get("update_id").and_then(|id| id.as_i64()) {
                                    offset = update_id + 1; // Ack the message
                                    if !seat_process_update(&mut ctx, &mut session_id_overrides, update, update_id).await {
                                        // Runtime channel closed — seat is shutting down.
                                        return;
                                    }
                                }
                            }
                        } else if let Some(desc) = json.get("description").and_then(|d| d.as_str()) {
                            error!("Telegram API error: {}", desc);
                            // 409 Conflict: Telegram still has our previous connection open
                            // (common after sleep/wake). Back off exponentially so we don't
                            // hammer it. One JoinHandle = one connection; we just need to
                            // wait for Telegram's server to drop the old session.
                            poll_resume_at = Some(
                                tokio::time::Instant::now()
                                    + Duration::from_secs(poll_error_backoff_secs),
                            );
                            poll_error_backoff_secs = next_error_backoff_secs(poll_error_backoff_secs);
                            warn!(
                                backoff_secs = poll_error_backoff_secs,
                                "Telegram API error — will retry after back-off."
                            );
                        }
                    }
                    Err(e) => {
                        warn!(
                            backoff_secs = poll_error_backoff_secs,
                            "Telegram long-polling failed: {e}. Retrying after back-off."
                        );
                        poll_resume_at = Some(
                            tokio::time::Instant::now()
                                + Duration::from_secs(poll_error_backoff_secs),
                        );
                        poll_error_backoff_secs = next_error_backoff_secs(poll_error_backoff_secs);
                    }
                }
            }

            // Back-off timer: fires when poll_resume_at expires so we don't busy-loop.
            _ = tokio::time::sleep_until(poll_resume_at.unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(86400))), if poll_handle.is_none() && poll_resume_at.is_some() => {
                poll_resume_at = None;
            }

            // NetworkState transition from the hotel (via handle_push).
            changed = ctx.online_rx.changed() => {
                if changed.is_err() {
                    // Guest dropped the sender — seat is gone.
                    return;
                }
                let online = *ctx.online_rx.borrow();
                info!(online, "Network state changed; adjusting Telegram polling.");
                if online {
                    // Resume immediately: clear back-off and let the loop start a
                    // fresh poll on the next iteration.
                    poll_resume_at = None;
                    poll_error_backoff_secs = MEMBRANE_ERROR_BACKOFF_INITIAL_SECS;
                }
                // When offline: the in-flight poll (if any) is left to complete or
                // fail naturally. network_online=false prevents new polls from
                // starting, so the membrane goes quiet without cancelling HTTP requests.
            }
        }
    }
}

/// Process one Telegram update inside the poll loop: normalize, hydrate
/// attachments, apply /new session overrides, handle membrane-local commands,
/// then queue the envelope for IPC dispatch and start the turn-UX tracking.
///
/// Returns `false` when the runtime's inbound channel is closed (shutdown).
async fn seat_process_update(
    ctx: &mut SeatPollContext,
    session_id_overrides: &mut HashMap<String, String>,
    update: &Value,
    update_id: i64,
) -> bool {
    let Some(mut envelope) = telegram_inbound_envelope(update, update_id, &ctx.target_agent_id)
    else {
        return true;
    };

    if !envelope.attachments.is_empty() {
        envelope.attachments = hydrate_telegram_attachments(
            &ctx.http_client,
            &ctx.tg_base,
            &ctx.tg_file_base,
            &ctx.blob_base,
            envelope.attachments,
        )
        .await;
    }

    // Apply any active /new session override for this chat.
    let session_key = format!(
        "{}:{}",
        envelope.chat_id,
        envelope.thread_id.as_deref().unwrap_or("")
    );
    if let Some(sid) = session_id_overrides.get(&session_key) {
        envelope.session_id = sid.clone();
    }

    info!(
        "Received Telegram {} message from chat [{}]{}: {}",
        envelope.message_kind,
        envelope.chat_id,
        envelope
            .thread_id
            .as_deref()
            .map(|thread| format!(" thread [{}]", thread))
            .unwrap_or_default(),
        envelope.content
    );

    // Promote callback_data that looks like a slash command
    // (e.g. "/role bjork" from an inline keyboard button)
    // so it routes through handle_membrane_command and the
    // agent's command dispatch identically to a typed command.
    // Also ack the callback so Telegram removes the spinner.
    if envelope.command.is_none() {
        if let Some(data) = envelope.callback_data.as_deref() {
            if data.starts_with('/') {
                // Parse: first token is the command, rest are args.
                let cmd = data.split_whitespace().next().unwrap_or(data);
                envelope.command = Some(cmd.to_string());
                // Re-surface as a text message with the full command string.
                envelope.content = data.to_string();
                // Ack the callback query to dismiss Telegram's loading indicator.
                if let Some(callback_id) = envelope
                    .raw_transport_event
                    .get("callback_query")
                    .and_then(|q| q.get("id"))
                    .and_then(Value::as_str)
                {
                    let ack_url = format!("{}answerCallbackQuery", ctx.tg_base);
                    let _ = ctx
                        .http_client
                        .post(&ack_url)
                        .json(&json!({"callback_query_id": callback_id}))
                        .send()
                        .await;
                }
            }
        }
    }

    // Group chat operator gate: only operators may issue commands or
    // approval callbacks. Non-operators in group chats have their slash
    // commands stripped so the message is forwarded as context-only text.
    if is_group_chat(envelope.chat_type.as_deref(), &envelope.chat_id)
        && !ctx.operator_usernames.is_empty()
        && !is_operator_sender(envelope.sender_username.as_deref(), &ctx.operator_usernames)
    {
        if envelope.message_kind == "callback" {
            // Silently ignore approval callbacks from non-operators.
            info!(
                "Group chat: dropping callback from non-operator {:?}",
                envelope.sender_username
            );
            return true;
        }
        // Strip any slash command so the message reaches the agent as
        // plain context, not a command.
        if envelope.command.is_some() {
            info!(
                "Group chat: stripping command from non-operator {:?}",
                envelope.sender_username
            );
            envelope.command = None;
        }
    }

    // Elevation: handle deterministic commands in membrane
    // before they reach agent-core.
    if handle_membrane_command(
        &ctx.http_client,
        &ctx.tg_base,
        &envelope,
        session_id_overrides,
        &ctx.target_agent_id,
        &ctx.agent_cmds,
    )
    .await
    {
        return true;
    }

    let inbound = seat_inbound_envelope(
        &envelope,
        &ctx.node_id,
        &ctx.target_agent_id,
        &ctx.seat_guest_id,
    );

    // Start the turn UX (typing heartbeat) before queueing so the reply
    // handlers in handle_push always find the turn entry.
    let (cancel_tx, _handle) = spawn_typing_heartbeat(
        ctx.http_client.clone(),
        ctx.tg_base.clone(),
        envelope.chat_id.clone(),
    );
    {
        let mut turns = ctx.active_turns.lock().unwrap();
        if let Some(previous) = turns.insert(
            envelope.session_id.clone(),
            ActiveTurn::new(cancel_tx, envelope.thread_id.clone()),
        ) {
            previous.cancel();
        }
    }

    if ctx.inbound_tx.send(inbound).await.is_err() {
        warn!("Membrane runtime inbound channel closed; stopping poll loop.");
        if let Some(turn) = ctx
            .active_turns
            .lock()
            .unwrap()
            .remove(&envelope.session_id)
        {
            turn.cancel();
        }
        return false;
    }

    true
}

/// One Telegram seat: a bot token + target agent pair, driven by the SDK
/// [`MembraneRuntime`]. The runtime owns IPC registration, reconnect, the
/// renew tick, and inbound dispatch; this guest owns the Telegram protocol
/// behaviour and the lease policy (via the SDK [`LeaseDriver`]).
struct TelegramSeatGuest {
    seat_guest_id: String,
    telegram_token_key: String,
    target_agent_id: String,
    node_id: String,
    http_client: reqwest::Client,
    telegram_api_base: String,
    telegram_file_api_base: String,
    blob_base: String,
    inbound_tx: mpsc::Sender<InboundEnvelope>,
    online_tx: watch::Sender<bool>,
    online_rx: watch::Receiver<bool>,
    active_turns: ActiveTurns,
    lease_driver: LeaseDriver,
    lease_key: Option<String>,
    tg_base: Option<String>,
    poll_task: Option<JoinHandle<()>>,
    /// Permanent stand-down: invalid/missing bot token, or the lease is held
    /// by another live seat after the single re-acquire attempt. Mirrors the
    /// deployed behaviour where such a seat stopped polling for good.
    yielded: bool,
}

impl TelegramSeatGuest {
    #[allow(clippy::too_many_arguments)]
    fn new(
        seat_guest_id: String,
        telegram_token_key: String,
        target_agent_id: String,
        http_client: reqwest::Client,
        telegram_api_base: String,
        telegram_file_api_base: String,
        blob_base: String,
        inbound_tx: mpsc::Sender<InboundEnvelope>,
    ) -> Self {
        let (online_tx, online_rx) = watch::channel(true);
        Self {
            seat_guest_id,
            telegram_token_key,
            target_agent_id,
            node_id: local_node_id(),
            http_client,
            telegram_api_base,
            telegram_file_api_base,
            blob_base,
            inbound_tx,
            online_tx,
            online_rx,
            active_turns: Arc::new(StdMutex::new(HashMap::new())),
            lease_driver: LeaseDriver::new(LeaseDriverConfig::default()),
            lease_key: None,
            tg_base: None,
            poll_task: None,
            yielded: false,
        }
    }

    /// Abort the poll task (if running) and cancel all in-flight turn UX.
    /// Called on IPC reconnect (before re-setup) and on teardown.
    fn stop_poll_task(&mut self) {
        if let Some(handle) = self.poll_task.take() {
            handle.abort();
        }
        let drained: Vec<ActiveTurn> = {
            let mut turns = self.active_turns.lock().unwrap();
            turns.drain().map(|(_, turn)| turn).collect()
        };
        for turn in drained {
            turn.cancel();
        }
    }

    /// Resolve the bot token: env override first, then the hotel context graph.
    async fn fetch_bot_token(&self, client: &mut PhiloticClient) -> Result<Option<String>> {
        if let Some(env_token) = std::env::var("PHILOTIC_TELEGRAM_BOT_TOKEN")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
        {
            return Ok(Some(env_token));
        }

        info!("Requesting Telegram Configuration from Ansible Context Graph...");
        let config_req = IpcRequest::GetConfig {
            key: self.telegram_token_key.clone(),
        };
        let token = match client.send_request(config_req).await? {
            IpcResponse::ConfigData { key: _, value_json } => match value_json {
                Some(json_str) => {
                    if let Ok(val) = serde_json::from_str::<Value>(&json_str) {
                        val.as_str().unwrap_or("").to_string()
                    } else {
                        json_str
                    }
                }
                None => {
                    warn!(
                        "Telegram Bot Token key [{}] found, but value was empty in Context Graph.",
                        self.telegram_token_key
                    );
                    String::new()
                }
            },
            _ => {
                warn!(
                    "Failed to retrieve Telegram Bot Token from Context Graph key [{}].",
                    self.telegram_token_key
                );
                String::new()
            }
        };

        Ok((!token.is_empty()).then_some(token))
    }
}

#[async_trait]
impl MembraneGuest for TelegramSeatGuest {
    fn role(&self) -> &'static str {
        "membrane"
    }

    fn lease_key(&self) -> String {
        self.lease_key.clone().unwrap_or_default()
    }

    async fn setup(&mut self, client: &mut PhiloticClient) -> Result<()> {
        if self.yielded {
            info!(
                "Seat [{}] has stood down; skipping setup.",
                self.seat_guest_id
            );
            return Ok(());
        }

        // IPC reconnect path: stop the previous poll task and clear turn UX
        // before rebuilding — exactly one getUpdates loop per seat, always.
        self.stop_poll_task();

        let Some(bot_token) = self.fetch_bot_token(client).await? else {
            warn!(
                "No valid Telegram Bot Token for seat [{}]. Membrane will stop instead of polling without authority.",
                self.seat_guest_id
            );
            self.yielded = true;
            return Ok(());
        };

        // Load the operator allowlist for group-chat command gating.
        let allowed_users_key = telegram_allowed_users_key(&self.telegram_token_key);
        let operator_usernames: HashSet<String> = match client
            .send_request(IpcRequest::GetConfig {
                key: allowed_users_key.clone(),
            })
            .await
            .ok()
        {
            Some(IpcResponse::ConfigData {
                value_json: Some(json_str),
                ..
            }) => serde_json::from_str::<Vec<String>>(&json_str)
                .unwrap_or_default()
                .into_iter()
                .map(|s| s.to_lowercase())
                .collect(),
            _ => HashSet::new(),
        };
        if !operator_usernames.is_empty() {
            info!(
                "Loaded {} operator username(s) for group chat gating from key [{}]",
                operator_usernames.len(),
                allowed_users_key
            );
        }

        let lease_key = telegram_poll_lease_key(&self.telegram_token_key, &bot_token);
        self.lease_key = Some(lease_key.clone());

        // A fresh IPC connection means the hotel is reachable: clear any
        // stale offline latch so the acquire below can proceed. The next
        // NetworkState push re-syncs the real connectivity state.
        self.lease_driver.set_online(true);
        if self.lease_driver.is_lost() {
            // Seat-restart semantics: after losing the lease we get exactly
            // one fresh acquire attempt (below). If another live holder still
            // owns it the seat stands down for good.
            self.lease_driver = LeaseDriver::new(LeaseDriverConfig::default());
        }

        // Drive the lease driver until the initial acquire settles.
        loop {
            let mut backend = TelegramLeaseBackend {
                client,
                lease_key: &lease_key,
                agent_id: &self.target_agent_id,
                resource_ref: &self.telegram_token_key,
                seat_guest_id: &self.seat_guest_id,
            };
            match self.lease_driver.tick(&mut backend).await {
                LeaseEvent::Acquired { epoch }
                | LeaseEvent::Reacquired { epoch }
                | LeaseEvent::Renewed { epoch } => {
                    info!(
                        "Acquired Telegram poll lease [{}] at epoch {}; membrane may poll.",
                        lease_key, epoch
                    );
                    break;
                }
                LeaseEvent::Lost { owner } => {
                    warn!(
                        "Telegram poll lease [{}] is held by {:?}. Seat [{}] will stop instead of polling without authority.",
                        lease_key, owner, self.seat_guest_id
                    );
                    self.yielded = true;
                    return Ok(());
                }
                LeaseEvent::BackingOff { retry_in, error } => {
                    // Transient IPC failure: hand control back to the runtime,
                    // which redials IPC and calls setup again. The driver keeps
                    // its backoff state across attempts.
                    anyhow::bail!(
                        "Telegram poll lease [{}] acquire failed ({}); retrying in {:?}",
                        lease_key,
                        error,
                        retry_in
                    );
                }
                LeaseEvent::Idle => {
                    // Not due yet (backoff from a previous attempt). Wait for
                    // the driver's own schedule.
                    match self.lease_driver.next_deadline() {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => anyhow::bail!(
                            "Telegram poll lease [{}] driver idle with no deadline",
                            lease_key
                        ),
                    }
                }
                LeaseEvent::Offline => {
                    anyhow::bail!(
                        "network reported offline while acquiring Telegram poll lease [{}]",
                        lease_key
                    );
                }
            }
        }

        let tg_base = format!("{}/bot{}/", self.telegram_api_base, bot_token);
        let tg_file_base = format!("{}/file/bot{}/", self.telegram_file_api_base, bot_token);
        self.tg_base = Some(tg_base.clone());

        let agent_cmds = fetch_agent_command_manifest(client, &self.target_agent_id).await;
        register_telegram_commands(&self.http_client, &tg_base, &agent_cmds).await;

        let ctx = SeatPollContext {
            seat_guest_id: self.seat_guest_id.clone(),
            target_agent_id: self.target_agent_id.clone(),
            node_id: self.node_id.clone(),
            http_client: self.http_client.clone(),
            tg_base,
            tg_file_base,
            blob_base: self.blob_base.clone(),
            agent_cmds,
            operator_usernames,
            active_turns: self.active_turns.clone(),
            inbound_tx: self.inbound_tx.clone(),
            online_rx: self.online_rx.clone(),
        };
        self.poll_task = Some(tokio::spawn(seat_poll_loop(ctx)));

        Ok(())
    }

    async fn renew(&mut self, client: &mut PhiloticClient) -> Result<LeaseRenewResult> {
        if self.yielded {
            return Ok(LeaseRenewResult::Ok { epoch: 0 });
        }
        let Some(lease_key) = self.lease_key.clone() else {
            return Ok(LeaseRenewResult::Ok { epoch: 0 });
        };

        let mut backend = TelegramLeaseBackend {
            client,
            lease_key: &lease_key,
            agent_id: &self.target_agent_id,
            resource_ref: &self.telegram_token_key,
            seat_guest_id: &self.seat_guest_id,
        };
        match self.lease_driver.tick(&mut backend).await {
            LeaseEvent::Renewed { epoch } | LeaseEvent::Acquired { epoch } => {
                Ok(LeaseRenewResult::Ok { epoch })
            }
            LeaseEvent::Reacquired { epoch } => {
                info!(
                    "Re-acquired Telegram poll lease [{}] at epoch {} after renew lapse.",
                    lease_key, epoch
                );
                Ok(LeaseRenewResult::Ok { epoch })
            }
            LeaseEvent::Idle | LeaseEvent::Offline => Ok(LeaseRenewResult::Ok {
                epoch: self.lease_driver.epoch().unwrap_or(0),
            }),
            LeaseEvent::BackingOff { retry_in, error } => {
                // Transient IPC failure: the driver retries on its own
                // schedule at the next runtime tick. Never fatal by itself —
                // a genuinely dead IPC connection surfaces through the
                // runtime's recv loop and triggers a reconnect there.
                warn!(
                    "Telegram poll lease [{}] renew/acquire error ({}); retrying in {:?}.",
                    lease_key, error, retry_in
                );
                Ok(LeaseRenewResult::Ok {
                    epoch: self.lease_driver.epoch().unwrap_or(0),
                })
            }
            LeaseEvent::Lost { owner } => {
                // Another live seat owns the lease. Stop polling and hand the
                // runtime a Lost so it re-runs setup, which performs the
                // single seat-restart re-acquire attempt (then stands down).
                warn!(
                    "Telegram poll lease [{}] lost to {:?}. Seat will restart to attempt one re-acquire.",
                    lease_key, owner
                );
                self.stop_poll_task();
                Ok(LeaseRenewResult::Lost { owner })
            }
        }
    }

    async fn teardown(&mut self, client: &mut PhiloticClient) {
        self.stop_poll_task();
        if let Some(lease_key) = self.lease_key.clone() {
            info!(
                "Releasing Telegram poll lease [{}] before shutdown.",
                lease_key
            );
            let mut backend = TelegramLeaseBackend {
                client,
                lease_key: &lease_key,
                agent_id: &self.target_agent_id,
                resource_ref: &self.telegram_token_key,
                seat_guest_id: &self.seat_guest_id,
            };
            if let Err(err) = self.lease_driver.release(&mut backend).await {
                warn!(
                    "Failed to release Telegram poll lease [{}] during shutdown: {}",
                    lease_key, err
                );
            }
        }
    }

    async fn deliver(&mut self, reply: OutboundReply) -> Result<()> {
        // All hotel pushes are consumed by handle_push (the Telegram reply
        // payload carries fields like chat_id / audio_artifact / reply_markup
        // that the generic OutboundReply does not model).
        debug!(
            session_id = reply.session_id(),
            "deliver() reached — push should have been handled by handle_push"
        );
        Ok(())
    }

    async fn handle_push(&mut self, msg: &IpcResponse) -> Result<bool> {
        match msg {
            IpcResponse::InboundTask {
                source_node,
                task_id,
                task_json,
            } => {
                info!(
                    "Membrane received IPC task [{}] from [{}]",
                    task_id, source_node
                );
                self.handle_inbound_task(task_json).await;
                Ok(true)
            }
            IpcResponse::NetworkState { online } => {
                let online = *online;
                if *self.online_rx.borrow() != online {
                    info!(online, "Network state changed; adjusting Telegram polling.");
                    self.online_tx.send_replace(online);
                    // Feed the lease driver: renewals are suppressed while
                    // offline; on reconnect it renews or re-acquires
                    // immediately (TTL-drift tolerant).
                    self.lease_driver.set_online(online);
                }
                Ok(true)
            }
            other => {
                info!("Membrane received non-task IPC message: {:?}", other);
                Ok(true)
            }
        }
    }
}

impl TelegramSeatGuest {
    /// Handle a hotel push task for this seat: turn lifecycle events,
    /// progressive drafts, ephemeral status, and final reply delivery.
    async fn handle_inbound_task(&mut self, task_json: &str) {
        let Ok(task) = serde_json::from_str::<Value>(task_json) else {
            return;
        };
        let Some(tg_base) = self.tg_base.clone() else {
            warn!("Received a reply task before Telegram setup completed; dropping.");
            return;
        };

        let action = task
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("send_reply");
        let session_id = task
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let chat_id = task
            .get("chat_id")
            .and_then(|id| id.as_str())
            .unwrap_or_default()
            .to_string();

        if action == "turn_event" {
            // Turn lifecycle signal from agent-core: update delivery UX without
            // delivering a final reply.
            let event = task
                .get("event")
                .and_then(Value::as_str)
                .unwrap_or_default();
            info!("Turn event [{}] for session [{}]", event, session_id);
            // waiting_approval stops the typing; the approval reply arrives as
            // a separate send_reply which will also cancel the turn entry.
            if event == "waiting_approval" {
                let removed = self.active_turns.lock().unwrap().remove(&session_id);
                if let Some(active) = removed {
                    if let Some(sid) = active.status_message_id {
                        let c = self.http_client.clone();
                        let b = tg_base.clone();
                        let cid = chat_id.clone();
                        tokio::spawn(async move {
                            delete_telegram_message(&c, &b, &cid, sid).await;
                        });
                    }
                    active.cancel();
                }
            }
            // waiting_tool and waiting_model: typing continues — no action needed.
        } else if action == "partial_reply" {
            let content = task
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            info!(
                "Partial reply observed for session [{}] ({} chars); typing continues until final reply.",
                session_id,
                content.len()
            );
            if !chat_id.is_empty() && !content.is_empty() {
                let (draft_message_id, thread_id) = {
                    let turns = self.active_turns.lock().unwrap();
                    turns
                        .get(&session_id)
                        .map(|active| (active.draft_message_id, active.thread_id.clone()))
                        .unwrap_or((None, None))
                };
                if let Some(message_id) = upsert_formatted_text(
                    &self.http_client,
                    &tg_base,
                    &chat_id,
                    thread_id.as_deref(),
                    draft_message_id,
                    &content,
                    None, // no button on partial/draft messages
                )
                .await
                {
                    if let Some(active) = self.active_turns.lock().unwrap().get_mut(&session_id) {
                        active.draft_message_id = Some(message_id);
                    }
                }
            }
        } else if action == "turn_status" {
            // Ephemeral status message: shows what the agent is doing right
            // now (e.g. "Searching the web..."). Created on first call,
            // edited on subsequent calls, deleted on final reply.
            let status = task
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if !chat_id.is_empty() && !status.is_empty() {
                let (existing_id, thread_id) = {
                    let turns = self.active_turns.lock().unwrap();
                    turns
                        .get(&session_id)
                        .map(|a| (a.status_message_id, a.thread_id.clone()))
                        .unwrap_or((None, None))
                };
                let formatted = format!("_{}_", status);
                if let Some(new_id) = upsert_formatted_text(
                    &self.http_client,
                    &tg_base,
                    &chat_id,
                    thread_id.as_deref(),
                    existing_id,
                    &formatted,
                    None,
                )
                .await
                {
                    if let Some(active) = self.active_turns.lock().unwrap().get_mut(&session_id) {
                        active.status_message_id = Some(new_id);
                    }
                }
            }
        } else {
            // send_reply (or any unrecognised action): deliver to Telegram and
            // cancel the typing heartbeat for this session.
            let (draft_message_id, status_message_id, active_thread_id) = {
                let removed = self.active_turns.lock().unwrap().remove(&session_id);
                if let Some(active) = removed {
                    let draft_message_id = active.draft_message_id;
                    let status_message_id = active.status_message_id;
                    let active_thread_id = active.thread_id.clone();
                    active.cancel();
                    (draft_message_id, status_message_id, active_thread_id)
                } else {
                    (None, None, None)
                }
            };

            let raw_content = task
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or_default()
                .to_string();
            // Interceptor: strip @agent:<role> attribution tag and
            // build an inline button so the user can switch to that role.
            // reply_markup from the agent (e.g. roles keyboard) takes
            // priority over the per-reply role_switch_button.
            let (content, role_button) = {
                let explicit_markup = task.get("reply_markup").cloned();
                let (clean, role) = strip_attribution_tag(&raw_content);
                let markup = explicit_markup.or_else(|| role.as_deref().map(role_switch_button));
                (clean, markup)
            };
            let thread_id = task
                .get("thread_id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .or(active_thread_id);
            let audio_artifact_json = task
                .get("audio_artifact")
                .and_then(|a| a.as_str())
                .map(str::to_string);
            let send_text_caption = task
                .get("send_text_caption")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            if !chat_id.is_empty() {
                let http_client_clone = self.http_client.clone();
                let tg_base_clone = tg_base.clone();
                let thread_id_clone = thread_id.clone();

                tokio::spawn(async move {
                    // Delete ephemeral status message (e.g. "Searching...") before reply.
                    if let Some(sid) = status_message_id {
                        delete_telegram_message(&http_client_clone, &tg_base_clone, &chat_id, sid)
                            .await;
                    }

                    // Voice path: send audio first, then optional text caption.
                    if let Some(artifact_json) = audio_artifact_json {
                        if let Ok(artifact) = serde_json::from_str::<Value>(&artifact_json) {
                            let mime_type = artifact
                                .get("mime_type")
                                .and_then(Value::as_str)
                                .unwrap_or("audio/mpeg");
                            let audio_b64 = artifact
                                .get("audio_base64")
                                .and_then(Value::as_str)
                                .unwrap_or_default();

                            use base64::Engine;
                            match base64::engine::general_purpose::STANDARD.decode(audio_b64) {
                                Ok(audio_bytes) => {
                                    // Deliver as a real Telegram voice note (sendVoice: round bubble,
                                    // plays inline) whenever possible. That needs OGG/OPUS; ElevenLabs
                                    // returns MP3, so transcode when needed and only fall back to
                                    // sendAudio (a music-file card) if transcoding is unavailable.
                                    let (send_bytes, send_mime, endpoint, field_name, file_name) =
                                        if mime_type.contains("ogg") {
                                            (
                                                audio_bytes,
                                                "audio/ogg".to_string(),
                                                "sendVoice",
                                                "voice",
                                                "voice.ogg",
                                            )
                                        } else {
                                            match transcode_to_voice_ogg(&audio_bytes).await {
                                                Some(ogg) => (
                                                    ogg,
                                                    "audio/ogg".to_string(),
                                                    "sendVoice",
                                                    "voice",
                                                    "voice.ogg",
                                                ),
                                                None => {
                                                    warn!(
                                                        "Voice-note transcode unavailable; falling back to sendAudio (music-file card)."
                                                    );
                                                    (
                                                        audio_bytes,
                                                        mime_type.to_string(),
                                                        "sendAudio",
                                                        "audio",
                                                        "audio.mp3",
                                                    )
                                                }
                                            }
                                        };
                                    let send_url = format!("{}{}", tg_base_clone, endpoint);
                                    let part = reqwest::multipart::Part::bytes(send_bytes)
                                        .file_name(file_name)
                                        .mime_str(&send_mime)
                                        .unwrap_or_else(|_| {
                                            reqwest::multipart::Part::bytes(Vec::new())
                                        });
                                    let form = reqwest::multipart::Form::new()
                                        .text("chat_id", chat_id.clone())
                                        .part(field_name, part);
                                    info!(
                                        "Sending voice/audio via {} to Telegram Chat [{}]...",
                                        endpoint, chat_id
                                    );
                                    match http_client_clone
                                        .post(&send_url)
                                        .multipart(form)
                                        .send()
                                        .await
                                    {
                                        Ok(_) => info!("Telegram audio sent successfully."),
                                        Err(e) => error!("Failed to send Telegram audio: {}", e),
                                    }
                                }
                                Err(e) => error!("Failed to decode audio_base64: {}", e),
                            }
                        } else {
                            error!("Failed to parse audio_artifact JSON; skipping audio delivery.");
                        }

                        // Also send text as a follow-up caption if requested.
                        if send_text_caption && !content.is_empty() {
                            let _ = upsert_formatted_text(
                                &http_client_clone,
                                &tg_base_clone,
                                &chat_id,
                                thread_id_clone.as_deref(),
                                draft_message_id,
                                &content,
                                role_button.clone(),
                            )
                            .await;
                        }
                    } else if !content.is_empty() {
                        // Text-only path.
                        info!(
                            "Sending final response back to Telegram Chat [{}]...",
                            chat_id
                        );
                        let _ = upsert_formatted_text(
                            &http_client_clone,
                            &tg_base_clone,
                            &chat_id,
                            thread_id_clone.as_deref(),
                            draft_message_id,
                            &content,
                            role_button.clone(),
                        )
                        .await;
                    }
                });
            } else {
                warn!("Received a reply task but 'chat_id' was missing. Cannot route to Telegram.");
            }
        }
    }
}

pub async fn run() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    info!("Starting Materialized Membrane (Telegram Gateway) Guest Process...");

    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;

    let telegram_api_base = std::env::var("PHILOTIC_TELEGRAM_API_BASE_URL")
        .unwrap_or_else(|_| "https://api.telegram.org".to_string())
        .trim_end_matches('/')
        .to_string();
    let telegram_file_api_base = std::env::var("PHILOTIC_TELEGRAM_FILE_API_BASE_URL")
        .unwrap_or_else(|_| telegram_api_base.clone())
        .trim_end_matches('/')
        .to_string();
    let blob_base = std::env::var("PHILOTIC_BLOB_BASE_URL")
        .unwrap_or_else(|_| format!("http://127.0.0.1:{}", args.ansible_port + 1))
        .trim_end_matches('/')
        .to_string();

    // ── Multi-seat mode ────────────────────────────────────────────────────────
    // PHILOTIC_AGENT_ROSTER is set by aiua when running a multi-agent hotel.
    // It's a JSON array of {agent_key, agent_id} objects — one entry per agent.
    // One seat task is spawned per entry, each with its own IPC connection.
    if let Ok(roster_json) = std::env::var("PHILOTIC_AGENT_ROSTER") {
        if !roster_json.trim().is_empty() {
            let roster: Vec<Value> = serde_json::from_str(&roster_json).unwrap_or_default();
            let hotel_guest_id = local_guest_id(); // e.g. "default:membrane-gateway"

            let mut tasks = Vec::new();
            for entry in &roster {
                let agent_key = entry.get("agent_key").and_then(Value::as_str).unwrap_or("");
                let agent_id = entry.get("agent_id").and_then(Value::as_str).unwrap_or("");
                if agent_key.is_empty() || agent_id.is_empty() {
                    warn!(
                        "Skipping roster entry with missing agent_key or agent_id: {}",
                        entry
                    );
                    continue;
                }
                // Each seat registers under the per-agent guest_id so that philote reply
                // routing (final_reply_guest_id) lands in the correct seat's inbox.
                let seat_guest_id = format!("{}-{}", hotel_guest_id, agent_key);
                let token_key = format!("telegram_bot_token_{}", agent_key);

                info!(
                    "Spawning seat for agent [{}] (guest_id: {})",
                    agent_id, seat_guest_id
                );
                let (inbound_tx, inbound_rx) = mpsc::channel::<InboundEnvelope>(64);
                let guest = TelegramSeatGuest::new(
                    seat_guest_id.clone(),
                    token_key,
                    agent_id.to_string(),
                    http_client.clone(),
                    telegram_api_base.clone(),
                    telegram_file_api_base.clone(),
                    blob_base.clone(),
                    inbound_tx,
                );
                let runtime =
                    MembraneRuntime::new(hotel_socket_path(), &seat_guest_id, local_node_id())
                        .with_inbound_rx(inbound_rx);

                tasks.push(tokio::spawn(async move {
                    if let Err(e) = runtime.run(guest).await {
                        error!("Seat [{}] exited with error: {}", seat_guest_id, e);
                    }
                }));
            }

            if tasks.is_empty() {
                warn!("PHILOTIC_AGENT_ROSTER contained no valid seats. Membrane exiting.");
                return Ok(());
            }

            // All seats run indefinitely. Wait for all to exit (IPC disconnect or ctrl-c).
            for task in tasks {
                let _ = task.await;
            }
            return Ok(());
        }
    }

    // ── Single-seat legacy mode (backward compat / single-agent hotels) ────────
    let guest_id = local_guest_id();
    let target_agent_id = configured_target_agent_id();
    let telegram_token_key = configured_telegram_token_key();

    let (inbound_tx, inbound_rx) = mpsc::channel::<InboundEnvelope>(64);
    let guest = TelegramSeatGuest::new(
        guest_id.clone(),
        telegram_token_key,
        target_agent_id,
        http_client,
        telegram_api_base,
        telegram_file_api_base,
        blob_base,
        inbound_tx,
    );
    MembraneRuntime::new(hotel_socket_path(), guest_id, local_node_id())
        .with_inbound_rx(inbound_rx)
        .run(guest)
        .await
}

#[cfg(test)]
mod tests {
    use super::{
        TELEGRAM_MAX_COMMANDS, TELEGRAM_MENU_COMMANDS, TelegramBotCommand, TelegramFileRef,
        approval_callback_content, build_combined_telegram_commands, build_telegram_menu_commands,
        default_attachment_name, enrich_attachment_with_transport, next_error_backoff_secs,
        normalize_telegram_menu_command_name, telegram_command, telegram_format_text,
        telegram_help_text, telegram_inbound_envelope,
    };
    use philotic_client::CommandManifestEntry;
    use serde_json::json;

    #[test]
    fn telegram_text_envelope_normalizes_threaded_message() {
        let update = json!({
            "update_id": 99,
            "message": {
                "message_thread_id": 77,
                "text": "/status show me the room",
                "chat": { "id": -10012345 },
                "from": { "id": 888, "username": "jared" }
            }
        });

        let envelope = telegram_inbound_envelope(&update, 99, "agent-jane-01")
            .expect("text update should normalize");

        assert_eq!(envelope.session_id, "telegram:-10012345:77:agent-jane-01");
        assert_eq!(envelope.turn_id, "telegram-update-99");
        assert_eq!(envelope.chat_id, "-10012345");
        assert_eq!(envelope.thread_id.as_deref(), Some("77"));
        assert_eq!(envelope.sender_id.as_deref(), Some("888"));
        assert_eq!(envelope.sender_username.as_deref(), Some("jared"));
        assert_eq!(envelope.message_kind, "text");
        assert_eq!(envelope.content, "/status show me the room");
        assert!(envelope.attachments.is_empty());
        assert_eq!(envelope.command.as_deref(), Some("/status"));
        assert_eq!(envelope.callback_data, None);
        assert_eq!(envelope.raw_transport_event, update);
    }

    #[test]
    fn telegram_command_returns_only_slash_token() {
        assert_eq!(
            telegram_command("/approve use staging"),
            Some("/approve".into())
        );
        assert_eq!(telegram_command("hello there"), None);
    }

    #[test]
    fn telegram_photo_message_normalizes_caption_and_attachment() {
        let update = json!({
            "update_id": 100,
            "message": {
                "caption": "look at this",
                "chat": { "id": 12345 },
                "from": { "id": 888, "username": "jared" },
                "photo": [
                    { "file_id": "photo-small" },
                    { "file_id": "photo-large" }
                ]
            }
        });

        let envelope = telegram_inbound_envelope(&update, 100, "agent-jane-01")
            .expect("photo update should normalize");

        assert_eq!(envelope.message_kind, "photo");
        assert_eq!(envelope.content, "look at this");
        assert_eq!(envelope.attachments.len(), 1);
        assert_eq!(envelope.attachments[0]["kind"], "photo");
        assert_eq!(envelope.attachments[0]["file_id"], "photo-large");
    }

    #[test]
    fn telegram_voice_message_normalizes_without_text() {
        let update = json!({
            "update_id": 101,
            "message": {
                "chat": { "id": 12345 },
                "from": { "id": 888 },
                "voice": {
                    "file_id": "voice-1",
                    "mime_type": "audio/ogg"
                }
            }
        });

        let envelope = telegram_inbound_envelope(&update, 101, "agent-jane-01")
            .expect("voice update should normalize");

        assert_eq!(envelope.message_kind, "voice");
        assert_eq!(envelope.content, "User sent a Telegram voice message.");
        assert_eq!(envelope.attachments.len(), 1);
        assert_eq!(envelope.attachments[0]["kind"], "voice");
        assert_eq!(envelope.attachments[0]["file_id"], "voice-1");
        assert_eq!(envelope.attachments[0]["mime_type"], "audio/ogg");
    }

    #[test]
    fn telegram_callback_query_normalizes_action() {
        let update = json!({
            "update_id": 102,
            "callback_query": {
                "data": "approve:turn-1",
                "from": { "id": 888, "username": "jared" },
                "message": {
                    "chat": { "id": 12345 },
                    "message_thread_id": 9
                }
            }
        });

        let envelope = telegram_inbound_envelope(&update, 102, "agent-jane-01")
            .expect("callback query should normalize");

        assert_eq!(envelope.session_id, "telegram:12345:9:agent-jane-01");
        assert_eq!(envelope.message_kind, "callback");
        // Approval callbacks must arrive as the slash command the philote resolver parses,
        // not as opaque "Telegram callback action: …" text (which would be treated as chat).
        assert_eq!(envelope.content, "/approve");
        // The original callback_data is preserved so the runtime can distinguish trust, etc.
        assert_eq!(envelope.callback_data.as_deref(), Some("approve:turn-1"));
        assert!(envelope.attachments.is_empty());
    }

    #[test]
    fn approval_callbacks_map_to_slash_commands() {
        assert_eq!(approval_callback_content("approve"), "/approve");
        assert_eq!(approval_callback_content("approve:turn-1"), "/approve");
        assert_eq!(approval_callback_content("deny"), "/deny");
        assert_eq!(approval_callback_content("deny:turn-1"), "/deny");
        // "Trust for session" resolves the turn via /approve; callback_data (preserved by
        // the envelope) is what the runtime keys on to also pre-approve the session.
        assert_eq!(approval_callback_content("trust"), "/approve");
        // Non-approval callbacks (e.g. role switches) are untouched.
        assert_eq!(
            approval_callback_content("/role architect"),
            "Telegram callback action: /role architect"
        );
        assert_eq!(
            approval_callback_content("something-else"),
            "Telegram callback action: something-else"
        );
    }

    #[test]
    fn telegram_formatter_projects_basic_markdown_to_html() {
        let formatted = telegram_format_text(
            "# Title\n\n**bold** and *italic* with `code`.\n\n- one\n- two\n\n[link](https://example.com)",
        );

        assert_eq!(formatted.parse_mode, "HTML");
        assert_eq!(
            formatted.text,
            "<b>Title</b>\n\n<b>bold</b> and <i>italic</i> with <code>code</code>.\n\n- one\n- two\n\n<a href=\"https://example.com\">link</a>"
        );
    }

    #[test]
    fn telegram_formatter_escapes_html_and_preserves_code_blocks() {
        let formatted = telegram_format_text("```rust\nif a < b && c > d {}\n```");

        assert_eq!(
            formatted.text,
            "<pre><code class=\"language-rust\">if a &lt; b &amp;&amp; c &gt; d {}\n</code></pre>"
        );
    }

    #[test]
    fn telegram_formatter_projects_blockquotes_without_raw_html() {
        let formatted = telegram_format_text("> quoted\n> still quoted");

        assert_eq!(formatted.text, "&gt; quoted\n&gt; still quoted");
    }

    #[test]
    fn attachment_transport_enrichment_adds_blob_and_file_refs() {
        let attachment = json!({
            "kind": "voice",
            "file_id": "voice-1"
        });

        let enriched = enrich_attachment_with_transport(
            attachment,
            Some(&TelegramFileRef {
                file_path: "voice/file.ogg".into(),
                file_size: Some(3210),
            }),
            Some("sha256-blob-1"),
            "http://127.0.0.1:9001",
            None,
        );

        assert_eq!(enriched["telegram_file_path"], "voice/file.ogg");
        assert_eq!(enriched["file_size"], 3210);
        assert_eq!(enriched["blob_id"], "sha256-blob-1");
        assert_eq!(
            enriched["blob_download_url"],
            "http://127.0.0.1:9001/download/sha256-blob-1"
        );
    }

    #[test]
    fn default_attachment_name_uses_kind_and_file_id() {
        let attachment = json!({
            "kind": "photo",
            "file_id": "abc123"
        });

        assert_eq!(default_attachment_name(&attachment), "photo-abc123");
    }

    #[test]
    fn ping_envelope_has_command_set() {
        let update = json!({
            "update_id": 200,
            "message": {
                "text": "/ping",
                "chat": { "id": 12345 },
                "from": { "id": 888, "username": "jared" }
            }
        });
        let envelope = telegram_inbound_envelope(&update, 200, "agent-jane-01")
            .expect("ping update should normalize");
        assert_eq!(envelope.command.as_deref(), Some("/ping"));
        assert_eq!(envelope.message_kind, "text");
    }

    #[test]
    fn non_command_envelope_has_no_command() {
        let update = json!({
            "update_id": 201,
            "message": {
                "text": "hello there",
                "chat": { "id": 12345 },
                "from": { "id": 888, "username": "jared" }
            }
        });
        let envelope = telegram_inbound_envelope(&update, 201, "agent-jane-01")
            .expect("regular message should normalize");
        assert_eq!(envelope.command, None);
    }

    #[test]
    fn telegram_help_text_lists_registered_commands() {
        let agent_cmds = vec![
            CommandManifestEntry {
                command: "status".into(),
                description: "Show current session status.".into(),
                usage_hint: None,
            },
            CommandManifestEntry {
                command: "role".into(),
                description: "Switch to a named role.".into(),
                usage_hint: Some("/role <name>".into()),
            },
            CommandManifestEntry {
                command: "roles".into(),
                description: "List configured roles.".into(),
                usage_hint: None,
            },
            CommandManifestEntry {
                command: "back".into(),
                description: "Return to orchestrator.".into(),
                usage_hint: None,
            },
            CommandManifestEntry {
                command: "approve".into(),
                description: "Approve the pending action.".into(),
                usage_hint: None,
            },
        ];
        let help = telegram_help_text(&agent_cmds);
        assert!(help.contains("/help"));
        assert!(help.contains("/status"));
        assert!(help.contains("/role"));
        assert!(help.contains("/roles"));
        assert!(help.contains("/back"));
        assert!(help.contains("/approve"));
    }

    #[test]
    fn telegram_menu_commands_are_safe_for_bot_api() {
        assert!(!TELEGRAM_MENU_COMMANDS.is_empty());
        for command in TELEGRAM_MENU_COMMANDS {
            assert!(!command.command.starts_with('/'));
            assert!(command.command.chars().all(|ch| ch.is_ascii_lowercase()));
            assert!(!command.description.trim().is_empty());
        }
    }

    #[test]
    fn telegram_menu_command_normalization_is_telegram_safe() {
        assert_eq!(
            normalize_telegram_menu_command_name("/Foo-Bar"),
            Some("foo_bar".into())
        );
        assert_eq!(
            normalize_telegram_menu_command_name(" approval-status "),
            Some("approval_status".into())
        );
        assert_eq!(normalize_telegram_menu_command_name("///"), None);
    }

    #[test]
    fn telegram_menu_builder_dedupes_and_caps_commands() {
        let mut commands = Vec::new();
        commands.push(TelegramBotCommand {
            command: "/Foo-Bar",
            description: "first",
        });
        commands.push(TelegramBotCommand {
            command: "foo_bar",
            description: "duplicate after normalization",
        });
        for _ in 0..TELEGRAM_MAX_COMMANDS {
            commands.push(TelegramBotCommand {
                command: "ping",
                description: "duplicate ping",
            });
        }
        for index in 0..(TELEGRAM_MAX_COMMANDS + 5) {
            let command = format!("cmd_{index}");
            let leaked_command: &'static str = Box::leak(command.into_boxed_str());
            commands.push(TelegramBotCommand {
                command: leaked_command,
                description: "generated",
            });
        }

        let built = build_telegram_menu_commands(&commands);
        assert_eq!(built.len(), TELEGRAM_MAX_COMMANDS);
        assert_eq!(built[0]["command"], "foo_bar");
    }

    #[test]
    fn telegram_poll_lease_key_uses_token_fingerprint() {
        let left = super::telegram_poll_lease_key("telegram_bot_token", "abc123:secret");
        let right = super::telegram_poll_lease_key("telegram_bot_token", "abc123:secret");
        let different = super::telegram_poll_lease_key("telegram_bot_token", "different-token");

        assert_eq!(left, right);
        assert!(left.starts_with("telegram:telegram_bot_token:"));
        assert_ne!(left, different);
        assert!(!left.contains("abc123:secret"));
    }

    #[test]
    fn split_at_paragraph_boundary_short_text_passthrough() {
        let chunks = super::split_at_paragraph_boundary("hello world", 4096);
        assert_eq!(chunks, vec!["hello world"]);
    }

    #[test]
    fn split_at_paragraph_boundary_splits_long_text_at_double_newline() {
        let para_a = "a".repeat(3000);
        let para_b = "b".repeat(3000);
        let text = format!("{}\n\n{}", para_a, para_b);
        let chunks = super::split_at_paragraph_boundary(&text, 4096);
        assert_eq!(chunks.len(), 2);
        assert!(chunks[0].starts_with('a'));
        assert!(chunks[1].starts_with('b'));
    }

    #[test]
    fn split_at_paragraph_boundary_hard_splits_when_no_breaks() {
        let text = "x".repeat(5000);
        let chunks = super::split_at_paragraph_boundary(&text, 4096);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 4096);
        assert_eq!(chunks[1].len(), 904);
    }

    #[test]
    fn help_text_includes_new_command() {
        let help = super::telegram_help_text(&[]);
        assert!(help.contains("/new"), "help text should mention /new");
    }

    #[test]
    fn menu_commands_include_new() {
        let has_new = super::TELEGRAM_MENU_COMMANDS
            .iter()
            .any(|c| c.command == "new");
        assert!(has_new, "TELEGRAM_MENU_COMMANDS should include 'new'");
    }

    #[test]
    fn combined_commands_include_native_and_agent_entries() {
        let agent_cmds = vec![CommandManifestEntry {
            command: "status".into(),
            description: "Show status.".into(),
            usage_hint: None,
        }];
        let combined = build_combined_telegram_commands(TELEGRAM_MENU_COMMANDS, &agent_cmds);
        let names: Vec<&str> = combined
            .iter()
            .filter_map(|v| v["command"].as_str())
            .collect();
        assert!(names.contains(&"help"));
        assert!(names.contains(&"ping"));
        assert!(names.contains(&"new"));
        assert!(names.contains(&"status"));
    }

    #[test]
    fn envelope_captures_chat_type_and_sender_first_name() {
        let update = json!({
            "update_id": 400,
            "message": {
                "text": "hello",
                "chat": { "id": -900123, "type": "supergroup" },
                "from": { "id": 888, "username": "jared", "first_name": "Jared" }
            }
        });
        let envelope = telegram_inbound_envelope(&update, 400, "agent-jane-01")
            .expect("group message should normalize");
        assert_eq!(envelope.chat_type.as_deref(), Some("supergroup"));
        assert_eq!(envelope.sender_first_name.as_deref(), Some("Jared"));

        let callback_update = json!({
            "update_id": 401,
            "callback_query": {
                "data": "approve",
                "from": { "id": 999, "username": "mallory", "first_name": "Mallory" },
                "message": {
                    "chat": { "id": -900123, "type": "group" }
                }
            }
        });
        let cb = telegram_inbound_envelope(&callback_update, 401, "agent-jane-01")
            .expect("callback should normalize");
        assert_eq!(cb.chat_type.as_deref(), Some("group"));
        assert_eq!(cb.sender_first_name.as_deref(), Some("Mallory"));
    }

    #[test]
    fn group_chat_detection_uses_chat_type_then_negative_id() {
        assert!(super::is_group_chat(Some("group"), "12345"));
        assert!(super::is_group_chat(Some("supergroup"), "12345"));
        assert!(super::is_group_chat(None, "-10012345"));
        assert!(!super::is_group_chat(Some("private"), "12345"));
        assert!(!super::is_group_chat(None, "12345"));
    }

    #[test]
    fn operator_check_is_case_insensitive_and_requires_username() {
        let operators: std::collections::HashSet<String> =
            ["jared".to_string()].into_iter().collect();
        assert!(super::is_operator_sender(Some("jared"), &operators));
        assert!(super::is_operator_sender(Some("JARED"), &operators));
        assert!(!super::is_operator_sender(Some("mallory"), &operators));
        // No username at all can never be an operator.
        assert!(!super::is_operator_sender(None, &operators));
    }

    #[test]
    fn allowed_users_key_derivation_matches_token_key_convention() {
        assert_eq!(
            super::telegram_allowed_users_key("telegram_bot_token_jane"),
            "telegram_allowed_users_jane"
        );
        assert_eq!(
            super::telegram_allowed_users_key("telegram_bot_token"),
            "telegram_allowed_users"
        );
    }

    #[test]
    fn seat_inbound_envelope_carries_chat_type_and_first_name_extras() {
        let update = json!({
            "update_id": 402,
            "message": {
                "text": "hi",
                "chat": { "id": -777, "type": "group" },
                "from": { "id": 1, "username": "jared", "first_name": "Jared" }
            }
        });
        let envelope = telegram_inbound_envelope(&update, 402, "agent-jane-01").unwrap();
        let inbound = super::seat_inbound_envelope(&envelope, "node", "agent-jane-01", "seat");
        assert_eq!(inbound.extra["chat_type"], "group");
        assert_eq!(inbound.extra["sender_first_name"], "Jared");
        assert_eq!(inbound.sender.display_name.as_deref(), Some("Jared"));
    }

    #[test]
    fn seat_inbound_envelope_reproduces_deployed_dispatch_shape() {
        let update = json!({
            "update_id": 300,
            "message": {
                "message_thread_id": 4,
                "text": "/status please",
                "chat": { "id": 12345 },
                "from": { "id": 888, "username": "jared" }
            }
        });
        let envelope = telegram_inbound_envelope(&update, 300, "agent-jane-01")
            .expect("update should normalize");

        let inbound = super::seat_inbound_envelope(
            &envelope,
            "mbp-jane-aiua-01",
            "agent-jane-01",
            "default:membrane-gateway-jane",
        );

        // Explicit local EmitTask routing (deployed behaviour).
        assert_eq!(inbound.target_node.as_deref(), Some("mbp-jane-aiua-01"));
        assert_eq!(inbound.target_guest_id.as_deref(), Some("agent-jane-01"));
        // Reply routing pins this seat's inbox.
        assert_eq!(inbound.final_reply_to.as_deref(), Some("mbp-jane-aiua-01"));
        assert_eq!(inbound.final_reply_role.as_deref(), Some("membrane"));
        assert_eq!(
            inbound.final_reply_guest_id.as_deref(),
            Some("default:membrane-gateway-jane")
        );
        // Standard fields.
        assert_eq!(inbound.session_id, "telegram:12345:4:agent-jane-01");
        assert_eq!(inbound.turn_id, "telegram-update-300");
        assert_eq!(inbound.content, "/status please");
        assert_eq!(inbound.command.as_deref(), Some("/status"));
        assert_eq!(inbound.sender.id.as_deref(), Some("888"));
        assert_eq!(inbound.sender.username.as_deref(), Some("jared"));
        // Telegram payload extras merged at top level of the task payload.
        assert_eq!(inbound.extra["source"], "telegram");
        assert_eq!(inbound.extra["transport"], "telegram");
        assert_eq!(inbound.extra["chat_id"], "12345");
        assert_eq!(inbound.extra["thread_id"], "4");
        assert_eq!(inbound.extra["message_kind"], "text");
        assert_eq!(inbound.extra["callback_data"], serde_json::Value::Null);
    }

    #[test]
    fn error_backoff_doubles_and_caps_at_ten_minutes() {
        assert_eq!(next_error_backoff_secs(1), 2);
        assert_eq!(next_error_backoff_secs(2), 4);
        assert_eq!(next_error_backoff_secs(300), 600);
        assert_eq!(next_error_backoff_secs(600), 600);
        assert_eq!(next_error_backoff_secs(900), 600);
    }
}
