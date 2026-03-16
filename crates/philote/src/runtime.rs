use crate::commands::{SlashCommand, parse_slash_command};
use crate::r#loop::{
    AgentAction, ApprovalRequest, ToolCall, ToolResult, TurnPhase, interpret_model_payload,
};
use crate::protocol::{
    FinalReplyPayload, InboundTaskPayload, ModelRequestPayload, TaskRunnerOverlay,
    ToolExecutionPayload, TransportAttachment, TurnEventPayload,
};
use crate::session::{
    MediaRoutingPolicy, SessionState, ToolExecutionRoute, TtsMode, VoiceResponsePolicy,
    WorkingTurn, merge_session_index,
};
use anyhow::Result;
use memory_core::{MuninnConfig, MuninnRestEngine, MemoryScope, VaultResolver};
use philotic_client::{HandoffBundle, IpcRequest, IpcResponse, PhiloticClient, TaskErrorPayload, is_ipc_disconnect};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{error, info, warn};
use uuid::Uuid;

pub const DEFAULT_AGENT_ID: &str = "agent-jane-01";
const DEFAULT_REPLY_ROLE: &str = "membrane";
const DEFAULT_TEXT_MODEL_ROLE: &str = "model";
const DEFAULT_VOICE_MODEL_ROLE: &str = "model.elevenlabs";
/// Maximum model round-trips per turn. Exhausting this budget fails the turn with a
/// clear error rather than looping indefinitely.
const MAX_TOOL_ITERATIONS: u32 = 10;

fn local_node_id() -> String {
    std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string())
}

#[cfg(test)]
const LOCAL_NODE: &str = "local-aiua-01";

fn extract_model_error(task: &InboundTaskPayload) -> Option<String> {
    if let Some(error) = task.error.as_ref() {
        return Some(error.display_message());
    }

    let agent_action = task.agent_action.as_ref()?;
    let mut parts = Vec::new();

    if let Some(message) = agent_action
        .get("message")
        .and_then(serde_json::Value::as_str)
    {
        parts.push(message.to_string());
    }

    if let Some(error) = agent_action
        .get("model_result")
        .and_then(|mr| mr.get("error"))
        .and_then(serde_json::Value::as_object)
    {
        if let Some(kind) = error.get("kind").and_then(serde_json::Value::as_str) {
            parts.push(format!("kind={kind}"));
        }
        if let Some(provider) = error.get("provider").and_then(serde_json::Value::as_str) {
            parts.push(format!("provider={provider}"));
        }
        if let Some(message) = error.get("message").and_then(serde_json::Value::as_str) {
            parts.push(message.to_string());
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" | "))
    }
}

fn implementation_to_model_role(implementation: &str) -> String {
    let normalized = implementation
        .split(['.', '-', '@', '/'])
        .find(|segment| !segment.is_empty())
        .unwrap_or("gemini");

    if normalized == "elevenlabs" {
        "model.elevenlabs".into()
    } else {
        "model".into()
    }
}

fn resolve_model_execution_target(
    state: Option<&SessionState>,
    capability: &str,
    fallback_role: &str,
) -> (String, String, Option<String>) {
    if let Some(route) = state.and_then(|state| state.resolve_component_execution_route(capability))
    {
        return (
            route.target_node.clone(),
            route.target_role.clone(),
            route.incarnation_id.clone(),
        );
    }

    let target_role = state
        .and_then(|state| state.preferred_component_implementation(capability))
        .map(implementation_to_model_role)
        .unwrap_or_else(|| fallback_role.into());

    (local_node_id(), target_role, None)
}

fn normalized_user_content(task: &InboundTaskPayload) -> Option<String> {
    if let Some(content) = task
        .content
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(content.to_string());
    }

    if let Some(callback_data) = task
        .callback_data
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(format!("Callback action: {callback_data}"));
    }

    if !task.attachments.is_empty() {
        let summaries = task
            .attachments
            .iter()
            .map(describe_transport_attachment)
            .collect::<Vec<_>>()
            .join(", ");
        let message_kind = task.message_kind.as_deref().unwrap_or("attachment");
        return Some(format!(
            "User sent a {message_kind} message with attachments: {summaries}."
        ));
    }

    task.message_kind
        .as_deref()
        .map(|message_kind| format!("User sent a {message_kind} message."))
}

fn describe_transport_attachment(attachment: &TransportAttachment) -> String {
    let mut parts = vec![attachment.kind.clone()];
    if let Some(file_name) = attachment
        .file_name
        .as_deref()
        .filter(|name| !name.is_empty())
    {
        parts.push(file_name.to_string());
    }
    if let Some(mime_type) = attachment
        .mime_type
        .as_deref()
        .filter(|mime| !mime.is_empty())
    {
        parts.push(mime_type.to_string());
    }
    if !attachment.file_id.is_empty() {
        parts.push(format!("file_id={}", attachment.file_id));
    }
    parts.join(" ")
}

fn media_analysis_attachments(task: &InboundTaskPayload) -> Vec<TransportAttachment> {
    task.attachments
        .iter()
        .filter(|attachment| {
            attachment
                .blob_download_url
                .as_deref()
                .map(|url| !url.is_empty())
                .unwrap_or(false)
                && attachment
                    .transport_error
                    .as_deref()
                    .map(|error| error.is_empty())
                    .unwrap_or(true)
                && matches!(
                    attachment.kind.as_str(),
                    "photo" | "image" | "voice" | "audio" | "document"
                )
        })
        .cloned()
        .collect()
}

fn media_analysis_prompt(content: &str, attachments: &[TransportAttachment]) -> String {
    let kinds = attachments
        .iter()
        .map(|attachment| attachment.kind.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Analyze the attached media and respond helpfully to the user. User message/context: {}. Attachment kinds: {}.",
        content, kinds
    )
}

fn transcription_prompt(content: &str) -> String {
    if content.trim().is_empty() {
        "Transcribe this audio message verbatim.".to_string()
    } else {
        format!(
            "Transcribe this audio message verbatim. User context: {}.",
            content
        )
    }
}

/// Deterministically strips markdown/markup from text for voice delivery.
/// Used as fallback when the model does not produce a `spoken_text` channel.
pub(crate) fn strip_markup(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_code_block = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // Fenced code blocks
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            if in_code_block {
                out.push_str("The following code: ");
            }
            continue;
        }
        if in_code_block {
            out.push_str(trimmed);
            out.push(' ');
            continue;
        }

        // ATX headings: # Heading → Heading
        let line = if let Some(rest) = trimmed
            .strip_prefix("######")
            .or_else(|| trimmed.strip_prefix("#####"))
            .or_else(|| trimmed.strip_prefix("####"))
            .or_else(|| trimmed.strip_prefix("###"))
            .or_else(|| trimmed.strip_prefix("##"))
            .or_else(|| trimmed.strip_prefix('#'))
        {
            rest.trim().to_string()
        } else {
            trimmed.to_string()
        };

        // List items: - item or * item or 1. item
        let line = if let Some(rest) = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .or_else(|| line.strip_prefix("• "))
        {
            rest.to_string()
        } else if line.len() > 2
            && line
                .chars()
                .next()
                .map(|c| c.is_ascii_digit())
                .unwrap_or(false)
            && line.contains(". ")
        {
            line.splitn(2, ". ").nth(1).unwrap_or(&line).to_string()
        } else {
            line
        };

        // Blockquotes
        let line = line.strip_prefix("> ").unwrap_or(&line).to_string();

        // Inline: **bold**, *italic*, __bold__, _italic_, `code`
        let line = line
            .replace("**", "")
            .replace("__", "")
            .replace('*', "")
            .replace('_', "")
            .replace('`', "");

        // Inline links: [text](url) → text
        let line = {
            let mut result = String::new();
            let mut rest = line.as_str();
            while let Some(start) = rest.find('[') {
                result.push_str(&rest[..start]);
                rest = &rest[start + 1..];
                if let Some(mid) = rest.find("](") {
                    let link_text = &rest[..mid];
                    rest = &rest[mid + 2..];
                    if let Some(end) = rest.find(')') {
                        result.push_str(link_text);
                        rest = &rest[end + 1..];
                    } else {
                        result.push('[');
                        result.push_str(link_text);
                    }
                } else {
                    result.push('[');
                }
            }
            result.push_str(rest);
            result
        };

        if !line.trim().is_empty() {
            out.push_str(line.trim());
            out.push_str(". ");
        }
    }

    // Collapse multiple spaces and trailing dots
    let result = out.trim_end_matches(". ").trim().to_string();
    // Collapse multiple ". "
    let result = result.replace(". . ", ". ");
    result
}

/// Maps a configured action name to the capability key used for component route resolution.
fn action_to_capability(action: &str) -> &'static str {
    match action {
        "transcribe" => "voice.transcribe",
        "describe" => "image.describe",
        "summarize" => "document.summarize",
        _ => "media.analyze",
    }
}

struct MediaRouting {
    action: String,
    capability: &'static str,
    attachments: Vec<TransportAttachment>,
    strip_tools: bool,
}

/// Applies the agent's `MediaRoutingPolicy` to the candidate blob-backed attachments and returns
/// routing parameters, or `None` if media should be ignored (policy disabled or no attachments).
///
/// Routing priority for mixed-kind turns: voice > image > document.
fn resolve_media_routing(
    policy: &MediaRoutingPolicy,
    candidate_attachments: Vec<TransportAttachment>,
) -> Option<MediaRouting> {
    if !policy.forward_media_to_model || candidate_attachments.is_empty() {
        return None;
    }

    let has_voice = candidate_attachments
        .iter()
        .any(|a| matches!(a.kind.as_str(), "voice" | "audio"));
    let has_image = candidate_attachments
        .iter()
        .any(|a| matches!(a.kind.as_str(), "photo" | "image"));
    let has_document = candidate_attachments
        .iter()
        .any(|a| a.kind.as_str() == "document");

    let action_str: &str = if has_voice {
        policy.voice_action.as_deref().unwrap_or("analyze_media")
    } else if has_image {
        policy.image_action.as_deref().unwrap_or("analyze_media")
    } else if has_document {
        policy.document_action.as_deref().unwrap_or("analyze_media")
    } else {
        return None;
    };

    Some(MediaRouting {
        action: action_str.to_string(),
        capability: action_to_capability(action_str),
        attachments: candidate_attachments,
        strip_tools: policy.strip_tools_on_media,
    })
}

fn format_role_command_reply(command: &SlashCommand, became_active: bool) -> String {
    match command {
        SlashCommand::Role { role_name } => {
            if became_active {
                format!("Switched to role {role_name}.")
            } else {
                format!("Switching to role {role_name} once it finishes materializing.")
            }
        }
        SlashCommand::Back => {
            if became_active {
                "Switched back to orchestrator.".into()
            } else {
                "Switching back to orchestrator once it finishes materializing.".into()
            }
        }
        _ => "Role command completed.".into(),
    }
}

fn format_roles_report(active_incarnation_id: Option<&str>, roles: &[serde_json::Value]) -> String {
    let active_role_name = active_incarnation_id
        .and_then(|guest_id| guest_id.rsplit(':').next())
        .unwrap_or("orchestrator");

    if roles.is_empty() {
        return format!(
            "Active role: {active_role_name}. No configured role incarnations were returned by the hotel."
        );
    }

    let mut lines = vec![format!("Active role: {active_role_name}.")];
    lines.push("Configured roles:".into());
    for role in roles {
        let role_name = role
            .get("role_name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let guest_id = role
            .get("guest_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let marker = if Some(guest_id) == active_incarnation_id || role_name == active_role_name {
            "*"
        } else {
            "-"
        };
        lines.push(format!("{marker} {role_name}"));
    }
    lines.join("\n")
}

pub struct AgentRuntime {
    ipc_client: PhiloticClient,
    agent_id: String,
    sessions: HashMap<String, SessionState>,
    /// MuninnDB config fetched from hotel at startup. None = NullMemoryEngine.
    muninn_config: Option<MuninnConfig>,
}

impl AgentRuntime {
    pub fn new(ipc_client: PhiloticClient, agent_id: impl Into<String>) -> Self {
        Self {
            ipc_client,
            agent_id: agent_id.into(),
            sessions: HashMap::new(),
            muninn_config: None,
        }
    }

    /// Fetch MuninnDB config from hotel IPC and store it for session use.
    async fn fetch_memory_config(&mut self) {
        info!("Requesting MuninnDB config from hotel...");
        match self.ipc_client.send_request(IpcRequest::FetchMemoryConfig).await {
            Ok(IpcResponse::MemoryConfig { config_json: Some(json) }) => {
                match serde_json::from_str::<MuninnConfig>(&json) {
                    Ok(cfg) => {
                        info!(endpoint = %cfg.base_url, vaults = cfg.vault_tokens.len(), "MuninnDB config loaded");
                        self.muninn_config = Some(cfg);
                    }
                    Err(e) => warn!("Failed to parse MuninnConfig from hotel: {}", e),
                }
            }
            Ok(IpcResponse::MemoryConfig { config_json: None }) => {
                info!("Hotel has no MuninnDB config — running without memory");
            }
            Ok(_) | Err(_) => {
                warn!("Unexpected response to FetchMemoryConfig — running without memory");
            }
        }
    }

    /// Build a `MuninnRestEngine` scoped to the given agent and user.
    fn memory_engine_for(&self, agent_id: &str, user_id: &str) -> Option<MuninnRestEngine> {
        self.muninn_config.clone().map(|cfg| {
            MuninnRestEngine::new(cfg, VaultResolver {
                agent_id: agent_id.to_string(),
                user_id: user_id.to_string(),
            })
        })
    }

    pub async fn run(&mut self) -> Result<()> {
        info!("Listening for inbound Persona tasks from the Philotic Web...");
        self.fetch_memory_config().await;

        loop {
            match tokio::time::timeout(Duration::from_secs(5), self.ipc_client.recv_task()).await {
                Ok(Ok(IpcResponse::InboundTask {
                    source_node,
                    task_id,
                    task_json,
                })) => {
                    info!(
                        "Agent [{}] received task [{}] from [{}]",
                        self.agent_id, task_id, source_node
                    );

                    match serde_json::from_str::<InboundTaskPayload>(&task_json) {
                        Ok(task) if task.is_model_response() => {
                            if let Err(err) = self.handle_model_response(task).await {
                                error!("Failed to handle model response: {}", err);
                            }
                        }
                        Ok(task) if task.is_tool_result() => {
                            if let Err(err) = self.handle_tool_result(task).await {
                                error!("Failed to handle tool result: {}", err);
                            }
                        }
                        Ok(task) => {
                            if let Err(err) = self.handle_user_message(task, task_id).await {
                                error!("Failed to handle user message: {}", err);
                            }
                        }
                        Err(err) => warn!("Could not parse inbound task payload: {}", err),
                    }
                }
                Ok(Ok(other)) => {
                    info!("Jane received non-task IPC message: {:?}", other);
                }
                Ok(Err(err)) => {
                    if is_ipc_disconnect(&err) {
                        info!("Hotel IPC disconnected; philote exiting.");
                        return Ok(());
                    }
                    warn!("IPC Recv error: {}", err);
                }
                Err(_) => {}
            }
        }
    }

    async fn handle_user_message(&mut self, task: InboundTaskPayload, task_id: Uuid) -> Result<()> {
        let content = match normalized_user_content(&task) {
            Some(content) => content.to_string(),
            None => return Ok(()),
        };
        let source = task
            .transport
            .clone()
            .or(task.source.clone())
            .unwrap_or_else(|| "unknown".into());
        let session_id = task.session_id_or_default(&self.agent_id);
        let turn_id = task.turn_id.clone().unwrap_or_else(|| task_id.to_string());
        let chat_id = task.chat_id.clone().unwrap_or_default();
        let local_node = local_node_id();
        let inbound_final_reply_to = task
            .final_reply_to
            .clone()
            .unwrap_or_else(|| local_node.clone());
        let inbound_final_reply_role = task
            .final_reply_role
            .clone()
            .unwrap_or_else(|| DEFAULT_REPLY_ROLE.to_string());
        let inbound_final_reply_guest_id = task.final_reply_guest_id.clone();

        self.ensure_session_loaded(&session_id, &source).await?;

        let (final_reply_to, final_reply_role, final_reply_guest_id) = {
            let state = self.sessions.entry(session_id.clone()).or_insert_with(|| {
                SessionState::new(session_id.clone(), self.agent_id.clone(), source)
            });
            state.set_transport_reply_target(
                inbound_final_reply_to,
                inbound_final_reply_role,
                inbound_final_reply_guest_id,
            );
            let target = state.resolved_transport_reply_target(
                local_node,
                DEFAULT_REPLY_ROLE.to_string(),
                None,
            );
            (
                target.target_node,
                target.target_role,
                target.target_guest_id,
            )
        };

        if let Some(command) = parse_slash_command(&content) {
            match command {
                SlashCommand::Ping => {}
                SlashCommand::Status | SlashCommand::Pause | SlashCommand::Resume => {}
                SlashCommand::Role { .. } | SlashCommand::Roles | SlashCommand::Back => {}
                SlashCommand::ToolsAdd { .. }
                | SlashCommand::ToolsClear
                | SlashCommand::SkillsAdd { .. }
                | SlashCommand::SkillsClear
                | SlashCommand::WorkspaceSet { .. }
                | SlashCommand::WorkspaceClear => {}
                SlashCommand::Approve { .. } | SlashCommand::Deny { .. } => {
                    return self
                        .handle_approval_command(
                            task_id,
                            session_id,
                            turn_id,
                            chat_id,
                            final_reply_to,
                            final_reply_role,
                            final_reply_guest_id,
                            command,
                        )
                        .await;
                }
                SlashCommand::PreapproveThisSession
                | SlashCommand::Preapprove { .. }
                | SlashCommand::ApprovalStatus
                | SlashCommand::ApprovalReset => {}
                SlashCommand::Tts { .. } => {}
                SlashCommand::Abandon { .. } => {}
            }
        }

        let had_voice_input = task
            .message_kind
            .as_deref()
            .map(|k| matches!(k, "voice" | "audio"))
            .unwrap_or(false)
            || task
                .attachments
                .iter()
                .any(|a| matches!(a.kind.as_str(), "voice" | "audio"));

        let (
            checkpoint_memory_type,
            checkpoint_json,
            index_state,
            model_prompt,
            model_context,
            context_projection,
            tools_for_model,
        ) = {
            let state = self
                .sessions
                .get_mut(&session_id)
                .expect("session should exist after ensuring and binding transport target");
            state.start_turn(WorkingTurn {
                task_id,
                turn_id: turn_id.clone(),
                chat_id: chat_id.clone(),
                user_content: content.clone(),
                final_reply_to: final_reply_to.clone(),
                final_reply_role: final_reply_role.clone(),
                final_reply_guest_id: final_reply_guest_id.clone(),
                phase: TurnPhase::Queued,
                iteration: 0,
                pending_tool_call: None,
                pending_approval: None,
                working_tool_history: Vec::new(),
                pending_text_reply: None,
                had_voice_input,
                awaiting_transcription_reentry: false,
            });
            state.set_active_turn_phase(TurnPhase::LoadingContext);

            let tools_for_model = state.project_tools_for_turn(&content);
            let (model_prompt, model_context, context_projection) =
                state.model_request_payloads(&content, &tools_for_model);
            (
                state.checkpoint_memory_type(),
                state.checkpoint_json(),
                state.clone(),
                model_prompt,
                model_context,
                context_projection,
                tools_for_model,
            )
        };

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        if let Some(command) = parse_slash_command(&content) {
            return match command {
                SlashCommand::Ping => {
                    self.complete_local_command(session_id, turn_id, "pong".into())
                        .await
                }
                SlashCommand::Status
                | SlashCommand::Pause
                | SlashCommand::Resume
                | SlashCommand::ToolsAdd { .. }
                | SlashCommand::ToolsClear
                | SlashCommand::SkillsAdd { .. }
                | SlashCommand::SkillsClear
                | SlashCommand::WorkspaceSet { .. }
                | SlashCommand::WorkspaceClear => {
                    self.handle_session_control_command(
                        task_id, session_id, turn_id, chat_id, command,
                    )
                    .await
                }
                SlashCommand::PreapproveThisSession
                | SlashCommand::Preapprove { .. }
                | SlashCommand::ApprovalStatus
                | SlashCommand::ApprovalReset => {
                    self.handle_session_control_command(
                        task_id, session_id, turn_id, chat_id, command,
                    )
                    .await
                }
                SlashCommand::Tts { .. } => {
                    self.handle_session_control_command(
                        task_id, session_id, turn_id, chat_id, command,
                    )
                    .await
                }
                SlashCommand::Role { .. } | SlashCommand::Roles | SlashCommand::Back => {
                    self.handle_role_command(task_id, session_id, turn_id, chat_id, command)
                        .await
                }
                SlashCommand::Approve { .. } | SlashCommand::Deny { .. } => Ok(()),
                SlashCommand::Abandon { reason } => {
                    self.handle_abandon_command(session_id, turn_id, reason)
                        .await
                }
            };
        }

        if self
            .sessions
            .get(&session_id)
            .map(|state| state.status == "paused")
            .unwrap_or(false)
        {
            return self
                .complete_local_command(
                    session_id,
                    turn_id,
                    "Session is paused. Use /resume to continue.".into(),
                )
                .await;
        }

        let media_policy = self
            .sessions
            .get(&session_id)
            .map(|s| s.agent_profile.media_routing_policy.clone())
            .unwrap_or_default();
        let media_attachments = media_analysis_attachments(&task);
        let media_routing = resolve_media_routing(&media_policy, media_attachments);
        let awaiting_transcription_reentry = media_routing
            .as_ref()
            .map(|routing| routing.action == "transcribe")
            .unwrap_or(false);

        let _ = self
            .ipc_client
            .send_request(IpcRequest::UpdateTask {
                task_id,
                state: "waiting_model".into(),
                payload: serde_json::json!({
                    "session_id": index_state.session_id,
                    "turn_id": turn_id,
                    "chat_id": chat_id,
                    "content": content,
                }),
            })
            .await?;

        let (checkpoint_memory_type, checkpoint_json, index_state) = {
            let state = self
                .sessions
                .get_mut(&session_id)
                .expect("active turn should still exist after context build");
            state.bump_active_turn_iteration();
            state.set_active_turn_phase(TurnPhase::WaitingModel);
            state.set_active_turn_awaiting_transcription_reentry(awaiting_transcription_reentry);
            (
                state.checkpoint_memory_type(),
                state.checkpoint_json(),
                state.clone(),
            )
        };

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let (action, prompt, attachments, tools_for_model, capability) =
            if let Some(routing) = media_routing {
                let prompt = if routing.action == "transcribe" {
                    transcription_prompt(&content)
                } else {
                    media_analysis_prompt(&content, &routing.attachments)
                };
                let effective_tools = if routing.strip_tools {
                    Vec::new()
                } else {
                    tools_for_model
                };
                (
                    routing.action,
                    prompt,
                    routing.attachments,
                    effective_tools,
                    routing.capability,
                )
            } else {
                (
                    "generate_text".to_string(),
                    model_prompt,
                    Vec::new(),
                    tools_for_model,
                    "text.generate",
                )
            };
        let (target_node, target_role, target_guest_id) = resolve_model_execution_target(
            self.sessions.get(&session_id),
            capability,
            DEFAULT_TEXT_MODEL_ROLE,
        );

        let attachment_kinds: Vec<&str> = attachments
            .iter()
            .filter_map(|attachment| {
                if attachment.kind.is_empty() {
                    None
                } else {
                    Some(attachment.kind.as_str())
                }
            })
            .collect();
        info!(
            "Session [{}] routing turn {:?} as action [{}] to role [{}] with {} attachment(s) kinds {:?}",
            session_id,
            task.turn_id,
            action,
            target_role,
            attachments.len(),
            attachment_kinds
        );
        for attachment in &attachments {
            info!(
                "Model-bound attachment kind [{}] file_id [{}] blob {:?} transport_error {:?}",
                attachment.kind,
                attachment.file_id,
                attachment.blob_id.as_deref(),
                attachment.transport_error.as_deref()
            );
        }

        let model_req = ModelRequestPayload {
            action,
            request_class: Some(
                if capability == "text.generate" {
                    "cognitive"
                } else {
                    "transform"
                }
                .to_string(),
            ),
            session_id: session_id.clone(),
            turn_id,
            prompt,
            user_content: content,
            context: Some(model_context),
            context_projection: Some(context_projection),
            attachments,
            tools_for_model,
            response_contract: Some(serde_json::json!({ "channels": ["spoken_text", "memory_concept"] })),
            chat_id,
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
        };

        info!("Asking the Hotel to route inference to the model controller...");
        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node,
                target_role,
                target_guest_id,
                task_json: serde_json::to_string(&model_req)?,
            })
            .await?;

        Ok(())
    }

    async fn handle_model_response(&mut self, task: InboundTaskPayload) -> Result<()> {
        let session_id = match task.session_id.as_deref().filter(|s| !s.is_empty()) {
            Some(session_id) => session_id.to_string(),
            None => return Ok(()),
        };
        let turn_id = match task.turn_id.as_deref().filter(|s| !s.is_empty()) {
            Some(turn_id) => turn_id.to_string(),
            None => return Ok(()),
        };
        self.ensure_session_loaded(&session_id, "unknown").await?;

        // If the turn is waiting for voice synthesis, this is the audio response — route it
        // directly to the voice handler regardless of the agent_action kind.
        let waiting_voice = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .map(|t| t.phase == TurnPhase::WaitingVoice)
            .unwrap_or(false);

        if waiting_voice {
            if let Some(model_error) = extract_model_error(&task) {
                warn!(
                    "Session [{}] voice synthesis failed before audio delivery: {}",
                    session_id, model_error
                );
                let voice_policy = self
                    .sessions
                    .get(&session_id)
                    .map(|s| s.agent_profile.voice_response_policy.clone())
                    .unwrap_or_default();

                if !voice_policy.fallback_to_text {
                    return self
                        .fail_active_turn(
                            session_id,
                            turn_id,
                            format!("Voice synthesis failed: {}", model_error),
                        )
                        .await;
                }
            }

            let raw_content = task.content.unwrap_or_default();
            return self
                .handle_voice_synthesis_response(session_id, turn_id, raw_content)
                .await;
        }

        if let Some(model_error) = extract_model_error(&task) {
            warn!(
                "Session [{}] model request failed before turn completion: {}",
                session_id, model_error
            );
            return self
                .fail_active_turn(
                    session_id,
                    turn_id,
                    format!("Model failed: {}", model_error),
                )
                .await;
        }

        if let Some(state) = self.sessions.get_mut(&session_id) {
            state.set_active_turn_phase(TurnPhase::Thinking);
        }

        let model_result = task
            .agent_action
            .as_ref()
            .and_then(|a| a.get("model_result"))
            .and_then(|mr| mr.get("result"));

        let spoken_text = model_result
            .and_then(|r| r.get("spoken_text"))
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string);

        let memory_concept = model_result
            .and_then(|r| r.get("memory_concept"))
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string);

        let awaiting_transcription_reentry = self
            .sessions
            .get(&session_id)
            .map(|state| state.active_turn_awaiting_transcription_reentry())
            .unwrap_or(false);

        let action = interpret_model_payload(task.agent_action.as_ref(), task.content.as_deref());
        if awaiting_transcription_reentry {
            return match action {
                AgentAction::Respond { content } => {
                    self.reenter_turn_after_transcription(session_id, turn_id, content)
                        .await
                }
                AgentAction::Fail { message } => {
                    self.fail_active_turn(session_id, turn_id, message).await
                }
                other => {
                    self.fail_active_turn(
                        session_id,
                        turn_id,
                        format!(
                            "voice.transcribe returned unexpected action {:?}; expected transcript text",
                            other
                        ),
                    )
                    .await
                }
            };
        }

        match action {
            AgentAction::Respond { content } => {
                self.complete_agent_response(session_id, turn_id, content, spoken_text, memory_concept)
                    .await
            }
            AgentAction::ToolCall(tool_call) => {
                self.handle_tool_call(session_id, turn_id, tool_call).await
            }
            AgentAction::RequestApproval(approval) => {
                self.handle_approval_request(session_id, turn_id, approval)
                    .await
            }
            AgentAction::Fail { message } => {
                self.fail_active_turn(session_id, turn_id, message).await
            }
        }
    }

    async fn handle_approval_request(
        &mut self,
        session_id: String,
        turn_id: String,
        approval: ApprovalRequest,
    ) -> Result<()> {
        let approval = Self::normalize_approval_request(approval);
        let preapproved = self
            .sessions
            .get(&session_id)
            .map(|state| {
                let tool = state
                    .active_turn
                    .as_ref()
                    .and_then(|t| t.pending_tool_call.as_ref());
                state.approval_policy_allows(&approval, tool)
            })
            .unwrap_or(false);

        let (
            task_id,
            chat_id,
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
            checkpoint_memory_type,
            checkpoint_json,
            index_state,
        ) = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                warn!(
                    "Received approval request for unknown session {}",
                    session_id
                );
                return Ok(());
            };
            let Some(active_turn) = state.active_turn.as_ref() else {
                warn!(
                    "Received approval request for session {} with no active turn",
                    session_id
                );
                return Ok(());
            };
            let task_id = active_turn.task_id;
            let chat_id = active_turn.chat_id.clone();
            let final_reply_to = active_turn.final_reply_to.clone();
            let final_reply_role = active_turn.final_reply_role.clone();
            let final_reply_guest_id = active_turn.final_reply_guest_id.clone();
            if preapproved {
                state.clear_pending_approval();
                state.set_active_turn_phase(TurnPhase::Thinking);
            } else {
                state.set_pending_approval(approval.clone());
                state.set_active_turn_phase(TurnPhase::WaitingApproval);
            }
            (
                task_id,
                chat_id,
                final_reply_to,
                final_reply_role,
                final_reply_guest_id,
                state.checkpoint_memory_type(),
                state.checkpoint_json(),
                state.clone(),
            )
        };

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        if preapproved {
            let _ = self
                .ipc_client
                .send_request(IpcRequest::UpdateTask {
                    task_id,
                    state: "approval_preapproved".into(),
                    payload: serde_json::json!({
                        "session_id": session_id,
                        "turn_id": turn_id,
                        "chat_id": chat_id,
                        "approval_request": {
                            "approval_id": approval.approval_id,
                            "reason": approval.reason,
                            "approved_response": approval.approved_response,
                        },
                        "approval_resolution": {
                            "approval_id": approval.approval_id,
                            "decision": "approved",
                            "reason": approval.reason,
                            "resolution_mode": "preapproved",
                        }
                    }),
                })
                .await?;

            return self
                .complete_agent_response(session_id, turn_id, approval.approved_response, None, None)
                .await;
        }

        let _ = self
            .ipc_client
            .send_request(IpcRequest::UpdateTask {
                task_id,
                state: "waiting_approval".into(),
                payload: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": chat_id,
                    "approval_request": {
                        "approval_id": approval.approval_id,
                        "reason": approval.reason,
                        "approved_response": approval.approved_response,
                    }
                }),
            })
            .await?;

        let _ = self
            .emit_turn_event(
                &session_id,
                "waiting_approval",
                Some(approval.reason.clone()),
            )
            .await;

        let reply_payload = FinalReplyPayload {
            action: "send_reply",
            session_id,
            turn_id,
            chat_id,
            content: format!(
                "Approval required: {}. Reply /approve or /deny.",
                approval.reason
            ),
            audio_artifact: None,
            send_text_caption: false,
        };

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: final_reply_to,
                target_role: final_reply_role,
                target_guest_id: final_reply_guest_id,
                task_json: serde_json::to_string(&reply_payload)?,
            })
            .await?;

        Ok(())
    }

    async fn handle_tool_call(
        &mut self,
        session_id: String,
        turn_id: String,
        tool_call: ToolCall,
    ) -> Result<()> {
        // Agent-level approval enforcement: if the tool's policy annotation marks it as
        // requiring approval, and the current approval policy does not preapprove it,
        // synthesize an ApprovalRequest before executing. This runs independently of
        // whether the model itself requested approval — it is the agent's safety gate.
        let force_approval = self
            .sessions
            .get(&session_id)
            .map(|state| {
                let requires = state
                    .tool_assembly
                    .policy_annotations
                    .get(&tool_call.tool_name)
                    .map(|a| a.approval_required)
                    .unwrap_or(false);
                if requires {
                    let synthetic = ApprovalRequest {
                        approval_id: None,
                        reason: format!(
                            "Tool '{}' requires approval before execution.",
                            tool_call.tool_name
                        ),
                        approved_response: format!("Executing {}.", tool_call.tool_name),
                    };
                    !state.approval_policy_allows(&synthetic, Some(&tool_call))
                } else {
                    false
                }
            })
            .unwrap_or(false);

        if force_approval {
            // Set pending_tool_call so the approval handler can read it for class lookup.
            if let Some(state) = self.sessions.get_mut(&session_id) {
                state.set_pending_tool_call(tool_call.clone());
            }
            let synthetic = ApprovalRequest {
                approval_id: Some(uuid::Uuid::new_v4().to_string()),
                reason: format!(
                    "Tool '{}' requires approval before execution.",
                    tool_call.tool_name
                ),
                approved_response: format!("Executing {}.", tool_call.tool_name),
            };
            return self
                .handle_approval_request(session_id, turn_id, synthetic)
                .await;
        }

        let (checkpoint_memory_type, checkpoint_json, index_state) = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                warn!("Received tool call for unknown session {}", session_id);
                return Ok(());
            };
            state.set_pending_tool_call(tool_call.clone());
            state.set_active_turn_phase(TurnPhase::WaitingTool);
            (
                state.checkpoint_memory_type(),
                state.checkpoint_json(),
                state.clone(),
            )
        };

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let _ = self
            .emit_turn_event(&session_id, "waiting_tool", None)
            .await;

        let (chat_id, final_reply_to, final_reply_role, final_reply_guest_id, workspace_ref, route, session_user_id) = {
            let Some(state) = self.sessions.get(&session_id) else {
                warn!(
                    "Tool execution requested for unknown session {}",
                    session_id
                );
                return Ok(());
            };
            let route = match Self::execute_bound_tool(state, &tool_call) {
                Ok(route) => route.clone(),
                Err(err) => {
                    return self
                        .fail_active_turn(session_id, turn_id, err.to_string())
                        .await;
                }
            };
            let active_turn = state
                .active_turn
                .as_ref()
                .expect("active turn should exist while routing tool call");
            (
                active_turn.chat_id.clone(),
                active_turn.final_reply_to.clone(),
                active_turn.final_reply_role.clone(),
                active_turn.final_reply_guest_id.clone(),
                state.bindings.effective_workspace_ref.clone(),
                route,
                state.source.clone(),
            )
        };

        let tool_req = ToolExecutionPayload {
            action: "execute_tool",
            session_id,
            turn_id,
            chat_id,
            tool_name: tool_call.tool_name,
            arguments: tool_call.arguments,
            execution_mode: route.execution_mode.clone(),
            agent_id: self.agent_id.clone(),
            user_id: Some(session_user_id),
            runner_id: route.runner_id.clone(),
            incarnation_id: route.incarnation_id.clone(),
            hotel_id: route.hotel_id.clone(),
            environment_id: route.environment_id.clone(),
            task_runner_kind: route.task_runner_kind.clone(),
            task_runner_config: route.task_runner_config.clone(),
            selection_reason: route.selection_reason.clone(),
            workspace_ref: workspace_ref.clone(),
            task_runner_overlay: route
                .task_runner_kind
                .as_deref()
                .map(|kind| TaskRunnerOverlay {
                    workspace_ref: if kind == "workspace" {
                        workspace_ref
                    } else {
                        None
                    },
                    allowed_tools: None,
                    max_read_bytes: None,
                    max_search_results: None,
                }),
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
        };

        if route.execution_mode == "local_agent" {
            return self.execute_local_agent_tool(tool_req).await;
        }

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: route.target_node,
                target_role: route.target_role,
                target_guest_id: route.incarnation_id.clone(),
                task_json: serde_json::to_string(&tool_req)?,
            })
            .await?;

        Ok(())
    }

    async fn handle_tool_result(&mut self, task: InboundTaskPayload) -> Result<()> {
        let session_id = match task.session_id.as_deref().filter(|s| !s.is_empty()) {
            Some(session_id) => session_id.to_string(),
            None => return Ok(()),
        };
        let turn_id = match task.turn_id.as_deref().filter(|s| !s.is_empty()) {
            Some(turn_id) => turn_id.to_string(),
            None => return Ok(()),
        };
        let tool_result = ToolResult {
            tool_name: task.tool_name.clone().unwrap_or_else(|| "unknown".into()),
            content: task.content.clone().unwrap_or_default(),
        };

        // Pair the result with the pending tool call, push to history, check iteration cap.
        let loop_outcome = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                warn!("Tool result returned for unknown session {}", session_id);
                return Ok(());
            };

            let tool_call = state
                .active_turn
                .as_ref()
                .and_then(|t| t.pending_tool_call.clone())
                .unwrap_or_else(|| ToolCall {
                    tool_name: tool_result.tool_name.clone(),
                    arguments: serde_json::json!({}),
                });

            state.push_tool_history(tool_call, tool_result.clone());
            state.clear_pending_tool_call();

            let iteration = state.active_turn.as_ref().map(|t| t.iteration).unwrap_or(0);

            if iteration >= MAX_TOOL_ITERATIONS {
                Err(format!(
                    "Turn exceeded maximum tool iterations ({MAX_TOOL_ITERATIONS}). Aborting."
                ))
            } else {
                // Build re-entry prompt with full tool history injected.
                match state.build_reentry_prompt() {
                    Some(prompt) => {
                        if let Some(turn) = state.active_turn.as_mut() {
                            turn.iteration += 1;
                            turn.phase = TurnPhase::WaitingModel;
                        }
                        let tools = state.tool_assembly.tools_for_model.clone();
                        let active_turn = state.active_turn.as_ref().expect("turn exists");
                        Ok((
                            prompt,
                            active_turn.task_id,
                            active_turn.user_content.clone(),
                            active_turn.chat_id.clone(),
                            active_turn.final_reply_to.clone(),
                            active_turn.final_reply_role.clone(),
                            active_turn.final_reply_guest_id.clone(),
                            tools,
                            state.checkpoint_memory_type(),
                            state.checkpoint_json(),
                            state.clone(),
                        ))
                    }
                    None => {
                        Err("Active turn vanished before re-entry prompt could be built".into())
                    }
                }
            }
        };

        match loop_outcome {
            Err(msg) => self.fail_active_turn(session_id, turn_id, msg).await,
            Ok((
                prompt,
                _task_id,
                user_content,
                chat_id,
                final_reply_to,
                final_reply_role,
                final_reply_guest_id,
                tools_for_model,
                checkpoint_memory_type,
                checkpoint_json,
                index_state,
            )) => {
                self.ipc_client
                    .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
                    .await?;
                self.sync_session_index(&index_state).await?;

                let _ = self
                    .emit_turn_event(&session_id, "waiting_tool", None)
                    .await;

                let model_req = ModelRequestPayload {
                    action: "generate_text".to_string(),
                    request_class: Some("cognitive".to_string()),
                    session_id: session_id.clone(),
                    turn_id,
                    prompt,
                    user_content,
                    context: None,
                    context_projection: None,
                    attachments: Vec::new(),
                    tools_for_model,
                    response_contract: None,
                    chat_id,
                    reply_to: local_node_id(),
                    reply_role: "agent".into(),
                    final_reply_to,
                    final_reply_role,
                    final_reply_guest_id,
                };

                let (target_node, target_role, target_guest_id) = resolve_model_execution_target(
                    self.sessions.get(&session_id),
                    "text.generate",
                    DEFAULT_TEXT_MODEL_ROLE,
                );

                info!(
                    "Session [{}] re-entering model loop (iteration {})",
                    session_id,
                    self.sessions
                        .get(&session_id)
                        .and_then(|s| s.active_turn.as_ref())
                        .map(|t| t.iteration)
                        .unwrap_or(0)
                );

                self.ipc_client
                    .send_request(IpcRequest::EmitTask {
                        target_node,
                        target_role,
                        target_guest_id,
                        task_json: serde_json::to_string(&model_req)?,
                    })
                    .await?;

                Ok(())
            }
        }
    }

    async fn reenter_turn_after_transcription(
        &mut self,
        session_id: String,
        turn_id: String,
        transcript: String,
    ) -> Result<()> {
        let transcript = transcript.trim().to_string();
        if transcript.is_empty() {
            return self
                .fail_active_turn(
                    session_id,
                    turn_id,
                    "Voice transcription returned an empty transcript".into(),
                )
                .await;
        }

        let (reentry, checkpoint_memory_type, checkpoint_json, index_state) = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                warn!(
                    "Received transcription re-entry for unknown session {}",
                    session_id
                );
                return Ok(());
            };
            let Some(reentry) = state.prepare_transcription_reentry(&transcript) else {
                return self
                    .fail_active_turn(
                        session_id,
                        turn_id,
                        "Voice transcription returned an empty transcript".into(),
                    )
                    .await;
            };
            (
                reentry,
                state.checkpoint_memory_type(),
                state.checkpoint_json(),
                state.clone(),
            )
        };

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let _ = self
            .ipc_client
            .send_request(IpcRequest::UpdateTask {
                task_id: reentry.task_id,
                state: "waiting_model".into(),
                payload: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": reentry.chat_id,
                    "content": reentry.user_content,
                }),
            })
            .await?;

        let (context, context_projection) = self
            .sessions
            .get(&session_id)
            .map(|state| {
                let projection = state.build_context_projection(&reentry.user_content);
                (
                    Some(state.model_context_from_projection(&projection)),
                    Some(
                        serde_json::to_value(&projection)
                            .expect("context projection should serialize"),
                    ),
                )
            })
            .unwrap_or((None, None));

        let model_req = ModelRequestPayload {
            action: "generate_text".to_string(),
            request_class: Some("cognitive".to_string()),
            session_id: session_id.clone(),
            turn_id,
            prompt: reentry.prompt,
            user_content: reentry.user_content.clone(),
            context,
            context_projection,
            attachments: Vec::new(),
            tools_for_model: reentry.tools_for_model,
            response_contract: Some(serde_json::json!({ "channels": ["spoken_text", "memory_concept"] })),
            chat_id: reentry.chat_id,
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            final_reply_to: reentry.final_reply_to,
            final_reply_role: reentry.final_reply_role,
            final_reply_guest_id: reentry.final_reply_guest_id,
        };

        let (target_node, target_role, target_guest_id) = resolve_model_execution_target(
            self.sessions.get(&session_id),
            "text.generate",
            DEFAULT_TEXT_MODEL_ROLE,
        );

        info!(
            "Session [{}] re-entering normal reasoning after voice transcription",
            session_id
        );

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node,
                target_role,
                target_guest_id,
                task_json: serde_json::to_string(&model_req)?,
            })
            .await?;

        Ok(())
    }

    /// Emit a turn lifecycle event back to the transport (membrane) so it can update delivery UX
    /// (typing indicator, approval card, error message) without waiting for the final reply.
    ///
    /// Silently skips if there is no active turn or the session is unknown — turn events are
    /// best-effort delivery signals, not transactional guarantees.
    async fn emit_turn_event(
        &mut self,
        session_id: &str,
        event: &str,
        partial_content: Option<String>,
    ) -> Result<()> {
        let (turn_id, chat_id, final_reply_to, final_reply_role, final_reply_guest_id) = {
            let Some(state) = self.sessions.get(session_id) else {
                return Ok(());
            };
            let Some(active_turn) = state.active_turn.as_ref() else {
                return Ok(());
            };
            (
                active_turn.turn_id.clone(),
                active_turn.chat_id.clone(),
                active_turn.final_reply_to.clone(),
                active_turn.final_reply_role.clone(),
                active_turn.final_reply_guest_id.clone(),
            )
        };

        let event_payload = TurnEventPayload {
            action: "turn_event",
            event: event.to_string(),
            session_id: session_id.to_string(),
            turn_id,
            chat_id,
            partial_content,
        };

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: final_reply_to,
                target_role: final_reply_role,
                target_guest_id: final_reply_guest_id,
                task_json: serde_json::to_string(&event_payload)?,
            })
            .await?;

        Ok(())
    }

    async fn complete_agent_response(
        &mut self,
        session_id: String,
        turn_id: String,
        content: String,
        spoken_text: Option<String>,
        memory_concept: Option<String>,
    ) -> Result<()> {
        let voice_policy = self
            .sessions
            .get(&session_id)
            .map(|s| s.agent_profile.voice_response_policy.clone())
            .unwrap_or_default();

        let had_voice_input = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .map(|t| t.had_voice_input)
            .unwrap_or(false);

        if voice_policy.is_active(had_voice_input) {
            return self
                .start_voice_synthesis(session_id, turn_id, content, spoken_text, voice_policy)
                .await;
        }

        self.deliver_text_reply(session_id, turn_id, content, None, false, memory_concept)
            .await
    }

    /// Stashes the text, sets `WaitingVoice`, and emits a `voice.synthesize` task.
    async fn start_voice_synthesis(
        &mut self,
        session_id: String,
        turn_id: String,
        display_text: String,
        spoken_text: Option<String>,
        policy: VoiceResponsePolicy,
    ) -> Result<()> {
        let (
            task_id,
            chat_id,
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
            checkpoint_memory_type,
            checkpoint_json,
            index_state,
        ) = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                warn!("start_voice_synthesis: unknown session {}", session_id);
                return Ok(());
            };
            let Some(active_turn) = state.active_turn.as_ref() else {
                warn!(
                    "start_voice_synthesis: no active turn for session {}",
                    session_id
                );
                return Ok(());
            };
            let task_id = active_turn.task_id;
            let chat_id = active_turn.chat_id.clone();
            let final_reply_to = active_turn.final_reply_to.clone();
            let final_reply_role = active_turn.final_reply_role.clone();
            let final_reply_guest_id = active_turn.final_reply_guest_id.clone();
            state.set_pending_text_reply(display_text.clone());
            state.set_active_turn_phase(TurnPhase::WaitingVoice);
            (
                task_id,
                chat_id,
                final_reply_to,
                final_reply_role,
                final_reply_guest_id,
                state.checkpoint_memory_type(),
                state.checkpoint_json(),
                state.clone(),
            )
        };

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let _ = self
            .ipc_client
            .send_request(IpcRequest::UpdateTask {
                task_id,
                state: "waiting_voice".into(),
                payload: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": chat_id,
                }),
            })
            .await?;

        let (target_node, target_role, target_guest_id) = resolve_model_execution_target(
            self.sessions.get(&session_id),
            "voice.synthesize",
            DEFAULT_VOICE_MODEL_ROLE,
        );

        info!(
            "Session [{}] routing voice synthesis for turn {:?} to role [{}] voice_id {:?}",
            session_id,
            turn_id,
            target_role,
            policy.voice_id.as_deref(),
        );

        let provider_options = if let Some(speed_percent) = policy.speed_percent {
            let speed = f64::from(speed_percent) / 100.0;
            serde_json::json!({
                "voice_settings": {
                    "speed": speed
                }
            })
        } else {
            serde_json::json!({})
        };

        let voice_task = serde_json::json!({
            "kind": "voice.synthesize",
            "request_class": "synthesis",
            "provider": policy.provider,
            "spoken_text": spoken_text.unwrap_or_else(|| strip_markup(&display_text)),
            "voice_id": policy.voice_id,
            "model": policy.model,
            "provider_options": provider_options,
            "session_id": session_id,
            "turn_id": turn_id,
            "chat_id": chat_id,
            "reply_to": local_node_id(),
            "reply_role": "agent",
            "final_reply_to": final_reply_to,
            "final_reply_role": final_reply_role,
            "final_reply_guest_id": final_reply_guest_id,
        });

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node,
                target_role,
                target_guest_id,
                task_json: serde_json::to_string(&voice_task)?,
            })
            .await?;

        Ok(())
    }

    /// Handles the voice synthesis response for a turn in `WaitingVoice` phase.
    async fn handle_voice_synthesis_response(
        &mut self,
        session_id: String,
        turn_id: String,
        raw_audio_content: String,
    ) -> Result<()> {
        let voice_policy = self
            .sessions
            .get(&session_id)
            .map(|s| s.agent_profile.voice_response_policy.clone())
            .unwrap_or_default();

        // Validate the audio content — if it doesn't look like a valid audio artifact, fall back.
        let audio_artifact = if raw_audio_content.trim_start().starts_with('{') {
            Some(raw_audio_content.clone())
        } else {
            warn!(
                "Session [{}] voice synthesis response does not look like an audio artifact; fallback={}",
                session_id, voice_policy.fallback_to_text
            );
            None
        };

        if audio_artifact.is_none() && !voice_policy.fallback_to_text {
            return self
                .fail_active_turn(
                    session_id,
                    turn_id,
                    "Voice synthesis failed and fallback_to_text is disabled.".into(),
                )
                .await;
        }

        // Recover the stashed text and complete the turn.
        let text = self
            .sessions
            .get_mut(&session_id)
            .and_then(|s| s.take_pending_text_reply())
            .unwrap_or_default();

        self.deliver_text_reply(
            session_id,
            turn_id,
            text,
            audio_artifact,
            voice_policy.caption_enabled(),
            None,
        )
        .await
    }

    /// Final step: complete the turn, sync state, and emit `FinalReplyPayload` to membrane.
    async fn deliver_text_reply(
        &mut self,
        session_id: String,
        turn_id: String,
        content: String,
        audio_artifact: Option<String>,
        send_text_caption: bool,
        memory_concept: Option<String>,
    ) -> Result<()> {
        let (completed_turn, checkpoint_memory_type, checkpoint_json, index_state) = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                warn!("deliver_text_reply: unknown session {}", session_id);
                return Ok(());
            };

            let Some(completed_turn) = state.complete_active_turn(content.clone()) else {
                warn!(
                    "deliver_text_reply: no active turn for session {}",
                    session_id
                );
                return Ok(());
            };

            if completed_turn.turn_id != turn_id {
                warn!(
                    "Turn mismatch for session {}: active={} response={}",
                    session_id, completed_turn.turn_id, turn_id
                );
            }
            state.set_active_turn_phase(TurnPhase::Completed);

            (
                completed_turn,
                state.checkpoint_memory_type(),
                state.checkpoint_json(),
                state.clone(),
            )
        };

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let _ = self
            .ipc_client
            .send_request(IpcRequest::CompleteTask {
                task_id: completed_turn.task_id,
                result: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": completed_turn.chat_id,
                    "content": content,
                }),
            })
            .await?;

        // Capture for attend hook before moving into reply_payload.
        let attend_turn_id = turn_id.clone();
        let attend_content = content.clone();
        let attend_session_id = session_id.clone();

        let reply_payload = FinalReplyPayload {
            action: "send_reply",
            session_id,
            turn_id,
            chat_id: completed_turn.chat_id,
            content,
            audio_artifact,
            send_text_caption,
        };

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: completed_turn.final_reply_to,
                target_role: completed_turn.final_reply_role,
                target_guest_id: completed_turn.final_reply_guest_id,
                task_json: serde_json::to_string(&reply_payload)?,
            })
            .await?;

        // Attend hook (Slice E): fire-and-forget autobiographical memory write.
        if let Some(engine) = self.memory_engine_for(&self.agent_id, &self.agent_id) {
            let agent_id = self.agent_id.clone();
            let concept = memory_concept.unwrap_or_else(|| format!("turn:{}", attend_turn_id));
            let content_snapshot = attend_content;
            let tags = vec![
                format!("agent:{}", agent_id),
                format!("session:{}", attend_session_id),
            ];
            tokio::spawn(async move {
                use memory_core::MemoryEngine as _;
                if let Err(e) = engine
                    .remember(MemoryScope::SelfOnly, &concept, &content_snapshot, tags)
                    .await
                {
                    warn!(agent = %agent_id, error = %e, "Attend: memory write failed (non-fatal)");
                }
            });
        }

        Ok(())
    }

    async fn fail_active_turn(
        &mut self,
        session_id: String,
        turn_id: String,
        message: String,
    ) -> Result<()> {
        let (
            task_id,
            checkpoint_memory_type,
            checkpoint_json,
            index_state,
            chat_id,
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
        ) = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                warn!("Received fail action for unknown session {}", session_id);
                return Ok(());
            };
            let Some(active_turn) = state.active_turn.as_ref() else {
                warn!(
                    "Received fail action for session {} with no active turn",
                    session_id
                );
                return Ok(());
            };
            let task_id = active_turn.task_id;
            let chat_id = active_turn.chat_id.clone();
            let final_reply_to = active_turn.final_reply_to.clone();
            let final_reply_role = active_turn.final_reply_role.clone();
            let final_reply_guest_id = active_turn.final_reply_guest_id.clone();
            state.set_active_turn_phase(TurnPhase::Failed);
            (
                task_id,
                state.checkpoint_memory_type(),
                state.checkpoint_json(),
                state.clone(),
                chat_id,
                final_reply_to,
                final_reply_role,
                final_reply_guest_id,
            )
        };

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let _ = self
            .ipc_client
            .send_request(IpcRequest::FailTask {
                task_id,
                error_code: "MODEL_EMPTY_RESPONSE".into(),
                reason: message.clone(),
            })
            .await?;

        let reply_payload = FinalReplyPayload {
            action: "send_reply",
            session_id,
            turn_id,
            chat_id,
            content: message,
            audio_artifact: None,
            send_text_caption: false,
        };

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: final_reply_to,
                target_role: final_reply_role,
                target_guest_id: final_reply_guest_id,
                task_json: serde_json::to_string(&reply_payload)?,
            })
            .await?;

        Ok(())
    }

    async fn handle_approval_command(
        &mut self,
        command_task_id: Uuid,
        session_id: String,
        command_turn_id: String,
        command_chat_id: String,
        command_reply_to: String,
        command_reply_role: String,
        command_reply_guest_id: Option<String>,
        command: SlashCommand,
    ) -> Result<()> {
        let pending = self
            .sessions
            .get(&session_id)
            .and_then(|state| state.active_turn.as_ref())
            .and_then(|turn| {
                if turn.phase == TurnPhase::WaitingApproval {
                    turn.pending_approval.clone().map(|approval| {
                        (
                            turn.task_id,
                            turn.turn_id.clone(),
                            turn.chat_id.clone(),
                            turn.final_reply_to.clone(),
                            turn.final_reply_role.clone(),
                            turn.final_reply_guest_id.clone(),
                            approval,
                        )
                    })
                } else {
                    None
                }
            });

        let Some((
            original_task_id,
            original_turn_id,
            original_chat_id,
            original_reply_to,
            original_reply_role,
            original_reply_guest_id,
            approval,
        )) = pending
        else {
            let _ = self
                .ipc_client
                .send_request(IpcRequest::CompleteTask {
                    task_id: command_task_id,
                    result: serde_json::json!({
                        "session_id": session_id,
                        "turn_id": command_turn_id,
                        "chat_id": command_chat_id,
                        "content": "No approval pending."
                    }),
                })
                .await?;
            let reply_payload = FinalReplyPayload {
                action: "send_reply",
                session_id,
                turn_id: command_turn_id,
                chat_id: command_chat_id,
                content: "No approval pending.".into(),
                audio_artifact: None,
                send_text_caption: false,
            };
            self.ipc_client
                .send_request(IpcRequest::EmitTask {
                    target_node: command_reply_to,
                    target_role: command_reply_role,
                    target_guest_id: command_reply_guest_id,
                    task_json: serde_json::to_string(&reply_payload)?,
                })
                .await?;
            return Ok(());
        };

        let command_has_steering = command.steering_note().is_some();
        let (checkpoint_memory_type, checkpoint_json, index_state) = {
            let state = self
                .sessions
                .get_mut(&session_id)
                .expect("session should exist while resolving approval");
            state.clear_pending_approval();
            match command {
                SlashCommand::Approve { .. } => state.set_active_turn_phase(TurnPhase::Thinking),
                SlashCommand::Deny { .. } => {
                    if command_has_steering {
                        state.set_active_turn_phase(TurnPhase::Thinking);
                    } else {
                        state.set_active_turn_phase(TurnPhase::Failed);
                    }
                }
                SlashCommand::Ping
                | SlashCommand::Status
                | SlashCommand::Pause
                | SlashCommand::Resume
                | SlashCommand::Role { .. }
                | SlashCommand::Roles
                | SlashCommand::Back
                | SlashCommand::ToolsAdd { .. }
                | SlashCommand::ToolsClear
                | SlashCommand::SkillsAdd { .. }
                | SlashCommand::SkillsClear
                | SlashCommand::WorkspaceSet { .. }
                | SlashCommand::WorkspaceClear
                | SlashCommand::PreapproveThisSession
                | SlashCommand::Preapprove { .. }
                | SlashCommand::ApprovalStatus
                | SlashCommand::ApprovalReset
                | SlashCommand::Abandon { .. }
                | SlashCommand::Tts { .. } => {}
            }
            (
                state.checkpoint_memory_type(),
                state.checkpoint_json(),
                state.clone(),
            )
        };

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let _ = self
            .ipc_client
            .send_request(IpcRequest::CompleteTask {
                task_id: command_task_id,
                result: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": command_turn_id,
                    "chat_id": command_chat_id,
                    "content": command.reply_text().unwrap_or("ok"),
                }),
            })
            .await?;

        match command {
            SlashCommand::Approve { note } => {
                let _ = self
                    .ipc_client
                    .send_request(IpcRequest::UpdateTask {
                        task_id: original_task_id,
                        state: "resuming".into(),
                        payload: serde_json::json!({
                            "session_id": session_id,
                            "turn_id": original_turn_id,
                            "chat_id": original_chat_id,
                            "approval_resolution": {
                                "approval_id": approval.approval_id,
                                "decision": "approved",
                                "reason": approval.reason,
                                "resolution_mode": "manual",
                                "note": note,
                            }
                        }),
                    })
                    .await?;
                if let Some(note) = note {
                    return self
                        .resume_turn_with_steering(
                            session_id,
                            original_turn_id,
                            original_chat_id,
                            note,
                            "resuming_with_steering",
                            "[User approval steering]",
                        )
                        .await;
                }
                let _ = self
                    .ipc_client
                    .send_request(IpcRequest::CompleteTask {
                        task_id: original_task_id,
                        result: serde_json::json!({
                            "session_id": session_id,
                            "turn_id": original_turn_id,
                            "chat_id": original_chat_id,
                            "content": approval.approved_response,
                        }),
                    })
                    .await?;

                let reply_payload = FinalReplyPayload {
                    action: "send_reply",
                    session_id,
                    turn_id: original_turn_id,
                    chat_id: original_chat_id,
                    content: approval.approved_response.clone(),
                    audio_artifact: None,
                    send_text_caption: false,
                };

                self.ipc_client
                    .send_request(IpcRequest::EmitTask {
                        target_node: original_reply_to,
                        target_role: original_reply_role,
                        target_guest_id: original_reply_guest_id.clone(),
                        task_json: serde_json::to_string(&reply_payload)?,
                    })
                    .await?;
            }
            SlashCommand::Deny { note } => {
                let _ = self
                    .ipc_client
                    .send_request(IpcRequest::UpdateTask {
                        task_id: original_task_id,
                        state: "approval_denied".into(),
                        payload: serde_json::json!({
                            "session_id": session_id,
                            "turn_id": original_turn_id,
                            "chat_id": original_chat_id,
                            "approval_resolution": {
                                "approval_id": approval.approval_id,
                                "decision": "denied",
                                "reason": approval.reason,
                                "resolution_mode": "manual",
                                "note": note,
                            }
                        }),
                    })
                    .await?;
                if let Some(note) = note {
                    return self
                        .resume_turn_with_steering(
                            session_id,
                            original_turn_id,
                            original_chat_id,
                            note,
                            "redirecting_after_denial",
                            "[User denied the proposed action. Do this instead]",
                        )
                        .await;
                }
                let _ = self
                    .ipc_client
                    .send_request(IpcRequest::FailTask {
                        task_id: original_task_id,
                        error_code: "APPROVAL_DENIED".into(),
                        reason: approval.reason.clone(),
                    })
                    .await?;

                let reply_payload = FinalReplyPayload {
                    action: "send_reply",
                    session_id,
                    turn_id: original_turn_id,
                    chat_id: original_chat_id,
                    content: format!("Denied: {}", approval.reason),
                    audio_artifact: None,
                    send_text_caption: false,
                };

                self.ipc_client
                    .send_request(IpcRequest::EmitTask {
                        target_node: original_reply_to,
                        target_role: original_reply_role,
                        target_guest_id: original_reply_guest_id,
                        task_json: serde_json::to_string(&reply_payload)?,
                    })
                    .await?;
            }
            SlashCommand::Ping
            | SlashCommand::Status
            | SlashCommand::Pause
            | SlashCommand::Resume
            | SlashCommand::Role { .. }
            | SlashCommand::Roles
            | SlashCommand::Back
            | SlashCommand::ToolsAdd { .. }
            | SlashCommand::ToolsClear
            | SlashCommand::SkillsAdd { .. }
            | SlashCommand::SkillsClear
            | SlashCommand::WorkspaceSet { .. }
            | SlashCommand::WorkspaceClear
            | SlashCommand::PreapproveThisSession
            | SlashCommand::Preapprove { .. }
            | SlashCommand::ApprovalStatus
            | SlashCommand::ApprovalReset
            | SlashCommand::Abandon { .. }
            | SlashCommand::Tts { .. } => {}
        }

        Ok(())
    }

    async fn handle_role_command(
        &mut self,
        command_task_id: Uuid,
        session_id: String,
        command_turn_id: String,
        command_chat_id: String,
        command: SlashCommand,
    ) -> Result<()> {
        let response = match &command {
            SlashCommand::Role { role_name } => {
                let handoff_bundle = self
                    .sessions
                    .get(&session_id)
                    .map(|state| {
                        state.build_same_identity_handoff_bundle(
                            role_name,
                            &command_turn_id,
                            "manual_role_switch",
                            Some("orchestrator".into()),
                        )
                    })
                    .unwrap_or_else(|| HandoffBundle {
                        goal: format!("Switch active role to {role_name} for this session."),
                        context_excerpt: "Manual role switch requested by user slash command."
                            .into(),
                        session_id: session_id.clone(),
                        initiating_turn_id: command_turn_id.clone(),
                        return_to: Some("orchestrator".into()),
                        handoff_reason: Some("manual_role_switch".into()),
                        from_role: Some("orchestrator".into()),
                        to_role: Some(role_name.clone()),
                        active_goal: None,
                        active_constraints: vec!["same_identity_role_handoff".into()],
                        relevant_session_facts: Vec::new(),
                        working_summary: None,
                        suggested_memory_refs: Vec::new(),
                        expected_return_mode: Some("required".into()),
                        cleanup_actions: vec!["switch_active_role".into()],
                    });
                self.ipc_client
                    .send_request(IpcRequest::HandoffToRole {
                        session_id: session_id.clone(),
                        role_name: role_name.clone(),
                        handoff_bundle,
                    })
                    .await?
            }
            SlashCommand::Back => {
                self.ipc_client
                    .send_request(IpcRequest::HandoffBack {
                        session_id: session_id.clone(),
                        summary: "Manual return to orchestrator requested by user slash command."
                            .into(),
                        return_to: None,
                    })
                    .await?
            }
            SlashCommand::Roles => {
                self.ipc_client
                    .send_request(IpcRequest::ListRoleIncarnations {
                        agent_id: self.agent_id.clone(),
                    })
                    .await?
            }
            _ => unreachable!("handle_role_command only accepts role handoff commands"),
        };

        let (reply_content, update_state, payload, next_active_incarnation) = match response {
            IpcResponse::HandoffAck {
                handoff_guest_id,
                became_active,
            } => (
                format_role_command_reply(&command, became_active),
                if became_active {
                    "role_handoff_completed"
                } else {
                    "role_handoff_materializing"
                },
                serde_json::json!({
                    "session_id": session_id,
                    "turn_id": command_turn_id,
                    "chat_id": command_chat_id,
                    "role_command": "handoff_to_role",
                    "handoff_guest_id": handoff_guest_id,
                    "became_active": became_active,
                }),
                became_active.then_some(handoff_guest_id),
            ),
            IpcResponse::HandoffBackAck {
                handoff_guest_id,
                became_active,
            } => (
                format_role_command_reply(&command, became_active),
                if became_active {
                    "role_handoff_completed"
                } else {
                    "role_handoff_materializing"
                },
                serde_json::json!({
                    "session_id": session_id,
                    "turn_id": command_turn_id,
                    "chat_id": command_chat_id,
                    "role_command": "handoff_back",
                    "handoff_guest_id": handoff_guest_id,
                    "became_active": became_active,
                }),
                became_active.then_some(handoff_guest_id),
            ),
            IpcResponse::Standard { ok: true, data, .. }
                if matches!(command, SlashCommand::Roles) =>
            {
                let roles = data
                    .as_ref()
                    .and_then(|value| value.get("roles"))
                    .and_then(serde_json::Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let active_incarnation_id = self
                    .sessions
                    .get(&session_id)
                    .and_then(|state| state.active_incarnation_id.clone());
                (
                    format_roles_report(active_incarnation_id.as_deref(), &roles),
                    "role_list_reported",
                    serde_json::json!({
                        "session_id": session_id,
                        "turn_id": command_turn_id,
                        "chat_id": command_chat_id,
                        "role_command": "list_roles",
                        "role_count": roles.len(),
                        "active_incarnation_id": active_incarnation_id,
                    }),
                    None,
                )
            }
            IpcResponse::Error(message) => (
                format!("Couldn't switch roles: {message}"),
                "role_handoff_failed",
                serde_json::json!({
                    "session_id": session_id,
                    "turn_id": command_turn_id,
                    "chat_id": command_chat_id,
                    "error": message,
                }),
                None,
            ),
            IpcResponse::Standard {
                ok: false,
                code,
                message,
                ..
            } => (
                format!("Couldn't switch roles: {message} ({code})"),
                "role_handoff_failed",
                serde_json::json!({
                    "session_id": session_id,
                    "turn_id": command_turn_id,
                    "chat_id": command_chat_id,
                    "error_code": code,
                    "error": message,
                }),
                None,
            ),
            other => (
                format!("Couldn't handle role command: unexpected hotel response {other:?}"),
                "role_handoff_failed",
                serde_json::json!({
                    "session_id": session_id,
                    "turn_id": command_turn_id,
                    "chat_id": command_chat_id,
                    "error": format!("unexpected hotel response: {other:?}"),
                }),
                None,
            ),
        };

        if let Some(active_incarnation_id) = next_active_incarnation {
            if let Some(state) = self.sessions.get_mut(&session_id) {
                state.active_incarnation_id = Some(active_incarnation_id);
            }
        }

        let (checkpoint_memory_type, checkpoint_json, index_state) = {
            let Some(state) = self.sessions.get(&session_id) else {
                warn!("Received role command for unknown session {}", session_id);
                return Ok(());
            };
            (
                state.checkpoint_memory_type(),
                state.checkpoint_json(),
                state.clone(),
            )
        };

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let _ = self
            .ipc_client
            .send_request(IpcRequest::UpdateTask {
                task_id: command_task_id,
                state: update_state.into(),
                payload,
            })
            .await?;

        self.complete_local_command(session_id, command_turn_id, reply_content)
            .await
    }

    async fn resume_turn_with_steering(
        &mut self,
        session_id: String,
        turn_id: String,
        chat_id: String,
        note: String,
        task_state: &str,
        steering_label: &str,
    ) -> Result<()> {
        let (
            task_id,
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
            prompt,
            checkpoint_memory_type,
            checkpoint_json,
            index_state,
            user_content,
        ) = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                warn!(
                    "Tried to resume steered turn for unknown session {}",
                    session_id
                );
                return Ok(());
            };
            let user_content = {
                let Some(active_turn) = state.active_turn.as_mut() else {
                    warn!(
                        "Tried to resume steered turn for session {} with no active turn",
                        session_id
                    );
                    return Ok(());
                };
                active_turn.user_content = format!(
                    "{}\n\n{}\n{}",
                    active_turn.user_content, steering_label, note
                );
                active_turn.iteration += 1;
                active_turn.phase = TurnPhase::WaitingModel;
                active_turn.user_content.clone()
            };
            let prompt = state.build_prompt(&user_content);
            let active_turn = state
                .active_turn
                .as_ref()
                .expect("active turn should exist after steering update");
            (
                active_turn.task_id,
                active_turn.final_reply_to.clone(),
                active_turn.final_reply_role.clone(),
                active_turn.final_reply_guest_id.clone(),
                prompt,
                state.checkpoint_memory_type(),
                state.checkpoint_json(),
                state.clone(),
                user_content,
            )
        };

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let _ = self
            .ipc_client
            .send_request(IpcRequest::UpdateTask {
                task_id,
                state: task_state.into(),
                payload: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": chat_id,
                    "content": user_content,
                }),
            })
            .await?;

        let tools_for_model = self
            .sessions
            .get(&session_id)
            .map(|state| state.tool_assembly.tools_for_model.clone())
            .unwrap_or_default();

        let (context, context_projection) = self
            .sessions
            .get(&session_id)
            .map(|state| {
                let projection = state.build_context_projection(&user_content);
                (
                    Some(state.model_context_from_projection(&projection)),
                    Some(
                        serde_json::to_value(&projection)
                            .expect("context projection should serialize"),
                    ),
                )
            })
            .unwrap_or((None, None));

        let model_req = ModelRequestPayload {
            action: "generate_text".to_string(),
            request_class: Some("cognitive".to_string()),
            session_id: session_id.clone(),
            turn_id,
            prompt,
            user_content,
            context,
            context_projection,
            attachments: Vec::new(),
            tools_for_model,
            response_contract: None,
            chat_id,
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
        };

        let (target_node, target_role, target_guest_id) = resolve_model_execution_target(
            self.sessions.get(&session_id),
            "text.generate",
            DEFAULT_TEXT_MODEL_ROLE,
        );

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node,
                target_role,
                target_guest_id,
                task_json: serde_json::to_string(&model_req)?,
            })
            .await?;
        Ok(())
    }

    /// Handle `/abandon [reason]`.
    ///
    /// If `PHILOTIC_PARENT_GUEST_ID` is set (this process is a subagent worker),
    /// fires a `FireSubagentHook(TurnCompleted, { completed: false })` to the hotel
    /// so the parent agent receives the failure notification.
    /// Always completes the local turn with an abandonment acknowledgement.
    async fn handle_abandon_command(
        &mut self,
        session_id: String,
        turn_id: String,
        reason: Option<String>,
    ) -> Result<()> {
        let reason_text = reason
            .as_deref()
            .unwrap_or("operator requested abandonment");

        // If we're running as a subagent, notify the parent via the hook channel.
        if let Ok(worker_id) = std::env::var("PHILOTIC_AGENT_ID") {
            if std::env::var("PHILOTIC_PARENT_GUEST_ID").is_ok() {
                let hook_result = self
                    .ipc_client
                    .send_request(IpcRequest::FireSubagentHook {
                        subagent_guest_id: worker_id.clone(),
                        hook_kind: philotic_client::HookKind::TurnCompleted,
                        payload: serde_json::json!({
                            "completed": false,
                            "error": reason_text,
                        }),
                    })
                    .await;
                match hook_result {
                    Ok(_) => info!(
                        agent_id = %worker_id,
                        "Abandon: failure hook fired to parent."
                    ),
                    Err(e) => warn!(
                        agent_id = %worker_id,
                        "Abandon: failed to fire failure hook: {}", e
                    ),
                }
            }
        }

        let ack = if let Some(r) = reason.as_deref() {
            format!("Abandoned: {r}")
        } else {
            "Abandoned.".into()
        };
        self.complete_local_command(session_id, turn_id, ack).await
    }

    async fn handle_session_control_command(
        &mut self,
        command_task_id: Uuid,
        session_id: String,
        command_turn_id: String,
        command_chat_id: String,
        command: SlashCommand,
    ) -> Result<()> {
        // Handle /tts early — it mutates voice policy and completes immediately.
        if let SlashCommand::Tts { ref mode } = command {
            let new_mode = match mode.as_deref() {
                Some("on") => TtsMode::On,
                Some("auto") => TtsMode::Auto,
                Some("off") => TtsMode::Off,
                _ => {
                    // cycle: off → auto → on → off
                    let current = self
                        .sessions
                        .get(&session_id)
                        .map(|s| s.agent_profile.voice_response_policy.mode)
                        .unwrap_or(TtsMode::Off);
                    match current {
                        TtsMode::Off => TtsMode::Auto,
                        TtsMode::Auto => TtsMode::On,
                        TtsMode::On => TtsMode::Off,
                    }
                }
            };
            if let Some(state) = self.sessions.get_mut(&session_id) {
                state.agent_profile.voice_response_policy.mode = new_mode;
            }
            let reply = match new_mode {
                TtsMode::Off => {
                    "Voice response off for text turns. Voice notes will still get voice-only replies."
                }
                TtsMode::Auto => "Voice response auto — I'll mirror your input modality.",
                TtsMode::On => "Voice response on. I'll speak all replies.",
            };
            let _ = self
                .ipc_client
                .send_request(IpcRequest::UpdateTask {
                    task_id: command_task_id,
                    state: "session_policy_updated".into(),
                    payload: serde_json::json!({
                        "session_id": session_id,
                        "turn_id": command_turn_id,
                        "chat_id": command_chat_id,
                    }),
                })
                .await?;
            return self
                .complete_local_command(session_id, command_turn_id, reply.into())
                .await;
        }

        let (
            reply_content,
            update_state,
            payload,
            checkpoint_memory_type,
            checkpoint_json,
            index_state,
        ) = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                warn!(
                    "Received session policy command for unknown session {}",
                    session_id
                );
                return Ok(());
            };

            let (reply_content, update_state, payload) = match command {
                SlashCommand::Status => (
                    state.session_status_text(),
                    "session_status_reported",
                    serde_json::json!({
                        "session_id": session_id,
                        "turn_id": command_turn_id,
                        "chat_id": command_chat_id,
                        "session_status": state.status,
                        "bindings": state.bindings,
                        "tool_assembly": state.tool_assembly,
                        "approval_policy": state.approval_policy,
                    }),
                ),
                SlashCommand::Pause => {
                    state.set_status("paused");
                    (
                        "Session paused.".to_string(),
                        "session_status_updated",
                        serde_json::json!({
                            "session_id": session_id,
                            "turn_id": command_turn_id,
                            "chat_id": command_chat_id,
                            "session_status": "paused",
                            "bindings": state.bindings,
                            "tool_assembly": state.tool_assembly,
                            "action": "session_status_update",
                        }),
                    )
                }
                SlashCommand::Resume => {
                    state.set_status("active");
                    (
                        "Session resumed.".to_string(),
                        "session_status_updated",
                        serde_json::json!({
                            "session_id": session_id,
                            "turn_id": command_turn_id,
                            "chat_id": command_chat_id,
                            "session_status": "active",
                            "bindings": state.bindings,
                            "tool_assembly": state.tool_assembly,
                            "action": "session_status_update",
                        }),
                    )
                }
                SlashCommand::ToolsAdd { tool } => {
                    state.add_tool_binding(tool);
                    (
                        format!(
                            "Tool bindings updated: {}.",
                            if state.bindings.effective_toolset.is_empty() {
                                "default".to_string()
                            } else {
                                state.bindings.effective_toolset.join(", ")
                            }
                        ),
                        "session_bindings_updated",
                        serde_json::json!({
                            "session_id": session_id,
                            "turn_id": command_turn_id,
                            "chat_id": command_chat_id,
                            "bindings": state.bindings,
                            "tool_assembly": state.tool_assembly,
                            "action": "session_bindings_update",
                        }),
                    )
                }
                SlashCommand::ToolsClear => {
                    state.clear_tool_bindings();
                    (
                        "Tool bindings reset to default.".to_string(),
                        "session_bindings_updated",
                        serde_json::json!({
                            "session_id": session_id,
                            "turn_id": command_turn_id,
                            "chat_id": command_chat_id,
                            "bindings": state.bindings,
                            "tool_assembly": state.tool_assembly,
                            "action": "session_bindings_update",
                        }),
                    )
                }
                SlashCommand::SkillsAdd { skill } => {
                    state.add_skill_binding(skill);
                    (
                        format!(
                            "Skill bindings updated: {}.",
                            if state.bindings.effective_skillset.is_empty() {
                                "default".to_string()
                            } else {
                                state.bindings.effective_skillset.join(", ")
                            }
                        ),
                        "session_bindings_updated",
                        serde_json::json!({
                            "session_id": session_id,
                            "turn_id": command_turn_id,
                            "chat_id": command_chat_id,
                            "bindings": state.bindings,
                            "tool_assembly": state.tool_assembly,
                            "action": "session_bindings_update",
                        }),
                    )
                }
                SlashCommand::SkillsClear => {
                    state.clear_skill_bindings();
                    (
                        "Skill bindings reset to default.".to_string(),
                        "session_bindings_updated",
                        serde_json::json!({
                            "session_id": session_id,
                            "turn_id": command_turn_id,
                            "chat_id": command_chat_id,
                            "bindings": state.bindings,
                            "tool_assembly": state.tool_assembly,
                            "action": "session_bindings_update",
                        }),
                    )
                }
                SlashCommand::WorkspaceSet { workspace } => {
                    state.set_workspace_binding(workspace);
                    (
                        format!(
                            "Workspace binding updated: {}.",
                            state
                                .bindings
                                .effective_workspace_ref
                                .as_deref()
                                .unwrap_or("default")
                        ),
                        "session_bindings_updated",
                        serde_json::json!({
                            "session_id": session_id,
                            "turn_id": command_turn_id,
                            "chat_id": command_chat_id,
                            "bindings": state.bindings,
                            "tool_assembly": state.tool_assembly,
                            "action": "session_bindings_update",
                        }),
                    )
                }
                SlashCommand::WorkspaceClear => {
                    state.clear_workspace_binding();
                    (
                        "Workspace binding reset to default.".to_string(),
                        "session_bindings_updated",
                        serde_json::json!({
                            "session_id": session_id,
                            "turn_id": command_turn_id,
                            "chat_id": command_chat_id,
                            "bindings": state.bindings,
                            "tool_assembly": state.tool_assembly,
                            "action": "session_bindings_update",
                        }),
                    )
                }
                SlashCommand::PreapproveThisSession => {
                    state.set_preapprove_this_session();
                    (
                        "Approval policy updated: this session is now pre-approved.".to_string(),
                        "session_policy_updated",
                        serde_json::json!({
                            "session_id": session_id,
                            "turn_id": command_turn_id,
                            "chat_id": command_chat_id,
                            "approval_policy": state.approval_policy,
                            "action": "approval_policy_update",
                        }),
                    )
                }
                SlashCommand::Preapprove { name } => {
                    let reply = state.preapprove_by_name(name.as_str());
                    (
                        reply,
                        "session_policy_updated",
                        serde_json::json!({
                            "session_id": session_id,
                            "turn_id": command_turn_id,
                            "chat_id": command_chat_id,
                            "approval_policy": state.approval_policy,
                            "action": "approval_policy_update",
                        }),
                    )
                }
                SlashCommand::ApprovalStatus => (
                    state.approval_policy_status_text(),
                    "session_policy_reported",
                    serde_json::json!({
                        "session_id": session_id,
                        "turn_id": command_turn_id,
                        "chat_id": command_chat_id,
                        "approval_policy": state.approval_policy,
                    }),
                ),
                SlashCommand::ApprovalReset => {
                    state.reset_approval_policy();
                    (
                        "Approval policy reset for this session.".to_string(),
                        "session_policy_updated",
                        serde_json::json!({
                            "session_id": session_id,
                            "turn_id": command_turn_id,
                            "chat_id": command_chat_id,
                            "approval_policy": state.approval_policy,
                            "action": "approval_policy_update",
                        }),
                    )
                }
                SlashCommand::Ping
                | SlashCommand::Tts { .. }
                | SlashCommand::Role { .. }
                | SlashCommand::Roles
                | SlashCommand::Back
                | SlashCommand::Approve { .. }
                | SlashCommand::Deny { .. }
                | SlashCommand::Abandon { .. } => (
                    "Unsupported session control command.".to_string(),
                    "session_control_unsupported",
                    serde_json::json!({
                        "session_id": session_id,
                        "turn_id": command_turn_id,
                        "chat_id": command_chat_id,
                    }),
                ),
            };

            (
                reply_content,
                update_state,
                payload,
                state.checkpoint_memory_type(),
                state.checkpoint_json(),
                state.clone(),
            )
        };

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let _ = self
            .ipc_client
            .send_request(IpcRequest::UpdateTask {
                task_id: command_task_id,
                state: update_state.into(),
                payload,
            })
            .await?;

        self.complete_local_command(session_id, command_turn_id, reply_content)
            .await
    }

    fn execute_bound_tool<'a>(
        state: &'a SessionState,
        tool_call: &ToolCall,
    ) -> Result<&'a ToolExecutionRoute> {
        if !state.tool_is_enabled(&tool_call.tool_name) {
            anyhow::bail!(
                "Tool {} is not enabled for this session",
                tool_call.tool_name
            );
        }
        state
            .resolve_tool_route(&tool_call.tool_name)
            .and_then(|route| {
                if route.execution_mode != "local_agent" && route.availability_state != "live" {
                    None
                } else {
                    Some(route)
                }
            })
            .ok_or_else(|| {
                if let Some(route) = state.resolve_tool_route(&tool_call.tool_name) {
                    anyhow::anyhow!(
                        "Tool {} requires runner materialization (availability: {}, runner: {})",
                        tool_call.tool_name,
                        route.availability_state,
                        route.runner_id.as_deref().unwrap_or("unknown")
                    )
                } else {
                    anyhow::anyhow!(
                        "Tool {} has no assembled execution route",
                        tool_call.tool_name
                    )
                }
            })
    }

    fn normalize_approval_request(mut approval: ApprovalRequest) -> ApprovalRequest {
        if approval
            .approval_id
            .as_deref()
            .unwrap_or_default()
            .is_empty()
        {
            approval.approval_id = Some(Uuid::new_v4().to_string());
        }
        approval
    }

    async fn execute_local_agent_tool(&mut self, payload: ToolExecutionPayload) -> Result<()> {
        match payload.tool_name.as_str() {
            "session.status" => {
                let content = self
                    .sessions
                    .get(&payload.session_id)
                    .map(SessionState::session_status_text)
                    .unwrap_or_else(|| "Session status unavailable.".into());

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    transport: None,
                    chat_id: Some(payload.chat_id),
                    thread_id: None,
                    sender_id: None,
                    sender_username: None,
                    message_kind: None,
                    content: Some(content),
                    attachments: Vec::new(),
                    command: None,
                    callback_data: None,
                    raw_transport_event: None,
                    error: None,
                    tool_name: Some(payload.tool_name),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                })
                .await
            }
            "agent.configure" => {
                let args = &payload.arguments;
                let config_path = match args.get("config_path").and_then(|v| v.as_str()) {
                    Some(p) => p.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "agent.configure: missing required argument 'config_path'".into(),
                            )
                            .await;
                    }
                };
                let value = match args.get("value") {
                    Some(v) => v.clone(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "agent.configure: missing required argument 'value'".into(),
                            )
                            .await;
                    }
                };
                let operation = args
                    .get("operation")
                    .and_then(|v| v.as_str())
                    .unwrap_or("set")
                    .to_string();

                let configure_result = {
                    let Some(state) = self.sessions.get_mut(&payload.session_id) else {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "agent.configure: session not found".into(),
                            )
                            .await;
                    };
                    let bindings_before = state.bindings.clone();
                    match state.apply_configure(&config_path, &value, &operation) {
                        Ok(msg) => {
                            let changed = state.bindings != bindings_before;
                            Ok((msg, changed))
                        }
                        Err(err) => Err(err),
                    }
                };
                let (content, bindings_changed) = match configure_result {
                    Ok(result) => result,
                    Err(err) => {
                        return self
                            .fail_active_turn(payload.session_id, payload.turn_id, err)
                            .await;
                    }
                };

                // Rebuild tool assembly if bindings changed so the new toolset takes effect
                // immediately within the same session.
                if bindings_changed {
                    if let Some(state) = self.sessions.get_mut(&payload.session_id) {
                        state.rebuild_default_tool_assembly();
                    }
                }

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    transport: None,
                    chat_id: Some(payload.chat_id),
                    thread_id: None,
                    sender_id: None,
                    sender_username: None,
                    message_kind: None,
                    content: Some(content),
                    attachments: Vec::new(),
                    command: None,
                    callback_data: None,
                    raw_transport_event: None,
                    error: None,
                    tool_name: Some(payload.tool_name),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                })
                .await
            }
            "role.configure" => {
                let args = &payload.arguments;

                macro_rules! require_str_arg {
                    ($key:literal) => {
                        match args.get($key).and_then(|v| v.as_str()) {
                            Some(s) => s.to_string(),
                            None => {
                                return self
                                    .fail_active_turn(
                                        payload.session_id,
                                        payload.turn_id,
                                        format!(
                                            "role.configure: missing required argument '{}'",
                                            $key
                                        ),
                                    )
                                    .await;
                            }
                        }
                    };
                }

                let role_name = require_str_arg!("role_name");
                let toolset_profile = require_str_arg!("toolset_profile");

                if let None = args.get("reasoning").and_then(|v| v.as_object()) {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "role.configure: missing required object argument 'reasoning'".into(),
                        )
                        .await;
                }

                let role_identity_addendum = args.get("role_identity_addendum").and_then(|v| v.as_str()).map(str::to_string);
                let inactive_ttl_seconds = args.get("inactive_ttl_seconds").and_then(|v| v.as_u64());
                let iteration_cap = args.get("iteration_cap").and_then(|v| v.as_u64()).map(|v| v as u32);
                let approval_policy = args.get("approval_policy").and_then(|v| v.as_str()).map(str::to_string);
                let model_profile = args.get("model_profile").and_then(|v| v.as_str()).map(str::to_string);
                let context_window_policy = args.get("context_window_policy").and_then(|v| v.as_str()).map(str::to_string);

                let req = IpcRequest::ConfigureRole {
                    agent_id: self.agent_id.clone(),
                    role_name: role_name.clone(),
                    guest_id: format!("{}:{}", self.agent_id, role_name),
                    toolset_profile,
                    role_identity_addendum,
                    inactive_ttl_seconds,
                    iteration_cap,
                    approval_policy,
                    model_profile,
                    context_window_policy,
                };

                let (content, tool_err) = match self.ipc_client.send_request(req).await {
                    Ok(IpcResponse::ConfigureRoleOk { role_name: name }) => {
                        (format!("Successfully configured role incarnation for '{}'.", name), None)
                    }
                    Ok(IpcResponse::Standard { ok: false, code, message, .. }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "IPC_ERROR", msg);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "UNEXPECTED_RESPONSE", "role.configure: unexpected hotel response");
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error("philote", format!("role.configure: IPC transport error — {e}"));
                        (err.display_message(), Some(err))
                    }
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    transport: None,
                    chat_id: Some(payload.chat_id),
                    thread_id: None,
                    sender_id: None,
                    sender_username: None,
                    message_kind: None,
                    content: Some(content),
                    attachments: Vec::new(),
                    command: None,
                    callback_data: None,
                    raw_transport_event: None,
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                })
                .await
            }
            "skill.register" => {
                let args = &payload.arguments;

                macro_rules! require_str_arg {
                    ($key:literal) => {
                        match args.get($key).and_then(|v| v.as_str()) {
                            Some(s) => s.to_string(),
                            None => {
                                return self
                                    .fail_active_turn(
                                        payload.session_id,
                                        payload.turn_id,
                                        format!(
                                            "skill.register: missing required argument '{}'",
                                            $key
                                        ),
                                    )
                                    .await;
                            }
                        }
                    };
                }

                let skill_name    = require_str_arg!("skill_name");
                let description   = require_str_arg!("description");
                let subagent_kind = require_str_arg!("subagent_kind");
                let goal          = require_str_arg!("goal");

                let str_vec = |key: &str| -> Vec<String> {
                    args.get(key)
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let allowed_tools   = str_vec("allowed_tools");
                let allowed_classes = str_vec("allowed_classes");

                let response = self
                    .ipc_client
                    .send_request(IpcRequest::RegisterSkill {
                        skill_name:        skill_name.clone(),
                        description,
                        subagent_kind,
                        goal,
                        allowed_tools,
                        allowed_classes,
                        hook_subscriptions: vec![],
                        completion_route:   Default::default(),
                        failure_route:      Default::default(),
                        idle_behavior:      Default::default(),
                        lease_terms:        Default::default(),
                    })
                    .await;

                let (content, tool_err) = match response {
                    Ok(IpcResponse::SkillRegistered {
                        skill_name: name,
                        validation_state,
                        validation_errors,
                    }) => {
                        let msg = if validation_errors.is_empty() {
                            format!("Skill '{}' registered (state: {}).", name, validation_state)
                        } else {
                            format!(
                                "Skill '{}' registered with state '{}'. Validation issues:\n{}",
                                name,
                                validation_state,
                                validation_errors
                                    .iter()
                                    .map(|e| format!("- {e}"))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            )
                        };
                        (msg, None)
                    }
                    Ok(IpcResponse::Standard { ok: false, code, message, .. }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "IPC_ERROR", msg);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "UNEXPECTED_RESPONSE", "skill.register: unexpected hotel response");
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error("philote", format!("skill.register: IPC transport error — {e}"));
                        (err.display_message(), Some(err))
                    }
                };

                self.handle_tool_result(InboundTaskPayload {
                    action:              Some("tool_result".into()),
                    agent_action:        None,
                    source:              Some("agent".into()),
                    session_id:          Some(payload.session_id),
                    turn_id:             Some(payload.turn_id),
                    transport:           None,
                    chat_id:             Some(payload.chat_id),
                    thread_id:           None,
                    sender_id:           None,
                    sender_username:     None,
                    message_kind:        None,
                    content:             Some(content),
                    attachments:         Vec::new(),
                    command:             None,
                    callback_data:       None,
                    raw_transport_event: None,
                    error: tool_err,
                    tool_name:           Some(payload.tool_name),
                    arguments:           None,
                    final_reply_to:      Some(payload.final_reply_to),
                    final_reply_role:    Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                })
                .await
            }

            "subagent.spawn" => {
                let args = &payload.arguments;

                let goal = match args.get("goal").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "subagent.spawn: missing required argument 'goal'".into(),
                            )
                            .await;
                    }
                };
                let subagent_kind = args
                    .get("subagent_kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("philote-worker")
                    .to_string();
                let context_summary = args
                    .get("context_summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let allowed_tools: Vec<String> = args
                    .get("allowed_tools")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let iteration_budget = args
                    .get("iteration_budget")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as u32);

                let delegation = philotic_client::SubagentDelegation {
                    parent_agent_id: self.agent_id.clone(),
                    parent_role:     "agent".to_string(),
                    subagent_kind,
                    goal,
                    context_packet: philotic_client::SubagentContextPacket {
                        summary: context_summary,
                        ..Default::default()
                    },
                    allowed_tools,
                    iteration_budget,
                    ..Default::default()
                };

                let response = self
                    .ipc_client
                    .send_request(IpcRequest::SpawnSubagent {
                        session_id: payload.session_id.clone(),
                        delegation,
                    })
                    .await;

                let (content, tool_err) = match response {
                    Ok(IpcResponse::SpawnSubagentOk {
                        subagent_guest_id,
                        confirmed_lease,
                    }) => {
                        (format!(
                            "Subagent spawned.\nGuest ID: {}\nLease expires at: {} (epoch {})",
                            subagent_guest_id,
                            confirmed_lease.lease_expires_at,
                            confirmed_lease.lease_epoch,
                        ), None)
                    }
                    Ok(IpcResponse::SpawnSubagentProposal {
                        subagent_guest_id,
                        confirmed_lease,
                        delta,
                    }) => {
                        (format!(
                            "Subagent spawned (TTL adjusted: {}s → {}s).\nGuest ID: {}\nLease expires at: {}",
                            delta.requested_ttl,
                            delta.confirmed_ttl,
                            subagent_guest_id,
                            confirmed_lease.lease_expires_at,
                        ), None)
                    }
                    Ok(IpcResponse::Standard { ok: false, code, message, .. }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "IPC_ERROR", msg);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "UNEXPECTED_RESPONSE", "subagent.spawn: unexpected hotel response");
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error("philote", format!("subagent.spawn: IPC transport error — {e}"));
                        (err.display_message(), Some(err))
                    }
                };

                self.handle_tool_result(InboundTaskPayload {
                    action:              Some("tool_result".into()),
                    agent_action:        None,
                    source:              Some("agent".into()),
                    session_id:          Some(payload.session_id),
                    turn_id:             Some(payload.turn_id),
                    transport:           None,
                    chat_id:             Some(payload.chat_id),
                    thread_id:           None,
                    sender_id:           None,
                    sender_username:     None,
                    message_kind:        None,
                    content:             Some(content),
                    attachments:         Vec::new(),
                    command:             None,
                    callback_data:       None,
                    raw_transport_event: None,
                    error: tool_err,
                    tool_name:           Some(payload.tool_name),
                    arguments:           None,
                    final_reply_to:      Some(payload.final_reply_to),
                    final_reply_role:    Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                })
                .await
            }

            "handoff.to_role" => {
                let args = payload.arguments.as_object();
                let role_name = args
                    .and_then(|a| a.get("role_name"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let reason = args
                    .and_then(|a| a.get("reason"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let Some(role_name) = role_name else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "handoff.to_role: missing required argument 'role_name'".into(),
                        )
                        .await;
                };
                let Some(reason) = reason else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "handoff.to_role: missing required argument 'reason'".into(),
                        )
                        .await;
                };

                let active_goal = args
                    .and_then(|a| a.get("active_goal"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let context_summary = args
                    .and_then(|a| a.get("context_summary"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let expected_return_mode = args
                    .and_then(|a| a.get("expected_return_mode"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let cleanup_actions: Vec<String> = args
                    .and_then(|a| a.get("cleanup_actions"))
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();

                let from_role = self
                    .sessions
                    .get(&payload.session_id)
                    .and_then(|s| s.role_activation.as_ref())
                    .map(|r| r.role_name.clone())
                    .or_else(|| Some("orchestrator".into()));

                let handoff_bundle = HandoffBundle {
                    goal: active_goal.clone().unwrap_or_else(|| reason.clone()),
                    context_excerpt: context_summary,
                    session_id: payload.session_id.clone(),
                    initiating_turn_id: payload.turn_id.clone(),
                    handoff_reason: Some(reason),
                    from_role,
                    to_role: Some(role_name.clone()),
                    active_goal,
                    expected_return_mode,
                    cleanup_actions,
                    ..Default::default()
                };

                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::HandoffToRole {
                        session_id: payload.session_id.clone(),
                        role_name: role_name.clone(),
                        handoff_bundle,
                    })
                    .await
                {
                    Ok(IpcResponse::HandoffAck { handoff_guest_id, .. }) => {
                        (format!("Handed off to role '{role_name}' (guest {handoff_guest_id})."), None)
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::tool_execution("handoff.to_role", msg, Some("HANDOFF_REJECTED"));
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "UNEXPECTED_RESPONSE", "handoff.to_role: unexpected hotel response");
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error("philote", format!("handoff.to_role: IPC transport error — {e}"));
                        (err.display_message(), Some(err))
                    }
                };

                self.handle_tool_result(InboundTaskPayload {
                    action:               Some("tool_result".into()),
                    agent_action:         None,
                    source:               Some("agent".into()),
                    session_id:           Some(payload.session_id),
                    turn_id:              Some(payload.turn_id),
                    transport:            None,
                    chat_id:              Some(payload.chat_id),
                    thread_id:            None,
                    sender_id:            None,
                    sender_username:      None,
                    message_kind:         None,
                    content:              Some(content),
                    attachments:          Vec::new(),
                    command:              None,
                    callback_data:        None,
                    raw_transport_event:  None,
                    error:                tool_err,
                    tool_name:            Some(payload.tool_name),
                    arguments:            None,
                    final_reply_to:       Some(payload.final_reply_to),
                    final_reply_role:     Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                })
                .await
            }

            "handoff.back" => {
                let args = payload.arguments.as_object();
                let summary = args
                    .and_then(|a| a.get("summary"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let Some(summary) = summary else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "handoff.back: missing required argument 'summary'".into(),
                        )
                        .await;
                };

                let return_to = args
                    .and_then(|a| a.get("return_to"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::HandoffBack {
                        session_id: payload.session_id.clone(),
                        summary: summary.clone(),
                        return_to,
                    })
                    .await
                {
                    Ok(IpcResponse::HandoffBackAck { handoff_guest_id, .. }) => {
                        (format!("Returned control (from guest {handoff_guest_id}). Summary: {summary}"), None)
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::tool_execution("handoff.back", msg, Some("HANDOFF_BACK_REJECTED"));
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "UNEXPECTED_RESPONSE", "handoff.back: unexpected hotel response");
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error("philote", format!("handoff.back: IPC transport error — {e}"));
                        (err.display_message(), Some(err))
                    }
                };

                self.handle_tool_result(InboundTaskPayload {
                    action:               Some("tool_result".into()),
                    agent_action:         None,
                    source:               Some("agent".into()),
                    session_id:           Some(payload.session_id),
                    turn_id:              Some(payload.turn_id),
                    transport:            None,
                    chat_id:              Some(payload.chat_id),
                    thread_id:            None,
                    sender_id:            None,
                    sender_username:      None,
                    message_kind:         None,
                    content:              Some(content),
                    attachments:          Vec::new(),
                    command:              None,
                    callback_data:        None,
                    raw_transport_event:  None,
                    error:                tool_err,
                    tool_name:            Some(payload.tool_name),
                    arguments:            None,
                    final_reply_to:       Some(payload.final_reply_to),
                    final_reply_role:     Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                })
                .await
            }

            other => {
                self.fail_active_turn(
                    payload.session_id,
                    payload.turn_id,
                    format!("Agent-local tool {} is not implemented", other),
                )
                .await
            }
        }
    }

    async fn complete_local_command(
        &mut self,
        session_id: String,
        turn_id: String,
        reply_content: String,
    ) -> Result<()> {
        let (completed_turn, checkpoint_memory_type, checkpoint_json, index_state) = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                warn!(
                    "Received local command completion for unknown session {}",
                    session_id
                );
                return Ok(());
            };

            let Some(completed_turn) = state.complete_active_turn(reply_content.clone()) else {
                warn!(
                    "Received local command completion for session {} with no active turn",
                    session_id
                );
                return Ok(());
            };

            (
                completed_turn,
                state.checkpoint_memory_type(),
                state.checkpoint_json(),
                state.clone(),
            )
        };

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let _ = self
            .ipc_client
            .send_request(IpcRequest::CompleteTask {
                task_id: completed_turn.task_id,
                result: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": completed_turn.chat_id,
                    "content": reply_content,
                }),
            })
            .await?;

        let reply_payload = FinalReplyPayload {
            action: "send_reply",
            session_id,
            turn_id,
            chat_id: completed_turn.chat_id,
            content: reply_content,
            audio_artifact: None,
            send_text_caption: false,
        };

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: completed_turn.final_reply_to,
                target_role: completed_turn.final_reply_role,
                target_guest_id: completed_turn.final_reply_guest_id,
                task_json: serde_json::to_string(&reply_payload)?,
            })
            .await?;

        Ok(())
    }

    async fn ensure_session_loaded(
        &mut self,
        session_id: &str,
        fallback_source: &str,
    ) -> Result<()> {
        if self.sessions.contains_key(session_id) {
            return Ok(());
        }

        let response = self
            .ipc_client
            .send_request(IpcRequest::GetConfig {
                key: format!("__session_snapshot__:{session_id}"),
            })
            .await?;

        if let IpcResponse::ConfigData {
            value_json: Some(value_json),
            ..
        } = response
        {
            if let Ok(checkpoint) = serde_json::from_str::<serde_json::Value>(&value_json) {
                if checkpoint
                    .get("session_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(session_id)
                {
                    if let Some(state) = SessionState::from_checkpoint(&checkpoint) {
                        self.sessions.insert(session_id.to_string(), state);
                        return Ok(());
                    }
                }
            }
        }

        self.sessions.insert(
            session_id.to_string(),
            SessionState::new(
                session_id.to_string(),
                self.agent_id.clone(),
                fallback_source.into(),
            ),
        );
        Ok(())
    }

    async fn sync_session_index(&mut self, state: &SessionState) -> Result<()> {
        let response = self
            .ipc_client
            .send_request(IpcRequest::GetConfig {
                key: format!("__apartment__:{}:short", self.agent_id),
            })
            .await?;

        let existing_index = match response {
            IpcResponse::ConfigData {
                value_json: Some(value_json),
                ..
            } => serde_json::from_str::<serde_json::Value>(&value_json).ok(),
            _ => None,
        };

        let merged_index = merge_session_index(existing_index.as_ref(), state);
        self.ipc_client
            .sync_apartment(&self.agent_id, "short", merged_index)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_TEXT_MODEL_ROLE, LOCAL_NODE, extract_model_error, format_role_command_reply,
        format_roles_report, media_analysis_attachments, normalized_user_content,
        resolve_media_routing, resolve_model_execution_target,
    };
    use crate::commands::SlashCommand;
    use crate::r#loop::{ApprovalRequest, ToolCall};
    use crate::protocol::{
        FinalReplyPayload, InboundTaskPayload, ModelRequestPayload, TransportAttachment,
    };
    use crate::session::{
        ApprovalPolicy, ComponentExecutionRoute, ComponentRouteAssembly, ComponentRouteBinding,
        SessionState,
    };
    use philotic_client::TaskErrorPayload;
    #[test]
    fn model_request_targets_agent_for_reply() {
        let request = ModelRequestPayload {
            action: "generate_text".to_string(),
            request_class: Some("cognitive".into()),
            session_id: "sess-1".into(),
            turn_id: "turn-1".into(),
            prompt: "hello".into(),
            user_content: "hello".into(),
            context: Some(serde_json::json!({
                "identity": [{"text": "You are Jane."}],
                "active_turn": {"role": "user", "text": "hello"}
            })),
            context_projection: Some(serde_json::json!({
                "conversation_turn": {"conversation_turn_id": "turn-1"}
            })),
            attachments: Vec::new(),
            tools_for_model: Vec::new(),
            response_contract: None,
            chat_id: "123".into(),
            reply_to: LOCAL_NODE.into(),
            reply_role: "agent".into(),
            final_reply_to: LOCAL_NODE.into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: None,
        };

        let json = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(json["reply_role"], "agent");
        assert_eq!(json["final_reply_role"], "membrane");
        assert_eq!(json["request_class"], "cognitive");
        assert_eq!(json["context"]["active_turn"]["text"], "hello");
        assert_eq!(
            json["context_projection"]["conversation_turn"]["conversation_turn_id"],
            "turn-1"
        );
    }

    #[test]
    fn default_text_model_role_targets_gemini_controller() {
        assert_eq!(DEFAULT_TEXT_MODEL_ROLE, "model");
    }

    #[test]
    fn implementation_names_map_to_model_roles() {
        assert_eq!(
            super::implementation_to_model_role("gemini"),
            "model"
        );
        assert_eq!(
            super::implementation_to_model_role("gemini-flash"),
            "model"
        );
        assert_eq!(
            super::implementation_to_model_role("elevenlabs-v1"),
            "model.elevenlabs"
        );
    }

    #[test]
    fn session_component_route_can_override_text_model_implementation() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.bindings.component_routes.push(ComponentRouteBinding {
            capability: "text.generate".into(),
            selection_mode: "preferred".into(),
            implementation: Some("elevenlabs".into()),
            incarnation: None,
            preferred_hotel_id: None,
            preferred_environment_id: None,
        });

        let target_role = state
            .preferred_component_implementation("text.generate")
            .map(super::implementation_to_model_role);

        assert_eq!(target_role.as_deref(), Some("model.elevenlabs"));
    }

    #[test]
    fn resolved_component_route_can_drive_remote_model_target() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.component_route_assembly = ComponentRouteAssembly {
            execution_routes: std::collections::BTreeMap::from([(
                "media.analyze".into(),
                ComponentExecutionRoute {
                    target_node: "aria-node".into(),
                    target_role: "model".into(),
                    incarnation_id: Some("aria-architect-hotel:model-controller-gemini".into()),
                    hotel_id: Some("aria-architect-hotel".into()),
                    environment_id: None,
                    execution_mode: "capability".into(),
                    availability_state: "live".into(),
                    selection_reason: Some("remote_latency_capacity".into()),
                },
            )]),
        };

        let target =
            resolve_model_execution_target(Some(&state), "media.analyze", DEFAULT_TEXT_MODEL_ROLE);
        assert_eq!(target.0, "aria-node");
        assert_eq!(target.1, "model");
        assert_eq!(
            target.2.as_deref(),
            Some("aria-architect-hotel:model-controller-gemini")
        );
    }

    #[test]
    fn final_reply_payload_preserves_session_and_turn() {
        let payload = FinalReplyPayload {
            action: "send_reply",
            session_id: "sess-1".into(),
            turn_id: "turn-1".into(),
            chat_id: "123".into(),
            content: "done".into(),
            audio_artifact: None,
            send_text_caption: false,
        };

        let json = serde_json::to_value(&payload).expect("serialize payload");
        assert_eq!(json["session_id"], "sess-1");
        assert_eq!(json["turn_id"], "turn-1");
    }

    #[test]
    fn extract_model_error_reads_structured_error_envelope() {
        let payload = InboundTaskPayload {
            agent_action: Some(serde_json::json!({
                "kind": "fail",
                "message": "Provider invocation failed: missing voice",
                "model_result": {
                    "error": {
                        "kind": "provider_failure",
                        "provider": "elevenlabs",
                        "message": "voice.synthesize task is missing voice override"
                    }
                }
            })),
            action: None,
            source: None,
            session_id: None,
            turn_id: None,
            transport: None,
            chat_id: None,
            thread_id: None,
            sender_id: None,
            sender_username: None,
            message_kind: None,
            content: None,
            attachments: Vec::new(),
            command: None,
            callback_data: None,
            raw_transport_event: None,
            tool_name: None,
            arguments: None,
            final_reply_to: None,
            final_reply_role: None,
            final_reply_guest_id: None,
            error: Some(TaskErrorPayload {
                kind: "provider_failure".into(),
                message: "Provider invocation failed: missing voice".into(),
                code: Some("ELEVENLABS_MISSING_VOICE".into()),
                component: Some("model-router".into()),
                provider: Some("elevenlabs".into()),
                capability: Some("voice.synthesize".into()),
                retryable: Some(false),
            }),
        };

        let error = extract_model_error(&payload).expect("structured error should be extracted");
        assert!(error.contains("Provider invocation failed"));
        assert!(error.contains("kind=provider_failure"));
        assert!(error.contains("code=ELEVENLABS_MISSING_VOICE"));
        assert!(error.contains("component=model-router"));
        assert!(error.contains("provider=elevenlabs"));
        assert!(error.contains("capability=voice.synthesize"));
    }

    #[test]
    fn bound_tool_execution_allows_listed_tools() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.add_tool_binding("echo");
        let route = super::AgentRuntime::execute_bound_tool(
            &state,
            &ToolCall {
                tool_name: "echo".into(),
                arguments: serde_json::json!({ "text": "hello" }),
            },
        )
        .expect("echo tool should be allowed");
        assert_eq!(route.target_role, "tool.echo");
    }

    #[test]
    fn approval_requests_get_ids_when_missing() {
        let approval = super::AgentRuntime::normalize_approval_request(ApprovalRequest {
            approval_id: None,
            reason: "deploy the thing".into(),
            approved_response: "Approved: deploy the thing".into(),
        });
        assert!(approval.approval_id.is_some());
    }

    #[test]
    fn auto_approval_uses_session_policy() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.approval_policy = ApprovalPolicy {
            auto_approve_all: true,
            preapproved_tools: Vec::new(),
            preapproved_classes: Vec::new(),
        };

        assert!(state.approval_policy_allows(
            &ApprovalRequest {
                approval_id: Some("appr-1".into()),
                reason: "deploy the thing".into(),
                approved_response: "Approved: deploy the thing".into(),
            },
            None
        ));
    }

    #[test]
    fn bound_tool_execution_rejects_unlisted_tools() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.add_tool_binding("echo");

        let err = super::AgentRuntime::execute_bound_tool(
            &state,
            &ToolCall {
                tool_name: "workspace.read".into(),
                arguments: serde_json::json!({}),
            },
        )
        .expect_err("tool should be blocked");
        assert!(err.to_string().contains("not enabled"));
    }

    #[test]
    fn bound_tool_execution_requires_live_route() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.add_tool_binding("echo");
        if let Some(route) = state.tool_assembly.execution_routes.get_mut("echo") {
            route.availability_state = "materialization_required".into();
        }

        let err = super::AgentRuntime::execute_bound_tool(
            &state,
            &ToolCall {
                tool_name: "echo".into(),
                arguments: serde_json::json!({ "text": "hello" }),
            },
        )
        .expect_err("dormant route should not execute");
        assert!(err.to_string().contains("requires runner materialization"));
    }

    #[test]
    fn local_agent_route_executes_without_external_runner_liveness() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.clear_tool_bindings();
        state.add_tool_binding("session.status");

        let route = super::AgentRuntime::execute_bound_tool(
            &state,
            &ToolCall {
                tool_name: "session.status".into(),
                arguments: serde_json::json!({}),
            },
        )
        .expect("local agent tools should not require an external runner");
        assert_eq!(route.execution_mode, "local_agent");
    }

    #[test]
    fn normalized_user_content_prefers_explicit_text() {
        let task = InboundTaskPayload {
            action: None,
            agent_action: None,
            source: Some("telegram".into()),
            session_id: None,
            turn_id: None,
            transport: Some("telegram".into()),
            chat_id: Some("123".into()),
            thread_id: None,
            sender_id: None,
            sender_username: None,
            message_kind: Some("photo".into()),
            content: Some("caption text".into()),
            attachments: vec![TransportAttachment {
                kind: "photo".into(),
                file_id: "photo-1".into(),
                mime_type: None,
                file_name: None,
                file_size: None,
                telegram_file_path: None,
                blob_id: None,
                blob_download_url: None,
                transport_error: None,
            }],
            command: None,
            callback_data: None,
            raw_transport_event: None,
            tool_name: None,
            arguments: None,
            final_reply_to: None,
            final_reply_role: None,
            final_reply_guest_id: None,
            error: None,
        };

        assert_eq!(normalized_user_content(&task), Some("caption text".into()));
    }

    #[test]
    fn normalized_user_content_summarizes_attachment_only_turns() {
        let task = InboundTaskPayload {
            action: None,
            agent_action: None,
            source: Some("telegram".into()),
            session_id: None,
            turn_id: None,
            transport: Some("telegram".into()),
            chat_id: Some("123".into()),
            thread_id: None,
            sender_id: None,
            sender_username: None,
            message_kind: Some("voice".into()),
            content: None,
            attachments: vec![TransportAttachment {
                kind: "voice".into(),
                file_id: "voice-1".into(),
                mime_type: Some("audio/ogg".into()),
                file_name: None,
                file_size: None,
                telegram_file_path: None,
                blob_id: None,
                blob_download_url: None,
                transport_error: None,
            }],
            command: None,
            callback_data: None,
            raw_transport_event: None,
            tool_name: None,
            arguments: None,
            final_reply_to: None,
            final_reply_role: None,
            final_reply_guest_id: None,
            error: None,
        };

        assert_eq!(
            normalized_user_content(&task),
            Some(
                "User sent a voice message with attachments: voice audio/ogg file_id=voice-1."
                    .into()
            )
        );
    }

    #[test]
    fn normalized_user_content_uses_callback_data_when_present() {
        let task = InboundTaskPayload {
            action: None,
            agent_action: None,
            source: Some("telegram".into()),
            session_id: None,
            turn_id: None,
            transport: Some("telegram".into()),
            chat_id: Some("123".into()),
            thread_id: None,
            sender_id: None,
            sender_username: None,
            message_kind: Some("callback".into()),
            content: None,
            attachments: Vec::new(),
            command: None,
            callback_data: Some("approve:turn-1".into()),
            raw_transport_event: None,
            tool_name: None,
            arguments: None,
            final_reply_to: None,
            final_reply_role: None,
            final_reply_guest_id: None,
            error: None,
        };

        assert_eq!(
            normalized_user_content(&task),
            Some("Callback action: approve:turn-1".into())
        );
    }

    #[test]
    fn media_analysis_attachments_only_include_blob_backed_supported_kinds() {
        let task = InboundTaskPayload {
            action: None,
            agent_action: None,
            source: Some("telegram".into()),
            session_id: None,
            turn_id: None,
            transport: Some("telegram".into()),
            chat_id: Some("123".into()),
            thread_id: None,
            sender_id: None,
            sender_username: None,
            message_kind: Some("photo".into()),
            content: Some("what is this?".into()),
            attachments: vec![
                TransportAttachment {
                    kind: "photo".into(),
                    file_id: "photo-1".into(),
                    mime_type: Some("image/jpeg".into()),
                    file_name: None,
                    file_size: None,
                    telegram_file_path: None,
                    blob_id: Some("sha256-1".into()),
                    blob_download_url: Some("http://127.0.0.1:9001/download/sha256-1".into()),
                    transport_error: None,
                },
                TransportAttachment {
                    kind: "sticker".into(),
                    file_id: "sticker-1".into(),
                    mime_type: Some("image/webp".into()),
                    file_name: None,
                    file_size: None,
                    telegram_file_path: None,
                    blob_id: Some("sha256-2".into()),
                    blob_download_url: Some("http://127.0.0.1:9001/download/sha256-2".into()),
                    transport_error: None,
                },
                TransportAttachment {
                    kind: "voice".into(),
                    file_id: "voice-1".into(),
                    mime_type: Some("audio/ogg".into()),
                    file_name: None,
                    file_size: None,
                    telegram_file_path: None,
                    blob_id: Some("sha256-3".into()),
                    blob_download_url: None,
                    transport_error: None,
                },
            ],
            command: None,
            callback_data: None,
            raw_transport_event: None,
            tool_name: None,
            arguments: None,
            final_reply_to: None,
            final_reply_role: None,
            final_reply_guest_id: None,
            error: None,
        };

        let attachments = media_analysis_attachments(&task);
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].kind, "photo");
    }

    fn blob_backed_attachment(kind: &str) -> TransportAttachment {
        TransportAttachment {
            kind: kind.into(),
            file_id: format!("{kind}-1"),
            mime_type: Some(if kind == "voice" || kind == "audio" {
                "audio/ogg".into()
            } else {
                "image/jpeg".into()
            }),
            file_name: None,
            file_size: None,
            telegram_file_path: None,
            blob_id: Some(format!("sha256-{kind}-1")),
            blob_download_url: Some(format!("http://127.0.0.1:9001/download/sha256-{kind}-1")),
            transport_error: None,
        }
    }

    #[test]
    fn default_media_routing_policy_routes_voice_to_analyze_media() {
        use crate::session::MediaRoutingPolicy;

        let policy = MediaRoutingPolicy::default();
        let atts = vec![blob_backed_attachment("voice")];
        let routing = resolve_media_routing(&policy, atts);
        let r = routing.expect("should produce routing for blob-backed voice");
        assert_eq!(r.action, "analyze_media");
        assert_eq!(r.capability, "media.analyze");
        assert!(r.strip_tools);
    }

    #[test]
    fn voice_action_transcribe_routes_to_voice_transcribe_capability() {
        use crate::session::MediaRoutingPolicy;

        let policy = MediaRoutingPolicy {
            voice_action: Some("transcribe".into()),
            ..Default::default()
        };
        let atts = vec![blob_backed_attachment("voice")];
        let routing = resolve_media_routing(&policy, atts);
        let r =
            routing.expect("should produce routing for blob-backed voice with transcribe policy");
        assert_eq!(r.action, "transcribe");
        assert_eq!(r.capability, "voice.transcribe");
    }

    #[test]
    fn image_action_describe_routes_to_image_describe_capability() {
        use crate::session::MediaRoutingPolicy;

        let policy = MediaRoutingPolicy {
            image_action: Some("describe".into()),
            ..Default::default()
        };
        let atts = vec![blob_backed_attachment("photo")];
        let routing = resolve_media_routing(&policy, atts);
        let r = routing.expect("should produce routing for blob-backed photo with describe policy");
        assert_eq!(r.action, "describe");
        assert_eq!(r.capability, "image.describe");
    }

    #[test]
    fn voice_takes_priority_over_image_in_mixed_turn() {
        use crate::session::MediaRoutingPolicy;

        let policy = MediaRoutingPolicy {
            voice_action: Some("transcribe".into()),
            image_action: Some("describe".into()),
            ..Default::default()
        };
        let atts = vec![
            blob_backed_attachment("voice"),
            blob_backed_attachment("photo"),
        ];
        let routing = resolve_media_routing(&policy, atts);
        let r = routing.expect("should produce routing for mixed attachments");
        assert_eq!(r.action, "transcribe");
        assert_eq!(r.capability, "voice.transcribe");
        assert_eq!(r.attachments.len(), 2, "all attachments forwarded");
    }

    #[test]
    fn forward_media_false_returns_none() {
        use crate::session::MediaRoutingPolicy;

        let policy = MediaRoutingPolicy {
            forward_media_to_model: false,
            ..Default::default()
        };
        let atts = vec![blob_backed_attachment("voice")];
        assert!(resolve_media_routing(&policy, atts).is_none());
    }

    #[test]
    fn strip_tools_false_preserved_in_routing() {
        use crate::session::MediaRoutingPolicy;

        let policy = MediaRoutingPolicy {
            strip_tools_on_media: false,
            ..Default::default()
        };
        let atts = vec![blob_backed_attachment("photo")];
        let r = resolve_media_routing(&policy, atts).unwrap();
        assert!(!r.strip_tools);
    }

    #[test]
    fn role_command_reply_text_distinguishes_active_and_materializing() {
        assert_eq!(
            format_role_command_reply(
                &SlashCommand::Role {
                    role_name: "developer".into()
                },
                true
            ),
            "Switched to role developer."
        );
        assert_eq!(
            format_role_command_reply(
                &SlashCommand::Role {
                    role_name: "developer".into()
                },
                false
            ),
            "Switching to role developer once it finishes materializing."
        );
        assert_eq!(
            format_role_command_reply(&SlashCommand::Back, true),
            "Switched back to orchestrator."
        );
        assert_eq!(
            format_role_command_reply(&SlashCommand::Back, false),
            "Switching back to orchestrator once it finishes materializing."
        );
    }

    #[test]
    fn roles_report_marks_active_role() {
        let roles = vec![
            serde_json::json!({
                "role_name": "orchestrator",
                "guest_id": "agent-jane:orchestrator"
            }),
            serde_json::json!({
                "role_name": "developer",
                "guest_id": "agent-jane:developer"
            }),
        ];
        let report = format_roles_report(Some("agent-jane:developer"), &roles);
        assert!(report.contains("Active role: developer."));
        assert!(report.contains("- orchestrator"));
        assert!(report.contains("* developer"));
    }
}
