use anyhow::Result;
use clap::Parser;
use philotic_client::{GuestIdentity, IpcRequest, IpcResponse, PhiloticClient, is_ipc_disconnect};
use pulldown_cmark::{
    CodeBlockKind, Event, LinkType, Options, Parser as MarkdownParser, Tag, TagEnd,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::sync::oneshot;
use tracing::{error, info, warn};

const TELEGRAM_POLL_TIMEOUT_SECS: u64 = 10;
const TELEGRAM_POLL_LEASE_RENEW_INTERVAL_SECS: u64 = 20;

fn local_node_id() -> String {
    std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-ansible-01".to_string())
}

fn local_guest_id() -> String {
    std::env::var("PHILOTIC_GUEST_ID").unwrap_or_else(|_| "membrane-telegram-01".to_string())
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
    #[allow(dead_code)]
    draft_message_id: Option<i64>,
}

impl ActiveTurn {
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

/// Send a plain text reply to a Telegram chat, formatted as HTML.
/// Returns the Telegram `message_id` if the send succeeded.
async fn send_telegram_text(
    http_client: &reqwest::Client,
    tg_base: &str,
    chat_id: &str,
    thread_id: Option<&str>,
    text: &str,
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

/// Sends `text` to a Telegram chat, splitting into multiple messages if it exceeds the
/// 4096-character limit. Splits at paragraph boundaries when possible.
async fn send_formatted_text(
    http_client: &reqwest::Client,
    tg_base: &str,
    chat_id: &str,
    thread_id: Option<&str>,
    text: &str,
) {
    const TELEGRAM_MSG_LIMIT: usize = 4096;
    for chunk in split_at_paragraph_boundary(text, TELEGRAM_MSG_LIMIT) {
        send_telegram_text(http_client, tg_base, chat_id, thread_id, &chunk).await;
    }
}

/// Commands handled entirely within membrane, before the envelope reaches agent-core.
/// Returns `true` if the command was handled locally (caller should skip the EmitTask).
async fn handle_membrane_command(
    http_client: &reqwest::Client,
    tg_base: &str,
    envelope: &TelegramMessageEnvelope,
    session_id_overrides: &mut HashMap<String, String>,
    agent_id: &str,
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
                &telegram_help_text(),
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
                Some(tid) => format!("telegram:{}:{}:{}:{}", envelope.chat_id, tid, epoch_ms, agent_id),
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

    Some(TelegramMessageEnvelope {
        session_id: telegram_session_id(&chat_id, thread_id.as_deref(), agent_id),
        turn_id: format!("telegram-update-{update_id}"),
        chat_id,
        thread_id,
        sender_id,
        sender_username,
        message_kind: "callback",
        content: format!("Telegram callback action: {callback_data}"),
        attachments: Vec::new(),
        command: None,
        callback_data: Some(callback_data),
        raw_transport_event: update.clone(),
    })
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

const TELEGRAM_MENU_COMMANDS: &[TelegramBotCommand] = &[
    TelegramBotCommand {
        command: "help",
        description: "Show available commands.",
    },
    TelegramBotCommand {
        command: "commands",
        description: "List all supported slash commands.",
    },
    TelegramBotCommand {
        command: "ping",
        description: "Quick health check.",
    },
    TelegramBotCommand {
        command: "status",
        description: "Show current session status.",
    },
    TelegramBotCommand {
        command: "pause",
        description: "Pause the current session.",
    },
    TelegramBotCommand {
        command: "resume",
        description: "Resume the current session.",
    },
    TelegramBotCommand {
        command: "role",
        description: "Switch to a named role.",
    },
    TelegramBotCommand {
        command: "roles",
        description: "List configured roles and active role.",
    },
    TelegramBotCommand {
        command: "back",
        description: "Return to the orchestrator role.",
    },
    TelegramBotCommand {
        command: "approve",
        description: "Approve the pending action.",
    },
    TelegramBotCommand {
        command: "deny",
        description: "Deny the pending action.",
    },
    TelegramBotCommand {
        command: "tts",
        description: "Set text-to-speech mode.",
    },
    TelegramBotCommand {
        command: "new",
        description: "Start a fresh conversation.",
    },
    TelegramBotCommand {
        command: "preapprove",
        description: "Pre-approve a tool or class for this session.",
    },
    TelegramBotCommand {
        command: "approval",
        description: "Show or reset session approval policy (status / reset).",
    },
];

const TELEGRAM_MAX_COMMANDS: usize = 100;

fn telegram_help_text() -> String {
    let mut help = String::from("Available Telegram slash commands:\n\n");
    for command in TELEGRAM_MENU_COMMANDS {
        help.push_str(&format!(
            "/{:<9} {}\n",
            command.command, command.description
        ));
    }
    help.push_str("\nNotes:\n");
    help.push_str("/role expects a role name, for example /role developer.\n");
    help.push_str("/roles lists configured roles and highlights the active one.\n");
    help.push_str("/back returns the session to the orchestrator role.\n");
    help.push_str("/approve and /deny can include a note.\n");
    help.push_str("/tts accepts optional modes like on, off, or auto.\n");
    help.push_str("/new starts a fresh conversation with a new session ID.\n");
    help.push_str("/preapprove <tool|class> adds a tool name or class to the session pre-approve list.\n");
    help.push_str("/preapprove this-session pre-approves all tools for this session.\n");
    help.push_str("/approval status shows the current session approval policy.\n");
    help.push_str("/approval reset clears session-scoped pre-approvals.");
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

async fn register_telegram_commands(http_client: &reqwest::Client, tg_base: &str) {
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

    let commands = build_telegram_menu_commands(TELEGRAM_MENU_COMMANDS);
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
                    "Registered {} Telegram bot commands for menu UI.",
                    TELEGRAM_MENU_COMMANDS.len()
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
        .unwrap_or_else(|| "agent-jane-01".to_string())
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

async fn acquire_telegram_poll_lease(
    ipc_client: &mut PhiloticClient,
    lease_key: &str,
    agent_id: &str,
) -> Result<u64> {
    match ipc_client
        .send_request(IpcRequest::AcquireTelegramPollLease {
            lease_key: lease_key.to_string(),
            agent_id: agent_id.to_string(),
        })
        .await?
    {
        IpcResponse::TelegramPollLease {
            granted: true,
            lease: Some(lease),
        } => Ok(lease.lease_epoch),
        IpcResponse::TelegramPollLease {
            granted: false,
            lease,
        } => {
            let owner = lease.as_ref().map(|entry| entry.owner_guest_id.as_str());
            let epoch = lease.as_ref().map(|entry| entry.lease_epoch).unwrap_or(0);
            anyhow::bail!("lease held by {:?} at epoch {}", owner, epoch)
        }
        other => anyhow::bail!(
            "unexpected Telegram poll lease response for [{}]: {:?}",
            lease_key,
            other
        ),
    }
}

async fn renew_telegram_poll_lease(
    ipc_client: &mut PhiloticClient,
    lease_key: &str,
    agent_id: &str,
    lease_epoch: u64,
) -> Result<u64> {
    match ipc_client
        .send_request(IpcRequest::RenewTelegramPollLease {
            lease_key: lease_key.to_string(),
            agent_id: agent_id.to_string(),
            lease_epoch,
        })
        .await?
    {
        IpcResponse::TelegramPollLease {
            granted: true,
            lease: Some(lease),
        } => Ok(lease.lease_epoch),
        IpcResponse::TelegramPollLease {
            granted: false,
            lease,
        } => {
            let owner = lease.as_ref().map(|entry| entry.owner_guest_id.as_str());
            let epoch = lease.as_ref().map(|entry| entry.lease_epoch).unwrap_or(0);
            anyhow::bail!("lease lost to {:?} at epoch {}", owner, epoch)
        }
        other => anyhow::bail!(
            "unexpected Telegram poll lease renew response for [{}]: {:?}",
            lease_key,
            other
        ),
    }
}

async fn release_telegram_poll_lease(
    ipc_client: &mut PhiloticClient,
    lease_key: &str,
) -> Result<()> {
    match ipc_client
        .send_request(IpcRequest::ReleaseTelegramPollLease {
            lease_key: lease_key.to_string(),
        })
        .await?
    {
        IpcResponse::Standard { ok: true, .. } => Ok(()),
        IpcResponse::Standard { message, .. } => anyhow::bail!(message),
        other => anyhow::bail!(
            "unexpected Telegram poll lease release response for [{}]: {:?}",
            lease_key,
            other
        ),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    info!("Starting Materialized Membrane (Telegram Gateway) Guest Process...");
    let guest_id = local_guest_id();

    let identity = GuestIdentity {
        guest_id: guest_id.clone(),
        role: "membrane".into(),
        supported_tools: Vec::new(),
    };

    let mut ipc_client = PhiloticClient::connect(identity).await?;
    let target_agent_id = configured_target_agent_id();
    let telegram_token_key = configured_telegram_token_key();

    // Pull configuration from the Hotel Graph dynamically
    info!("Requesting Telegram Configuration from Ansible Context Graph...");
    let bot_token = if let Some(env_token) = std::env::var("PHILOTIC_TELEGRAM_BOT_TOKEN")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        env_token
    } else {
        let config_req = IpcRequest::GetConfig {
            key: telegram_token_key.clone(),
        };

        match ipc_client.send_request(config_req).await? {
            IpcResponse::ConfigData { key: _, value_json } => {
                if let Some(json_str) = value_json {
                    if let Ok(val) = serde_json::from_str::<Value>(&json_str) {
                        val.as_str().unwrap_or("").to_string()
                    } else {
                        json_str
                    }
                } else {
                    warn!(
                        "Telegram Bot Token key [{}] found, but value was empty in Context Graph. Using Dummy Token.",
                        telegram_token_key
                    );
                    "dummy_token".to_string()
                }
            }
            _ => {
                warn!(
                    "Failed to retrieve Telegram Bot Token from Context Graph key [{}]. Using Dummy Token.",
                    telegram_token_key
                );
                "dummy_token".to_string()
            }
        }
    };

    if bot_token.is_empty() || bot_token == "dummy_token" {
        warn!(
            "No valid Telegram Bot Token found. Membrane will stop instead of polling without authority."
        );
        return Ok(());
    }

    let poll_lease_key = telegram_poll_lease_key(&telegram_token_key, &bot_token);
    let mut poll_lease_epoch = match acquire_telegram_poll_lease(
        &mut ipc_client,
        &poll_lease_key,
        &target_agent_id,
    )
    .await
    {
        Ok(lease_epoch) => {
            info!(
                "Acquired Telegram poll lease [{}] at epoch {}; membrane may poll.",
                poll_lease_key, lease_epoch
            );
            lease_epoch
        }
        Err(err) => {
            warn!(
                "Failed to acquire Telegram poll lease [{}]: {}. Membrane will stop instead of polling without authority.",
                poll_lease_key, err
            );
            return Ok(());
        }
    };
    let mut next_lease_renewal =
        tokio::time::Instant::now() + Duration::from_secs(TELEGRAM_POLL_LEASE_RENEW_INTERVAL_SECS);

    // Boot the reqwest client for Telegram API
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
    let tg_base = format!("{telegram_api_base}/bot{}/", bot_token);
    let tg_file_base = format!("{telegram_file_api_base}/file/bot{}/", bot_token);
    let blob_base = std::env::var("PHILOTIC_BLOB_BASE_URL")
        .unwrap_or_else(|_| format!("http://127.0.0.1:{}", args.ansible_port + 1))
        .trim_end_matches('/')
        .to_string();
    register_telegram_commands(&http_client, &tg_base).await;
    let mut offset: i64 = 0;
    // session_id → ActiveTurn for in-flight agent turns.
    let mut active_turns: HashMap<String, ActiveTurn> = HashMap::new();
    // (chat_id:thread_id) → overridden session_id, set by /new.
    let mut session_id_overrides: HashMap<String, String> = HashMap::new();

    info!("Starting Telegram long-polling loop...");

    // Main Long-Polling Loop
    loop {
        let url = format!("{}getUpdates", tg_base);
        let params = [
            ("offset", offset.to_string()),
            ("timeout", TELEGRAM_POLL_TIMEOUT_SECS.to_string()),
            (
                "allowed_updates",
                "[\"message\",\"callback_query\"]".to_string(),
            ),
        ];

        tokio::select! {
            // Branch 1: Wait for Telegram Updates (Long Polling)
            http_result = http_client.get(&url).query(&params).send() => {
                match http_result {
                    Ok(res) => {
                        if let Ok(json) = res.json::<Value>().await {
                            if let Some(result) = json.get("result").and_then(|r| r.as_array()) {
                                for update in result {
                                    if let Some(update_id) = update.get("update_id").and_then(|id| id.as_i64()) {
                                        offset = update_id + 1; // Ack the message

                                        if let Some(mut envelope) = telegram_inbound_envelope(update, update_id, &target_agent_id) {
                                            if !envelope.attachments.is_empty() {
                                                envelope.attachments = hydrate_telegram_attachments(
                                                    &http_client,
                                                    &tg_base,
                                                    &tg_file_base,
                                                    &blob_base,
                                                    envelope.attachments,
                                                ).await;
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

                                            // Elevation: handle deterministic commands in membrane
                                            // before they reach agent-core.
                                            if handle_membrane_command(
                                                &http_client,
                                                &tg_base,
                                                &envelope,
                                                &mut session_id_overrides,
                                                &target_agent_id,
                                            )
                                            .await
                                            {
                                                continue;
                                            }

                                            let local_node_id = local_node_id();
                                            let task_req = IpcRequest::EmitTask {
                                                target_node: local_node_id.clone(),
                                                target_role: "agent".into(),
                                                target_guest_id: None,
                                                task_json: json!({
                                                    "source": "telegram",
                                                    "transport": "telegram",
                                                    "session_id": envelope.session_id,
                                                    "turn_id": envelope.turn_id,
                                                    "chat_id": envelope.chat_id,
                                                    "thread_id": envelope.thread_id,
                                                    "sender_id": envelope.sender_id,
                                                    "sender_username": envelope.sender_username,
                                                    "message_kind": envelope.message_kind,
                                                    "content": envelope.content,
                                                    "attachments": envelope.attachments,
                                                    "command": envelope.command,
                                                    "callback_data": envelope.callback_data,
                                                    "raw_transport_event": envelope.raw_transport_event,
                                                    "final_reply_to": local_node_id,
                                                    "final_reply_role": "membrane",
                                                    "final_reply_guest_id": guest_id.clone()
                                                }).to_string(),
                                            };

                                            match ipc_client.send_request(task_req).await {
                    Ok(_) => {
                                                    info!("Routed message to Ansible successfully.");
                                                    let (cancel_tx, _handle) = spawn_typing_heartbeat(
                                                        http_client.clone(),
                                                        tg_base.clone(),
                                                        envelope.chat_id.clone(),
                                                    );
                                                    active_turns.insert(
                                                        envelope.session_id.clone(),
                                                        ActiveTurn {
                                                            cancel_typing: cancel_tx,
                                                            draft_message_id: None,
                                                        },
                                                    );
                                                }
                                                Err(e) => error!("Failed to route message to Ansible: {}", e),
                                            }
                                        }
                                    }
                                }
                            } else if let Some(desc) = json.get("description").and_then(|d| d.as_str()) {
                                error!("Telegram API Error: {}", desc);
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Telegram Long Polling failed: {}. Retrying in 5s...", e);
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            }

            _ = tokio::time::sleep_until(next_lease_renewal) => {
                match renew_telegram_poll_lease(
                    &mut ipc_client,
                    &poll_lease_key,
                    &target_agent_id,
                    poll_lease_epoch,
                ).await {
                    Ok(lease_epoch) => {
                        poll_lease_epoch = lease_epoch;
                        next_lease_renewal = tokio::time::Instant::now()
                            + Duration::from_secs(TELEGRAM_POLL_LEASE_RENEW_INTERVAL_SECS);
                    }
                    Err(err) => {
                        warn!(
                            "Failed to renew Telegram poll lease [{}]: {}. Membrane will stop polling.",
                            poll_lease_key, err
                        );
                        if let Err(release_err) =
                            release_telegram_poll_lease(&mut ipc_client, &poll_lease_key).await
                        {
                            warn!(
                                "Failed to release Telegram poll lease [{}] during shutdown: {}",
                                poll_lease_key, release_err
                            );
                        }
                        return Ok(());
                    }
                }
            }

            _ = tokio::signal::ctrl_c() => {
                info!(
                    "Ctrl-C received; releasing Telegram poll lease [{}] before shutdown.",
                    poll_lease_key
                );
                if let Err(err) = release_telegram_poll_lease(&mut ipc_client, &poll_lease_key).await {
                    warn!(
                        "Failed to release Telegram poll lease [{}] during Ctrl-C shutdown: {}",
                        poll_lease_key, err
                    );
                }
                return Ok(());
            }

            // Branch 2: Wait for outbound IPC tasks (LLM Responses)
            ipc_result = ipc_client.recv_task() => {
                match ipc_result {
                    Ok(IpcResponse::InboundTask { source_node, task_id, task_json }) => {
                        info!("Membrane received IPC task [{}] from [{}]", task_id, source_node);

                        if let Ok(task) = serde_json::from_str::<Value>(&task_json) {
                            let action = task.get("action").and_then(Value::as_str).unwrap_or("send_reply");
                            let session_id = task.get("session_id").and_then(Value::as_str).unwrap_or_default().to_string();
                            let chat_id = task.get("chat_id").and_then(|id| id.as_str()).unwrap_or_default().to_string();

                            if action == "turn_event" {
                                // Turn lifecycle signal from agent-core: update delivery UX without
                                // delivering a final reply.
                                let event = task.get("event").and_then(Value::as_str).unwrap_or_default();
                                info!("Turn event [{}] for session [{}]", event, session_id);
                                // waiting_approval stops the typing; the approval reply arrives as
                                // a separate send_reply which will also cancel the turn entry.
                                if event == "waiting_approval" {
                                    if let Some(active) = active_turns.remove(&session_id) {
                                        active.cancel();
                                    }
                                }
                                // waiting_tool and waiting_model: typing continues — no action needed.
                            } else {
                                // send_reply (or any unrecognised action): deliver to Telegram and
                                // cancel the typing heartbeat for this session.
                                if let Some(active) = active_turns.remove(&session_id) {
                                    active.cancel();
                                }

                                let content = task.get("content").and_then(|c| c.as_str()).unwrap_or_default().to_string();
                                let thread_id = task.get("thread_id").and_then(|v| v.as_str()).map(str::to_string);
                                let audio_artifact_json = task.get("audio_artifact").and_then(|a| a.as_str()).map(str::to_string);
                                let send_text_caption = task.get("send_text_caption").and_then(|v| v.as_bool()).unwrap_or(false);

                                if !chat_id.is_empty() {
                                    let http_client_clone = http_client.clone();
                                    let tg_base_clone = tg_base.clone();
                                    let thread_id_clone = thread_id.clone();

                                    tokio::spawn(async move {
                                        // Voice path: send audio first, then optional text caption.
                                        if let Some(artifact_json) = audio_artifact_json {
                                            if let Ok(artifact) = serde_json::from_str::<Value>(&artifact_json) {
                                                let mime_type = artifact.get("mime_type").and_then(Value::as_str).unwrap_or("audio/mpeg");
                                                let audio_b64 = artifact.get("audio_base64").and_then(Value::as_str).unwrap_or_default();

                                                use base64::Engine;
                                                match base64::engine::general_purpose::STANDARD.decode(audio_b64) {
                                                    Ok(audio_bytes) => {
                                                        // Use sendVoice for OGG (voice notes), sendAudio for everything else.
                                                        let (endpoint, field_name, file_name) = if mime_type.contains("ogg") {
                                                            ("sendVoice", "voice", "voice.ogg")
                                                        } else {
                                                            ("sendAudio", "audio", "audio.mp3")
                                                        };
                                                        let send_url = format!("{}{}", tg_base_clone, endpoint);
                                                        let part = reqwest::multipart::Part::bytes(audio_bytes)
                                                            .file_name(file_name)
                                                            .mime_str(mime_type)
                                                            .unwrap_or_else(|_| reqwest::multipart::Part::bytes(Vec::new()));
                                                        let form = reqwest::multipart::Form::new()
                                                            .text("chat_id", chat_id.clone())
                                                            .part(field_name, part);
                                                        info!("Sending voice/audio via {} to Telegram Chat [{}]...", endpoint, chat_id);
                                                        match http_client_clone.post(&send_url).multipart(form).send().await {
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
                                                send_formatted_text(
                                                    &http_client_clone,
                                                    &tg_base_clone,
                                                    &chat_id,
                                                    thread_id_clone.as_deref(),
                                                    &content,
                                                ).await;
                                            }
                                        } else if !content.is_empty() {
                                            // Text-only path.
                                            info!("Sending final response back to Telegram Chat [{}]...", chat_id);
                                            send_formatted_text(
                                                &http_client_clone,
                                                &tg_base_clone,
                                                &chat_id,
                                                thread_id_clone.as_deref(),
                                                &content,
                                            ).await;
                                        }
                                    });
                                } else {
                                    warn!("Received a reply task but 'chat_id' was missing. Cannot route to Telegram.");
                                }
                            }
                        }
                    }
                    Ok(other) => info!("Hegemon received non-task IPC message: {:?}", other),
                    Err(e) => {
                        if is_ipc_disconnect(&e) {
                            info!("Hotel IPC disconnected; membrane exiting.");
                            return Ok(());
                        }
                        warn!("IPC Recv error: {}", e);
                    },
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        TELEGRAM_MAX_COMMANDS, TELEGRAM_MENU_COMMANDS, TelegramBotCommand, TelegramFileRef,
        build_telegram_menu_commands, default_attachment_name, enrich_attachment_with_transport,
        normalize_telegram_menu_command_name, telegram_command, telegram_format_text,
        telegram_help_text, telegram_inbound_envelope,
    };
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
        assert_eq!(envelope.content, "Telegram callback action: approve:turn-1");
        assert_eq!(envelope.callback_data.as_deref(), Some("approve:turn-1"));
        assert!(envelope.attachments.is_empty());
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
        let help = telegram_help_text();
        assert!(help.contains("/help"));
        assert!(help.contains("/commands"));
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
        let help = super::telegram_help_text();
        assert!(help.contains("/new"), "help text should mention /new");
    }

    #[test]
    fn menu_commands_include_new() {
        let has_new = super::TELEGRAM_MENU_COMMANDS
            .iter()
            .any(|c| c.command == "new");
        assert!(has_new, "TELEGRAM_MENU_COMMANDS should include 'new'");
    }
}
