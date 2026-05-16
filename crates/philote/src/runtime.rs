use crate::commands::{SlashCommand, command_manifest, parse_slash_command};
use crate::r#loop::{
    AgentAction, ApprovalRequest, PlanProposalAction, ToolCall, ToolResult, TurnPhase,
    interpret_model_payload,
};
use crate::protocol::{
    FinalReplyPayload, InboundTaskPayload, LigandEnvelope, ModelRequestPayload,
    PartialReplyPayload, TaskRunnerOverlay, ToolExecutionPayload, TransportAttachment,
    TurnEventPayload,
};
use crate::reflex::{IngressAction, ReflexEvent};
use crate::session::{
    ActivePlan, AgentProfile, ComponentRouteAssembly, MediaRoutingPolicy, RecalledMemoryRecord,
    SessionState, ToolDefinition, ToolExecutionRoute, TtsMode, VoiceResponsePolicy, WorkingTurn,
    merge_session_index,
};
use anyhow::Result;
use memory_core::{
    MemoryScope, MuninnConfig, MuninnRestEngine, RecallContext, RecallTrigger, VaultResolver,
};
use philotic_client::{
    Exosome, HandoffBundle, IpcRequest, IpcResponse, ParacrineRouting, PhiloticClient,
    TaskErrorPayload, is_ipc_disconnect,
};
use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, HashMap};
use std::time::Duration;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

pub const DEFAULT_AGENT_ID: &str = "agent-bjork-01";
const DEFAULT_REPLY_ROLE: &str = "membrane";
const DEFAULT_TEXT_MODEL_ROLE: &str = "model";
const DEFAULT_VOICE_MODEL_ROLE: &str = "model.elevenlabs";

fn local_node_id() -> String {
    std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string())
}

fn debug_model_requests_enabled() -> bool {
    matches!(
        std::env::var("PHILOTIC_DEBUG_MODEL_REQUESTS")
            .ok()
            .as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

#[cfg(test)]
const LOCAL_NODE: &str = "local-aiua-01";

fn extract_model_error_payload(task: &InboundTaskPayload) -> Option<TaskErrorPayload> {
    if let Some(error) = task.error.as_ref() {
        return Some(error.clone());
    }

    task.agent_action
        .as_ref()
        .and_then(|action| action.get("model_result"))
        .and_then(|result| result.get("error"))
        .cloned()
        .and_then(|value| serde_json::from_value::<TaskErrorPayload>(value).ok())
}

fn extract_model_error(task: &InboundTaskPayload) -> Option<String> {
    let payload = extract_model_error_payload(task)?;
    Some(payload.display_message())
}

fn should_attempt_provider_repair(error: &TaskErrorPayload, state: Option<&SessionState>) -> bool {
    error.kind == "provider_failure"
        && error.retryable.unwrap_or(false)
        && error.capability.as_deref() == Some("text.generate")
        && state
            .map(|state| state.provider_repair_attempts() < 1)
            .unwrap_or(false)
}

/// True for malformed tool-call errors that benefit from a corrective prompt injection.
fn is_content_error(error: &TaskErrorPayload) -> bool {
    error.sub_kind.as_deref() == Some("content_error")
        || error.code.as_deref() == Some("MODEL_INVALID_TOOL_CALL")
}

/// True for errors that should escalate to the next provider fallback tier.
fn should_escalate_tier(error: &TaskErrorPayload) -> bool {
    matches!(
        error.sub_kind.as_deref(),
        Some("network_error")
            | Some("streaming_timeout")
            | Some("rate_limit")
            | Some("provider_error")
    ) || (error.kind == "provider_failure"
        && error.retryable.unwrap_or(false)
        && !is_content_error(error))
}

/// Default tier ordering when none is configured in TurnLoopConfig.
const DEFAULT_FALLBACK_TIERS: &[&str] = &["model", "model.local"];

/// Model role for a given tier index. Falls back gracefully when index is out of range.
fn role_for_tier<'a>(configured_tiers: &'a [String], tier: u8) -> &'a str {
    let idx = tier as usize;
    if !configured_tiers.is_empty() {
        configured_tiers
            .get(idx)
            .map(String::as_str)
            .unwrap_or_else(|| {
                configured_tiers
                    .last()
                    .map(String::as_str)
                    .unwrap_or("model.local")
            })
    } else {
        DEFAULT_FALLBACK_TIERS
            .get(idx)
            .copied()
            .unwrap_or("model.local")
    }
}

fn provider_repair_note(error: &TaskErrorPayload) -> String {
    let provider = error.provider.as_deref().unwrap_or("the model");
    format!(
        "The previous {provider} response attempted a tool call but returned an invalid tool payload. \
If you call a tool, output a complete tool_call object with a non-empty arguments object containing every required field. \
If no tool is needed, reply with structured JSON containing display_text, spoken_text, and memory_candidate"
    )
}

#[derive(Debug, Clone)]
struct MemoryCandidate {
    concept: String,
    content: String,
    tags: Vec<String>,
}

fn parse_memory_candidate(value: Option<&Value>) -> Option<MemoryCandidate> {
    let candidate = value?;
    let concept = candidate
        .get("concept")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let content = candidate
        .get("content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_string();
    let tags = candidate
        .get("tags")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Some(MemoryCandidate {
        concept,
        content,
        tags,
    })
}

fn implementation_to_model_role(implementation: &str) -> String {
    let normalized = implementation
        .split(['.', '-', '@', '/'])
        .find(|segment| !segment.is_empty())
        .unwrap_or("gemini");

    if normalized == "elevenlabs" {
        "model.elevenlabs".into()
    } else if matches!(normalized, "onnx" | "kokoro" | "local") {
        "model.local".into()
    } else if normalized == "ollama" {
        "model.ollama".into()
    } else if normalized == "mlx" {
        "model.mlx".into()
    } else {
        "model".into()
    }
}

fn voice_response_provider_options(policy: &VoiceResponsePolicy) -> Map<String, Value> {
    let mut options = Map::new();

    if policy.delivery_mode.is_native_audio() {
        options.insert("response_mode".into(), json!("native_audio"));
    }

    if let Some(voice_id) = policy
        .effective_voice_id()
        .map(str::trim)
        .filter(|voice_id| !voice_id.is_empty())
    {
        options.insert("voice_id".into(), json!(voice_id));
    }

    if let Some(model) = policy
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        options.insert("model".into(), json!(model));
    }

    options
}

fn voice_response_contract(policy: &VoiceResponsePolicy) -> Value {
    if policy.delivery_mode.is_native_audio() {
        json!({
            "channels": ["spoken_text", "memory_candidate", "active_plan", "memory_concept"],
            "modalities": ["text", "audio"]
        })
    } else {
        json!({
            "channels": ["spoken_text", "memory_candidate", "active_plan", "memory_concept"]
        })
    }
}

fn voice_delivery_envelope(
    state: Option<&SessionState>,
    base_contract: Option<Value>,
) -> (Option<Value>, Map<String, Value>) {
    let Some(state) = state else {
        return (base_contract, Map::new());
    };

    let voice_policy = state.agent_profile.voice_response_policy.clone();
    let had_voice_input = state
        .active_turn
        .as_ref()
        .map(|turn| turn.had_voice_input)
        .unwrap_or(false);

    if voice_policy.is_active(had_voice_input) && voice_policy.delivery_mode.is_native_audio() {
        let _ = &base_contract;
        (
            Some(voice_response_contract(&voice_policy)),
            voice_response_provider_options(&voice_policy),
        )
    } else {
        (base_contract, Map::new())
    }
}

fn model_response_route(
    state: Option<&SessionState>,
    response_contract: Option<&Value>,
    provider_options: &Map<String, Value>,
    attachments: &[TransportAttachment],
) -> String {
    if matches!(
        provider_options
            .get("response_mode")
            .and_then(Value::as_str),
        Some("realtime_websocket" | "realtime_ws" | "realtime")
    ) {
        return "realtime_websocket".into();
    }

    if response_contract
        .and_then(Value::as_object)
        .and_then(|object| object.get("modalities"))
        .and_then(Value::as_array)
        .map(|modalities| {
            modalities
                .iter()
                .any(|modality| modality.as_str() == Some("audio"))
        })
        .unwrap_or(false)
    {
        return "audio_multimodal".into();
    }

    if attachments.iter().any(|attachment| {
        attachment
            .mime_type
            .as_deref()
            .map(|mime| mime.starts_with("image/"))
            .unwrap_or(false)
            || attachment.kind.to_ascii_lowercase().contains("image")
    }) {
        return "image_multimodal".into();
    }

    if let Some(route) = state
        .map(|state| state.agent_profile.response_route_policy.default_route)
        .filter(|route| !route.is_auto())
    {
        return route.as_str().into();
    }

    "text_only".into()
}

fn planning_ligand(
    state: Option<&SessionState>,
    user_content: &str,
    tools_for_model: &[ToolDefinition],
) -> Option<LigandEnvelope> {
    let state = state?;
    let plan = state
        .active_turn
        .as_ref()
        .and_then(|turn| turn.active_plan.as_ref());
    let turn_is_planning = plan.map(|plan| plan.status == "planning").unwrap_or(false)
        || looks_like_planning_turn(user_content)
        || state
            .bindings
            .effective_skillset
            .iter()
            .any(|skill| skill == "planning");

    if !turn_is_planning {
        return None;
    }

    let visible_tools = tools_for_model
        .iter()
        .map(|tool| tool.tool_name.clone())
        .collect::<Vec<_>>();
    let visible_tool_classes = tools_for_model
        .iter()
        .filter_map(|tool| tool.class.as_ref().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    Some(LigandEnvelope {
        signal_type: "tool_planning".into(),
        purpose: "synchronous planning and tool-selection advisory".into(),
        preferred_provider: None,
        preferred_model: None,
        visible_tools,
        visible_tool_classes,
        approval_posture: Some(json!({
            "mode": "conservative_planning",
            "preapproved_classes": ["workspace", "utility"]
        })),
        rationale: plan.and_then(|plan| {
            plan.context_1_advisory
                .as_ref()
                .and_then(|advisory| advisory.rationale.clone())
        }),
    })
}

fn looks_like_planning_turn(user_content: &str) -> bool {
    let normalized = user_content.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }

    [
        "plan ",
        "planning",
        "slice",
        "roadmap",
        "strategy",
        "next step",
        "next steps",
        "design",
        "architecture",
        "proposal",
        "help me plan",
        "let's plan",
        "lets plan",
        "map out",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn extract_audio_artifact(model_result: Option<&Value>) -> Option<String> {
    let artifacts = model_result?.get("artifacts")?.as_array()?;
    for artifact in artifacts {
        let kind = artifact.get("kind").and_then(Value::as_str).unwrap_or("");
        let mime_type = artifact
            .get("mime_type")
            .and_then(Value::as_str)
            .unwrap_or("");
        if kind == "audio" || mime_type.starts_with("audio/") {
            return serde_json::to_string(artifact).ok();
        }
    }
    None
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

/// Returns `(action, effective_capability)` for model dispatch.
///
/// When the session has a `target_capability` reflex on the resolved route,
/// that capability drives both the `action` field sent to model-router and the
/// route lookup. Callers that previously hardcoded `"generate_text"` /
/// `"text.generate"` should use this helper instead so that the philote can
/// self-promote a turn to `"response.generate"` (Gemini Live) via its routing
/// reflexes.
fn resolve_dispatch(state: Option<&SessionState>, base_capability: &str) -> (String, String) {
    let effective = state
        .and_then(|s| s.resolve_component_execution_route(base_capability))
        .and_then(|r| r.target_capability.as_deref())
        .unwrap_or(base_capability);

    let action = match effective {
        "response.generate" => "response.generate".to_string(),
        _ => "generate_text".to_string(),
    };
    (action, effective.to_string())
}

/// Returns true if the task is from a Telegram group or supergroup.
/// Used to prefix user content with the sender's name for group attribution.
fn is_group_chat_task(task: &InboundTaskPayload) -> bool {
    match task.chat_type.as_deref() {
        Some("group") | Some("supergroup") => return true,
        Some(_) => return false,
        None => {}
    }
    // Fallback: Telegram group chats have negative chat_ids.
    task.chat_id
        .as_deref()
        .map(|id| id.starts_with('-'))
        .unwrap_or(false)
}

fn normalized_user_content(task: &InboundTaskPayload) -> Option<String> {
    let is_group = is_group_chat_task(task);

    // For group chats, prefix content with the sender's display name so the
    // model knows who is speaking.
    let sender_prefix: Option<String> = if is_group {
        task.sender_first_name
            .as_deref()
            .or(task.sender_username.as_deref())
            .map(|name| format!("[{name}]: "))
    } else {
        None
    };

    if let Some(content) = task
        .content
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(match &sender_prefix {
            Some(prefix) => format!("{prefix}{content}"),
            None => content.to_string(),
        });
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
            let has_url = attachment
                .blob_download_url
                .as_deref()
                .map(|url| !url.is_empty())
                .unwrap_or(false);
            let has_inline = attachment.inline_audio_b64.is_some();
            let no_error = attachment
                .transport_error
                .as_deref()
                .map(|error| error.is_empty())
                .unwrap_or(true);
            let right_kind = matches!(
                attachment.kind.as_str(),
                "photo" | "image" | "voice" | "audio" | "document"
            );
            (has_url || has_inline) && no_error && right_kind
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

fn command_bypasses_turn_start(command: &SlashCommand) -> bool {
    matches!(
        command,
        SlashCommand::Ping | SlashCommand::Status | SlashCommand::Context
    )
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

/// Locally cached role configuration, populated when `role.configure` succeeds.
/// Used to reconstruct `RoleActivation` on inbound handoff without an IPC round-trip.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct CachedRoleConfig {
    toolset_profile: String,
    role_identity_addendum: Option<String>,
    role_manifest: Option<String>,
    iteration_cap: Option<u32>,
    approval_policy: Option<String>,
    turn_loop_config: ansible_mesh_core::graph::TurnLoopConfig,
}

pub struct AgentRuntime {
    ipc_client: PhiloticClient,
    agent_id: String,
    sessions: HashMap<String, SessionState>,
    /// MuninnDB config fetched from hotel at startup. None = NullMemoryEngine.
    muninn_config: Option<MuninnConfig>,
    /// Role configurations registered via `role.configure`, keyed by role_name.
    configured_roles: HashMap<String, CachedRoleConfig>,
    /// Agent profile (identity_text, soul_text, etc.) fetched from hotel at startup.
    /// Applied to every new session so the correct persona is used from the first turn.
    default_agent_profile: AgentProfile,
    /// Tasks dequeued from a session's pending_user_tasks after a turn completed.
    /// Dispatched at the top of the main event loop to avoid async recursion.
    pending_drains: std::collections::VecDeque<(Uuid, InboundTaskPayload)>,
    /// Tracks when a stuck-waiting turn was first observed per session.
    /// Reconciled on every watchdog tick — entries are added on first observation
    /// and removed when the session is no longer in a waiting phase.
    stuck_turn_first_seen: HashMap<String, std::time::Instant>,
    /// Hotel-wide network reachability flag. Set true when the hotel broadcasts
    /// NetworkState { online: false }. When true, text.generate is routed directly
    /// to the local model tier without attempting cloud providers.
    network_offline: bool,
}

impl AgentRuntime {
    pub fn new(ipc_client: PhiloticClient, agent_id: impl Into<String>) -> Self {
        Self {
            ipc_client,
            agent_id: agent_id.into(),
            sessions: HashMap::new(),
            muninn_config: None,
            configured_roles: HashMap::new(),
            default_agent_profile: AgentProfile::default(),
            pending_drains: std::collections::VecDeque::new(),
            stuck_turn_first_seen: HashMap::new(),
            network_offline: false,
        }
    }

    /// Fetch this agent's identity bundle from the hotel and store it as the default profile.
    /// Applied to every new session so the correct persona is used from the first message.
    async fn fetch_agent_profile(&mut self) {
        let key = format!("__agent_bundle__:{}", self.agent_id);
        match self
            .ipc_client
            .send_request(IpcRequest::GetConfig { key })
            .await
        {
            Ok(IpcResponse::ConfigData {
                value_json: Some(json),
                ..
            }) => match serde_json::from_str::<AgentProfile>(&json) {
                Ok(mut profile) => {
                    info!(agent_id = %self.agent_id, "Agent profile loaded from hotel.");
                    profile.voice_response_policy.seed_voice_ids();
                    self.default_agent_profile = profile;
                }
                Err(e) => warn!("Failed to parse agent profile bundle: {}", e),
            },
            Ok(IpcResponse::ConfigData {
                value_json: None, ..
            }) => {
                info!(agent_id = %self.agent_id, "No agent identity bundle found in hotel — using default profile.");
            }
            Ok(_) | Err(_) => {
                warn!("Unexpected response to agent bundle fetch — using default profile.");
            }
        }

        // Fetch hotel-level user profile and inject into agent profile when the
        // agent-specific profile doesn't already override the field.
        if let Some(hotel_name) = local_hotel_name() {
            match self
                .ipc_client
                .send_request(IpcRequest::GetUserProfile {
                    hotel_name: hotel_name.clone(),
                })
                .await
            {
                Ok(IpcResponse::UserProfileData(p)) => {
                    if self.default_agent_profile.user_timezone.is_none() {
                        if let Some(tz) = p.timezone {
                            info!(hotel = %hotel_name, tz = %tz, "Injecting user timezone from hotel user profile.");
                            self.default_agent_profile.user_timezone = Some(tz);
                        }
                    }
                }
                Ok(_) | Err(_) => {
                    // Non-fatal — hotel may not have a user profile configured yet.
                }
            }
        }

        // Apply operator-persisted policy overrides from hotel config keys.
        // These take precedence over the bundle so /voice and agent.configure persist
        // correctly across restarts without requiring a bundle rebuild.
        if let Ok(IpcResponse::ConfigData {
            value_json: Some(ref json),
            ..
        }) = self
            .ipc_client
            .send_request(IpcRequest::GetConfig {
                key: "config:voice_response_policy".into(),
            })
            .await
        {
            if let Ok(policy) = serde_json::from_str::<VoiceResponsePolicy>(json) {
                self.default_agent_profile.voice_response_policy = policy;
            }
        }
        if let Ok(IpcResponse::ConfigData {
            value_json: Some(ref json),
            ..
        }) = self
            .ipc_client
            .send_request(IpcRequest::GetConfig {
                key: "config:media_routing_policy".into(),
            })
            .await
        {
            if let Ok(policy) = serde_json::from_str::<MediaRoutingPolicy>(json) {
                self.default_agent_profile.media_routing_policy = policy;
            }
        }
    }

    /// Fetch a role incarnation from the hotel and return a `RoleActivation` for it.
    /// Used by `ensure_session_loaded` to auto-activate the agent's default role.
    async fn fetch_role_activation(
        &mut self,
        role_name: &str,
    ) -> Option<crate::session::RoleActivation> {
        match self
            .ipc_client
            .send_request(IpcRequest::ListRoleIncarnations {
                agent_id: self.agent_id.clone(),
            })
            .await
        {
            Ok(IpcResponse::Standard {
                ok: true,
                data: Some(data),
                ..
            }) => {
                let roles = data.get("roles").and_then(|v| v.as_array())?;
                let rec = roles
                    .iter()
                    .find(|r| r.get("role_name").and_then(|n| n.as_str()) == Some(role_name))?;

                let toolset_profile = rec
                    .get("toolset_profile")
                    .and_then(|v| v.as_str())
                    .unwrap_or("orchestrator")
                    .to_string();
                let role_manifest = rec
                    .get("role_manifest")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let role_addendum = rec
                    .get("role_identity_addendum")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let turn_loop_config = rec.get("turn_loop_config").and_then(|v| {
                    serde_json::from_value::<ansible_mesh_core::graph::TurnLoopConfig>(v.clone())
                        .ok()
                });

                Some(crate::session::RoleActivation {
                    role_name: role_name.to_string(),
                    active_incarnation_id: None,
                    activation_reason: "default_role_auto_activation".into(),
                    requested_by: None,
                    role_addendum,
                    role_manifest,
                    base_identity_ref: None,
                    activation_requester_class: Some("default_role".into()),
                    activation_policy_owner: None,
                    toolset_profile_ref: Some(toolset_profile),
                    skillset_profile_ref: None,
                    effective_skillset: vec![],
                    effective_skill_guidance: vec![],
                    working_memory_policy: None,
                    memory_projection_policy: None,
                    turn_loop_config,
                })
            }
            _ => {
                warn!(
                    agent_id = %self.agent_id,
                    role = %role_name,
                    "Failed to fetch role incarnation for default activation."
                );
                None
            }
        }
    }

    /// Fetch all role incarnation names for this agent from the hotel and store them
    /// on the default agent profile. Called once at startup so every session gets
    /// the authoritative list injected into its system prompt.
    async fn fetch_role_names(&mut self) {
        match self
            .ipc_client
            .send_request(IpcRequest::ListRoleIncarnations {
                agent_id: self.agent_id.clone(),
            })
            .await
        {
            Ok(IpcResponse::Standard {
                ok: true,
                data: Some(data),
                ..
            }) => {
                if let Some(roles) = data.get("roles").and_then(|v| v.as_array()) {
                    let names: Vec<String> = roles
                        .iter()
                        .filter_map(|r| {
                            r.get("role_name")
                                .and_then(|n| n.as_str())
                                .map(str::to_string)
                        })
                        .collect();
                    info!(
                        agent_id = %self.agent_id,
                        count = names.len(),
                        roles = %names.join(", "),
                        "Delegation roster loaded from agent graph."
                    );
                    self.default_agent_profile.agent_role_names = names;
                }
            }
            _ => {
                info!(agent_id = %self.agent_id, "No role incarnations found for delegation roster.");
            }
        }
    }

    /// Fetch MuninnDB config from hotel IPC and store it for session use.
    async fn fetch_memory_config(&mut self) {
        info!("Requesting MuninnDB config from hotel...");
        match self
            .ipc_client
            .send_request(IpcRequest::FetchMemoryConfig)
            .await
        {
            Ok(IpcResponse::MemoryConfig {
                config_json: Some(json),
            }) => match serde_json::from_str::<MuninnConfig>(&json) {
                Ok(cfg) => {
                    info!(endpoint = %cfg.base_url, vaults = cfg.vault_tokens.len(), "MuninnDB config loaded");
                    self.muninn_config = Some(cfg);
                }
                Err(e) => warn!("Failed to parse MuninnConfig from hotel: {}", e),
            },
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
            MuninnRestEngine::new(
                cfg,
                VaultResolver {
                    agent_id: agent_id.to_string(),
                    user_id: user_id.to_string(),
                },
            )
        })
    }

    /// Build `McpRouteRecord`s from the agent's effective toolset.
    ///
    /// Operator-stored overrides (key `__mcp_routes__:<agent_id>`) take precedence.
    /// When no overrides are stored, every tool in `default_toolset` that has a real
    /// catalog entry is projected as a self-targeting `Philote` route. Tools with no
    /// catalog entry are skipped — they are model-internal only.
    async fn mcp_routes_from_profile(
        &mut self,
    ) -> Vec<ansible_mesh_core::mcp_route::McpRouteRecord> {
        use ansible_mesh_core::mcp_route::{
            McpAuthScheme, McpRouteRecord, McpRouteSecurity, McpRouteTarget,
        };

        // Operator override takes precedence.
        let key = format!("__mcp_routes__:{}", self.agent_id);
        if let Ok(IpcResponse::ConfigData {
            value_json: Some(json),
            ..
        }) = self
            .ipc_client
            .send_request(IpcRequest::GetConfig { key: key.clone() })
            .await
        {
            match serde_json::from_str::<Vec<McpRouteRecord>>(&json) {
                Ok(r) if !r.is_empty() => {
                    info!(agent_id = %self.agent_id, count = r.len(), "Using operator-stored MCP route overrides.");
                    return r;
                }
                Ok(_) => {}
                Err(e) => {
                    warn!(key = %key, err = %e, "Ignoring malformed operator MCP route override.")
                }
            }
        }

        // Derive from default_toolset — catalog entries only.
        let catalog = crate::catalog::tool_catalog();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        self.default_agent_profile
            .default_toolset
            .iter()
            .filter_map(|tool_name| {
                let def = catalog.get(tool_name.as_str())?;
                Some(McpRouteRecord {
                    agent_id: self.agent_id.clone(),
                    tool_name: tool_name.clone(),
                    description: def.description.clone(),
                    input_schema: def.input_schema.clone(),
                    target: McpRouteTarget::Philote {
                        agent_id: self.agent_id.clone(),
                    },
                    security: McpRouteSecurity {
                        auth: McpAuthScheme::None,
                        global_allotment: None,
                        require_approval: crate::catalog::tool_requires_approval(tool_name),
                    },
                    updated_at: now,
                })
            })
            .collect()
    }

    /// At startup, derive `McpRouteRecord`s from the agent profile and push them to
    /// the hotel so the `membrane-mcp` guest advertises this philote's tools
    /// immediately after restart. Operator-stored overrides take precedence over
    /// the profile-derived set.
    async fn register_mcp_routes(&mut self) {
        let routes = self.mcp_routes_from_profile().await;
        if routes.is_empty() {
            info!(agent_id = %self.agent_id, "No MCP routes to register (empty default_toolset or no catalog matches).");
            return;
        }
        let count = routes.len();
        match self
            .ipc_client
            .send_request(IpcRequest::UpdateMcpRoutes {
                agent_id: self.agent_id.clone(),
                routes,
                vault_ref: None,
            })
            .await
        {
            Ok(_) => info!(agent_id = %self.agent_id, count, "MCP routes registered with hotel."),
            Err(e) => warn!(agent_id = %self.agent_id, err = %e, "Failed to register MCP routes"),
        }
    }

    /// At startup, enumerate all session apartments for this agent and purge any
    /// stale active turns left over from an unclean shutdown. Cleans the DB so
    /// sessions are not blocked before the first inbound message arrives.
    async fn sweep_stale_session_turns(&mut self) {
        let list_key = format!("__session_apartments__:{}", self.agent_id);
        let memory_types: Vec<String> = match self
            .ipc_client
            .send_request(IpcRequest::GetConfig { key: list_key })
            .await
        {
            Ok(IpcResponse::ConfigData {
                value_json: Some(json),
                ..
            }) => serde_json::from_str::<Vec<String>>(&json).unwrap_or_default(),
            _ => {
                return;
            }
        };

        if memory_types.is_empty() {
            return;
        }

        info!(
            agent_id = %self.agent_id,
            sessions = memory_types.len(),
            "Startup stale-turn sweep: checking {} session apartment(s)",
            memory_types.len()
        );

        for memory_type in &memory_types {
            // Derive session_id from memory_type: "short_session:{session_id}"
            let Some(session_id) = memory_type.strip_prefix("short_session:") else {
                continue;
            };
            let snapshot_key = format!("__session_snapshot__:{session_id}");
            let checkpoint = match self
                .ipc_client
                .send_request(IpcRequest::GetConfig { key: snapshot_key })
                .await
            {
                Ok(IpcResponse::ConfigData {
                    value_json: Some(json),
                    ..
                }) => match serde_json::from_str::<serde_json::Value>(&json) {
                    Ok(v) => v,
                    Err(_) => continue,
                },
                _ => continue,
            };

            let had_active_turn = checkpoint
                .get("active_turn")
                .map(|v| !v.is_null())
                .unwrap_or(false);
            if !had_active_turn {
                continue;
            }

            let Some(mut state) = SessionState::from_checkpoint(&checkpoint) else {
                continue;
            };

            if state.active_turn.is_some() {
                // Phase is resumable — leave it alone.
                continue;
            }

            let stale_phase = checkpoint
                .get("active_turn")
                .and_then(|t| t.get("phase"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            warn!(
                session_id = %session_id,
                stale_phase = %stale_phase,
                "Startup sweep: dropping stale active turn — persisting clean checkpoint"
            );
            let clean_checkpoint = state.checkpoint_json();
            if let Err(e) = self
                .ipc_client
                .sync_apartment(&self.agent_id, memory_type, clean_checkpoint)
                .await
            {
                warn!(
                    "Startup sweep: failed to persist clean checkpoint for session {}: {}",
                    session_id, e
                );
            }
        }
    }

    /// Scan all active sessions and evict any turn stuck in a waiting phase past its
    /// deadline. Deadlines:
    ///   WaitingModel  — 120 s  (accounts for slow/retriable model calls)
    ///   WaitingTool   — 90 s   (tool runners should be fast)
    ///   WaitingVoice  — 60 s   (ElevenLabs is normally < 10 s)
    ///
    /// Uses `stuck_turn_first_seen` to track when a waiting-phase turn was first
    /// observed. This map is reconciled each tick — entries are added on first
    /// observation and cleared when the session leaves the waiting phase or has no
    /// active turn. This approach works regardless of which code path set the phase.
    ///
    /// On eviction: clear the active turn, persist a clean checkpoint, and send the
    /// user a brief notice so they know the session is unblocked.
    async fn evict_timed_out_turns(&mut self) {
        const WAITING_MODEL_SECS: u64 = 120;
        const WAITING_TOOL_SECS: u64 = 90;
        const WAITING_VOICE_SECS: u64 = 60;
        const WAITING_APPROVAL_SECS: u64 = 300; // 5 min — operator may be slow

        let now = std::time::Instant::now();

        // Step 1: reconcile stuck_turn_first_seen against current session state.
        // Add sessions newly in a waiting phase; remove those that are no longer waiting.
        // Parked approval turns count as waiting (they live in parked_approval_turn, not active_turn).
        let session_ids: Vec<String> = self.sessions.keys().cloned().collect();
        for session_id in &session_ids {
            let is_waiting = self
                .sessions
                .get(session_id)
                .map(|s| {
                    let active_waiting = s
                        .active_turn
                        .as_ref()
                        .map(|t| {
                            matches!(
                                t.phase,
                                TurnPhase::WaitingModel
                                    | TurnPhase::WaitingTool
                                    | TurnPhase::WaitingVoice
                            )
                        })
                        .unwrap_or(false);
                    active_waiting || s.has_parked_approval_turn()
                })
                .unwrap_or(false);

            if is_waiting {
                self.stuck_turn_first_seen
                    .entry(session_id.clone())
                    .or_insert(now);
            } else {
                self.stuck_turn_first_seen.remove(session_id);
            }
        }
        // Also remove entries for sessions that no longer exist.
        self.stuck_turn_first_seen
            .retain(|id, _| self.sessions.contains_key(id));

        // Step 2: collect sessions whose waiting turn has exceeded the deadline.
        // Parked approval turns (in parked_approval_turn) use WAITING_APPROVAL_SECS.
        let timed_out: Vec<(String, String, String, Option<String>, String, String, u64)> = self
            .sessions
            .iter()
            .filter_map(|(session_id, state)| {
                let first_seen = *self.stuck_turn_first_seen.get(session_id)?;
                let elapsed = first_seen.elapsed().as_secs();
                // Parked approval turn: check it first, since active_turn is None when parked.
                if let Some(turn) = state.parked_approval_turn.as_ref() {
                    if elapsed >= WAITING_APPROVAL_SECS {
                        return Some((
                            session_id.clone(),
                            turn.final_reply_to.clone(),
                            turn.final_reply_role.clone(),
                            turn.final_reply_guest_id.clone(),
                            turn.chat_id.clone(),
                            "WaitingApproval(parked)".into(),
                            elapsed,
                        ));
                    }
                    return None;
                }
                // Parked plan turn: same timeout budget as approval — operator may be slow.
                if let Some(turn) = state.parked_plan_turn.as_ref() {
                    if elapsed >= WAITING_APPROVAL_SECS {
                        return Some((
                            session_id.clone(),
                            turn.final_reply_to.clone(),
                            turn.final_reply_role.clone(),
                            turn.final_reply_guest_id.clone(),
                            turn.chat_id.clone(),
                            "PlanningDiscussion(parked)".into(),
                            elapsed,
                        ));
                    }
                    return None;
                }
                let turn = state.active_turn.as_ref()?;
                let limit = match turn.phase {
                    TurnPhase::WaitingModel => WAITING_MODEL_SECS,
                    TurnPhase::WaitingTool => WAITING_TOOL_SECS,
                    TurnPhase::WaitingVoice => WAITING_VOICE_SECS,
                    _ => return None,
                };
                if elapsed < limit {
                    return None;
                }
                Some((
                    session_id.clone(),
                    turn.final_reply_to.clone(),
                    turn.final_reply_role.clone(),
                    turn.final_reply_guest_id.clone(),
                    turn.chat_id.clone(),
                    format!("{:?}", turn.phase),
                    elapsed,
                ))
            })
            .collect();

        // Step 3: evict.
        for (session_id, reply_to, reply_role, reply_guest_id, chat_id, phase, elapsed_secs) in
            timed_out
        {
            warn!(
                session_id = %session_id,
                phase = %phase,
                elapsed_secs = %elapsed_secs,
                "Turn watchdog: evicting stuck turn"
            );

            self.stuck_turn_first_seen.remove(&session_id);

            if let Some(state) = self.sessions.get_mut(&session_id) {
                state.active_turn = None;
                state.parked_approval_turn = None;
                state.parked_approval_since = None;
                state.turn_waiting_since = None;

                // Persist clean checkpoint so a restart also starts unblocked.
                let mem_type = state.checkpoint_memory_type();
                let clean_checkpoint = state.checkpoint_json();
                if let Err(e) = self
                    .ipc_client
                    .sync_apartment(&self.agent_id, &mem_type, clean_checkpoint)
                    .await
                {
                    warn!("Turn watchdog: failed to persist clean checkpoint: {}", e);
                }
            }

            // Notify the user that the session is unblocked.
            let notify_req = IpcRequest::EmitTask {
                target_node: reply_to,
                target_role: reply_role,
                target_guest_id: reply_guest_id,
                task_json: serde_json::json!({
                    "action": "send_reply",
                    "session_id": session_id,
                    "chat_id": chat_id,
                    "content": "*(I seem to have gotten stuck waiting for a response. The session is unblocked — please try again.)*",
                    "final": true,
                })
                .to_string(),
            };
            if let Err(e) = self.ipc_client.send_request(notify_req).await {
                warn!("Turn watchdog: failed to send unblock notification: {}", e);
            }
        }

        // Step 4: evict stale queued tasks from all sessions.
        const QUEUE_STALE_SECS: u64 = 120;
        let session_ids_for_stale: Vec<String> = self.sessions.keys().cloned().collect();
        for session_id in session_ids_for_stale {
            if let Some(state) = self.sessions.get_mut(&session_id) {
                let dropped = state.evict_stale_queued_tasks(QUEUE_STALE_SECS);
                if dropped > 0 {
                    warn!(
                        session_id = %session_id,
                        dropped = dropped,
                        "Watchdog: evicted stale queued tasks older than {}s",
                        QUEUE_STALE_SECS
                    );
                }
            }
        }
    }

    /// Test-only: inspect session state by id.
    #[doc(hidden)]
    pub fn session(&self, session_id: &str) -> Option<&crate::session::SessionState> {
        self.sessions.get(session_id)
    }

    /// Test-only: mutate session state by id.
    #[doc(hidden)]
    pub fn session_mut_for_test(
        &mut self,
        session_id: &str,
    ) -> Option<&mut crate::session::SessionState> {
        self.sessions.get_mut(session_id)
    }

    pub async fn run(&mut self) -> Result<()> {
        info!("Listening for inbound Persona tasks from the Philotic Web...");
        self.fetch_agent_profile().await;
        self.fetch_role_names().await;
        self.fetch_memory_config().await;

        // Publish command manifest to the hotel so membrane can discover it.
        let manifest = command_manifest(&[]);
        if let Ok(content_json) = serde_json::to_value(&manifest) {
            match self
                .ipc_client
                .send_request(IpcRequest::SyncApartment {
                    agent_id: self.agent_id.clone(),
                    memory_type: "command_manifest".into(),
                    content_json,
                })
                .await
            {
                Ok(_) => info!("Command manifest published ({} entries).", manifest.len()),
                Err(e) => warn!("Failed to publish command manifest: {}", e),
            }
        }

        // Sweep all session apartments for stale active turns left over from a prior
        // crash or unclean shutdown. This runs once at startup so callers don't have
        // to wait for the next inbound message to get a clean checkpoint.
        self.sweep_stale_session_turns().await;

        // Re-advertise this philote's MCP tool routes to the hotel so that the
        // membrane-mcp guest picks them up immediately on restart.
        self.register_mcp_routes().await;

        loop {
            // Dispatch any tasks that were dequeued from a session's pending_user_tasks
            // after the previous turn completed. Processed before blocking on IPC so that
            // back-to-back voice memos are handled in order without additional round-trips.
            while let Some((drain_task_id, drain_task)) = self.pending_drains.pop_front() {
                if let Err(e) = self.handle_user_message(drain_task, drain_task_id).await {
                    warn!("Failed to dispatch drained queued task: {}", e);
                }
            }

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

                    if let Ok(peek) = serde_json::from_str::<serde_json::Value>(&task_json) {
                        let peeked_action = peek
                            .get("action")
                            .and_then(|v| v.as_str())
                            .unwrap_or("<none>");
                        let snippet: String = task_json.chars().take(300).collect();
                        info!(
                            task_id = %task_id,
                            action = %peeked_action,
                            payload_bytes = %task_json.len(),
                            snippet = %snippet,
                            "Agent dispatch: action peek"
                        );
                    }

                    match serde_json::from_str::<InboundTaskPayload>(&task_json) {
                        Ok(task) if task.is_model_response() => {
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_model_response(task).await {
                                error!("Failed to handle model response: {}", err);
                                let _ = self.emit_error_reply(&task_ref, task_id, err).await;
                            }
                        }
                        Ok(task) if task.is_tool_result() => {
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_tool_result(task).await {
                                error!("Failed to handle tool result: {}", err);
                                let _ = self.emit_error_reply(&task_ref, task_id, err).await;
                            }
                        }
                        Ok(task) if task.action.as_deref() == Some("handoff_bundle") => {
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_handoff_bundle(task, task_id).await {
                                error!("Failed to handle handoff_bundle: {}", err);
                                let _ = self.emit_error_reply(&task_ref, task_id, err).await;
                            }
                        }
                        Ok(task) if task.action.as_deref() == Some("handoff_return") => {
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_handoff_return(task, task_id).await {
                                error!("Failed to handle handoff_return: {}", err);
                                let _ = self.emit_error_reply(&task_ref, task_id, err).await;
                            }
                        }
                        Ok(task) if task.action.as_deref() == Some("peer.delegate") => {
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_user_message(task, task_id).await {
                                error!("Failed to handle peer delegation as user message: {}", err);
                                let _ = self.emit_error_reply(&task_ref, task_id, err).await;
                            }
                        }
                        Ok(task) if task.action.as_deref() == Some("voice.dialogue") => {
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_voice_dialogue(task, task_id).await {
                                error!("Failed to handle voice.dialogue: {}", err);
                            }
                        }
                        Ok(task) if task.action.as_deref() == Some("paracrine_request") => {
                            // Paracrine sub-call — this philote is the specialist receiving
                            // an Exosome from a peer or Orchestrator. final_reply_to/role
                            // are set by the ParacrineEmit caller to route the response.
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_user_message(task, task_id).await {
                                error!("Failed to handle paracrine_request: {}", err);
                                let _ = self.emit_error_reply(&task_ref, task_id, err).await;
                            }
                        }
                        Ok(task) if task.action.as_deref() == Some("paracrine_response") => {
                            // Exosome response arriving back from a prior paracrine
                            // dispatch. Route through the lookaside reflex — not the
                            // main user-message path.
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_paracrine_response(task, task_id).await {
                                error!("Failed to handle paracrine_response: {}", err);
                                let _ = self.emit_error_reply(&task_ref, task_id, err).await;
                            }
                        }
                        Ok(task) if task.action.as_deref() == Some("streaming_token") => {
                            // LLM token fragment emitted by model-router during a streaming
                            // response. Forward immediately to membrane for progressive display.
                            if let Err(err) = self.handle_streaming_token(task).await {
                                warn!("Failed to forward streaming_token: {}", err);
                            }
                        }
                        Ok(task) if task.action.as_deref() == Some("datasource_response") => {
                            if let Err(err) = self.handle_datasource_response(task).await {
                                warn!("Failed to handle datasource_response: {}", err);
                            }
                        }
                        Ok(task) if task.command.as_deref() == Some("context.capture") => {
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_context_capture(task, task_id).await {
                                error!("Failed to handle context.capture: {}", err);
                                let _ = self.emit_error_reply(&task_ref, task_id, err).await;
                            }
                        }
                        Ok(task) => {
                            let task_ref = task.clone();
                            if let Err(err) = self.handle_user_message(task, task_id).await {
                                error!("Failed to handle user message: {}", err);
                                let _ = self.emit_error_reply(&task_ref, task_id, err).await;
                            }
                        }
                        Err(err) => warn!("Could not parse inbound task payload: {}", err),
                    }
                }
                Ok(Ok(IpcResponse::NetworkState { online })) => {
                    self.network_offline = !online;
                    if !online {
                        warn!("Network offline — routing text.generate to local fallback tier");
                    } else {
                        info!("Network restored — cloud model tiers re-enabled");
                    }
                }
                Ok(Ok(IpcResponse::GracefulShutdown { drain_timeout_secs })) => {
                    info!(
                        "Hotel requested graceful shutdown (drain window: {}s). \
                         Finishing in-flight work then exiting.",
                        drain_timeout_secs
                    );
                    // Wait up to drain_timeout_secs for any active turns to complete,
                    // then exit. We poll once per second.
                    let deadline = std::time::Instant::now()
                        + std::time::Duration::from_secs(drain_timeout_secs);
                    loop {
                        let has_active = self.sessions.values().any(|s| s.active_turn.is_some());
                        if !has_active || std::time::Instant::now() >= deadline {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }
                    info!("Graceful shutdown drain complete; philote exiting.");
                    return Ok(());
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
                Err(_) => {
                    // 5-second tick — no task arrived. Check for stuck turns.
                    self.evict_timed_out_turns().await;
                }
            }
        }
    }

    /// Lookaside routing reflex — dispatches an incoming `paracrine_response`
    /// based on the [`ParacrineRouting`] hint carried in the exosome.
    ///
    /// This is a separate path from [`handle_user_message`]: the main cognitive
    /// loop is not re-entered unless the routing explicitly calls for it. The
    /// `paracrine_id` threads through every branch for cross-mesh provenance.
    async fn handle_paracrine_response(
        &mut self,
        task: InboundTaskPayload,
        task_id: Uuid,
    ) -> Result<()> {
        // Extract the exosome envelope from the task payload so we can read the
        // paracrine_id and response_routing hint set at dispatch time.
        let exosome: Option<Exosome> = task
            .exosome
            .as_ref()
            .and_then(|v| serde_json::from_value::<Exosome>(v.clone()).ok());

        let paracrine_id = exosome.as_ref().and_then(|e| e.paracrine_id.clone());

        let routing = exosome
            .as_ref()
            .and_then(|e| e.response_routing.clone())
            .unwrap_or(ParacrineRouting::CognitiveReEntry);

        // Locate the owning session by matching paracrine_id against turn logs,
        // falling back to the task's session_id field.
        let session_id = task
            .session_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .or_else(|| {
                if let Some(pid) = &paracrine_id {
                    self.sessions.iter().find_map(|(sid, state)| {
                        state
                            .active_turn
                            .as_ref()
                            .filter(|t| t.associated_paracrine_ids.contains(pid))
                            .map(|_| sid.as_str())
                    })
                } else {
                    None
                }
            })
            .map(str::to_string);

        match routing {
            ParacrineRouting::RawForward => {
                // Emit content directly to membrane — no model loop.
                let content = task.content.clone().unwrap_or_default();
                let node_id = local_node_id();
                let _ = self
                    .ipc_client
                    .send_request(IpcRequest::EmitTask {
                        target_node: node_id,
                        target_role: "membrane".into(),
                        target_guest_id: None,
                        task_json: serde_json::json!({
                            "action": "send_message",
                            "content": content,
                            "paracrine_id": paracrine_id,
                        })
                        .to_string(),
                    })
                    .await;
            }

            ParacrineRouting::ProgressUpdate => {
                // Emit a partial/ephemeral update to membrane without closing
                // or interrupting the active turn.
                let content = task.content.clone().unwrap_or_default();
                if let Some(sid) = &session_id {
                    let _ = self.emit_partial_reply(sid, content).await;
                }
            }

            ParacrineRouting::Heartbeat => {
                // No model involvement — just log and acknowledge.
                info!(
                    paracrine_id = paracrine_id.as_deref().unwrap_or("?"),
                    "paracrine heartbeat received"
                );
            }

            ParacrineRouting::MemoryEnrichment => {
                // Push specialist content into the session memory window.
                // Falls through to CognitiveReEntry if no session found.
                if session_id.is_none() {
                    warn!(
                        paracrine_id = paracrine_id.as_deref().unwrap_or("?"),
                        "MemoryEnrichment: no session found, dropping"
                    );
                    return Ok(());
                }
                // Memory injection handled by model re-entry with enriched context.
                self.handle_user_message(task, task_id).await?;
            }

            ParacrineRouting::DatasourceInjection => {
                // Structured retrieval — inject into session context and re-enter
                // the model so it can reason over the data.
                if session_id.is_none() {
                    warn!(
                        paracrine_id = paracrine_id.as_deref().unwrap_or("?"),
                        "DatasourceInjection: no session found, dropping"
                    );
                    return Ok(());
                }
                self.handle_user_message(task, task_id).await?;
            }

            ParacrineRouting::EnrichedToolResult => {
                // Replace the "paracrine dispatched" placeholder with the real
                // specialist response and re-enter the model as if the tool call
                // completed normally.
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    tool_name: Some("delegate.whisper".into()),
                    content: task.content.clone(),
                    session_id: session_id.clone().or(task.session_id.clone()),
                    turn_id: task.turn_id.clone(),
                    chat_id: task.chat_id.clone(),
                    source: Some("paracrine".into()),
                    final_reply_to: task.final_reply_to.clone(),
                    final_reply_role: task.final_reply_role.clone(),
                    ..task
                })
                .await?;
            }

            ParacrineRouting::CognitiveReEntry => {
                // Standard path: feed into cognitive re-entry.
                // If there is an active turn, the re-entry will merge this
                // response into its context. If not, a new synthesis turn begins.
                self.handle_user_message(task, task_id).await?;
            }

            ParacrineRouting::PriorityReEntry => {
                // Arbiter-promoted: prepend to the session queue so this task is
                // processed NEXT, ahead of any already-waiting messages.
                let session_id = task.session_id_or_default(&self.agent_id);
                if let Some(state) = self.sessions.get_mut(&session_id) {
                    if state.is_turn_active() {
                        info!(
                            session_id = %session_id,
                            "PriorityReEntry: prepending arbiter-promoted task to front of queue"
                        );
                        state.prepend_user_task(task_id, task);
                    } else {
                        // No active turn — dispatch immediately.
                        self.handle_user_message(task, task_id).await?;
                    }
                }
            }

            ParacrineRouting::ApprovalResolution => {
                // The operator role (e.g. membrane + human) has sent an approval decision
                // for a parked turn. Extract `decision` and optional `note` from the content
                // field (parsed as JSON), then synthesize a SlashCommand to reuse the existing
                // approval resolution path.
                let session_id =
                    session_id.unwrap_or_else(|| task.session_id_or_default(&self.agent_id));
                // The sender encodes the decision as JSON in `content`, e.g.:
                //   {"decision": "approved", "note": "looks good"}
                let parsed_content = task
                    .content
                    .as_deref()
                    .and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok());
                let decision = parsed_content
                    .as_ref()
                    .and_then(|p| p.get("decision"))
                    .and_then(|d| d.as_str())
                    .unwrap_or("approved");
                let note = parsed_content
                    .as_ref()
                    .and_then(|p| p.get("note"))
                    .and_then(|n| n.as_str())
                    .map(str::to_string);
                let command = if decision == "denied" {
                    SlashCommand::Deny { note }
                } else {
                    SlashCommand::Approve { note }
                };
                // Use inbound routing fields for the command reply; handle_approval_command
                // extracts the real turn values from the restored parked turn.
                let local_node = local_node_id();
                let cmd_chat_id = task.chat_id.clone().unwrap_or_default();
                let cmd_reply_to = task.final_reply_to.clone().unwrap_or(local_node);
                let cmd_reply_role = task
                    .final_reply_role
                    .clone()
                    .unwrap_or_else(|| "membrane".into());
                let cmd_reply_guest_id = task.final_reply_guest_id.clone();
                info!(
                    session_id = %session_id,
                    decision = %decision,
                    "paracrine ApprovalResolution: applying operator decision to parked turn"
                );
                self.handle_approval_command(
                    task_id,
                    session_id,
                    task.turn_id.clone().unwrap_or_default(),
                    cmd_chat_id,
                    cmd_reply_to,
                    cmd_reply_role,
                    cmd_reply_guest_id,
                    command,
                )
                .await?;
            }
        }

        Ok(())
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
        self.refresh_bindings_from_snapshot(&session_id).await;

        // Fire-and-forget: fetch the agent's personal knowledge graph once per session load.
        let should_preload = self
            .sessions
            .get_mut(&session_id)
            .map(|s| {
                if !s.graph_preload_dispatched {
                    s.graph_preload_dispatched = true;
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false);

        if should_preload {
            let node_id = local_node_id();
            let agent_id = self.agent_id.clone();
            let _ = self
                .ipc_client
                .send_request(IpcRequest::EmitTask {
                    target_node: node_id.clone(),
                    target_role: "graph-datasource".into(),
                    target_guest_id: None,
                    task_json: serde_json::json!({
                        "action": "graph.query",
                        "graph_id": agent_id,
                        "query": "MATCH (n) RETURN n",
                        "reply_to": node_id,
                        "reply_role": "agent",
                        "session_id": session_id,
                        "turn_id": "",
                        "chat_id": "",
                    })
                    .to_string(),
                })
                .await;
        }

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
                _ if command_bypasses_turn_start(&command) => {
                    return match command {
                        SlashCommand::Ping => {
                            self.complete_command_without_turn(
                                task_id,
                                session_id,
                                turn_id,
                                chat_id,
                                final_reply_to,
                                final_reply_role,
                                final_reply_guest_id,
                                "pong".into(),
                                None,
                                None,
                            )
                            .await
                        }
                        SlashCommand::Status | SlashCommand::Context => {
                            self.handle_read_only_session_command(
                                task_id,
                                session_id,
                                turn_id,
                                chat_id,
                                final_reply_to,
                                final_reply_role,
                                final_reply_guest_id,
                                command,
                            )
                            .await
                        }
                        _ => unreachable!("command_bypasses_turn_start gate should be exhaustive"),
                    };
                }
                SlashCommand::Ping | SlashCommand::Status | SlashCommand::Context => {
                    unreachable!("read-only commands should bypass turn start")
                }
                SlashCommand::Pause | SlashCommand::Resume => {}
                SlashCommand::Role { .. } | SlashCommand::Roles | SlashCommand::Back => {}
                SlashCommand::ToolsAdd { .. }
                | SlashCommand::ToolsClear
                | SlashCommand::SkillsAdd { .. }
                | SlashCommand::SkillsClear
                | SlashCommand::WorkspaceSet { .. }
                | SlashCommand::WorkspaceClear => {}
                SlashCommand::Approve { .. } | SlashCommand::Deny { .. } => {
                    // "Trust for session" button sends callback_data="trust" which membrane
                    // translates to /approve + preserves the original callback_data.
                    // Pre-approve the session before resolving the parked turn, and
                    // immediately checkpoint so the policy survives process restarts
                    // and the next refresh_bindings_from_snapshot call.
                    if task.callback_data.as_deref() == Some("trust") {
                        let checkpoint_info = self.sessions.get_mut(&session_id).map(|state| {
                            state.set_preapprove_this_session();
                            (state.checkpoint_memory_type(), state.checkpoint_json())
                        });
                        if let Some((mem_type, checkpoint)) = checkpoint_info {
                            let _ = self
                                .ipc_client
                                .sync_apartment(&self.agent_id, &mem_type, checkpoint)
                                .await;
                        }
                    }
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
                SlashCommand::ApprovalClear { .. } => {
                    return self
                        .handle_approval_clear(
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
                SlashCommand::Voice { .. } => {}
                SlashCommand::Abandon { .. } => {}
                SlashCommand::Correct {
                    turn_id: voice_turn_id,
                    text,
                } => {
                    return self
                        .handle_correction_command(
                            task_id,
                            session_id,
                            turn_id,
                            chat_id,
                            final_reply_to,
                            final_reply_role,
                            final_reply_guest_id,
                            voice_turn_id,
                            text,
                        )
                        .await;
                }
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

        {
            let state = self
                .sessions
                .get_mut(&session_id)
                .expect("session should exist after ensuring and binding transport target");

            // Plan-gate resume: if there's a parked plan turn and no active turn,
            // treat this user message as operator confirmation of the plan rather
            // than starting a fresh turn. Any non-/deny, non-/cancel message confirms.
            let is_slash_deny_or_cancel = parse_slash_command(&content)
                .map(|cmd| {
                    matches!(
                        cmd,
                        SlashCommand::Deny { .. } | SlashCommand::Abandon { .. }
                    )
                })
                .unwrap_or(false);
            if state.has_parked_plan_turn() && !state.is_turn_active() && !is_slash_deny_or_cancel {
                let operator_note = {
                    let lower = content.trim().to_lowercase();
                    if lower == "yes" || lower == "proceed" || lower == "go" || lower == "ok" {
                        None
                    } else {
                        Some(content.clone())
                    }
                };
                state.restore_parked_plan_turn(operator_note);
                state.set_active_turn_phase(TurnPhase::WaitingModel);
                info!(
                    "Session [{}] plan gate: operator confirmed plan, re-entering model.",
                    session_id
                );
                // Re-enter via build_reentry_context_envelope and return — skip new turn creation.
                let reentry = state.build_reentry_context_envelope();
                let (
                    prompt,
                    context,
                    context_projection,
                    tools,
                    restored_task_id,
                    restored_user_content,
                    restored_chat_id,
                    restored_reply_to,
                    restored_reply_role,
                    restored_reply_guest_id,
                    checkpoint_memory_type,
                    checkpoint_json,
                    index_state,
                ) = match reentry {
                    Some((p, c, cp, t)) => {
                        let turn = state.active_turn.as_ref().expect("just restored");
                        let out = (
                            p,
                            c,
                            cp,
                            t,
                            turn.task_id,
                            turn.user_content.clone(),
                            turn.chat_id.clone(),
                            turn.final_reply_to.clone(),
                            turn.final_reply_role.clone(),
                            turn.final_reply_guest_id.clone(),
                            state.checkpoint_memory_type(),
                            state.checkpoint_json(),
                            state.clone(),
                        );
                        out
                    }
                    None => {
                        warn!(
                            "Session [{}] plan resume: no reentry envelope; aborting.",
                            session_id
                        );
                        drop(state);
                        return Ok(());
                    }
                };
                drop(state);
                self.ipc_client
                    .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
                    .await?;
                self.sync_session_index(&index_state).await?;
                let response_contract = Some(serde_json::json!({
                    "channels": ["spoken_text", "memory_candidate", "active_plan"]
                }));
                let response_route = Some(model_response_route(
                    self.sessions.get(&session_id),
                    response_contract.as_ref(),
                    &serde_json::Map::new(),
                    &Vec::new(),
                ));
                let ligand = planning_ligand(self.sessions.get(&session_id), &prompt, &tools);
                let model_req = ModelRequestPayload {
                    action: "generate_text".to_string(),
                    request_class: Some("cognitive".to_string()),
                    session_id: session_id.clone(),
                    turn_id: restored_task_id.to_string(),
                    prompt,
                    user_content: restored_user_content,
                    context: Some(context),
                    context_projection: Some(context_projection),
                    attachments: Vec::new(),
                    tools_for_model: tools,
                    response_contract,
                    response_route,
                    ligand,
                    provider_options: serde_json::Map::new(),
                    chat_id: restored_chat_id,
                    reply_to: local_node_id(),
                    reply_role: "agent".into(),
                    final_reply_to: restored_reply_to,
                    final_reply_role: restored_reply_role,
                    final_reply_guest_id: restored_reply_guest_id,
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
                return Ok(());
            } else if state.has_parked_plan_turn()
                && !state.is_turn_active()
                && is_slash_deny_or_cancel
            {
                // Operator cancelled the plan — fail the parked turn.
                let parked_turn = state.parked_plan_turn.take();
                state.parked_plan_since = None;
                if let Some(parked) = parked_turn {
                    state.active_turn = Some(parked);
                }
                // Let fail_active_turn handle the rest.
                drop(state);
                return self
                    .fail_active_turn(
                        session_id,
                        turn_id.clone(),
                        "Plan cancelled by operator.".into(),
                    )
                    .await;
            }

            // If a turn is already active, queue this task for dispatch after the turn
            // completes rather than overwriting the active turn. Paracrine requests are
            // never queued — they must always start their own specialist turn.
            let is_paracrine = task.action.as_deref() == Some("paracrine_request");
            if !is_paracrine && state.is_turn_active() {
                let queue_len = state.pending_user_task_count();
                let is_voice = task
                    .message_kind
                    .as_deref()
                    .map(|k| matches!(k, "voice" | "audio"))
                    .unwrap_or(false)
                    || task
                        .attachments
                        .iter()
                        .any(|a| matches!(a.kind.as_str(), "voice" | "audio"));

                // Safety valve: route TEXT tasks through the queue arbiter if one is
                // configured. The arbiter evaluates priority and may call delegate.merge
                // with PriorityReEntry to jump to the front of the queue.
                // Voice tasks bypass the arbiter — they are always queued raw.
                let arbiter_role = state.queue_arbiter_role.clone();
                if !is_voice {
                    if let Some(ref role) = arbiter_role {
                        let task_content = task.content.clone().unwrap_or_default();
                        let task_chat_id = task.chat_id.clone().unwrap_or_default();
                        let task_session_id = session_id.clone();
                        info!(
                            session_id = %session_id,
                            arbiter = %role,
                            queue_depth = queue_len,
                            "Routing queued TEXT task through queue arbiter for priority evaluation"
                        );
                        // Still enqueue the task normally so it's processed even if the arbiter
                        // doesn't promote it. The arbiter's PriorityReEntry prepends on top if needed.
                        state.enqueue_user_task(task_id, task);
                        let arbiter_prompt = format!(
                            "Incoming message queued behind {} waiting task(s): \"{}\". \
                             Evaluate urgency and intent. If this requires immediate attention, \
                             call delegate.merge with the message content to promote it to the front \
                             of the queue. Otherwise do nothing — it will be processed in order.",
                            queue_len, task_content
                        );
                        let exosome = philotic_client::Exosome {
                            prompt: arbiter_prompt,
                            context: None,
                            paracrine_id: None,
                            response_routing: Some(
                                philotic_client::ParacrineRouting::PriorityReEntry,
                            ),
                            source_session_id: Some(task_session_id),
                            source_chat_id: Some(task_chat_id),
                        };
                        let _ = self
                            .ipc_client
                            .send_request(IpcRequest::ParacrineEmit {
                                role: role.clone(),
                                exosome,
                                reply_to_node: local_node_id(),
                                reply_to_role: "orchestrator".into(),
                                timeout_secs: None,
                            })
                            .await;
                        return Ok(());
                    }
                }

                // Queue depth cap: reject when 3 tasks are already waiting.
                // Emit a persona-agnostic notice; drop the task entirely so stale
                // work cannot pile up behind a slow active turn.
                const QUEUE_DEPTH_CAP: usize = 3;
                if queue_len >= QUEUE_DEPTH_CAP {
                    warn!(
                        session_id = %session_id,
                        queue_depth = queue_len,
                        "Queue at capacity — rejecting inbound task with busy notice"
                    );
                    let busy_reply_to = final_reply_to.clone();
                    let busy_reply_role = final_reply_role.clone();
                    let busy_reply_guest_id = final_reply_guest_id.clone();
                    let busy_chat_id = chat_id.clone();
                    let busy_session_id = session_id.clone();
                    drop(state);
                    let _ = self
                        .ipc_client
                        .send_request(IpcRequest::EmitTask {
                            target_node: busy_reply_to,
                            target_role: busy_reply_role,
                            target_guest_id: busy_reply_guest_id,
                            task_json: serde_json::json!({
                                "action": "send_reply",
                                "session_id": busy_session_id,
                                "chat_id": busy_chat_id,
                                "content": "*(I'm a bit backed up right now — please try again in a moment.)*",
                                "final": true,
                            })
                            .to_string(),
                        })
                        .await;
                    return Ok(());
                }

                info!(
                    session_id = %session_id,
                    queue_depth = queue_len + 1,
                    "Turn already active — queuing inbound task (session context preserved on payload)"
                );
                state.enqueue_user_task(task_id, task);
                return Ok(());
            }

            // If this turn was triggered by a paracrine_request, capture the
            // originating paracrine_id so deliver_text_reply can echo it back
            // as a `paracrine_response` rather than a `send_reply`.
            // Also capture source_session_id / source_chat_id from the exosome so the
            // response is routed back to the originating conversation channel, not the
            // specialist's own ephemeral session.
            let (paracrine_origin, paracrine_reply_session_id, paracrine_reply_chat_id) = {
                let exosome = task
                    .exosome
                    .as_ref()
                    .and_then(|v| serde_json::from_value::<Exosome>(v.clone()).ok());
                (
                    exosome.as_ref().and_then(|e| e.paracrine_id.clone()),
                    exosome.as_ref().and_then(|e| e.source_session_id.clone()),
                    exosome.as_ref().and_then(|e| e.source_chat_id.clone()),
                )
            };

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
                recalled_memories: Vec::new(),
                active_plan: None,
                consecutive_step_failures: 0,
                provider_repair_note: None,
                provider_repair_attempts: 0,
                pending_text_reply: None,
                had_voice_input,
                awaiting_transcription_reentry: false,
                scripted_loop_context: None,
                associated_paracrine_ids: Vec::new(),
                paracrine_origin,
                paracrine_reply_session_id,
                paracrine_reply_chat_id,
                paracrine_merge_completed: false,
                plan_confirmed: false,
                plan_confirm_note: None,
                fallback_tier: if self.network_offline { 1 } else { 0 },
                streaming_retry_attempts: 0,
            });
            state.set_active_turn_phase(TurnPhase::LoadingContext);

            // Activate scripted loop if the current role has a loop_script configured.
            if let Some(loop_script) = state
                .role_activation
                .as_ref()
                .and_then(|ra| ra.turn_loop_config.as_ref())
                .and_then(|tlc| tlc.loop_script.clone())
            {
                if let Some(turn) = state.active_turn.as_mut() {
                    tracing::debug!(
                        session_id = %state.session_id,
                        variant = %loop_script.variant,
                        "scripted turn loop activated"
                    );
                    turn.scripted_loop_context =
                        Some(crate::scripted_loop::ScriptedLoopExecutor::new(loop_script));
                }
            }
        }

        // MCP approval gate: if the inbound task requires operator approval
        // before model invocation, park the turn and notify the sender.
        // The membrane keeps the HTTP connection open; the Text reply fires
        // the oneshot when the operator resolves via paracrine pipe.
        if task.requires_approval {
            let (checkpoint_memory_type, checkpoint_json, index_state) = {
                let state = self
                    .sessions
                    .get_mut(&session_id)
                    .ok_or_else(|| anyhow::anyhow!("session missing after start_turn"))?;

                // Build a synthetic approval so handle_approval_command can resolve it.
                let tool_name = task.command.as_deref().unwrap_or("unknown tool");
                let approval = ApprovalRequest {
                    approval_id: Some(format!("mcp-gate:{}", turn_id)),
                    reason: format!("MCP tool call '{}' requires operator approval.", tool_name),
                    approved_response: format!("Executing '{}'.", tool_name),
                };
                state.set_pending_approval(approval);
                state.set_active_turn_phase(TurnPhase::WaitingApproval);
                state.park_active_turn_for_approval();
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

            // Tell the membrane to keep the HTTP response parked.
            let notify = serde_json::json!({
                "action":      "approval_required",
                "session_id":  session_id,
                "turn_id":     turn_id,
                "description": "This tool call requires operator approval before execution.",
                "options":     ["approve", "deny"],
            });
            let _ = self
                .ipc_client
                .send_request(IpcRequest::EmitTask {
                    target_node: final_reply_to.clone(),
                    target_role: final_reply_role.clone(),
                    target_guest_id: final_reply_guest_id.clone(),
                    task_json: serde_json::to_string(&notify)?,
                })
                .await;

            return Ok(());
        }

        self.maybe_auto_recall_turn_memory(&session_id).await?;

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
                | SlashCommand::Context
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
                // ApprovalClear always returns from the pre-turn gate above; unreachable here.
                SlashCommand::ApprovalClear { .. } => Ok(()),
                // Correct always returns from the pre-turn gate above; unreachable here.
                SlashCommand::Correct { .. } => Ok(()),
                SlashCommand::Tts { .. } | SlashCommand::Voice { .. } => {
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

        let (action, prompt, attachments, tools_for_model, capability): (
            String,
            String,
            Vec<TransportAttachment>,
            Vec<ToolDefinition>,
            String,
        ) = if let Some(routing) = media_routing {
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
                    routing.capability.to_string(),
                )
            } else {
                let (dispatch_action, dispatch_cap) =
                    resolve_dispatch(self.sessions.get(&session_id), "text.generate");
                (
                    dispatch_action,
                    model_prompt,
                    Vec::new(),
                    tools_for_model,
                    dispatch_cap,
                )
            };
        let (response_contract, provider_options) = voice_delivery_envelope(
            self.sessions.get(&session_id),
            Some(serde_json::json!({
                "channels": ["spoken_text", "memory_candidate", "active_plan", "memory_concept"]
            })),
        );
        let (target_node, target_role, target_guest_id) = {
            let (node, role, guest_id) = resolve_model_execution_target(
                self.sessions.get(&session_id),
                &capability,
                DEFAULT_TEXT_MODEL_ROLE,
            );
            // Network-offline fast-path: skip cloud tiers entirely and go straight
            // to the local model. Uses the last entry in the configured fallback
            // tiers, falling back to DEFAULT_FALLBACK_TIERS.
            if self.network_offline && matches!(capability.as_str(), "text.generate" | "response.generate") {
                let offline_role_name: Option<String> = self
                    .sessions
                    .get(&session_id)
                    .and_then(|s| s.role_activation.as_ref())
                    .map(|ra| ra.role_name.clone());
                let offline_tiers: Vec<String> = offline_role_name
                    .as_deref()
                    .and_then(|rn| self.configured_roles.get(rn))
                    .map(|c| c.turn_loop_config.fallback_tiers.clone())
                    .unwrap_or_default();
                let local_role = if !offline_tiers.is_empty() {
                    offline_tiers
                        .last()
                        .map(String::as_str)
                        .unwrap_or("model.local")
                        .to_string()
                } else {
                    DEFAULT_FALLBACK_TIERS
                        .last()
                        .copied()
                        .unwrap_or("model.local")
                        .to_string()
                };
                warn!(
                    session_id = %session_id,
                    local_role = %local_role,
                    "Network offline — routing text.generate directly to local model"
                );
                (node, local_role, guest_id)
            } else {
                (node, role, guest_id)
            }
        };

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

        let response_route = Some(model_response_route(
            self.sessions.get(&session_id),
            response_contract.as_ref(),
            &provider_options,
            &attachments,
        ));
        let ligand = planning_ligand(self.sessions.get(&session_id), &content, &tools_for_model);
        let model_req = ModelRequestPayload {
            action,
            request_class: Some(
                if matches!(capability.as_str(), "text.generate" | "response.generate") {
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
            response_contract,
            response_route,
            ligand,
            provider_options,
            chat_id,
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
        };

        if debug_model_requests_enabled() && matches!(capability.as_str(), "text.generate" | "response.generate") {
            match serde_json::to_string_pretty(&model_req) {
                Ok(json) => info!(
                    "PHILOTIC_DEBUG_MODEL_REQUESTS philote outbound model request session={} turn={}:\n{}",
                    session_id, model_req.turn_id, json
                ),
                Err(err) => warn!(
                    "PHILOTIC_DEBUG_MODEL_REQUESTS could not serialize outbound model request: {}",
                    err
                ),
            }
        }

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

        // Guard: if the active turn's turn_id doesn't match the incoming response, drop it.
        // This prevents stale model or synthesis responses from corrupting a newer active turn
        // (e.g., two overlapping voice memos where VM1's synthesis arrives after VM2's turn started).
        let active_turn_id = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .map(|t| t.turn_id.clone());

        match active_turn_id {
            None => {
                // No active turn — the turn may have already completed (e.g. a duplicate
                // response from a second controller on the same role inbox arriving after
                // the first one already resolved the turn). Drop silently.
                return Ok(());
            }
            Some(ref active_id) if active_id != &turn_id => {
                warn!(
                    "handle_model_response: dropping stale response for turn {} (active turn is {})",
                    turn_id, active_id
                );
                return Ok(());
            }
            Some(_) => {}
        }

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
                // Fire TTS failure reflex — ensures fallback_to_text is on and
                // any future turns know synthesis is unreliable.
                if let Some(state) = self.sessions.get_mut(&session_id) {
                    state.fire_reflex_event(ReflexEvent::TtsFailure {
                        provider: state
                            .agent_profile
                            .voice_response_policy
                            .provider
                            .clone()
                            .unwrap_or_else(|| "unknown".into()),
                    });
                }

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

        if let Some(error_payload) = extract_model_error_payload(&task) {
            let sub_kind = error_payload.sub_kind.as_deref().unwrap_or("unknown");
            // Content errors (malformed tool call): repair with prompt injection, once.
            if is_content_error(&error_payload)
                && should_attempt_provider_repair(&error_payload, self.sessions.get(&session_id))
            {
                warn!(
                    session_id = %session_id,
                    sub_kind = %sub_kind,
                    "Retrying model turn with corrective note after content error"
                );
                return self
                    .retry_active_turn_after_provider_failure(
                        session_id,
                        turn_id,
                        Some(provider_repair_note(&error_payload)),
                    )
                    .await;
            }
            // Network / timeout / rate-limit errors: escalate to next fallback tier.
            if should_escalate_tier(&error_payload) {
                // For streaming_timeout specifically, allow one same-tier retry before
                // escalating — handles transient empty SSE responses from Gemini.
                if error_payload.sub_kind.as_deref() == Some("streaming_timeout") {
                    let attempts = self
                        .sessions
                        .get(&session_id)
                        .map(|s| s.streaming_retry_attempts())
                        .unwrap_or(0);
                    if attempts < 1 {
                        if let Some(state) = self.sessions.get_mut(&session_id) {
                            state.increment_streaming_retry_attempts();
                        }
                        warn!(
                            session_id = %session_id,
                            "Retrying same tier after streaming_timeout (attempt 1)"
                        );
                        return self
                            .retry_active_turn_after_provider_failure(
                                session_id,
                                turn_id,
                                None,
                            )
                            .await;
                    }
                }
                warn!(
                    session_id = %session_id,
                    sub_kind = %sub_kind,
                    provider = %error_payload.provider.as_deref().unwrap_or("unknown"),
                    "Escalating to next fallback tier after provider failure"
                );
                return self
                    .advance_turn_to_next_fallback_tier(session_id, turn_id)
                    .await;
            }
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

        let partial_replies = model_result
            .and_then(|r| r.get("partial_replies"))
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let memory_concept = model_result
            .and_then(|r| r.get("memory_concept"))
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string);
        let memory_candidate =
            parse_memory_candidate(model_result.and_then(|r| r.get("memory_candidate")));
        let audio_artifact = extract_audio_artifact(model_result);

        // Capture active_plan from the model response and store it on the turn.
        // Present whenever the model outputs a structured plan — optional on all turns.
        if let Some(plan_value) = model_result.and_then(|r| r.get("active_plan")) {
            if let Ok(plan) = serde_json::from_value::<ActivePlan>(plan_value.clone()) {
                if let Some(state) = self.sessions.get_mut(&session_id) {
                    let is_first = state
                        .active_turn
                        .as_ref()
                        .map(|t| t.active_plan.is_none())
                        .unwrap_or(false);
                    state.set_active_plan(plan);
                    if is_first {
                        let _ = self.emit_turn_event(&session_id, "plan_ready", None).await;
                    }
                }
            }
        }

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

        // Scripted loop fork: when the turn is running under a LoopScript,
        // model responses are consumed by the ScriptedLoopExecutor rather than
        // dispatched through the standard AgentAction match below.
        // Full routing (ParkForApproval, ExecuteNextTool, etc.) is implemented
        // in handle_scripted_loop_model_response — stubbed here until Phase 1b.
        let is_scripted = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .map(|t| t.scripted_loop_context.is_some())
            .unwrap_or(false);

        if is_scripted {
            let model_result_owned = model_result.cloned();
            return self
                .handle_scripted_loop_model_response(
                    session_id,
                    turn_id,
                    model_result_owned,
                    spoken_text,
                    memory_concept,
                    memory_candidate,
                )
                .await;
        }

        match action {
            AgentAction::Respond { content } => {
                for partial in partial_replies {
                    self.emit_partial_reply(&session_id, partial).await?;
                }
                self.complete_agent_response(
                    session_id,
                    turn_id,
                    content,
                    spoken_text,
                    audio_artifact,
                    memory_concept,
                    memory_candidate,
                )
                .await
            }
            AgentAction::ToolCall(tool_call) => {
                self.handle_tool_call(session_id, turn_id, tool_call).await
            }
            AgentAction::RequestApproval(approval) => {
                self.handle_approval_request(session_id, turn_id, approval, false)
                    .await
            }
            AgentAction::PlanProposal(proposal) => {
                self.handle_plan_proposal(session_id, turn_id, proposal)
                    .await
            }
            AgentAction::Fail { message } => {
                self.fail_active_turn(session_id, turn_id, message).await
            }
        }
    }

    /// Handle a model plan_proposal: surface the plan to the operator, park the turn
    /// in PlanningDiscussion so the session stays free, and wait for the operator's
    /// next message to confirm or redirect before any tools execute.
    async fn handle_plan_proposal(
        &mut self,
        session_id: String,
        turn_id: String,
        proposal: PlanProposalAction,
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
                warn!("Received plan_proposal for unknown session {}", session_id);
                return Ok(());
            };
            let Some(active_turn) = state.active_turn.as_ref() else {
                warn!(
                    "Received plan_proposal for session {} with no active turn",
                    session_id
                );
                return Ok(());
            };
            let task_id = active_turn.task_id;
            let chat_id = active_turn.chat_id.clone();
            let final_reply_to = active_turn.final_reply_to.clone();
            let final_reply_role = active_turn.final_reply_role.clone();
            let final_reply_guest_id = active_turn.final_reply_guest_id.clone();
            state.set_active_turn_phase(TurnPhase::PlanningDiscussion);
            state.park_active_turn_for_plan();
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

        // Build the plan summary for the operator.
        let steps_text = if proposal.steps.is_empty() {
            String::new()
        } else {
            let formatted: Vec<String> = proposal
                .steps
                .iter()
                .enumerate()
                .map(|(i, step)| {
                    let desc = step
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(step)");
                    let tool = step
                        .get("tool_name")
                        .and_then(|v| v.as_str())
                        .map(|t| format!(" [{t}]"))
                        .unwrap_or_default();
                    format!("{}. {desc}{tool}", i + 1)
                })
                .collect();
            format!("\n\nSteps:\n{}", formatted.join("\n"))
        };

        let risk_note = proposal
            .approval_risk_hint
            .as_deref()
            .map(|h| format!("\nRisk: {h}."))
            .unwrap_or_default();

        let plan_text = format!(
            "📋 Plan proposal:\n\n{summary}{steps}{risk}\n\nReply to proceed, or redirect me.",
            summary = proposal.summary,
            steps = steps_text,
            risk = risk_note,
        );

        let _ = self
            .ipc_client
            .send_request(IpcRequest::UpdateTask {
                task_id,
                state: "plan_proposed".into(),
                payload: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "plan_summary": proposal.summary,
                }),
            })
            .await?;

        let reply_payload = FinalReplyPayload {
            action: "send_reply",
            session_id: session_id.clone(),
            turn_id,
            chat_id,
            content: plan_text,
            audio_artifact: None,
            send_text_caption: false,
            reply_markup: None,
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

    async fn handle_approval_request(
        &mut self,
        session_id: String,
        turn_id: String,
        approval: ApprovalRequest,
        always_require_human: bool,
    ) -> Result<()> {
        let approval = Self::normalize_approval_request(approval);
        // `always_require_human` bypasses the approval policy entirely — the human operator
        // must approve in this session. Used for admin role creation, which cannot be
        // preapproved or bypassed by `auto_approve_all`.
        let preapproved = if always_require_human {
            false
        } else {
            self.sessions
                .get(&session_id)
                .map(|state| {
                    let tool = state
                        .active_turn
                        .as_ref()
                        .and_then(|t| t.pending_tool_call.as_ref());
                    state.approval_policy_allows(&approval, tool)
                })
                .unwrap_or(false)
        };

        let (
            task_id,
            chat_id,
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
            checkpoint_memory_type,
            checkpoint_json,
            index_state,
            approval_active_plan,
            approval_pending_tool_call,
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
            let approval_active_plan = active_turn.active_plan.clone();
            let approval_pending_tool_call = active_turn.pending_tool_call.clone();
            if preapproved {
                state.clear_pending_approval();
                state.set_active_turn_phase(TurnPhase::Thinking);
            } else {
                state.set_pending_approval(approval.clone());
                state.set_active_turn_phase(TurnPhase::WaitingApproval);
                // Park the turn so this session can accept new work while the operator
                // decides. active_turn becomes None; parked_approval_turn holds the state.
                state.park_active_turn_for_approval();
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
                approval_active_plan,
                approval_pending_tool_call,
            )
        };

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        if preapproved {
            let pending_tool_call = self
                .sessions
                .get(&session_id)
                .and_then(|state| state.active_turn.as_ref())
                .and_then(|turn| turn.pending_tool_call.clone());
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

            let reply_payload = FinalReplyPayload {
                action: "send_reply",
                session_id: session_id.clone(),
                turn_id: turn_id.clone(),
                chat_id,
                content: approval.approved_response.clone(),
                audio_artifact: None,
                send_text_caption: false,
                reply_markup: None,
            };

            self.ipc_client
                .send_request(IpcRequest::EmitTask {
                    target_node: final_reply_to,
                    target_role: final_reply_role,
                    target_guest_id: final_reply_guest_id,
                    task_json: serde_json::to_string(&reply_payload)?,
                })
                .await?;

            if let Some(tool_call) = pending_tool_call {
                // bypass_approval=true: preapproved path, no re-gate needed.
                return self
                    .route_tool_call_execution(session_id, turn_id, tool_call, true)
                    .await;
            }

            return self
                .complete_agent_response(
                    session_id,
                    turn_id,
                    approval.approved_response,
                    None,
                    None,
                    None,
                    None,
                )
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

        let approval_keyboard = serde_json::json!({
            "inline_keyboard": [
                [
                    {"text": "✅ Approve", "callback_data": "approve"},
                    {"text": "❌ Deny", "callback_data": "deny"}
                ],
                [
                    {"text": "🔓 Trust for session", "callback_data": "trust"}
                ]
            ]
        });

        let reply_payload = FinalReplyPayload {
            action: "send_reply",
            session_id,
            turn_id,
            chat_id,
            content: Self::format_approval_message(
                &approval,
                approval_active_plan.as_ref(),
                approval_pending_tool_call.as_ref(),
            ),
            audio_artifact: None,
            send_text_caption: false,
            reply_markup: Some(approval_keyboard),
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

    async fn emit_partial_reply(&mut self, session_id: &str, content: String) -> Result<()> {
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

        let payload = PartialReplyPayload {
            action: "partial_reply",
            session_id: session_id.to_string(),
            turn_id,
            chat_id,
            content,
        };

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: final_reply_to,
                target_role: final_reply_role,
                target_guest_id: final_reply_guest_id,
                task_json: serde_json::to_string(&payload)?,
            })
            .await?;

        Ok(())
    }

    fn route_tool_call_execution(
        &mut self,
        session_id: String,
        turn_id: String,
        tool_call: ToolCall,
        // When `true`, the approval gate is skipped entirely — the caller has already
        // obtained a manual or preapproved resolution and must not re-gate the tool.
        bypass_approval: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // Agent-level approval enforcement: if the tool's policy annotation marks it as
            // requiring approval, and the current approval policy does not preapprove it,
            // synthesize an ApprovalRequest before executing. This runs independently of
            // whether the model itself requested approval — it is the agent's safety gate.
            // Skipped when bypass_approval is true (i.e. we are resuming after a resolution).
            let force_approval = if bypass_approval {
                false
            } else {
                self.sessions
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
                    .unwrap_or(false)
            };

            // Admin role creation requires live operator approval regardless of any approval policy.
            // This check runs before the normal force_approval gate so it can set always_require_human.
            // Bypassed when bypass_approval is true (already resolved by the operator).
            let is_admin_role_creation = !bypass_approval
                && tool_call.tool_name == "role.configure"
                && tool_call
                    .arguments
                    .get("is_admin")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

            // rule.propose always requires live operator approval — cannot be preapproved or bypassed.
            // Rules are durable and permanently affect agent behavior, so human confirmation is required.
            // (bypass_approval does NOT bypass rule.propose — that one is unconditional.)
            let is_rule_propose = !bypass_approval && tool_call.tool_name == "rule.propose";

            // routing.policy.propose requires the same live-approval gate: proposals are stored
            // durably in the hotel graph and influence future routing decisions, so they must
            // not be silently submitted without operator visibility.
            let is_routing_policy_propose =
                !bypass_approval && tool_call.tool_name == "routing.policy.propose";

            if is_admin_role_creation
                || is_rule_propose
                || is_routing_policy_propose
                || force_approval
            {
                // Set pending_tool_call so the approval handler can read it for class lookup.
                if let Some(state) = self.sessions.get_mut(&session_id) {
                    state.set_pending_tool_call(tool_call.clone());
                }
                let role_name_hint = if is_admin_role_creation {
                    tool_call
                        .arguments
                        .get("role_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string()
                } else {
                    String::new()
                };
                let (reason, approved_response) = if is_admin_role_creation {
                    (
                        format!(
                            "Admin role '{}' creation requires your explicit live approval. \
                         This cannot be preapproved or bypassed by policy.",
                            role_name_hint
                        ),
                        format!("Admin role '{}' approved.", role_name_hint),
                    )
                } else if is_rule_propose {
                    (
                        "Rule proposal requires your explicit live approval.".to_string(),
                        "Rule proposal approved.".to_string(),
                    )
                } else if is_routing_policy_propose {
                    (
                        "Routing policy proposal requires your explicit live approval.".to_string(),
                        "Routing policy proposal approved.".to_string(),
                    )
                } else {
                    (
                        format!(
                            "Tool '{}' requires approval before execution.",
                            tool_call.tool_name
                        ),
                        format!("Executing {}.", tool_call.tool_name),
                    )
                };
                let synthetic = ApprovalRequest {
                    approval_id: Some(uuid::Uuid::new_v4().to_string()),
                    reason,
                    approved_response,
                };
                return self
                    .handle_approval_request(
                        session_id,
                        turn_id,
                        synthetic,
                        is_admin_role_creation || is_rule_propose || is_routing_policy_propose,
                    )
                    .await;
            }

            // Dedup guard: if (tool_name, canonical_args) already appears in this
            // turn's history with a non-error result, inject a correction note and
            // re-enter the model without dispatching the tool again. This prevents
            // spin loops where the model calls an idempotent tool (e.g. role.create_or_update)
            // repeatedly after it already succeeded.
            let canonical_args = serde_json::to_string(&tool_call.arguments).unwrap_or_default();
            let (
                already_succeeded,
                dedup_chat_id,
                dedup_reply_to,
                dedup_reply_role,
                dedup_reply_guest_id,
            ) = {
                let state = self.sessions.get(&session_id);
                let prev_success = state
                    .and_then(|s| s.active_turn.as_ref())
                    .map(|turn| {
                        turn.working_tool_history
                            .iter()
                            .any(|(prev_call, prev_result)| {
                                prev_call.tool_name == tool_call.tool_name
                                    && serde_json::to_string(&prev_call.arguments)
                                        .unwrap_or_default()
                                        == canonical_args
                                    && !prev_result.content.to_lowercase().contains("error")
                                    && !prev_result.content.to_lowercase().contains("failed")
                            })
                    })
                    .unwrap_or(false);
                let (chat_id, reply_to, reply_role, reply_guest) = state
                    .and_then(|s| s.active_turn.as_ref())
                    .map(|t| {
                        (
                            t.chat_id.clone(),
                            t.final_reply_to.clone(),
                            t.final_reply_role.clone(),
                            t.final_reply_guest_id.clone(),
                        )
                    })
                    .unwrap_or_default();
                (prev_success, chat_id, reply_to, reply_role, reply_guest)
            };

            if already_succeeded {
                warn!(
                    "Session [{}] dedup guard: `{}` already succeeded this turn; \
                     injecting correction note instead of re-dispatching.",
                    session_id, tool_call.tool_name
                );
                if let Some(state) = self.sessions.get_mut(&session_id) {
                    state.set_provider_repair_note(format!(
                        "`{}` with these arguments already succeeded earlier in this turn. \
                         Do not call it again. Review the tool history and either proceed \
                         to the next plan step or deliver your final response.",
                        tool_call.tool_name
                    ));
                }
                return self
                    .handle_tool_result(InboundTaskPayload {
                        action: Some("tool_result".into()),
                        source: Some("agent".into()),
                        session_id: Some(session_id),
                        turn_id: Some(turn_id),
                        chat_id: Some(dedup_chat_id),
                        content: Some(format!(
                            "[Duplicate call skipped] `{}` already ran and succeeded \
                             earlier in this turn with these arguments.",
                            tool_call.tool_name
                        )),
                        tool_name: Some(tool_call.tool_name),
                        final_reply_to: Some(dedup_reply_to),
                        final_reply_role: Some(dedup_reply_role),
                        final_reply_guest_id: dedup_reply_guest_id,
                        ..Default::default()
                    })
                    .await;
            }

            // Emit step_started if streaming is enabled.
            let stream_events = self
                .sessions
                .get(&session_id)
                .map(|s| s.settings.execution.stream_tool_events)
                .unwrap_or(true);
            if stream_events {
                let step_info = Some(tool_call.tool_name.clone());
                let _ = self
                    .emit_turn_event(&session_id, "step_started", step_info)
                    .await;
            }
            let (
                chat_id,
                final_reply_to,
                final_reply_role,
                final_reply_guest_id,
                workspace_ref,
                route,
                session_user_id,
            ) = {
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
                let Some(active_turn) = state.active_turn.as_ref() else {
                    warn!(
                        "Dropping tool execution routing for session {} turn {} after active turn disappeared",
                        session_id, turn_id
                    );
                    return Ok(());
                };
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

            // Store the tool call on the active turn BEFORE dispatching so that when the
            // result returns, handle_tool_result can recover the full (name + arguments)
            // pair for the working_tool_history. Without this, the fallback uses empty args.
            if let Some(state) = self.sessions.get_mut(&session_id) {
                state.set_pending_tool_call(tool_call.clone());
            }

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
                task_runner_overlay: route.task_runner_kind.as_deref().map(|kind| {
                    TaskRunnerOverlay {
                        workspace_ref: if kind == "workspace" {
                            workspace_ref
                        } else {
                            None
                        },
                        allowed_tools: None,
                        max_read_bytes: None,
                        max_search_results: None,
                    }
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
        })
    }

    async fn handle_tool_call(
        &mut self,
        session_id: String,
        turn_id: String,
        tool_call: ToolCall,
    ) -> Result<()> {
        // bypass_approval=false: this is the normal model-driven path; gate applies.
        self.route_tool_call_execution(session_id, turn_id, tool_call, false)
            .await
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

        // step_failed is determined by the presence of a non-empty error payload.
        let step_failed = task.error.is_some();
        let stream_events = self
            .sessions
            .get(&session_id)
            .map(|s| s.settings.execution.stream_tool_events)
            .unwrap_or(true);

        // Scripted-loop fork: if this turn is running under a LoopScript, accumulate
        // the tool result into the executor and dispatch the next scripted action
        // instead of re-entering the open-ended model loop.
        let is_scripted_turn = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .map(|t| t.scripted_loop_context.is_some())
            .unwrap_or(false);

        if is_scripted_turn {
            let (checkpoint_memory_type, checkpoint_json, index_state) = {
                let Some(state) = self.sessions.get_mut(&session_id) else {
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
                let result_value = Value::String(tool_result.content.clone());
                state.with_scripted_executor_mut(|exec| {
                    exec.record_tool_result(result_value);
                    exec.advance_tool_cursor();
                });
                (
                    state.checkpoint_memory_type(),
                    state.checkpoint_json(),
                    state.clone(),
                )
            };
            if stream_events {
                let event = if step_failed {
                    "step_failed"
                } else {
                    "step_completed"
                };
                let _ = self.emit_turn_event(&session_id, event, None).await;
            }
            self.ipc_client
                .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
                .await?;
            self.sync_session_index(&index_state).await?;
            return self
                .scripted_dispatch_after_advance(session_id, turn_id, None, None, None)
                .await;
        }

        // Pair the result with the pending tool call, push to history, check iteration cap.
        let mut is_finalizing = false;
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

            state.push_tool_history(tool_call.clone(), tool_result.clone());
            state.clear_pending_tool_call();

            // Update conditional-preapproval streak tracking.
            if step_failed {
                state.record_tool_streak_failure(&tool_call.tool_name);
            } else {
                state.record_tool_streak_success(&tool_call.tool_name);
            }

            // Track consecutive failures for stall detection.
            let consecutive_failures = if step_failed {
                state.increment_step_failures()
            } else {
                state.reset_step_failures();
                0
            };
            let stall_threshold = state.settings.execution.stall_detection_threshold;

            // Increment iteration before building the envelope so the context projection
            // carries the correct (post-increment) iteration number. This ensures
            // turn/iteration-aware stubs and any model-side telemetry see the right value.
            if let Some(turn) = state.active_turn.as_mut() {
                turn.iteration += 1;
                turn.phase = TurnPhase::WaitingModel;
            }

            let iteration = state.active_turn.as_ref().map(|t| t.iteration).unwrap_or(0);
            let iteration_cap = state.settings.execution.iteration_cap;

            if consecutive_failures >= stall_threshold {
                Err(format!(
                    "Stall detected: {consecutive_failures} consecutive step failures \
                     (threshold: {stall_threshold}). Surfacing to user."
                ))
            } else if iteration == iteration_cap {
                // Soft cap: one final no-tool call so the model can wrap up gracefully.
                warn!(
                    "Session [{}] reached iteration cap ({}); doing final no-tool wrap-up.",
                    session_id, iteration_cap
                );
                is_finalizing = true;
                match state.build_reentry_context_envelope() {
                    Some((mut prompt, context, context_projection, _tools)) => {
                        prompt.push_str(
                            "\n\n[You have reached the maximum number of tool calls for this turn. \
                             Do not call any more tools. Provide your final response to the user now.]",
                        );
                        let active_turn = state.active_turn.as_ref().expect("turn exists");
                        Ok((
                            prompt,
                            context,
                            context_projection,
                            active_turn.task_id,
                            active_turn.user_content.clone(),
                            active_turn.chat_id.clone(),
                            active_turn.final_reply_to.clone(),
                            active_turn.final_reply_role.clone(),
                            active_turn.final_reply_guest_id.clone(),
                            vec![], // strip tools — forces text-only reply
                            state.checkpoint_memory_type(),
                            state.checkpoint_json(),
                            state.clone(),
                        ))
                    }
                    None => Err("Active turn vanished at iteration cap".into()),
                }
            } else if iteration > iteration_cap {
                // Hard cap: finalizing call itself produced another tool call (shouldn't happen
                // with empty tool list, but guard anyway).
                Err(format!(
                    "Turn exceeded maximum tool iterations ({iteration_cap}). Aborting."
                ))
            } else if state
                .active_turn
                .as_ref()
                .and_then(|t| t.active_plan.as_ref())
                .map(|plan| {
                    plan.status == "done"
                        || (!plan.steps.is_empty()
                            && plan
                                .steps
                                .iter()
                                .all(|s| s.status == "done" || s.status == "failed"))
                })
                .unwrap_or(false)
            {
                // Plan-done early exit: the model has declared its plan complete.
                // Force a final no-tool wrap-up so the model delivers its summary
                // without being prompted to call more tools.
                info!(
                    "Session [{}] plan marked done after tool result; doing no-tool wrap-up.",
                    session_id
                );
                is_finalizing = true;
                match state.build_reentry_context_envelope() {
                    Some((mut prompt, context, context_projection, _tools)) => {
                        prompt.push_str(
                            "\n\n[Your plan is complete. All steps have been executed. \
                             Do not call any more tools. Provide your final response to the user now.]",
                        );
                        let active_turn = state.active_turn.as_ref().expect("turn exists");
                        Ok((
                            prompt,
                            context,
                            context_projection,
                            active_turn.task_id,
                            active_turn.user_content.clone(),
                            active_turn.chat_id.clone(),
                            active_turn.final_reply_to.clone(),
                            active_turn.final_reply_role.clone(),
                            active_turn.final_reply_guest_id.clone(),
                            vec![], // strip tools — forces text-only reply
                            state.checkpoint_memory_type(),
                            state.checkpoint_json(),
                            state.clone(),
                        ))
                    }
                    None => Err("Active turn vanished at plan-done wrap-up".into()),
                }
            } else {
                // Build the full cognitive context envelope for re-entry.
                // This ensures identity, instructions, memory, dialogue_window, active_turn,
                // and tool_history all reach model-router — not just a flat prompt.
                match state.build_reentry_context_envelope() {
                    Some((prompt, context, context_projection, tools)) => {
                        let active_turn = state.active_turn.as_ref().expect("turn exists");
                        Ok((
                            prompt,
                            context,
                            context_projection,
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
                        Err("Active turn vanished before re-entry context could be built".into())
                    }
                }
            }
        };

        // Emit step_completed or step_failed event before continuing the loop.
        if stream_events {
            let event = if step_failed {
                "step_failed"
            } else {
                "step_completed"
            };
            let _ = self.emit_turn_event(&session_id, event, None).await;
        }

        if is_finalizing && stream_events {
            let _ = self
                .emit_turn_event(&session_id, "loop_finalizing", None)
                .await;
        }

        match loop_outcome {
            Err(msg) => {
                if stream_events && !step_failed && !is_finalizing {
                    // stall/hard-cap hit — emit loop_recovering so observers know we stopped
                    let _ = self
                        .emit_turn_event(&session_id, "loop_recovering", None)
                        .await;
                }
                self.fail_active_turn(session_id, turn_id, msg).await
            }
            Ok((
                prompt,
                context,
                context_projection,
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

                let response_contract = Some(
                    serde_json::json!({ "channels": ["spoken_text", "memory_candidate", "active_plan"] }),
                );
                let response_route = Some(model_response_route(
                    self.sessions.get(&session_id),
                    response_contract.as_ref(),
                    &Map::new(),
                    &Vec::new(),
                ));
                let ligand =
                    planning_ligand(self.sessions.get(&session_id), &prompt, &tools_for_model);
                let model_req = ModelRequestPayload {
                    action: "generate_text".to_string(),
                    request_class: Some("cognitive".to_string()),
                    session_id: session_id.clone(),
                    turn_id,
                    prompt,
                    user_content,
                    context: Some(context),
                    context_projection: Some(context_projection),
                    attachments: Vec::new(),
                    tools_for_model,
                    response_contract,
                    response_route,
                    ligand,
                    provider_options: Map::new(),
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

    async fn retry_active_turn_after_provider_failure(
        &mut self,
        session_id: String,
        turn_id: String,
        note: Option<String>,
    ) -> Result<()> {
        let retry_plan = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                return Ok(());
            };
            if let Some(n) = note {
                state.set_provider_repair_note(n);
            }
            state.increment_provider_repair_attempts();

            match state.build_reentry_context_envelope() {
                Some((prompt, context, context_projection, tools_for_model)) => {
                    if let Some(turn) = state.active_turn.as_mut() {
                        turn.iteration += 1;
                        turn.phase = TurnPhase::WaitingModel;
                    }
                    let active_turn = state.active_turn.as_ref().expect("turn exists");
                    Ok((
                        prompt,
                        context,
                        context_projection,
                        active_turn.user_content.clone(),
                        active_turn.chat_id.clone(),
                        active_turn.final_reply_to.clone(),
                        active_turn.final_reply_role.clone(),
                        active_turn.final_reply_guest_id.clone(),
                        tools_for_model,
                        state.checkpoint_memory_type(),
                        state.checkpoint_json(),
                        state.clone(),
                    ))
                }
                None => Err(anyhow::anyhow!(
                    "Active turn vanished before provider-failure retry context could be built"
                )),
            }
        }?;

        let (
            prompt,
            context,
            context_projection,
            user_content,
            chat_id,
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
            tools_for_model,
            checkpoint_memory_type,
            checkpoint_json,
            index_state,
        ) = retry_plan;

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let _ = self
            .emit_turn_event(&session_id, "loop_recovering", None)
            .await;

        let response_contract = Some(
            serde_json::json!({ "channels": ["spoken_text", "memory_candidate", "active_plan"] }),
        );
        let response_route = Some(model_response_route(
            self.sessions.get(&session_id),
            response_contract.as_ref(),
            &Map::new(),
            &Vec::new(),
        ));
        let ligand = planning_ligand(self.sessions.get(&session_id), &prompt, &tools_for_model);
        let model_req = ModelRequestPayload {
            action: "generate_text".to_string(),
            request_class: Some("cognitive".to_string()),
            session_id: session_id.clone(),
            turn_id,
            prompt,
            user_content,
            context: Some(context),
            context_projection: Some(context_projection),
            attachments: Vec::new(),
            tools_for_model,
            response_contract,
            response_route,
            ligand,
            provider_options: serde_json::Map::new(),
            chat_id,
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
        };

        if debug_model_requests_enabled() {
            match serde_json::to_string_pretty(&model_req) {
                Ok(json) => info!(
                    "PHILOTIC_DEBUG_MODEL_REQUESTS philote retry model request session={} turn={}:\n{}",
                    session_id, model_req.turn_id, json
                ),
                Err(err) => warn!(
                    "PHILOTIC_DEBUG_MODEL_REQUESTS could not serialize retry model request: {}",
                    err
                ),
            }
        }

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

    /// Advance the active turn to the next fallback tier and re-dispatch the
    /// model request to that tier's role. If already at the last tier, fails
    /// the turn with a user-visible error.
    async fn advance_turn_to_next_fallback_tier(
        &mut self,
        session_id: String,
        turn_id: String,
    ) -> Result<()> {
        // Extract configured tiers before any mutable session borrow.
        let active_role_name: Option<String> = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.role_activation.as_ref())
            .map(|ra| ra.role_name.clone());
        // Prefer configured_roles (set via ConfigureRole tool calls); fall back to the
        // turn_loop_config embedded in the active role_activation (set at session load
        // from the DB record via fetch_role_activation).
        let configured_tiers: Vec<String> = active_role_name
            .as_deref()
            .and_then(|rn| self.configured_roles.get(rn))
            .map(|c| c.turn_loop_config.fallback_tiers.clone())
            .or_else(|| {
                self.sessions
                    .get(&session_id)
                    .and_then(|s| s.role_activation.as_ref())
                    .and_then(|ra| ra.turn_loop_config.as_ref())
                    .map(|tlc| tlc.fallback_tiers.clone())
            })
            .unwrap_or_default();

        // Check tier boundaries before any mutable borrow.
        let current_tier = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .map(|t| t.fallback_tier)
            .unwrap_or(0);
        let max_tier = if !configured_tiers.is_empty() {
            configured_tiers.len().saturating_sub(1) as u8
        } else {
            DEFAULT_FALLBACK_TIERS.len().saturating_sub(1) as u8
        };

        if current_tier >= max_tier {
            return self
                .fail_active_turn(
                    session_id,
                    turn_id,
                    "All model providers failed. Please try again later.".into(),
                )
                .await;
        }

        let next_tier = current_tier + 1;
        let next_role = role_for_tier(&configured_tiers, next_tier).to_string();

        // Gather everything we need before dropping the mutable state borrow.
        let plan = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                return Ok(());
            };

            if let Some(turn) = state.active_turn.as_mut() {
                turn.fallback_tier = next_tier;
                turn.phase = TurnPhase::WaitingModel;
                turn.iteration += 1;
            }

            match state.build_reentry_context_envelope() {
                Some((prompt, context, context_projection, tools_for_model)) => {
                    let active_turn = state.active_turn.as_ref().expect("turn exists");
                    Ok((
                        prompt,
                        context,
                        context_projection,
                        active_turn.user_content.clone(),
                        active_turn.chat_id.clone(),
                        active_turn.final_reply_to.clone(),
                        active_turn.final_reply_role.clone(),
                        active_turn.final_reply_guest_id.clone(),
                        tools_for_model,
                        state.checkpoint_memory_type(),
                        state.checkpoint_json(),
                        state.clone(),
                    ))
                }
                None => Err(anyhow::anyhow!(
                    "Active turn vanished before fallback tier re-dispatch"
                )),
            }
        }?;

        let (
            prompt,
            context,
            context_projection,
            user_content,
            chat_id,
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
            tools_for_model,
            checkpoint_memory_type,
            checkpoint_json,
            index_state,
        ) = plan;

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let _ = self
            .emit_turn_event(&session_id, "loop_recovering", None)
            .await;

        let response_contract = Some(
            serde_json::json!({ "channels": ["spoken_text", "memory_candidate", "active_plan"] }),
        );
        let response_route = Some(model_response_route(
            self.sessions.get(&session_id),
            response_contract.as_ref(),
            &Map::new(),
            &Vec::new(),
        ));
        let ligand = planning_ligand(self.sessions.get(&session_id), &prompt, &tools_for_model);
        let model_req = ModelRequestPayload {
            action: "generate_text".to_string(),
            request_class: Some("cognitive".to_string()),
            session_id: session_id.clone(),
            turn_id,
            prompt,
            user_content,
            context: Some(context),
            context_projection: Some(context_projection),
            attachments: Vec::new(),
            tools_for_model,
            response_contract,
            response_route,
            ligand,
            provider_options: serde_json::Map::new(),
            chat_id,
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
        };

        info!(
            session_id = %session_id,
            fallback_tier = next_tier,
            target_role = %next_role,
            "Dispatching model request to fallback tier"
        );

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: local_node_id(),
                target_role: next_role,
                target_guest_id: None,
                task_json: serde_json::to_string(&model_req)?,
            })
            .await?;

        Ok(())
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
        if let Some(state) = self.sessions.get_mut(&session_id) {
            state.clear_handoff_summary();
        }

        let response_contract = Some(
            serde_json::json!({ "channels": ["spoken_text", "memory_candidate", "active_plan", "memory_concept"] }),
        );
        let response_route = Some(model_response_route(
            self.sessions.get(&session_id),
            response_contract.as_ref(),
            &Map::new(),
            &Vec::new(),
        ));
        let ligand = planning_ligand(
            self.sessions.get(&session_id),
            &reentry.user_content,
            &reentry.tools_for_model,
        );
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
            response_contract,
            response_route,
            ligand,
            provider_options: serde_json::Map::new(),
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
    async fn emit_error_reply(
        &mut self,
        task: &InboundTaskPayload,
        task_id: Uuid,
        err: anyhow::Error,
    ) -> Result<()> {
        let (final_reply_to, final_reply_role, final_reply_guest_id) = (
            task.final_reply_to
                .clone()
                .unwrap_or_else(|| local_node_id()),
            task.final_reply_role
                .clone()
                .unwrap_or_else(|| DEFAULT_REPLY_ROLE.to_string()),
            task.final_reply_guest_id.clone(),
        );
        let session_id = task.session_id_or_default(&self.agent_id);
        let turn_id = task.turn_id.clone().unwrap_or_else(|| task_id.to_string());
        let chat_id = task.chat_id.clone().unwrap_or_default();

        let content = format!("⚠️ Agent Error: {}", err);

        let payload = FinalReplyPayload {
            action: "send_reply",
            session_id,
            turn_id,
            chat_id,
            content,
            audio_artifact: None,
            send_text_caption: false,
            reply_markup: None,
        };

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: final_reply_to,
                target_role: final_reply_role,
                target_guest_id: final_reply_guest_id,
                task_json: serde_json::to_string(&payload)?,
            })
            .await?;

        Ok(())
    }

    /// Handles a `context.capture` tool call arriving from membrane-mcp.
    ///
    /// Bypasses the LLM: parses the capture payload, stores it in Muninn, and
    /// emits a `send_reply` back to the mcp-membrane so the HTTP caller gets a
    /// response.
    async fn handle_context_capture(
        &mut self,
        task: InboundTaskPayload,
        task_id: Uuid,
    ) -> Result<()> {
        use memory_core::MemoryEngine as _;

        // Content is JSON: {"tool": "context.capture", "args": {...}}
        let args: serde_json::Value = task
            .content
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|v| v.get("args").cloned())
            .unwrap_or_default();

        let capture_text = args
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let category = args
            .get("category")
            .and_then(|v| v.as_str())
            .unwrap_or("note")
            .to_string();

        let mut tags: Vec<String> = args
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|t| t.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        tags.push("perplexity".to_string());
        if !tags.contains(&category) {
            tags.push(category.clone());
        }

        let first_line: String = capture_text
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(50)
            .collect();
        let concept = format!("perplexity.{}: {}", category, first_line);

        let result_text = match self.memory_engine_for(&self.agent_id, &self.agent_id) {
            None => "Captured (Muninn not configured on this node).".to_string(),
            Some(engine) => {
                match engine
                    .remember(MemoryScope::SelfOnly, &concept, &capture_text, tags)
                    .await
                {
                    Ok(engram_ref) => format!("Captured to memory (id: {}).", engram_ref.id),
                    Err(e) => format!("context.capture: memory error — {e}"),
                }
            }
        };

        info!(turn_id = ?task.turn_id, result = %result_text, "context.capture handled");

        let final_reply_to = task
            .final_reply_to
            .clone()
            .unwrap_or_else(|| local_node_id());
        let final_reply_role = task
            .final_reply_role
            .clone()
            .unwrap_or_else(|| DEFAULT_REPLY_ROLE.to_string());
        let final_reply_guest_id = task.final_reply_guest_id.clone();
        let session_id = task.session_id_or_default(&self.agent_id);
        let turn_id = task.turn_id.clone().unwrap_or_else(|| task_id.to_string());
        let chat_id = task.chat_id.clone().unwrap_or_default();

        let payload = FinalReplyPayload {
            action: "send_reply",
            session_id,
            turn_id,
            chat_id,
            content: result_text,
            audio_artifact: None,
            send_text_caption: false,
            reply_markup: None,
        };

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: final_reply_to,
                target_role: final_reply_role,
                target_guest_id: final_reply_guest_id,
                task_json: serde_json::to_string(&payload)?,
            })
            .await?;

        Ok(())
    }

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

    async fn maybe_auto_recall_turn_memory(&mut self, session_id: &str) -> Result<()> {
        use memory_core::MemoryEngine as _;

        let Some(state) = self.sessions.get(session_id) else {
            return Ok(());
        };
        let Some(active_turn) = state.active_turn.as_ref() else {
            return Ok(());
        };
        if active_turn.user_content.trim_start().starts_with('/') {
            return Ok(());
        }

        let recall_context = RecallContext {
            trigger: RecallTrigger::UserTurnStart,
            scope: MemoryScope::SelfOnly,
            recall_seed_text: active_turn.user_content.clone(),
            active_goal: active_turn
                .active_plan
                .as_ref()
                .map(|plan| plan.goal.clone()),
            role_name: state
                .role_activation
                .as_ref()
                .map(|role| role.role_name.clone()),
            recent_turns: state
                .recent_turns
                .iter()
                .rev()
                .take(3)
                .map(|turn| turn.user_content.clone())
                .collect(),
            local_memory_summaries: state
                .agent_profile
                .memory_summary
                .as_deref()
                .map(|text| vec![text.to_string()])
                .unwrap_or_default(),
            tool_history_summary: active_turn
                .working_tool_history
                .iter()
                .map(|(call, _)| call.tool_name.clone())
                .collect(),
            lens: None,
        };

        let Some(engine) = self.memory_engine_for(&self.agent_id, &self.agent_id) else {
            let decision = memory_core::evaluate_recall(&recall_context);
            info!(
                session_id = %session_id,
                reason = %decision.reason,
                "Auto recall skipped: no Muninn memory backend configured."
            );
            let _ = self
                .emit_turn_event(
                    session_id,
                    "memory_auto_recall_skipped",
                    Some(format!("Skipped auto recall: no memory backend configured")),
                )
                .await;
            return Ok(());
        };

        let decision = match engine.evaluate_recall(&recall_context).await {
            Ok(d) => d,
            Err(err) => {
                warn!(session_id = %session_id, error = %err, "Auto recall skipped: memory engine unavailable.");
                let _ = self
                    .emit_turn_event(
                        session_id,
                        "memory_auto_recall_skipped",
                        Some(format!("Skipped auto recall: memory engine unavailable")),
                    )
                    .await;
                return Ok(());
            }
        };
        if !decision.should_recall() {
            info!(
                session_id = %session_id,
                reason = %decision.reason,
                "Auto recall skipped for turn."
            );
            let _ = self
                .emit_turn_event(
                    session_id,
                    "memory_auto_recall_skipped",
                    Some(format!("Skipped auto recall: {}", decision.reason)),
                )
                .await;
            return Ok(());
        }

        info!(
            session_id = %session_id,
            reason = %decision.reason,
            query = %decision.query.as_deref().unwrap_or(""),
            limit = ?decision.limit,
            "Running auto recall for turn."
        );

        let result = match engine.recall_for_turn(&recall_context).await {
            Ok(r) => r,
            Err(err) => {
                warn!(session_id = %session_id, error = %err, "Auto recall failed: memory engine error.");
                return Ok(());
            }
        };
        let recalled_memories = result
            .engrams
            .into_iter()
            .map(|engram| RecalledMemoryRecord {
                concept: engram.concept,
                content: engram.content,
                tags: engram.tags,
            })
            .collect::<Vec<_>>();
        let recalled_count = recalled_memories.len();
        let concept_summary = recalled_memories
            .iter()
            .take(3)
            .map(|memory| memory.concept.clone())
            .collect::<Vec<_>>()
            .join(", ");

        if let Some(state) = self.sessions.get_mut(session_id) {
            if let Some(active_turn) = state.active_turn.as_mut() {
                active_turn.recalled_memories = recalled_memories;
            }
        }

        info!(
            session_id = %session_id,
            total = recalled_count,
            concepts = %concept_summary,
            "Auto recall completed for turn."
        );
        let _ = self
            .emit_turn_event(
                session_id,
                "memory_auto_recall_completed",
                Some(format!(
                    "Recalled {} memory item(s){}",
                    recalled_count,
                    if concept_summary.is_empty() {
                        String::new()
                    } else {
                        format!(": {concept_summary}")
                    }
                )),
            )
            .await;

        Ok(())
    }

    async fn complete_agent_response(
        &mut self,
        session_id: String,
        turn_id: String,
        content: String,
        spoken_text: Option<String>,
        audio_artifact: Option<String>,
        memory_concept: Option<String>,
        memory_candidate: Option<MemoryCandidate>,
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
            if voice_policy.delivery_mode.is_native_audio() {
                if audio_artifact.is_none() && !voice_policy.fallback_to_text {
                    return self
                        .fail_active_turn(
                            session_id,
                            turn_id,
                            "Provider-native audio response was requested but no audio artifact was returned.".into(),
                        )
                        .await;
                }

                return self
                    .deliver_text_reply(
                        session_id,
                        turn_id,
                        content,
                        audio_artifact,
                        voice_policy.caption_enabled(),
                        memory_concept,
                        memory_candidate,
                    )
                    .await;
            }

            return self
                .start_voice_synthesis(session_id, turn_id, content, spoken_text, voice_policy)
                .await;
        }

        self.deliver_text_reply(
            session_id,
            turn_id,
            content,
            None,
            false,
            memory_concept,
            memory_candidate,
        )
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
            // Reset the stuck-turn timer so WaitingVoice gets its own full budget.
            // Without this, time spent in earlier waiting phases (WaitingModel retries)
            // counts against the WaitingVoice deadline and fires the watchdog too early.
            self.stuck_turn_first_seen.remove(&session_id);
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

        let voice_role_fallback = policy
            .provider
            .as_deref()
            .map(implementation_to_model_role)
            .unwrap_or_else(|| DEFAULT_VOICE_MODEL_ROLE.into());
        let (target_node, target_role, target_guest_id) = resolve_model_execution_target(
            self.sessions.get(&session_id),
            "voice.synthesize",
            &voice_role_fallback,
        );

        info!(
            "Session [{}] routing voice synthesis for turn {:?} to role [{}] voice_id {:?}",
            session_id,
            turn_id,
            target_role,
            policy.effective_voice_id(),
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
            "voice_id": policy.effective_voice_id(),
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
            None,
        )
        .await
    }

    /// Handle a `voice.dialogue` task from the Discord membrane.
    ///
    /// Converts the inline PCM payload into a synthetic voice-attachment message and routes it
    /// through the normal media-routing path (typically → `voice.transcribe` → Claude → reply).
    async fn handle_voice_dialogue(
        &mut self,
        task: InboundTaskPayload,
        task_id: Uuid,
    ) -> Result<()> {
        use crate::protocol::TransportAttachment;

        // Ingress-time reflex: ensure voice_action == "transcribe" regardless of
        // what the agent profile defaulted to. Belt-and-suspenders over materialization.
        let session_id = task.session_id_or_default(&self.agent_id);
        if let Some(state) = self.sessions.get_mut(&session_id) {
            state.apply_reflex_ingress(IngressAction::VoiceDialogue);
        }

        let pcm_b64 = match task.pcm_b64.as_deref() {
            Some(b) if !b.is_empty() => b.to_string(),
            _ => {
                debug!("voice.dialogue task has no pcm_b64, skipping");
                return Ok(());
            }
        };

        let sample_rate = task.sample_rate.unwrap_or(48_000);
        let speaker_ssrc = task.speaker_ssrc.unwrap_or(0);

        // Build a synthetic inbound task that looks like a voice-note message.
        // The action is cleared so the normal handle_user_message dispatch applies.
        let mut synthetic = task.clone();
        synthetic.action = None;
        synthetic.message_kind = Some("voice".into());
        synthetic.pcm_b64 = None;
        synthetic.sample_rate = None;
        synthetic.speaker_ssrc = None;
        synthetic.attachments = vec![TransportAttachment {
            kind: "voice".into(),
            file_id: format!("discord-voice-{}", speaker_ssrc),
            mime_type: Some("audio/wav".into()),
            inline_audio_b64: Some(pcm_b64),
            inline_audio_sample_rate: Some(sample_rate),
            inline_audio_channels: Some(2),
            ..Default::default()
        }];
        // If the task had no content, provide a minimal placeholder so
        // normalized_user_content doesn't skip the message.
        if synthetic
            .content
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        {
            synthetic.content = Some(String::new());
        }

        self.handle_user_message(synthetic, task_id).await
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
        memory_candidate: Option<MemoryCandidate>,
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

        // Named specialist philotes always append an @agent attribution tag so the
        // membrane (or receiving philote) knows which role secreted this Exosome.
        // Format: `@agent:<role_name>` on its own line at the end of content.
        // Membrane strips the tag before transport delivery and attaches an affordance.
        let content = if let Ok(role_name) = std::env::var("PHILOTIC_ROLE_NAME") {
            if !role_name.is_empty() {
                format!("{}\n\n@agent:{}", content, role_name)
            } else {
                content
            }
        } else {
            content
        };

        // If this turn was triggered by a paracrine_request, reply as a
        // `paracrine_response` so A's routing reflex handles it correctly.
        // Use source_session_id / source_chat_id from the exosome (stored on the turn)
        // so the orchestrator routes the reply to the originating conversation channel
        // rather than to this specialist's ephemeral session.
        //
        // If delegate.merge was called explicitly during this turn, the paracrine_response
        // was already emitted — skip the auto-emit to avoid double-delivery.
        // Otherwise use the normal `send_reply` path.
        let task_json = if let Some(ref pid) = completed_turn.paracrine_origin {
            if completed_turn.paracrine_merge_completed {
                // Explicit merge already fired — complete the task but don't re-send.
                info!(
                    "deliver_text_reply: delegate.merge already emitted paracrine_response for turn {}; skipping auto-emit",
                    turn_id
                );
                self.drain_next_user_task(&attend_session_id);
                return Ok(());
            }
            let reply_session_id = completed_turn
                .paracrine_reply_session_id
                .as_deref()
                .unwrap_or(&session_id);
            let reply_chat_id = completed_turn
                .paracrine_reply_chat_id
                .as_deref()
                .unwrap_or(&completed_turn.chat_id);
            serde_json::json!({
                "action": "paracrine_response",
                "session_id": reply_session_id,
                "turn_id": turn_id,
                "chat_id": reply_chat_id,
                "content": content,
                "exosome": {
                    "prompt": "",
                    "paracrine_id": pid,
                    "source_session_id": reply_session_id,
                    "source_chat_id": reply_chat_id,
                },
            })
            .to_string()
        } else {
            let reply_payload = FinalReplyPayload {
                action: "send_reply",
                session_id,
                turn_id,
                chat_id: completed_turn.chat_id,
                content,
                audio_artifact,
                send_text_caption,
                reply_markup: None,
            };
            serde_json::to_string(&reply_payload)?
        };

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: completed_turn.final_reply_to,
                target_role: completed_turn.final_reply_role,
                target_guest_id: completed_turn.final_reply_guest_id,
                task_json,
            })
            .await?;

        // After completing this turn, schedule the next pending user task for dispatch.
        self.drain_next_user_task(&attend_session_id);

        // Attend hook (Slice E): fire-and-forget autobiographical memory write.
        // Only saves when the model provided an explicit memory_candidate — raw turn
        // content is never written as a fallback so the vault stays signal-only.
        if let (Some(engine), Some(candidate)) = (
            self.memory_engine_for(&self.agent_id, &self.agent_id),
            memory_candidate,
        ) {
            let agent_id = self.agent_id.clone();
            let mut tags = vec![
                format!("agent:{}", agent_id),
                format!("session:{}", attend_session_id),
            ];
            tags.extend(candidate.tags);
            let concept = memory_concept.unwrap_or(candidate.concept);
            let content_snapshot = candidate.content;
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
            let checkpoint_memory_type = state.checkpoint_memory_type();
            let checkpoint_json = state.checkpoint_json();
            let index_state = state.clone();
            // Clear the active turn NOW so is_turn_active() returns false before the
            // drain runs. Without this, drain_next_user_task moves the next queued task
            // into pending_drains, then handle_user_message sees an active turn and
            // re-queues it — creating an infinite queue/drain loop.
            state.active_turn = None;
            state.turn_waiting_since = None;
            (
                task_id,
                checkpoint_memory_type,
                checkpoint_json,
                index_state,
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

        let drain_session_id = session_id.clone();
        let reply_payload = FinalReplyPayload {
            action: "send_reply",
            session_id,
            turn_id,
            chat_id,
            content: message,
            audio_artifact: None,
            send_text_caption: false,
            reply_markup: None,
        };

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: final_reply_to,
                target_role: final_reply_role,
                target_guest_id: final_reply_guest_id,
                task_json: serde_json::to_string(&reply_payload)?,
            })
            .await?;

        // After failing this turn, schedule the next pending user task for dispatch.
        self.drain_next_user_task(&drain_session_id);

        Ok(())
    }

    /// Move the next queued user task into `pending_drains` so the main event loop
    /// can dispatch it without creating async recursion.
    ///
    /// Called after every turn completes or fails. The task retains its original
    /// `session_id`, `chat_id`, and `exosome` context so the correct Telegram
    /// session/chat is restored when the turn eventually starts.
    fn drain_next_user_task(&mut self, session_id: &str) {
        let next = if let Some(state) = self.sessions.get_mut(session_id) {
            state.dequeue_user_task()
        } else {
            return;
        };

        if let Some((task_id, task)) = next {
            let queued_depth = self
                .sessions
                .get(session_id)
                .map(|s| s.pending_user_task_count())
                .unwrap_or(0);
            info!(
                session_id = %session_id,
                remaining_in_queue = queued_depth,
                "Scheduling queued user task for dispatch after turn completion"
            );
            self.pending_drains.push_back((task_id, task));
        }
    }

    // ── Scripted-loop routing ───────────────────────────────────────────────

    /// Entry point called by handle_model_response when the turn is running under
    /// a LoopScript. Parses the model content (tries JSON, falls back to string),
    /// records it as the current step output, then dispatches the next decision.
    async fn handle_scripted_loop_model_response(
        &mut self,
        session_id: String,
        turn_id: String,
        model_result: Option<Value>,
        spoken_text: Option<String>,
        memory_concept: Option<String>,
        memory_candidate: Option<MemoryCandidate>,
    ) -> Result<()> {
        // Extract the primary text from the model result.
        let raw_text = model_result
            .as_ref()
            .and_then(|r| {
                r.get("display_text")
                    .or_else(|| r.get("content"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("")
            .to_string();

        // Try to parse as JSON; fall back to a plain string value.
        let output_value: Value =
            serde_json::from_str(&raw_text).unwrap_or(Value::String(raw_text));

        {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                return Ok(());
            };
            state.with_scripted_executor_mut(|exec| exec.record_step_output(output_value));
        }

        self.scripted_dispatch_after_advance(
            session_id,
            turn_id,
            spoken_text,
            memory_concept,
            memory_candidate,
        )
        .await
    }

    /// Read the current ScriptedLoopDecision and route to the correct leaf handler.
    fn scripted_dispatch_after_advance(
        &mut self,
        session_id: String,
        turn_id: String,
        spoken_text: Option<String>,
        memory_concept: Option<String>,
        memory_candidate: Option<MemoryCandidate>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        use crate::scripted_loop::ScriptedLoopDecision;
        Box::pin(async move {
            let decision = self
                .sessions
                .get(&session_id)
                .and_then(|s| s.scripted_executor_advance());

            match decision {
                None => {
                    self.fail_active_turn(
                        session_id,
                        turn_id,
                        "Scripted loop executor missing".into(),
                    )
                    .await
                }
                Some(ScriptedLoopDecision::EmitModelCall { phase, .. }) => {
                    self.scripted_emit_model_call(session_id, turn_id, phase)
                        .await
                }
                Some(ScriptedLoopDecision::ParkForApproval {
                    gate,
                    surface_as,
                    reject_action,
                }) => {
                    self.scripted_park_for_approval(
                        session_id,
                        turn_id,
                        gate,
                        surface_as,
                        reject_action,
                    )
                    .await
                }
                Some(ScriptedLoopDecision::ExecuteNextTool {
                    tool_name,
                    arguments,
                }) => {
                    let tool_call = ToolCall {
                        tool_name,
                        arguments,
                    };
                    self.scripted_dispatch_next_tool(session_id, turn_id, tool_call)
                        .await
                }
                Some(ScriptedLoopDecision::ToolSequenceComplete) => {
                    {
                        let Some(state) = self.sessions.get_mut(&session_id) else {
                            return Ok(());
                        };
                        state.with_scripted_executor_mut(|exec| exec.advance_past_tool_sequence());
                    }
                    self.scripted_dispatch_after_advance(
                        session_id,
                        turn_id,
                        spoken_text,
                        memory_concept,
                        memory_candidate,
                    )
                    .await
                }
                Some(ScriptedLoopDecision::ForceCheckpoint) => {
                    let (checkpoint_memory_type, checkpoint_json, index_state) = {
                        let Some(state) = self.sessions.get_mut(&session_id) else {
                            return Ok(());
                        };
                        // Record a null output for the checkpoint step and advance.
                        state.with_scripted_executor_mut(|exec| {
                            exec.record_step_output(Value::Null)
                        });
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
                    self.scripted_dispatch_after_advance(
                        session_id,
                        turn_id,
                        spoken_text,
                        memory_concept,
                        memory_candidate,
                    )
                    .await
                }
                Some(ScriptedLoopDecision::Complete { final_output }) => {
                    let content = match &final_output {
                        Value::String(s) => s.clone(),
                        Value::Null => String::new(),
                        other => other.to_string(),
                    };
                    self.complete_agent_response(
                        session_id,
                        turn_id,
                        content,
                        spoken_text,
                        None,
                        memory_concept,
                        memory_candidate,
                    )
                    .await
                }
                Some(ScriptedLoopDecision::Fail { reason }) => {
                    self.fail_active_turn(session_id, turn_id, reason).await
                }
            }
        })
    }

    /// Emit a model request for a scripted loop model_call step.
    async fn scripted_emit_model_call(
        &mut self,
        session_id: String,
        turn_id: String,
        phase: String,
    ) -> Result<()> {
        let (
            prompt,
            context,
            context_projection,
            tools,
            task_id,
            user_content,
            chat_id,
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
            checkpoint_memory_type,
            checkpoint_json,
            index_state,
        ) = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                return Ok(());
            };
            let Some((prompt, context, context_projection, tools)) =
                state.build_reentry_context_envelope()
            else {
                return self
                    .fail_active_turn(
                        session_id,
                        turn_id,
                        "scripted_emit_model_call: no active turn context".into(),
                    )
                    .await;
            };
            let turn = state
                .active_turn
                .as_mut()
                .expect("turn exists after envelope");
            turn.iteration += 1;
            turn.phase = TurnPhase::WaitingModel;
            let task_id = turn.task_id;
            let user_content = turn.user_content.clone();
            let chat_id = turn.chat_id.clone();
            let final_reply_to = turn.final_reply_to.clone();
            let final_reply_role = turn.final_reply_role.clone();
            let final_reply_guest_id = turn.final_reply_guest_id.clone();
            (
                prompt,
                context,
                context_projection,
                tools,
                task_id,
                user_content,
                chat_id,
                final_reply_to,
                final_reply_role,
                final_reply_guest_id,
                state.checkpoint_memory_type(),
                state.checkpoint_json(),
                state.clone(),
            )
        };

        let _ = self
            .ipc_client
            .send_request(IpcRequest::UpdateTask {
                task_id,
                state: "waiting_model".into(),
                payload: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "phase": phase,
                }),
            })
            .await?;

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let response_contract = Some(
            serde_json::json!({ "channels": ["spoken_text", "memory_candidate", "active_plan"] }),
        );
        let response_route = Some(model_response_route(
            self.sessions.get(&session_id),
            response_contract.as_ref(),
            &Map::new(),
            &Vec::new(),
        ));
        let ligand = planning_ligand(self.sessions.get(&session_id), &prompt, &tools);
        let model_req = ModelRequestPayload {
            action: "generate_text".to_string(),
            request_class: Some("cognitive".to_string()),
            session_id: session_id.clone(),
            turn_id,
            prompt,
            user_content,
            context: Some(context),
            context_projection: Some(context_projection),
            attachments: Vec::new(),
            tools_for_model: tools,
            response_contract,
            response_route,
            ligand,
            provider_options: Map::new(),
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
            "Scripted loop [{}] emitting model call for phase '{}'",
            session_id, phase
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

    /// Park the turn waiting for operator plan approval.
    /// Uses the approval_id sentinel "scripted_gate:<gate>" so that
    /// handle_approval_command can route the resolution back to the scripted executor.
    async fn scripted_park_for_approval(
        &mut self,
        session_id: String,
        turn_id: String,
        gate: String,
        _surface_as: String,
        _reject_action: String,
    ) -> Result<()> {
        // Surface the plan content as the approval reason.
        let plan_content = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .and_then(|t| t.scripted_loop_context.as_ref())
            .and_then(|exec| {
                let input_path = exec
                    .script
                    .steps
                    .get(exec.step_cursor)
                    .and_then(|s| s.input.as_deref())
                    .unwrap_or("");
                exec.resolve_input(input_path)
            })
            .map(|v| v.to_string())
            .unwrap_or_else(|| "Plan ready for review.".to_string());

        let approval = ApprovalRequest {
            approval_id: Some(format!("scripted_gate:{}", gate)),
            reason: plan_content,
            approved_response: "Plan approved. Executing now.".to_string(),
        };

        self.handle_approval_request(session_id, turn_id, approval, false)
            .await
    }

    /// Dispatch the next tool in a scripted tool_sequence step.
    /// bypass_approval=true — the operator already approved the full plan.
    async fn scripted_dispatch_next_tool(
        &mut self,
        session_id: String,
        turn_id: String,
        tool_call: ToolCall,
    ) -> Result<()> {
        if let Some(state) = self.sessions.get_mut(&session_id) {
            state.set_pending_tool_call(tool_call.clone());
        }
        self.route_tool_call_execution(session_id, turn_id, tool_call, true)
            .await
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
        // Approval turns are parked in `parked_approval_turn` while the session stays free.
        // Restore the parked turn into active_turn so the resolution logic proceeds normally.
        if let Some(state) = self.sessions.get_mut(&session_id) {
            if state.has_parked_approval_turn() && !state.is_turn_active() {
                state.restore_parked_approval_turn();
            }
        }

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
                            turn.pending_tool_call.clone(),
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
            original_pending_tool_call,
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
                reply_markup: None,
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
                | SlashCommand::Context
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
                | SlashCommand::ApprovalClear { .. }
                | SlashCommand::Abandon { .. }
                | SlashCommand::Tts { .. }
                | SlashCommand::Voice { .. }
                | SlashCommand::Correct { .. } => {}
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
                    session_id: session_id.clone(),
                    turn_id: original_turn_id.clone(),
                    chat_id: original_chat_id,
                    content: approval.approved_response.clone(),
                    audio_artifact: None,
                    send_text_caption: false,
                    reply_markup: None,
                };

                self.ipc_client
                    .send_request(IpcRequest::EmitTask {
                        target_node: original_reply_to,
                        target_role: original_reply_role,
                        target_guest_id: original_reply_guest_id.clone(),
                        task_json: serde_json::to_string(&reply_payload)?,
                    })
                    .await?;

                // Scripted gate: record approval as the current step's output and
                // let the executor drive what comes next.
                if approval
                    .approval_id
                    .as_deref()
                    .map(|id| id.starts_with("scripted_gate:"))
                    .unwrap_or(false)
                {
                    if let Some(state) = self.sessions.get_mut(&session_id) {
                        state.with_scripted_executor_mut(|exec| {
                            exec.record_step_output(serde_json::json!({"approved": true}));
                        });
                    }
                    return self
                        .scripted_dispatch_after_advance(
                            session_id,
                            original_turn_id,
                            None,
                            None,
                            None,
                        )
                        .await;
                }

                if let Some(tool_call) = original_pending_tool_call {
                    // bypass_approval=true: this tool was already manually approved above.
                    return self
                        .route_tool_call_execution(session_id, original_turn_id, tool_call, true)
                        .await;
                }
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
                    session_id: session_id.clone(),
                    turn_id: original_turn_id.clone(),
                    chat_id: original_chat_id,
                    content: format!("Denied: {}", approval.reason),
                    audio_artifact: None,
                    send_text_caption: false,
                    reply_markup: None,
                };

                self.ipc_client
                    .send_request(IpcRequest::EmitTask {
                        target_node: original_reply_to,
                        target_role: original_reply_role,
                        target_guest_id: original_reply_guest_id,
                        task_json: serde_json::to_string(&reply_payload)?,
                    })
                    .await?;

                // Scripted gate denial: fail the turn (default reject_action = fail_turn).
                if approval
                    .approval_id
                    .as_deref()
                    .map(|id| id.starts_with("scripted_gate:"))
                    .unwrap_or(false)
                {
                    return self
                        .fail_active_turn(
                            session_id,
                            original_turn_id,
                            format!("Plan rejected: {}", approval.reason),
                        )
                        .await;
                }
            }
            SlashCommand::Ping
            | SlashCommand::Status
            | SlashCommand::Context
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
            | SlashCommand::ApprovalClear { .. }
            | SlashCommand::Abandon { .. }
            | SlashCommand::Tts { .. }
            | SlashCommand::Voice { .. }
            | SlashCommand::Correct { .. } => {}
        }

        Ok(())
    }

    /// Forward a streaming LLM token fragment to membrane for progressive display.
    ///
    /// model-router emits `action = "streaming_token"` tasks as the Gemini SSE stream
    /// produces tokens.  We look up the session's active turn to find the membrane
    /// routing (final_reply_to / final_reply_role) and re-emit to membrane.  If there is
    /// no active turn for the session the token is silently dropped — the turn already
    /// completed or was abandoned.
    async fn handle_streaming_token(&mut self, task: InboundTaskPayload) -> Result<()> {
        let session_id = match &task.session_id {
            Some(s) => s.clone(),
            None => return Ok(()),
        };
        let token = match &task.content {
            Some(c) if !c.is_empty() => c.clone(),
            _ => return Ok(()),
        };

        // Resolve routing from the active turn of the session.
        let routing = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .map(|t| {
                (
                    t.final_reply_to.clone(),
                    t.final_reply_role.clone(),
                    t.final_reply_guest_id.clone(),
                    t.turn_id.clone(),
                    t.chat_id.clone(),
                )
            });

        let Some((reply_to, reply_role, reply_guest_id, turn_id, chat_id)) = routing else {
            // No active turn — token arrived after turn completed; drop silently.
            return Ok(());
        };

        let task_json = serde_json::to_string(&serde_json::json!({
            "action": "partial_reply",
            "session_id": session_id,
            "turn_id": turn_id,
            "chat_id": chat_id,
            "content": token,
        }))?;

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: reply_to,
                target_role: reply_role,
                target_guest_id: reply_guest_id,
                task_json,
            })
            .await?;

        Ok(())
    }

    /// Store the result from a fire-and-forget graph.query preload into the session snapshot.
    /// The snapshot is then injected as an AgentGraph context layer on the next model call.
    async fn handle_datasource_response(&mut self, task: InboundTaskPayload) -> Result<()> {
        let session_id = match &task.session_id {
            Some(s) if !s.is_empty() => s.clone(),
            _ => return Ok(()),
        };

        // Only handle graph.query preload responses; other datasource responses may arrive
        // for user-initiated tool calls which have their own result path.
        if task.capability.as_deref() != Some("graph.query") {
            return Ok(());
        }

        let data = task
            .result
            .as_ref()
            .and_then(|r| r.get("data"))
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();

        if data.is_empty() {
            return Ok(());
        }

        let snapshot = data
            .iter()
            .filter_map(|node| {
                let label = node.get("label").and_then(|v| v.as_str())?;
                let props = node
                    .get("properties")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                Some(format!("{label}: {props}"))
            })
            .collect::<Vec<_>>()
            .join("\n");

        if let Some(state) = self.sessions.get_mut(&session_id) {
            state.agent_graph_snapshot = Some(snapshot);
        }

        Ok(())
    }

    /// Handle `/approval clear [reason]` — explicitly cancel and drop a parked approval
    /// turn, unblocking the session. Sends a cancellation notice to the original chat so
    /// the user sees it instead of "typing" forever, then acks the operator's command.
    async fn handle_approval_clear(
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
        let reason = if let SlashCommand::ApprovalClear { reason } = &command {
            reason.clone()
        } else {
            None
        };

        // Snapshot the parked turn's routing info before we clear it.
        let parked_info = self.sessions.get(&session_id).and_then(|state| {
            state.parked_approval_turn.as_ref().map(|turn| {
                (
                    turn.task_id,
                    turn.turn_id.clone(),
                    turn.chat_id.clone(),
                    turn.final_reply_to.clone(),
                    turn.final_reply_role.clone(),
                    turn.final_reply_guest_id.clone(),
                )
            })
        });

        let Some((
            original_task_id,
            original_turn_id,
            original_chat_id,
            original_reply_to,
            original_reply_role,
            original_reply_guest_id,
        )) = parked_info
        else {
            // Nothing parked — ack the command and bail.
            return self
                .complete_command_without_turn(
                    command_task_id,
                    session_id,
                    command_turn_id,
                    command_chat_id,
                    command_reply_to,
                    command_reply_role,
                    command_reply_guest_id,
                    "No parked approval to clear.".into(),
                    None,
                    None,
                )
                .await;
        };

        // Clear the parked turn and write a clean checkpoint.
        let (checkpoint_memory_type, checkpoint_json, index_state) = {
            let state = self
                .sessions
                .get_mut(&session_id)
                .expect("session should exist while clearing parked approval");
            state.parked_approval_turn = None;
            state.parked_approval_since = None;
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

        // Fail the original task so the task ledger is closed.
        let cancel_reason = reason
            .as_deref()
            .unwrap_or("operator cancelled approval request");
        let _ = self
            .ipc_client
            .send_request(IpcRequest::FailTask {
                task_id: original_task_id,
                error_code: "APPROVAL_CANCELLED".into(),
                reason: cancel_reason.to_string(),
            })
            .await?;

        // Notify the original chat that the approval request was cancelled.
        let original_notice = if let Some(r) = reason.as_deref() {
            format!("Approval request cancelled: {r}")
        } else {
            "Approval request cancelled by operator.".into()
        };
        let notice_payload = FinalReplyPayload {
            action: "send_reply",
            session_id: session_id.clone(),
            turn_id: original_turn_id,
            chat_id: original_chat_id,
            content: original_notice,
            audio_artifact: None,
            send_text_caption: false,
            reply_markup: None,
        };
        let _ = self
            .ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: original_reply_to,
                target_role: original_reply_role,
                target_guest_id: original_reply_guest_id,
                task_json: serde_json::to_string(&notice_payload)?,
            })
            .await?;

        info!(
            session_id = %session_id,
            original_task_id = %original_task_id,
            "Parked approval turn cleared by operator."
        );

        // Ack the operator's command turn.
        self.complete_command_without_turn(
            command_task_id,
            session_id,
            command_turn_id,
            command_chat_id,
            command_reply_to,
            command_reply_role,
            command_reply_guest_id,
            "Parked approval cleared.".into(),
            None,
            None,
        )
        .await
    }

    /// Receive an inbound `handoff_bundle` task — the hotel is asking this philote
    /// to take over the session in the requested role. Apply the role context swap
    /// and acknowledge back to the original turn's reply target.
    pub async fn handle_handoff_bundle(
        &mut self,
        task: InboundTaskPayload,
        task_id: Uuid,
    ) -> Result<()> {
        let session_id = task.session_id_or_default(&self.agent_id);
        let turn_id = task.turn_id.clone().unwrap_or_else(|| task_id.to_string());

        let bundle: HandoffBundle = match task.handoff_bundle {
            Some(b) => b,
            None => {
                warn!(
                    "Received handoff_bundle for session [{}] with no parseable bundle; ignoring.",
                    session_id
                );
                return Ok(());
            }
        };

        let to_role = match bundle.to_role.as_deref() {
            Some(r) => r.to_string(),
            None => {
                warn!(
                    "handoff_bundle for session [{}] has no to_role; ignoring.",
                    session_id
                );
                return Ok(());
            }
        };

        info!(
            "Philote [{}] applying role context swap: {:?} → {} for session [{}]",
            self.agent_id, bundle.from_role, to_role, session_id
        );

        self.ensure_session_loaded(&session_id, "handoff").await?;

        let role_config = self.configured_roles.get(&to_role).cloned();

        {
            let state = self.sessions.entry(session_id.clone()).or_insert_with(|| {
                SessionState::new(session_id.clone(), self.agent_id.clone(), "handoff".into())
            });

            let activation = crate::session::RoleActivation {
                role_name: to_role.clone(),
                active_incarnation_id: None,
                activation_reason: bundle
                    .handoff_reason
                    .clone()
                    .unwrap_or_else(|| "handoff".into()),
                requested_by: bundle.from_role.clone(),
                role_addendum: role_config
                    .as_ref()
                    .and_then(|c| c.role_identity_addendum.clone()),
                role_manifest: role_config.as_ref().and_then(|c| c.role_manifest.clone()),
                base_identity_ref: None,
                activation_requester_class: Some("role_handoff".into()),
                activation_policy_owner: None,
                toolset_profile_ref: role_config.as_ref().map(|c| c.toolset_profile.clone()),
                skillset_profile_ref: None,
                effective_skillset: vec![],
                effective_skill_guidance: vec![],
                working_memory_policy: None,
                memory_projection_policy: None,
                turn_loop_config: role_config.as_ref().map(|c| c.turn_loop_config.clone()),
            };

            state.role_activation = Some(activation);
            // Carry over the handoff context as the working summary for the new role.
            if let Some(summary) = bundle.working_summary {
                if state.active_turn.is_none() {
                    state.last_handoff_summary = Some(summary);
                }
            }
        }

        let reply = format!("Switched to role {}.", to_role);
        self.complete_local_command(session_id, turn_id, reply)
            .await
    }

    /// Receive an inbound `handoff_return` task — a role is handing control back
    /// to the orchestrator. Clear role activation and acknowledge.
    pub async fn handle_handoff_return(
        &mut self,
        task: InboundTaskPayload,
        task_id: Uuid,
    ) -> Result<()> {
        let session_id = task.session_id_or_default(&self.agent_id);
        let turn_id = task.turn_id.clone().unwrap_or_else(|| task_id.to_string());

        info!(
            "Philote [{}] handling handoff_return for session [{}]",
            self.agent_id, session_id
        );

        self.ensure_session_loaded(&session_id, "handoff_return")
            .await?;

        let previous_role = {
            let state = self.sessions.entry(session_id.clone()).or_insert_with(|| {
                SessionState::new(
                    session_id.clone(),
                    self.agent_id.clone(),
                    "handoff_return".into(),
                )
            });
            let prev = state.role_activation.as_ref().map(|r| r.role_name.clone());
            state.role_activation = None;
            prev
        };

        let reply = match previous_role {
            Some(role) => format!("Returned from role {}. Back to orchestrator.", role),
            None => "Back to orchestrator.".into(),
        };
        self.complete_local_command(session_id, turn_id, reply)
            .await
    }

    async fn handle_role_command(
        &mut self,
        command_task_id: Uuid,
        session_id: String,
        command_turn_id: String,
        command_chat_id: String,
        command: SlashCommand,
    ) -> Result<()> {
        // Set by the Roles arm of the match below to carry the inline keyboard to the reply.
        let mut roles_keyboard_holder: Option<serde_json::Value> = None;

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
                // Build an inline keyboard with one button per role so the user can
                // tap directly in Telegram to fire `/role <name>`.
                let keyboard_rows: Vec<Vec<serde_json::Value>> = roles
                    .iter()
                    .filter_map(|r| {
                        let role_name = r.get("role_name")?.as_str()?;
                        Some(vec![serde_json::json!({
                            "text": format!("🎭 {role_name}"),
                            "callback_data": format!("/role {role_name}"),
                        })])
                    })
                    .collect();
                let roles_keyboard = if keyboard_rows.is_empty() {
                    None
                } else {
                    Some(serde_json::json!({ "inline_keyboard": keyboard_rows }))
                };
                roles_keyboard_holder = roles_keyboard;
                let active_role_name = active_incarnation_id
                    .as_deref()
                    .and_then(|id| id.rsplit(':').next())
                    .unwrap_or("orchestrator");
                let reply_text = if roles.is_empty() {
                    format!("Active: {active_role_name}. No configured roles.")
                } else {
                    format!("Active: {active_role_name}")
                };
                (
                    reply_text,
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

        self.complete_local_command_with_markup(
            session_id,
            command_turn_id,
            reply_content,
            roles_keyboard_holder,
        )
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
        if let Some(state) = self.sessions.get_mut(&session_id) {
            state.clear_handoff_summary();
        }

        let response_route = Some(model_response_route(None, None, &Map::new(), &Vec::new()));
        let ligand = None;
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
            response_route,
            ligand,
            provider_options: Map::new(),
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

    /// Emit a `transcription_correction` envelope to the `router-listener` guest so it can
    /// apply the operator correction to the stored Whisper training sample and mark it
    /// `training_eligible = true`.
    #[allow(clippy::too_many_arguments)]
    async fn handle_correction_command(
        &mut self,
        task_id: Uuid,
        session_id: String,
        command_turn_id: String,
        command_chat_id: String,
        final_reply_to: String,
        final_reply_role: String,
        final_reply_guest_id: Option<String>,
        voice_turn_id: String,
        corrected_text: String,
    ) -> Result<()> {
        let correction_json = serde_json::to_string(&serde_json::json!({
            "kind": "transcription_correction",
            "session_id": session_id,
            "turn_id": voice_turn_id,
            "corrected_transcript": corrected_text,
            "correction_source": "operator",
        }))?;

        let _ = self
            .ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: local_node_id(),
                target_role: "router-listener".to_string(),
                target_guest_id: None,
                task_json: correction_json,
            })
            .await;

        // Mark the command turn as started before completing.
        let _ = self
            .ipc_client
            .send_request(IpcRequest::UpdateTask {
                task_id,
                state: "transcription_correction_submitted".into(),
                payload: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": command_turn_id,
                    "chat_id": command_chat_id,
                    "voice_turn_id": voice_turn_id,
                }),
            })
            .await;

        self.complete_command_without_turn(
            task_id,
            session_id,
            command_turn_id,
            command_chat_id,
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
            format!("Correction submitted for turn `{voice_turn_id}`."),
            None,
            None,
        )
        .await
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

        // Handle /voice — switches provider and optional voice_id for this session.
        if let SlashCommand::Voice {
            ref provider,
            ref voice_id,
        } = command
        {
            let reply = if let Some(state) = self.sessions.get_mut(&session_id) {
                let policy = &mut state.agent_profile.voice_response_policy;
                match provider.as_deref() {
                    Some(p) => {
                        let resolved = policy.switch_provider(p, voice_id.as_deref());
                        match (voice_id.as_deref(), resolved.as_deref()) {
                            (Some(vid), _) => {
                                format!("Switched to {p} voice, using {vid} for this session.")
                            }
                            (None, Some(stored)) => {
                                format!("Switched to {p} voice, using stored ID {stored}.")
                            }
                            (None, None) => {
                                format!(
                                    "Switched to {p} voice. No voice ID stored for {p} — use `/voice {p} <id>` to set one."
                                )
                            }
                        }
                    }
                    None => {
                        let current_provider = policy.provider.as_deref().unwrap_or("default");
                        let current_voice = policy.effective_voice_id().unwrap_or("default");
                        format!(
                            "Current voice provider: {current_provider}, voice: {current_voice}."
                        )
                    }
                }
            } else {
                "No active session.".to_string()
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
                .complete_local_command(session_id, command_turn_id, reply)
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
                SlashCommand::Context => (
                    state.context_breakdown_text(),
                    "context_breakdown_reported",
                    serde_json::json!({
                        "session_id": session_id,
                        "turn_id": command_turn_id,
                        "chat_id": command_chat_id,
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
                | SlashCommand::Voice { .. }
                | SlashCommand::Role { .. }
                | SlashCommand::Roles
                | SlashCommand::Back
                | SlashCommand::Approve { .. }
                | SlashCommand::Deny { .. }
                | SlashCommand::ApprovalClear { .. }
                | SlashCommand::Abandon { .. }
                | SlashCommand::Correct { .. } => (
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

    async fn handle_read_only_session_command(
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
        let Some(state) = self.sessions.get(&session_id) else {
            warn!(
                "Received read-only session command for unknown session {}",
                session_id
            );
            return Ok(());
        };

        let (reply_content, update_state, payload) = match command {
            SlashCommand::Status => (
                state.session_status_text(),
                Some("session_status_reported"),
                Some(serde_json::json!({
                    "session_id": session_id,
                    "turn_id": command_turn_id,
                    "chat_id": command_chat_id,
                    "session_status": state.status,
                    "bindings": state.bindings,
                    "tool_assembly": state.tool_assembly,
                    "approval_policy": state.approval_policy,
                })),
            ),
            SlashCommand::Context => (
                state.context_breakdown_text(),
                Some("context_breakdown_reported"),
                Some(serde_json::json!({
                    "session_id": session_id,
                    "turn_id": command_turn_id,
                    "chat_id": command_chat_id,
                })),
            ),
            _ => {
                warn!("Unsupported read-only session command: {:?}", command);
                return Ok(());
            }
        };

        self.complete_command_without_turn(
            command_task_id,
            session_id,
            command_turn_id,
            command_chat_id,
            command_reply_to,
            command_reply_role,
            command_reply_guest_id,
            reply_content,
            update_state,
            payload,
        )
        .await
    }

    async fn complete_command_without_turn(
        &mut self,
        command_task_id: Uuid,
        session_id: String,
        turn_id: String,
        chat_id: String,
        reply_to: String,
        reply_role: String,
        reply_guest_id: Option<String>,
        reply_content: String,
        update_state: Option<&str>,
        payload: Option<serde_json::Value>,
    ) -> Result<()> {
        if let Some(state) = update_state {
            let _ = self
                .ipc_client
                .send_request(IpcRequest::UpdateTask {
                    task_id: command_task_id,
                    state: state.into(),
                    payload: payload.unwrap_or_else(|| {
                        serde_json::json!({
                            "session_id": session_id,
                            "turn_id": turn_id,
                            "chat_id": chat_id,
                        })
                    }),
                })
                .await?;
        }

        let _ = self
            .ipc_client
            .send_request(IpcRequest::CompleteTask {
                task_id: command_task_id,
                result: serde_json::json!({
                    "session_id": session_id,
                    "turn_id": turn_id,
                    "chat_id": chat_id,
                    "content": reply_content,
                }),
            })
            .await?;

        let reply_payload = FinalReplyPayload {
            action: "send_reply",
            session_id,
            turn_id,
            chat_id,
            content: reply_content,
            audio_artifact: None,
            send_text_caption: false,
            reply_markup: None,
        };

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: reply_to,
                target_role: reply_role,
                target_guest_id: reply_guest_id,
                task_json: serde_json::to_string(&reply_payload)?,
            })
            .await?;
        Ok(())
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

    /// Build the Telegram-facing approval message from the approval request plus any
    /// available plan and tool-call context, so the operator can make an informed decision.
    fn format_approval_message(
        approval: &ApprovalRequest,
        active_plan: Option<&ActivePlan>,
        pending_tool_call: Option<&ToolCall>,
    ) -> String {
        let mut lines: Vec<String> = Vec::new();

        if let Some(plan) = active_plan {
            lines.push(format!("Goal: {}", plan.goal));
            if let Some(step) = plan.steps.iter().find(|s| s.status == "in_progress") {
                let tool_hint = step
                    .tool_name
                    .as_deref()
                    .map(|t| format!(" ({})", t))
                    .unwrap_or_default();
                lines.push(format!(
                    "Step {}: {}{}",
                    step.id, step.description, tool_hint
                ));
            }
        }

        if let Some(tool) = pending_tool_call {
            lines.push(format!("Tool: {}", tool.tool_name));
            let args_summary = Self::summarize_tool_args(&tool.tool_name, &tool.arguments);
            if !args_summary.is_empty() {
                lines.push(args_summary);
            }
        }

        lines.push(approval.reason.clone());
        lines.join("\n")
    }

    /// Produce a short, human-readable summary of the most relevant tool arguments.
    fn summarize_tool_args(tool_name: &str, args: &serde_json::Value) -> String {
        match tool_name {
            "bash.exec" => {
                if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                    let s = if cmd.len() > 300 { &cmd[..300] } else { cmd };
                    return format!("$ {}", s);
                }
            }
            "role.configure" | "role.handoff" => {
                if let Some(role) = args.get("role_name").and_then(|v| v.as_str()) {
                    return format!("role: {}", role);
                }
            }
            "rule.propose" => {
                if let Some(rule) = args.get("rule").and_then(|v| v.as_str()) {
                    let s = if rule.len() > 300 { &rule[..300] } else { rule };
                    return format!("rule: {}", s);
                }
            }
            _ => {}
        }
        let json = args.to_string();
        if json == "{}" || json == "null" {
            return String::new();
        }
        if json.len() > 300 {
            format!("{}…", &json[..300])
        } else {
            json
        }
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
                    handoff_bundle: None,
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
                    ..Default::default()
                })
                .await
            }
            "hotel.status" => {
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::GetHotelStatus)
                    .await
                {
                    Ok(IpcResponse::Standard {
                        ok: true,
                        data: Some(data),
                        ..
                    }) => {
                        let text = serde_json::to_string_pretty(&data)
                            .unwrap_or_else(|_| data.to_string());
                        (text, None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("Hotel status unavailable.".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("hotel.status: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }
            "hotel.logs" => {
                let lines = payload
                    .arguments
                    .get("lines")
                    .and_then(|v| v.as_u64())
                    .map(|v| v.min(500) as u32)
                    .unwrap_or(50);
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::GetHotelLogs { lines })
                    .await
                {
                    Ok(IpcResponse::Standard {
                        ok: true,
                        data: Some(data),
                        ..
                    }) => {
                        let log = data
                            .get("log")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        (log, None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("Hotel logs unavailable.".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("hotel.logs: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
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
                    handoff_bundle: None,
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
                    ..Default::default()
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

                let role_identity_addendum = args
                    .get("role_identity_addendum")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let role_manifest = args
                    .get("role_manifest")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let is_admin = args
                    .get("is_admin")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let inactive_ttl_seconds =
                    args.get("inactive_ttl_seconds").and_then(|v| v.as_u64());
                let iteration_cap = args
                    .get("iteration_cap")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                let approval_policy = args
                    .get("approval_policy")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let model_profile = args
                    .get("model_profile")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let context_window_policy = args
                    .get("context_window_policy")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                // Read the active persona role from session state to pass as calling authority.
                // Falls back to "orchestrator" when no role is active (default persona).
                let calling_role = self
                    .sessions
                    .get(&payload.session_id)
                    .and_then(|s| s.role_activation.as_ref())
                    .map(|r| r.role_name.clone())
                    .unwrap_or_else(|| "orchestrator".to_string());

                let req = IpcRequest::ConfigureRole {
                    agent_id: self.agent_id.clone(),
                    role_name: role_name.clone(),
                    guest_id: format!("{}:{}", self.agent_id, role_name),
                    calling_role,
                    toolset_profile,
                    role_identity_addendum,
                    role_manifest,
                    is_admin,
                    inactive_ttl_seconds,
                    iteration_cap,
                    approval_policy,
                    model_profile,
                    context_window_policy,
                };

                let (content, tool_err) = match self.ipc_client.send_request(req).await {
                    Ok(IpcResponse::ConfigureRoleOk { role_name: name }) => {
                        self.configured_roles.insert(
                            name.clone(),
                            CachedRoleConfig {
                                toolset_profile: args
                                    .get("toolset_profile")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("default")
                                    .to_string(),
                                role_identity_addendum: args
                                    .get("role_identity_addendum")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                                role_manifest: args
                                    .get("role_manifest")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                                iteration_cap: args
                                    .get("iteration_cap")
                                    .and_then(|v| v.as_u64())
                                    .map(|v| v as u32),
                                approval_policy: args
                                    .get("approval_policy")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                                turn_loop_config: args
                                    .get("turn_loop_config")
                                    .and_then(|v| {
                                        serde_json::from_value::<
                                            ansible_mesh_core::graph::TurnLoopConfig,
                                        >(v.clone())
                                        .ok()
                                    })
                                    .unwrap_or_default(),
                            },
                        );
                        // Refresh the delegation roster so new/updated roles appear
                        // in the system prompt for subsequent sessions without a restart.
                        self.fetch_role_names().await;
                        (
                            format!("Successfully configured role incarnation for '{}'.", name),
                            None,
                        )
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "IPC_ERROR", msg);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "role.configure: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("role.configure: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
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
                    ..Default::default()
                })
                .await
            }
            // role.create_or_update is the governed workflow surface for role authoring.
            // It validates the same required fields as role.configure and resolves through
            // the same IpcRequest::ConfigureRole hotel path — no external subscriber needed.
            "role.create_or_update" => {
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
                                            "role.create_or_update: missing required argument '{}'",
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

                if args.get("reasoning").and_then(|v| v.as_object()).is_none() {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "role.create_or_update: missing required object argument 'reasoning'"
                                .into(),
                        )
                        .await;
                }

                let role_identity_addendum = args
                    .get("role_identity_addendum")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let role_manifest = args
                    .get("role_manifest")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let is_admin = args
                    .get("is_admin")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let inactive_ttl_seconds =
                    args.get("inactive_ttl_seconds").and_then(|v| v.as_u64());
                let iteration_cap = args
                    .get("iteration_cap")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32);
                let approval_policy = args
                    .get("approval_policy")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let model_profile = args
                    .get("model_profile")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let context_window_policy = args
                    .get("context_window_policy")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let calling_role = self
                    .sessions
                    .get(&payload.session_id)
                    .and_then(|s| s.role_activation.as_ref())
                    .map(|r| r.role_name.clone())
                    .unwrap_or_else(|| "orchestrator".to_string());

                let req = IpcRequest::ConfigureRole {
                    agent_id: self.agent_id.clone(),
                    role_name: role_name.clone(),
                    guest_id: format!("{}:{}", self.agent_id, role_name),
                    calling_role,
                    toolset_profile,
                    role_identity_addendum,
                    role_manifest,
                    is_admin,
                    inactive_ttl_seconds,
                    iteration_cap,
                    approval_policy,
                    model_profile,
                    context_window_policy,
                };

                let (content, tool_err) = match self.ipc_client.send_request(req).await {
                    Ok(IpcResponse::ConfigureRoleOk { role_name: name }) => {
                        self.configured_roles.insert(
                            name.clone(),
                            CachedRoleConfig {
                                toolset_profile: args
                                    .get("toolset_profile")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("default")
                                    .to_string(),
                                role_identity_addendum: args
                                    .get("role_identity_addendum")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                                role_manifest: args
                                    .get("role_manifest")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                                iteration_cap: args
                                    .get("iteration_cap")
                                    .and_then(|v| v.as_u64())
                                    .map(|v| v as u32),
                                approval_policy: args
                                    .get("approval_policy")
                                    .and_then(|v| v.as_str())
                                    .map(str::to_string),
                                turn_loop_config: args
                                    .get("turn_loop_config")
                                    .and_then(|v| {
                                        serde_json::from_value::<
                                            ansible_mesh_core::graph::TurnLoopConfig,
                                        >(v.clone())
                                        .ok()
                                    })
                                    .unwrap_or_default(),
                            },
                        );
                        self.fetch_role_names().await;
                        (
                            format!("Role '{}' created/updated successfully.", name),
                            None,
                        )
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "IPC_ERROR", msg);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "role.create_or_update: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("role.create_or_update: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
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
                    ..Default::default()
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

                let skill_name = require_str_arg!("skill_name");
                let description = require_str_arg!("description");
                let subagent_kind = require_str_arg!("subagent_kind");
                let goal = require_str_arg!("goal");

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
                let allowed_tools = str_vec("allowed_tools");
                let allowed_classes = str_vec("allowed_classes");

                let response = self
                    .ipc_client
                    .send_request(IpcRequest::RegisterSkill {
                        skill_name: skill_name.clone(),
                        description,
                        subagent_kind,
                        goal,
                        allowed_tools,
                        allowed_classes,
                        hook_subscriptions: vec![],
                        completion_route: Default::default(),
                        failure_route: Default::default(),
                        idle_behavior: Default::default(),
                        lease_terms: Default::default(),
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
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "IPC_ERROR", msg);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "skill.register: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("skill.register: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
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
                    ..Default::default()
                })
                .await
            }

            "skill.list" => {
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::ListSkills {})
                    .await
                {
                    Ok(IpcResponse::SkillList { skills }) => {
                        let msg = if skills.is_empty() {
                            "No skills registered.".to_string()
                        } else {
                            let lines: Vec<String> = skills
                                .iter()
                                .map(|s| {
                                    let name =
                                        s.get("skill_name").and_then(|v| v.as_str()).unwrap_or("?");
                                    let state = s
                                        .get("validation_state")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("?");
                                    let desc =
                                        s.get("description").and_then(|v| v.as_str()).unwrap_or("");
                                    let brief: String = desc.chars().take(80).collect();
                                    format!("- {} [{}] — {}", name, state, brief)
                                })
                                .collect();
                            format!("Registered skills:\n{}", lines.join("\n"))
                        };
                        (msg, None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "IPC_ERROR", msg);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "skill.list: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("skill.list: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
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
                    ..Default::default()
                })
                .await
            }

            "skill.assign" | "skill.revoke" => {
                let args = &payload.arguments;
                let op = payload.tool_name.as_str();

                let role_name = match args.get("role_name").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                format!("{op}: missing required argument 'role_name'"),
                            )
                            .await;
                    }
                };
                let skill_name = match args.get("skill_name").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                format!("{op}: missing required argument 'skill_name'"),
                            )
                            .await;
                    }
                };

                let req = if op == "skill.assign" {
                    IpcRequest::AssignSkill {
                        agent_id: self.agent_id.clone(),
                        role_name: role_name.clone(),
                        skill_name: skill_name.clone(),
                    }
                } else {
                    IpcRequest::RevokeSkill {
                        agent_id: self.agent_id.clone(),
                        role_name: role_name.clone(),
                        skill_name: skill_name.clone(),
                    }
                };

                let (content, tool_err) = match self.ipc_client.send_request(req).await {
                    Ok(IpcResponse::SkillAssigned {
                        role_name: rn,
                        skill_name: sn,
                        operation,
                    }) => (format!("Skill '{}' {} role '{}'.", sn, operation, rn), None),
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "IPC_ERROR", msg);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            &format!("{op}: unexpected hotel response"),
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("{op}: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
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
                    ..Default::default()
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
                    parent_role: "agent".to_string(),
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
                    }) => (
                        format!(
                            "Subagent spawned.\nGuest ID: {}\nLease expires at: {} (epoch {})",
                            subagent_guest_id,
                            confirmed_lease.lease_expires_at,
                            confirmed_lease.lease_epoch,
                        ),
                        None,
                    ),
                    Ok(IpcResponse::SpawnSubagentProposal {
                        subagent_guest_id,
                        confirmed_lease,
                        delta,
                    }) => (
                        format!(
                            "Subagent spawned (TTL adjusted: {}s → {}s).\nGuest ID: {}\nLease expires at: {}",
                            delta.requested_ttl,
                            delta.confirmed_ttl,
                            subagent_guest_id,
                            confirmed_lease.lease_expires_at,
                        ),
                        None,
                    ),
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", "IPC_ERROR", msg);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "subagent.spawn: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("subagent.spawn: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
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
                    ..Default::default()
                })
                .await
            }

            "role.set_home" => {
                let args = payload.arguments.as_object();
                let role_name = args
                    .and_then(|a| a.get("role_name"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let target_hotel = args
                    .and_then(|a| a.get("target_hotel"))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty() && s.to_lowercase() != "null")
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
                            "role.set_home: missing required argument 'role_name'".into(),
                        )
                        .await;
                };
                let Some(reason) = reason else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "role.set_home: missing required argument 'reason'".into(),
                        )
                        .await;
                };

                // Resolve the calling role from active session state.
                let calling_role = self
                    .sessions
                    .get(&payload.session_id)
                    .and_then(|s| s.role_activation.as_ref())
                    .map(|r| r.role_name.clone())
                    .unwrap_or_else(|| "orchestrator".into());

                let _ = reason; // recorded for operator visibility in approval surface
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::SetRoleHome {
                        agent_id: self.agent_id.clone(),
                        role_name: role_name.clone(),
                        calling_role,
                        target_hotel: target_hotel.clone(),
                    })
                    .await
                {
                    Ok(IpcResponse::RoleHomeSet {
                        role_name: name,
                        home_node,
                    }) => {
                        let msg = match home_node {
                            Some(ref node) => format!(
                                "Role '{name}' pinned to hotel '{node}'. Next handoff.to_role will route there."
                            ),
                            None => {
                                format!("Role '{name}' home cleared — will run on authority hotel.")
                            }
                        };
                        (msg, None)
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::tool_execution(
                            "role.set_home",
                            msg,
                            Some("SET_ROLE_HOME_REJECTED"),
                        );
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "role.set_home: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("role.set_home: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };

                if let Some(err) = tool_err {
                    self.handle_tool_result(InboundTaskPayload {
                        action: Some("tool_result".into()),
                        agent_action: None,
                        handoff_bundle: None,
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
                        error: Some(err),
                        tool_name: Some(payload.tool_name),
                        arguments: None,
                        final_reply_to: Some(payload.final_reply_to),
                        final_reply_role: Some(payload.final_reply_role),
                        final_reply_guest_id: payload.final_reply_guest_id,
                        ..Default::default()
                    })
                    .await
                } else {
                    self.complete_local_command(payload.session_id, payload.turn_id, content)
                        .await
                }
            }

            "role.list" => {
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::ListRoleIncarnations {
                        agent_id: self.agent_id.clone(),
                    })
                    .await
                {
                    Ok(IpcResponse::Standard {
                        ok: true,
                        data: Some(data),
                        ..
                    }) => {
                        let roles = data
                            .get("roles")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default();
                        if roles.is_empty() {
                            ("No roles configured for this agent.".into(), None)
                        } else {
                            let mut lines = vec![format!(
                                "Role roster for {} ({} roles):",
                                self.agent_id,
                                roles.len()
                            )];
                            for role in &roles {
                                let name = role
                                    .get("role_name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("?");
                                let profile = role
                                    .get("toolset_profile")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("?");
                                let state = role
                                    .get("readiness_state")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown");
                                let home = role
                                    .get("home_node")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("(authority hotel)");
                                lines.push(format!(
                                    "  {name}  profile={profile}  state={state}  home={home}"
                                ));
                            }
                            (lines.join("\n"), None)
                        }
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("Role list unavailable.".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("role.list: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "training.list" => {
                use ansible_mesh_core::whisper_training::TrainingFilter;
                use philotic_client::IpcRequest;
                let args = payload.arguments.as_object();
                let limit = args
                    .and_then(|a| a.get("limit"))
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize)
                    .unwrap_or(20);
                let filter_str = args
                    .and_then(|a| a.get("filter"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("all");
                let filter = match filter_str {
                    "uncorrected" => TrainingFilter::Uncorrected,
                    "eligible" => TrainingFilter::Eligible,
                    "exported" => TrainingFilter::Exported,
                    _ => TrainingFilter::All,
                };
                let agent_id_filter = args
                    .and_then(|a| a.get("agent_id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::ListTrainingSamples {
                        agent_id: agent_id_filter,
                        limit,
                        filter,
                    })
                    .await
                {
                    Ok(IpcResponse::Standard {
                        ok: true,
                        data: Some(data),
                        ..
                    }) => {
                        let count = data.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
                        let samples = data
                            .get("samples")
                            .and_then(|v| v.as_array())
                            .cloned()
                            .unwrap_or_default();
                        if samples.is_empty() {
                            (
                                format!("No training samples found (filter: {filter_str})."),
                                None,
                            )
                        } else {
                            let lines: Vec<String> = std::iter::once(format!(
                                "{count} training sample(s) (filter: {filter_str}):"
                            ))
                            .chain(samples.iter().map(|s| {
                                let sid =
                                    s.get("sample_id").and_then(|v| v.as_str()).unwrap_or("?");
                                let turn = s.get("turn_id").and_then(|v| v.as_str()).unwrap_or("?");
                                let raw = s
                                    .get("raw_transcript")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                let corrected =
                                    s.get("corrected_transcript").and_then(|v| v.as_str());
                                let eligible = s
                                    .get("training_eligible")
                                    .and_then(|v| v.as_bool())
                                    .unwrap_or(false);
                                let exported =
                                    s.get("exported_at").and_then(|v| v.as_u64()).is_some();
                                let state = if exported {
                                    "exported"
                                } else if eligible {
                                    "eligible"
                                } else if corrected.is_some() {
                                    "corrected"
                                } else {
                                    "uncorrected"
                                };
                                let transcript = corrected.unwrap_or(raw);
                                format!(
                                    "  [{state}] {sid}  turn={turn}  transcript={transcript:.80}"
                                )
                            }))
                            .collect();
                            (lines.join("\n"), None)
                        }
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("training.list: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("training.list: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "training.correct" => {
                use philotic_client::IpcRequest;
                let args = payload.arguments.as_object();
                let turn_id = args
                    .and_then(|a| a.get("turn_id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let corrected_transcript = args
                    .and_then(|a| a.get("corrected_transcript"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let (Some(turn_id), Some(corrected_transcript)) = (turn_id, corrected_transcript)
                else {
                    let err = TaskErrorPayload::ipc_failure(
                        "philote",
                        "MISSING_ARGS",
                        "training.correct requires 'turn_id' and 'corrected_transcript'",
                    );
                    return self
                        .handle_tool_result(InboundTaskPayload {
                            action: Some("tool_result".into()),
                            source: Some("agent".into()),
                            session_id: Some(payload.session_id),
                            turn_id: Some(payload.turn_id),
                            chat_id: Some(payload.chat_id),
                            content: Some(err.display_message()),
                            error: Some(err),
                            tool_name: Some(payload.tool_name),
                            final_reply_to: Some(payload.final_reply_to),
                            final_reply_role: Some(payload.final_reply_role),
                            final_reply_guest_id: payload.final_reply_guest_id,
                            ..Default::default()
                        })
                        .await;
                };
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::CorrectTrainingSample {
                        turn_id: turn_id.clone(),
                        corrected_transcript,
                    })
                    .await
                {
                    Ok(IpcResponse::Standard { ok: true, .. }) => (
                        format!(
                            "Correction applied to turn '{turn_id}'. Sample marked training_eligible."
                        ),
                        None,
                    ),
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("training.correct: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("training.correct: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "training.export" => {
                use ansible_mesh_core::whisper_training::TrainingExportFormat;
                use philotic_client::IpcRequest;
                let args = payload.arguments.as_object();
                let format_str = args
                    .and_then(|a| a.get("format"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("huggingface");
                let output_path = args
                    .and_then(|a| a.get("output_path"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let limit = args
                    .and_then(|a| a.get("limit"))
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);
                let Some(output_path) = output_path else {
                    let err = TaskErrorPayload::ipc_failure(
                        "philote",
                        "MISSING_ARGS",
                        "training.export requires 'format' and 'output_path'",
                    );
                    return self
                        .handle_tool_result(InboundTaskPayload {
                            action: Some("tool_result".into()),
                            source: Some("agent".into()),
                            session_id: Some(payload.session_id),
                            turn_id: Some(payload.turn_id),
                            chat_id: Some(payload.chat_id),
                            content: Some(err.display_message()),
                            error: Some(err),
                            tool_name: Some(payload.tool_name),
                            final_reply_to: Some(payload.final_reply_to),
                            final_reply_role: Some(payload.final_reply_role),
                            final_reply_guest_id: payload.final_reply_guest_id,
                            ..Default::default()
                        })
                        .await;
                };
                let format = if format_str == "nemo" {
                    TrainingExportFormat::Nemo
                } else {
                    TrainingExportFormat::HuggingFace
                };
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::ExportTrainingSamples {
                        format,
                        output_path: output_path.clone(),
                        limit,
                    })
                    .await
                {
                    Ok(IpcResponse::Standard { ok: true, data, .. }) => {
                        let count = data
                            .as_ref()
                            .and_then(|d| d.get("exported_count"))
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        (
                            format!(
                                "Exported {count} sample(s) ({format_str} format) → {output_path}"
                            ),
                            None,
                        )
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("training.export: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("training.export: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "training.status" => {
                use philotic_client::IpcRequest;
                let args = payload.arguments.as_object();
                let agent_id_filter = args
                    .and_then(|a| a.get("agent_id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::GetTrainingStatus {
                        agent_id: agent_id_filter,
                    })
                    .await
                {
                    Ok(IpcResponse::Standard {
                        ok: true,
                        data: Some(data),
                        ..
                    }) => {
                        let status = data.get("status").cloned().unwrap_or_default();
                        let total = status.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
                        let uncorrected = status
                            .get("uncorrected")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        let eligible = status.get("eligible").and_then(|v| v.as_u64()).unwrap_or(0);
                        let exported = status.get("exported").and_then(|v| v.as_u64()).unwrap_or(0);
                        let content = format!(
                            "Training data status:\n  total captured: {total}\n  uncorrected: {uncorrected}\n  eligible for export: {eligible}\n  exported: {exported}"
                        );
                        (content, None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("training.status: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("training.status: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "asr.setup" => {
                use philotic_client::IpcRequest;
                let args = payload.arguments.as_object();
                let python_path = args
                    .and_then(|a| a.get("python_path"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let model_name = args
                    .and_then(|a| a.get("model_name"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let auto_install = args
                    .and_then(|a| a.get("auto_install"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::AsrSetup {
                        python_path,
                        model_name,
                        auto_install,
                    })
                    .await
                {
                    Ok(IpcResponse::Standard {
                        ok: true,
                        data: Some(data),
                        ..
                    }) => {
                        let msg = data
                            .get("message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("ASR provider configured.")
                            .to_string();
                        (msg, None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("asr.setup: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("asr.setup: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "asr.status" => {
                use philotic_client::IpcRequest;
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::AsrStatus {})
                    .await
                {
                    Ok(IpcResponse::Standard {
                        ok: true,
                        data: Some(data),
                        ..
                    }) => {
                        let registered = data
                            .get("registered")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let active = data
                            .get("active")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let pid = data.get("pid").and_then(|v| v.as_str()).unwrap_or("none");
                        let nemo = data
                            .get("nemo_available")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        let guest_id = data.get("guest_id").and_then(|v| v.as_str()).unwrap_or("?");
                        let content = format!(
                            "Parakeet ASR status:\n  guest: {guest_id}\n  registered: {registered}\n  active: {active}\n  pid: {pid}\n  nemo_available: {nemo}"
                        );
                        (content, None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("asr.status: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("asr.status: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "cron.list" => {
                use philotic_client::IpcRequest;
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::ListCronJobs)
                    .await
                {
                    Ok(IpcResponse::CronJobList { jobs }) => {
                        if jobs.is_empty() {
                            ("No cron jobs registered on this hotel.".into(), None)
                        } else {
                            let lines: Vec<String> = jobs
                                .iter()
                                .map(|j| {
                                    format!(
                                        "- id={} role={} schedule={} enabled={} next_fire={}",
                                        j.id, j.target_role, j.schedule, j.enabled, j.next_fire_at,
                                    )
                                })
                                .collect();
                            (
                                format!("Cron jobs ({}):\n{}", jobs.len(), lines.join("\n")),
                                None,
                            )
                        }
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("cron.list: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("cron.list: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "cron.register" => {
                use ansible_mesh_core::cron::{CronJob, CronJobSource};
                use philotic_client::IpcRequest;
                let args = &payload.arguments;
                let schedule = match args.get("schedule").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "cron.register: missing required argument 'schedule'".into(),
                            )
                            .await;
                    }
                };
                let target_role = match args.get("target_role").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "cron.register: missing required argument 'target_role'".into(),
                            )
                            .await;
                    }
                };
                let payload_str = match args.get("payload").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "cron.register: missing required argument 'payload'".into(),
                            )
                            .await;
                    }
                };
                let guaranteed = args
                    .get("guaranteed")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);

                let next_fire = match ansible_mesh_core::cron::next_fire_after(&schedule, now_ms) {
                    Ok(t) => t,
                    Err(e) => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                format!("cron.register: invalid schedule — {e}"),
                            )
                            .await;
                    }
                };

                let job = CronJob {
                    id: uuid::Uuid::new_v4().to_string(),
                    schedule,
                    target_role,
                    target_node_id: None,
                    payload: payload_str,
                    guaranteed,
                    enabled: true,
                    last_fired_epoch: None,
                    next_fire_at: next_fire,
                    created_at: now_ms,
                    created_by: CronJobSource::Guest(self.agent_id.clone()),
                };
                let job_id = job.id.clone();

                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::RegisterCronJob { job })
                    .await
                {
                    Ok(IpcResponse::Standard { ok: true, .. }) => {
                        (format!("Cron job registered. id={job_id}"), None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("cron.register: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("cron.register: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "cron.enable" => {
                use philotic_client::IpcRequest;
                let job_id = match payload.arguments.get("job_id").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "cron.enable: missing required argument 'job_id'".into(),
                            )
                            .await;
                    }
                };
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::EnableCronJob {
                        job_id: job_id.clone(),
                    })
                    .await
                {
                    Ok(IpcResponse::Standard { ok: true, .. }) => {
                        (format!("Cron job {job_id} enabled."), None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("cron.enable: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("cron.enable: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "cron.disable" => {
                use philotic_client::IpcRequest;
                let job_id = match payload.arguments.get("job_id").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "cron.disable: missing required argument 'job_id'".into(),
                            )
                            .await;
                    }
                };
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::DisableCronJob {
                        job_id: job_id.clone(),
                    })
                    .await
                {
                    Ok(IpcResponse::Standard { ok: true, .. }) => {
                        (format!("Cron job {job_id} disabled."), None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("cron.disable: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("cron.disable: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "cron.remove" => {
                use philotic_client::IpcRequest;
                let job_id = match payload.arguments.get("job_id").and_then(|v| v.as_str()) {
                    Some(s) => s.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "cron.remove: missing required argument 'job_id'".into(),
                            )
                            .await;
                    }
                };
                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::RemoveCronJob {
                        job_id: job_id.clone(),
                    })
                    .await
                {
                    Ok(IpcResponse::Standard { ok: true, .. }) => {
                        (format!("Cron job {job_id} removed."), None)
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e = TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("cron.remove: unexpected response".into(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("cron.remove: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
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

                let target_focus_framing = args
                    .and_then(|a| a.get("target_focus_framing"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let Some(target_focus_framing) = target_focus_framing else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "handoff.to_role: missing required argument 'target_focus_framing'"
                                .into(),
                        )
                        .await;
                };

                let active_goal = active_goal
                    .map(|g| format!("{}\n\nTarget Focus Framing:\n{}", g, target_focus_framing));

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

                // Retry HandoffPending up to ~3 seconds while the target role materializes.
                const HANDOFF_MAX_RETRIES: u32 = 12;
                const HANDOFF_DEFAULT_WAIT_MS: u64 = 250;
                let handoff_req = IpcRequest::HandoffToRole {
                    session_id: payload.session_id.clone(),
                    role_name: role_name.clone(),
                    handoff_bundle,
                };
                let mut handoff_attempt = 0u32;
                let (content, tool_err) = loop {
                    let resp = self.ipc_client.send_request(handoff_req.clone()).await;
                    match resp {
                        Ok(IpcResponse::HandoffAck {
                            handoff_guest_id, ..
                        }) => {
                            break (
                                format!(
                                    "Handed off to role '{role_name}' (guest {handoff_guest_id})."
                                ),
                                None,
                            );
                        }
                        Ok(IpcResponse::HandoffPending { retry_after_ms, .. }) => {
                            handoff_attempt += 1;
                            if handoff_attempt >= HANDOFF_MAX_RETRIES {
                                let e = TaskErrorPayload::tool_execution(
                                    "handoff.to_role",
                                    format!(
                                        "Role '{role_name}' did not become live after {HANDOFF_MAX_RETRIES} retries"
                                    ),
                                    Some("HANDOFF_TIMEOUT"),
                                );
                                break (e.display_message(), Some(e));
                            }
                            let wait_ms = retry_after_ms.unwrap_or(HANDOFF_DEFAULT_WAIT_MS);
                            tokio::time::sleep(std::time::Duration::from_millis(wait_ms)).await;
                        }
                        Ok(IpcResponse::Error(msg)) => {
                            let e = TaskErrorPayload::tool_execution(
                                "handoff.to_role",
                                msg,
                                Some("HANDOFF_REJECTED"),
                            );
                            break (e.display_message(), Some(e));
                        }
                        Ok(_) => {
                            let e = TaskErrorPayload::ipc_failure(
                                "aiua",
                                "UNEXPECTED_RESPONSE",
                                "handoff.to_role: unexpected hotel response",
                            );
                            break (e.display_message(), Some(e));
                        }
                        Err(e) => {
                            let err = TaskErrorPayload::transport_error(
                                "philote",
                                format!("handoff.to_role: IPC transport error — {e}"),
                            );
                            break (err.display_message(), Some(err));
                        }
                    }
                };

                if let Some(err) = tool_err {
                    self.handle_tool_result(InboundTaskPayload {
                        action: Some("tool_result".into()),
                        agent_action: None,
                        handoff_bundle: None,
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
                        error: Some(err),
                        tool_name: Some(payload.tool_name),
                        arguments: None,
                        final_reply_to: Some(payload.final_reply_to),
                        final_reply_role: Some(payload.final_reply_role),
                        final_reply_guest_id: payload.final_reply_guest_id,
                        ..Default::default()
                    })
                    .await
                } else {
                    self.complete_local_command(payload.session_id, payload.turn_id, content)
                        .await
                }
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
                    Ok(IpcResponse::HandoffBackAck {
                        handoff_guest_id, ..
                    }) => (
                        format!(
                            "Returned control (from guest {handoff_guest_id}). Summary: {summary}"
                        ),
                        None,
                    ),
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::tool_execution(
                            "handoff.back",
                            msg,
                            Some("HANDOFF_BACK_REJECTED"),
                        );
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "handoff.back: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("handoff.back: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };

                if let Some(err) = tool_err {
                    self.handle_tool_result(InboundTaskPayload {
                        action: Some("tool_result".into()),
                        agent_action: None,
                        handoff_bundle: None,
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
                        error: Some(err),
                        tool_name: Some(payload.tool_name),
                        arguments: None,
                        final_reply_to: Some(payload.final_reply_to),
                        final_reply_role: Some(payload.final_reply_role),
                        final_reply_guest_id: payload.final_reply_guest_id,
                        ..Default::default()
                    })
                    .await
                } else {
                    self.complete_local_command(payload.session_id, payload.turn_id, content)
                        .await
                }
            }

            "delegate.to_peer" => {
                let args = payload.arguments.as_object();
                let target_agent_id = args
                    .and_then(|a| a.get("target_agent_id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let task_description = args
                    .and_then(|a| a.get("task_description"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let context_package = args
                    .and_then(|a| a.get("context_package"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let Some(target_agent_id) = target_agent_id else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "delegate.to_peer: missing required argument 'target_agent_id'".into(),
                        )
                        .await;
                };
                let Some(task_description) = task_description else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "delegate.to_peer: missing required argument 'task_description'".into(),
                        )
                        .await;
                };
                let Some(context_package) = context_package else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "delegate.to_peer: missing required argument 'context_package'".into(),
                        )
                        .await;
                };

                let expected_artifacts: Vec<String> = args
                    .and_then(|a| a.get("expected_artifacts"))
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let timeout_secs = args
                    .and_then(|a| a.get("timeout_secs"))
                    .and_then(|v| v.as_u64());

                let _ = self
                    .emit_partial_reply(
                        &payload.session_id,
                        format!(
                            "Let me hand you over to {} to help with this...",
                            target_agent_id
                        ),
                    )
                    .await;

                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::DelegateToPeer {
                        target_agent_id: target_agent_id.clone(),
                        task_description,
                        context_package,
                        chat_id: payload.chat_id.clone(),
                        source: Some("peer".into()),
                        expected_artifacts,
                        timeout_secs,
                    })
                    .await
                {
                    Ok(IpcResponse::DelegationAck {
                        delegation_id,
                        status,
                    }) => (
                        format!(
                            "Delegated task to peer '{target_agent_id}' (delegation {delegation_id}, status: {status})."
                        ),
                        None,
                    ),
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::tool_execution(
                            "delegate.to_peer",
                            msg,
                            Some("DELEGATION_REJECTED"),
                        );
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "delegate.to_peer: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("delegate.to_peer: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
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
                    ..Default::default()
                })
                .await
            }

            "delegate.to_external_cognitive_peer" => {
                let args = payload.arguments.as_object();
                let target_peer_type = args
                    .and_then(|a| a.get("target_peer_type"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let task_description = args
                    .and_then(|a| a.get("task_description"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let context_package = args
                    .and_then(|a| a.get("context_package"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let Some(target_peer_type) = target_peer_type else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "delegate.to_external_cognitive_peer: missing required argument 'target_peer_type'".into(),
                        )
                        .await;
                };
                let Some(task_description) = task_description else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "delegate.to_external_cognitive_peer: missing required argument 'task_description'".into(),
                        )
                        .await;
                };
                let Some(context_package) = context_package else {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "delegate.to_external_cognitive_peer: missing required argument 'context_package'".into(),
                        )
                        .await;
                };

                let expected_artifacts: Vec<String> = args
                    .and_then(|a| a.get("expected_artifacts"))
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();

                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::DelegateToExternalPeer {
                        target_peer_type: target_peer_type.clone(),
                        task_description,
                        context_package,
                        expected_artifacts,
                    })
                    .await
                {
                    Ok(IpcResponse::DelegationAck {
                        delegation_id,
                        status,
                    }) => (
                        format!(
                            "Delegated task to external peer type '{target_peer_type}' (delegation {delegation_id}, status: {status})."
                        ),
                        None,
                    ),
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::tool_execution(
                            "delegate.to_external_cognitive_peer",
                            msg,
                            Some("DELEGATION_REJECTED"),
                        );
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "delegate.to_external_cognitive_peer: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!(
                                "delegate.to_external_cognitive_peer: IPC transport error — {e}"
                            ),
                        );
                        (err.display_message(), Some(err))
                    }
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
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
                    ..Default::default()
                })
                .await
            }

            "bash.exec" => {
                let args = &payload.arguments;

                let command = match args.get("command").and_then(|v| v.as_str()) {
                    Some(c) => c.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "bash.exec: missing required argument 'command'".into(),
                            )
                            .await;
                    }
                };

                let working_dir = args
                    .get("working_dir")
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .or_else(|| {
                        // Fall back to the agent session's import_workspace path if set.
                        self.sessions
                            .get(&payload.session_id)
                            .and_then(|s| s.agent_profile.import_workspace.as_deref())
                            .map(str::to_string)
                    });

                let timeout_secs = args
                    .get("timeout_secs")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(30);

                let exec_result = self
                    .execute_bash_command(command, working_dir, timeout_secs)
                    .await;

                let (content, tool_err) = match exec_result {
                    Ok(json) => (json.to_string(), None),
                    Err(e) => {
                        let err = TaskErrorPayload::tool_execution(
                            "bash.exec",
                            e.to_string(),
                            Some("EXEC_ERROR"),
                        );
                        (err.display_message(), Some(err))
                    }
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
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
                    ..Default::default()
                })
                .await
            }

            "memory.recall" => {
                use memory_core::MemoryEngine as _;

                let query = payload
                    .arguments
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let explicit_limit = payload
                    .arguments
                    .get("limit")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);

                let recall_limit = self
                    .sessions
                    .get(&payload.session_id)
                    .map(|s| s.settings.memory.recall_limit)
                    .unwrap_or(5);
                let limit = explicit_limit.unwrap_or(recall_limit).clamp(1, 20);

                let content = match self.memory_engine_for(&self.agent_id, &self.agent_id) {
                    None => "Memory unavailable: MuninnDB not configured.".to_string(),
                    Some(engine) => match engine
                        .activate(&query, MemoryScope::SelfOnly, Some(limit))
                        .await
                    {
                        Err(e) => format!("memory.recall error: {e}"),
                        Ok(result) if result.engrams.is_empty() => {
                            "No relevant memories found.".to_string()
                        }
                        Ok(result) => {
                            let mut out = format!("{} engram(s) recalled:\n", result.engrams.len());
                            for (i, eng) in result.engrams.iter().enumerate() {
                                out.push_str(&format!(
                                    "{}. [{}] {} — {}\n",
                                    i + 1,
                                    eng.concept,
                                    eng.content,
                                    eng.tags.join(", ")
                                ));
                            }
                            out
                        }
                    },
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
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
                    ..Default::default()
                })
                .await
            }

            "memory.remember" => {
                use memory_core::MemoryEngine as _;

                let concept = payload
                    .arguments
                    .get("concept")
                    .and_then(|v| v.as_str())
                    .unwrap_or("untitled")
                    .to_string();
                let content_str = payload
                    .arguments
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let tags: Vec<String> = payload
                    .arguments
                    .get("tags")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|t| t.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();

                let result_text = match self.memory_engine_for(&self.agent_id, &self.agent_id) {
                    None => "Memory unavailable: MuninnDB not configured.".to_string(),
                    Some(engine) => match engine
                        .remember(MemoryScope::SelfOnly, &concept, &content_str, tags)
                        .await
                    {
                        Ok(engram_ref) => {
                            format!("Stored memory '{}' (id: {}).", concept, engram_ref.id)
                        }
                        Err(e) => format!("memory.remember error: {e}"),
                    },
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    transport: None,
                    chat_id: Some(payload.chat_id),
                    thread_id: None,
                    sender_id: None,
                    sender_username: None,
                    message_kind: None,
                    content: Some(result_text),
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
                    ..Default::default()
                })
                .await
            }

            "rule.propose" => {
                let description = payload
                    .arguments
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let rationale = payload
                    .arguments
                    .get("rationale")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                if description.is_empty() {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "rule.propose: 'description' is required.".into(),
                        )
                        .await;
                }

                let agent_id = self.agent_id.clone();
                let result_text = match self
                    .ipc_client
                    .send_request(IpcRequest::ProposeRule {
                        agent_id: agent_id.clone(),
                        description: description.clone(),
                        rationale: rationale.clone(),
                    })
                    .await
                {
                    Ok(IpcResponse::RuleProposed { rule_id }) => {
                        // Optimistically push the new rule into session state so it is visible
                        // in the next turn's context without requiring a restart.
                        if let Some(state) = self.sessions.get_mut(&payload.session_id) {
                            state.rules.push(serde_json::json!({
                                "rule_id": rule_id,
                                "description": description,
                                "rationale": rationale,
                            }));
                        }
                        format!(
                            "Rule stored permanently (id: {rule_id}). It will be injected into every future cognitive turn."
                        )
                    }
                    Ok(IpcResponse::Standard {
                        ok: true, message, ..
                    }) => message,
                    Ok(IpcResponse::Standard {
                        ok: false, message, ..
                    }) => {
                        format!("rule.propose: hotel rejected — {message}")
                    }
                    Ok(_) => "rule.propose: unexpected response from hotel.".into(),
                    Err(e) => format!("rule.propose: IPC error — {e}"),
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    transport: None,
                    chat_id: Some(payload.chat_id),
                    thread_id: None,
                    sender_id: None,
                    sender_username: None,
                    message_kind: None,
                    content: Some(result_text),
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
                    ..Default::default()
                })
                .await
            }

            "delegate.whisper" => {
                let args = &payload.arguments;
                let role = match args.get("role").and_then(|v| v.as_str()) {
                    Some(r) => r.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "delegate.whisper: missing required argument 'role'".into(),
                            )
                            .await;
                    }
                };
                let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
                    Some(p) => p.to_string(),
                    None => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "delegate.whisper: missing required argument 'prompt'".into(),
                            )
                            .await;
                    }
                };

                // `reply_to` controls where the specialist's response goes.
                // "self"     → back to this philote as paracrine_response
                // "membrane" → directly to the membrane role
                // "<node>/<role>" → explicit routing
                // default    → "self"
                let reply_to_str = args
                    .get("reply_to")
                    .and_then(|v| v.as_str())
                    .unwrap_or("self");

                let node_id = local_node_id();
                let (reply_to_node, reply_to_role) = match reply_to_str {
                    "membrane" => (node_id.clone(), "membrane".to_string()),
                    "self" | "" => (node_id.clone(), "agent".to_string()),
                    other => {
                        if let Some((node, role_part)) = other.split_once('/') {
                            (node.to_string(), role_part.to_string())
                        } else {
                            (node_id.clone(), other.to_string())
                        }
                    }
                };

                // Parse optional response_routing hint from arguments.
                // Defaults to CognitiveReEntry if absent or unrecognised.
                let response_routing = args.get("routing").and_then(|v| v.as_str()).and_then(|s| {
                    serde_json::from_value::<ParacrineRouting>(serde_json::Value::String(
                        s.to_string(),
                    ))
                    .ok()
                });

                // Always generate a paracrine_id — it threads through the full
                // thought graph and ties the response back to this turn.
                let paracrine_id = Uuid::new_v4().to_string();

                // Log the outbound exosome ID on the active turn so the routing
                // reflex can correlate the response when it arrives.
                // Also capture the current session_id and chat_id so the specialist's
                // response can be routed back to the originating conversation channel.
                let (source_session_id, source_chat_id) = {
                    let mut sess_id = None;
                    let mut chat_id = None;
                    if let Some(state) = self.sessions.get_mut(&payload.session_id) {
                        if let Some(turn) = state.active_turn.as_mut() {
                            turn.associated_paracrine_ids.push(paracrine_id.clone());
                            sess_id = Some(state.session_id.clone());
                            if !turn.chat_id.is_empty() {
                                chat_id = Some(turn.chat_id.clone());
                            }
                        }
                    }
                    (sess_id, chat_id)
                };

                let exosome = Exosome {
                    prompt,
                    context: None,
                    paracrine_id: Some(paracrine_id.clone()),
                    response_routing,
                    source_session_id,
                    source_chat_id,
                };

                let (content, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::ParacrineEmit {
                        role,
                        exosome,
                        reply_to_node,
                        reply_to_role,
                        timeout_secs: None,
                    })
                    .await
                {
                    Ok(_) => (
                        format!(
                            "Whisper sent to specialist (paracrine_id: {paracrine_id}). \
                             The specialist is processing asynchronously — their response \
                             will arrive separately. Do NOT call delegate.whisper again. \
                             Respond to the user now with a brief acknowledgment."
                        ),
                        None,
                    ),
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("delegate.whisper: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
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
                    ..Default::default()
                })
                .await
            }

            // ── delegate.merge ───────────────────────────────────────────────
            // Explicit paracrine merge: emit a paracrine_response back to the
            // orchestrator immediately, without waiting for turn completion.
            // Sets paracrine_merge_completed on the turn so deliver_text_reply
            // does not auto-emit a duplicate response when the turn later closes.
            //
            // Call signature: { "content": "<response to send to orchestrator>" }
            // Available in specialist (paracrine) toolsets.
            "delegate.merge" => {
                let session_id = payload.session_id.clone();
                let turn_id = payload.turn_id.clone();
                let args = &payload.arguments;

                let content = match args.get("content").and_then(|v| v.as_str()) {
                    Some(c) if !c.trim().is_empty() => c.to_string(),
                    _ => {
                        return self
                            .fail_active_turn(
                                session_id,
                                turn_id,
                                "delegate.merge: missing required argument 'content'".into(),
                            )
                            .await;
                    }
                };

                // Capture routing info from the active turn before muting it.
                let (
                    paracrine_id,
                    reply_session_id,
                    reply_chat_id,
                    final_reply_to,
                    final_reply_role,
                    final_reply_guest_id,
                ) = {
                    let Some(state) = self.sessions.get_mut(&session_id) else {
                        warn!("delegate.merge: unknown session {}", session_id);
                        return Ok(());
                    };
                    let Some(turn) = state.active_turn.as_mut() else {
                        warn!("delegate.merge: no active turn for session {}", session_id);
                        return Ok(());
                    };
                    if turn.paracrine_origin.is_none() {
                        return self
                            .fail_active_turn(
                                session_id,
                                turn_id,
                                "delegate.merge: not in a paracrine context — this tool is only available to specialist roles".into(),
                            )
                            .await;
                    }
                    let pid = turn.paracrine_origin.clone().unwrap();
                    let rs = turn
                        .paracrine_reply_session_id
                        .clone()
                        .unwrap_or_else(|| session_id.clone());
                    let rc = turn
                        .paracrine_reply_chat_id
                        .clone()
                        .unwrap_or_else(|| turn.chat_id.clone());
                    let frt = turn.final_reply_to.clone();
                    let frr = turn.final_reply_role.clone();
                    let frg = turn.final_reply_guest_id.clone();
                    // Mark merge as done so deliver_text_reply suppresses the auto-emit.
                    turn.paracrine_merge_completed = true;
                    (pid, rs, rc, frt, frr, frg)
                };

                // Append role attribution tag (same as deliver_text_reply does).
                let attributed_content = if let Ok(role_name) = std::env::var("PHILOTIC_ROLE_NAME")
                {
                    if !role_name.is_empty() {
                        format!("{}\n\n@agent:{}", content, role_name)
                    } else {
                        content.clone()
                    }
                } else {
                    content.clone()
                };

                // Fire the paracrine_response into the orchestrator's session.
                let merge_task = serde_json::json!({
                    "action": "paracrine_response",
                    "session_id": reply_session_id,
                    "turn_id": turn_id,
                    "chat_id": reply_chat_id,
                    "content": attributed_content,
                    "exosome": {
                        "prompt": "",
                        "paracrine_id": paracrine_id,
                        "source_session_id": reply_session_id,
                        "source_chat_id": reply_chat_id,
                    },
                });
                info!(
                    session_id = %session_id,
                    reply_session = %reply_session_id,
                    "delegate.merge: emitting paracrine_response to orchestrator"
                );
                let _ = self
                    .ipc_client
                    .send_request(IpcRequest::EmitTask {
                        target_node: final_reply_to,
                        target_role: final_reply_role,
                        target_guest_id: final_reply_guest_id,
                        task_json: merge_task.to_string(),
                    })
                    .await;

                // Return a tool result so the specialist's turn can continue or close.
                let result_content = format!(
                    "Merge sent to orchestrator (paracrine_id: {}). Your response has been delivered to the main conversation. Complete your turn now.",
                    paracrine_id
                );
                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
                    source: Some("agent".into()),
                    session_id: Some(session_id),
                    turn_id: Some(turn_id),
                    transport: None,
                    chat_id: Some(payload.chat_id),
                    thread_id: None,
                    sender_id: None,
                    sender_username: None,
                    message_kind: None,
                    content: Some(result_content),
                    attachments: Vec::new(),
                    command: None,
                    callback_data: None,
                    raw_transport_event: None,
                    error: None,
                    tool_name: Some("delegate.merge".into()),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            "approval.request_standing" => {
                let session_id = payload.session_id.clone();
                let turn_id = payload.turn_id.clone();
                let tool_name = payload
                    .arguments
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let required_successes = payload
                    .arguments
                    .get("required_successes")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(3) as u32;

                if tool_name.is_empty() {
                    return self
                        .fail_active_turn(
                            session_id,
                            turn_id,
                            "approval.request_standing: missing required argument 'tool_name'"
                                .into(),
                        )
                        .await;
                }

                let content = if let Some(state) = self.sessions.get_mut(&session_id) {
                    state.register_standing_preapproval(&tool_name, required_successes);
                    let current_streak = *state.tool_success_streak.get(&tool_name).unwrap_or(&0);
                    if state.approval_policy.preapproved_tools.contains(&tool_name) {
                        format!(
                            "Standing approval granted immediately for '{}' — \
                             current streak ({}) already meets the threshold ({}).",
                            tool_name, current_streak, required_successes
                        )
                    } else {
                        format!(
                            "Standing approval registered for '{}'. \
                             It will be auto-granted after {} successive successes \
                             (current streak: {}).",
                            tool_name, required_successes, current_streak
                        )
                    }
                } else {
                    "Session not found.".into()
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(session_id),
                    turn_id: Some(turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content),
                    tool_name: Some("approval.request_standing".into()),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            // ── mcp.provision ────────────────────────────────────────────────
            "mcp.provision" => {
                let session_id = payload.session_id.clone();
                let turn_id = payload.turn_id.clone();
                let args = &payload.arguments;

                let endpoint_id = match args.get("endpoint_id").and_then(|v| v.as_str()) {
                    Some(s) if !s.trim().is_empty() => s.to_string(),
                    _ => {
                        return self
                            .fail_active_turn(
                                session_id,
                                turn_id,
                                "mcp.provision: missing required argument 'endpoint_id'".into(),
                            )
                            .await;
                    }
                };
                let port = match args.get("port").and_then(|v| v.as_u64()) {
                    Some(p) if p > 0 && p < 65536 => p as u16,
                    _ => {
                        return self
                            .fail_active_turn(
                                session_id,
                                turn_id,
                                "mcp.provision: missing or invalid 'port' (must be 1–65535)".into(),
                            )
                            .await;
                    }
                };

                let tools_raw = args.get("tools").cloned().unwrap_or(serde_json::json!([]));
                let tools: Vec<ansible_mesh_core::mcp_endpoint::McpToolSpec> =
                    match serde_json::from_value(tools_raw) {
                        Ok(t) => t,
                        Err(e) => {
                            return self
                                .fail_active_turn(
                                    session_id,
                                    turn_id,
                                    format!("mcp.provision: invalid 'tools' shape — {e}"),
                                )
                                .await;
                        }
                    };

                let preapproval_rules: Vec<ansible_mesh_core::mcp_endpoint::McpPreapprovalRule> =
                    args.get("preapproval_rules")
                        .and_then(|v| {
                            serde_json::from_value::<
                                Vec<ansible_mesh_core::mcp_endpoint::McpPreapprovalRule>,
                            >(v.clone())
                            .ok()
                        })
                        .unwrap_or_default()
                        .into_iter()
                        .map(
                            |mut rule: ansible_mesh_core::mcp_endpoint::McpPreapprovalRule| {
                                rule.approved_by_turn = turn_id.clone();
                                rule.approved_at = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0);
                                rule
                            },
                        )
                        .collect();

                let updated_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                let config = ansible_mesh_core::mcp_endpoint::McpEndpointConfig {
                    endpoint_id: endpoint_id.clone(),
                    owner_agent_id: self.agent_id.clone(),
                    port,
                    path: None,
                    tools,
                    preapproval_rules,
                    updated_at,
                };

                let response = self
                    .ipc_client
                    .send_request(IpcRequest::ProvisionMcpEndpoint { config })
                    .await;

                let (content, tool_err) = match response {
                    Ok(IpcResponse::McpEndpointProvisioned {
                        endpoint_id: ref eid,
                        port: p,
                        materialized,
                    }) => {
                        let status = if materialized {
                            "spawned a new membrane-mcp guest"
                        } else {
                            "updated config on existing membrane-mcp guest"
                        };
                        (
                            format!(
                                "MCP endpoint provisioned.\n\
                                 Endpoint ID: {eid}\n\
                                 Port: {p}\n\
                                 Status: {status}\n\
                                 Pre-approval rules for this endpoint are now active."
                            ),
                            None,
                        )
                    }
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e =
                            philotic_client::TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = philotic_client::TaskErrorPayload::ipc_failure(
                            "aiua",
                            "IPC_ERROR",
                            msg,
                        );
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = philotic_client::TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "mcp.provision: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = philotic_client::TaskErrorPayload::transport_error(
                            "philote",
                            format!("mcp.provision: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
                    source: Some("agent".into()),
                    session_id: Some(session_id),
                    turn_id: Some(turn_id),
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
                    tool_name: Some("mcp.provision".into()),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            // ── mcp.revoke ───────────────────────────────────────────────────
            "mcp.revoke" => {
                let session_id = payload.session_id.clone();
                let turn_id = payload.turn_id.clone();

                let endpoint_id = match payload
                    .arguments
                    .get("endpoint_id")
                    .and_then(|v| v.as_str())
                {
                    Some(s) if !s.trim().is_empty() => s.to_string(),
                    _ => {
                        return self
                            .fail_active_turn(
                                session_id,
                                turn_id,
                                "mcp.revoke: missing required argument 'endpoint_id'".into(),
                            )
                            .await;
                    }
                };

                let response = self
                    .ipc_client
                    .send_request(IpcRequest::RevokeMcpEndpoint {
                        endpoint_id: endpoint_id.clone(),
                        owner_agent_id: self.agent_id.clone(),
                    })
                    .await;

                let (content, tool_err) = match response {
                    Ok(IpcResponse::McpEndpointProvisioned {
                        endpoint_id: ref eid,
                        ..
                    }) => (
                        format!(
                            "MCP endpoint '{eid}' revoked. The membrane-mcp guest has been signalled to shut down."
                        ),
                        None,
                    ),
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e =
                            philotic_client::TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(IpcResponse::Error(msg)) => {
                        let e = philotic_client::TaskErrorPayload::ipc_failure(
                            "aiua",
                            "IPC_ERROR",
                            msg,
                        );
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = philotic_client::TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "mcp.revoke: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = philotic_client::TaskErrorPayload::transport_error(
                            "philote",
                            format!("mcp.revoke: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
                    source: Some("agent".into()),
                    session_id: Some(session_id),
                    turn_id: Some(turn_id),
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
                    tool_name: Some("mcp.revoke".into()),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            // ── table.add_listener ───────────────────────────────────────────
            "table.add_listener" => {
                let args = payload.arguments.as_object();

                let event_kind = match args
                    .and_then(|a| a.get("event_kind"))
                    .and_then(|v| v.as_str())
                {
                    Some(s) if !s.is_empty() => s.to_string(),
                    _ => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "table.add_listener: missing required argument 'event_kind'".into(),
                            )
                            .await;
                    }
                };

                let table_name = match args
                    .and_then(|a| a.get("table_name"))
                    .and_then(|v| v.as_str())
                {
                    Some(s) if !s.is_empty() => s.to_string(),
                    _ => {
                        return self
                            .fail_active_turn(
                                payload.session_id,
                                payload.turn_id,
                                "table.add_listener: missing required argument 'table_name'".into(),
                            )
                            .await;
                    }
                };

                let schema_map = args
                    .and_then(|a| a.get("schema_map"))
                    .and_then(|v| v.as_object())
                    .cloned();
                let filter_keys = args
                    .and_then(|a| a.get("filter_keys"))
                    .and_then(|v| v.as_object())
                    .cloned();
                let adapter_script = args
                    .and_then(|a| a.get("adapter_script"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let target_role = args
                    .and_then(|a| a.get("target_role"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("table-datasource")
                    .to_string();

                // Read current listener config.
                let mut config: serde_json::Value = match self
                    .ipc_client
                    .send_request(IpcRequest::GetConfig {
                        key: "router_listener.config".into(),
                    })
                    .await
                {
                    Ok(IpcResponse::ConfigData {
                        value_json: Some(raw),
                        ..
                    }) => serde_json::from_str(&raw).unwrap_or(serde_json::json!({
                        "filter_keys": {},
                        "event_kinds": {}
                    })),
                    _ => serde_json::json!({ "filter_keys": {}, "event_kinds": {} }),
                };

                // Merge filter_keys if provided.
                if let Some(fk) = filter_keys {
                    if let Some(obj) = config
                        .get_mut("filter_keys")
                        .and_then(|v| v.as_object_mut())
                    {
                        obj.extend(fk);
                    }
                }

                // Ensure event_kinds map exists.
                if config.get("event_kinds").is_none() {
                    config["event_kinds"] = serde_json::json!({});
                }

                // Build and insert the event handler.
                let mut handler = serde_json::json!({
                    "mode": "table_insert",
                    "table_name": table_name,
                    "target_role": target_role,
                });
                if let Some(sm) = schema_map {
                    handler["schema_map"] = serde_json::Value::Object(sm);
                }
                if let Some(script) = adapter_script {
                    handler["adapter_script"] = serde_json::Value::String(script);
                }

                if let Some(kinds) = config
                    .get_mut("event_kinds")
                    .and_then(|v| v.as_object_mut())
                {
                    kinds.insert(event_kind.clone(), handler);
                }

                // Write back.
                let (content_str, tool_err) = match self
                    .ipc_client
                    .send_request(IpcRequest::SetConfig {
                        key: "router_listener.config".into(),
                        value_json: config.to_string(),
                    })
                    .await
                {
                    Ok(IpcResponse::Standard { ok: true, .. }) => (
                        format!(
                            "Listener registered: event_kind='{event_kind}' → table='{table_name}'. \
                             The router-listener applies this on its next reconnect. \
                             Next: call graph.query to CREATE a (TableConfig {{id:'table_config:{table_name}', \
                             name:'{table_name}'}}) node in your partition so this table appears in \
                             your cognitive envelope on future sessions."
                        ),
                        None,
                    ),
                    Ok(IpcResponse::Standard {
                        ok: false,
                        code,
                        message,
                        ..
                    }) => {
                        let e =
                            philotic_client::TaskErrorPayload::ipc_failure("aiua", &*code, message);
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => ("table.add_listener: unexpected hotel response".into(), None),
                    Err(e) => {
                        let err = philotic_client::TaskErrorPayload::transport_error(
                            "philote",
                            format!("table.add_listener: IPC transport error — {e}"),
                        );
                        (err.display_message(), Some(err))
                    }
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    chat_id: Some(payload.chat_id),
                    content: Some(content_str),
                    error: tool_err,
                    tool_name: Some(payload.tool_name),
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            // ── routing.policy.propose ───────────────────────────────────────
            "routing.policy.propose" => {
                let args = payload.arguments.as_object();
                let problem = args
                    .and_then(|a| a.get("problem"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let proposed_change = args
                    .and_then(|a| a.get("proposed_change"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let evidence = args
                    .and_then(|a| a.get("evidence"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let affected_stage = args
                    .and_then(|a| a.get("affected_stage"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let affected_capability = args
                    .and_then(|a| a.get("affected_capability"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let learned_reflex_preference_key = args
                    .and_then(|a| a.get("learned_reflex_preference_key"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                if problem.is_empty() || proposed_change.is_empty() {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "routing.policy.propose: 'problem' and 'proposed_change' are required."
                                .into(),
                        )
                        .await;
                }

                let result_text = match self
                    .ipc_client
                    .send_request(IpcRequest::RecordRoutingPolicyProposal {
                        agent_id: self.agent_id.clone(),
                        problem: problem.clone(),
                        proposed_change,
                        evidence,
                        affected_stage,
                        affected_capability,
                        learned_reflex_preference_key,
                    })
                    .await
                {
                    Ok(IpcResponse::RoutingPolicyRecorded { proposal_id }) => {
                        format!(
                            "Routing policy proposal recorded (id: {proposal_id}). \
                             An operator will review and approve or reject the proposed change."
                        )
                    }
                    Ok(IpcResponse::Standard {
                        ok: true, message, ..
                    }) => message,
                    Ok(IpcResponse::Standard {
                        ok: false, message, ..
                    }) => {
                        format!("routing.policy.propose: hotel rejected — {message}")
                    }
                    Ok(_) => "routing.policy.propose: unexpected response from hotel.".into(),
                    Err(e) => format!("routing.policy.propose: IPC error — {e}"),
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    transport: None,
                    chat_id: Some(payload.chat_id),
                    thread_id: None,
                    sender_id: None,
                    sender_username: None,
                    message_kind: None,
                    content: Some(result_text),
                    attachments: Vec::new(),
                    command: None,
                    callback_data: None,
                    raw_transport_event: None,
                    error: None,
                    tool_name: Some("routing.policy.propose".into()),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            // ── routing.reflex.set ───────────────────────────────────────────
            "routing.reflex.set" => {
                let args = payload.arguments.as_object();
                let preference_key = args
                    .and_then(|a| a.get("preference_key"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("generation_capability_preference")
                    .to_string();
                let reflexes_json = args
                    .and_then(|a| a.get("reflexes"))
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                let reason = args
                    .and_then(|a| a.get("reason"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let result_text = match self
                    .ipc_client
                    .send_request(IpcRequest::UpsertAgentReflexPreference {
                        agent_id: self.agent_id.clone(),
                        preference_key: preference_key.clone(),
                        precedence: 70,
                        reflexes_json,
                        config_json: serde_json::json!({ "reason": reason }),
                    })
                    .await
                {
                    Ok(IpcResponse::Standard { ok: true, .. }) => {
                        format!("Routing reflex '{preference_key}' stored. Takes effect on the next turn.")
                    }
                    Ok(IpcResponse::Standard { ok: false, message, .. }) => {
                        format!("routing.reflex.set: hotel rejected — {message}")
                    }
                    Ok(_) => "routing.reflex.set: unexpected response from hotel.".into(),
                    Err(e) => format!("routing.reflex.set: IPC error — {e}"),
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    transport: None,
                    chat_id: Some(payload.chat_id),
                    thread_id: None,
                    sender_id: None,
                    sender_username: None,
                    message_kind: None,
                    content: Some(result_text),
                    attachments: Vec::new(),
                    command: None,
                    callback_data: None,
                    raw_transport_event: None,
                    error: None,
                    tool_name: Some("routing.reflex.set".into()),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            // ── routing.reflex.get ───────────────────────────────────────────
            "routing.reflex.get" => {
                let args = payload.arguments.as_object();
                let filter_key = args
                    .and_then(|a| a.get("preference_key"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                let result_text = match self
                    .ipc_client
                    .send_request(IpcRequest::GetAgentReflexPreferences {
                        agent_id: self.agent_id.clone(),
                        preference_key: filter_key,
                    })
                    .await
                {
                    Ok(IpcResponse::AgentReflexPreferences { rows }) => {
                        serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".into())
                    }
                    Ok(IpcResponse::Standard { ok: true, message, .. }) => message,
                    Ok(_) => "routing.reflex.get: unexpected response from hotel.".into(),
                    Err(e) => format!("routing.reflex.get: IPC error — {e}"),
                };

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    transport: None,
                    chat_id: Some(payload.chat_id),
                    thread_id: None,
                    sender_id: None,
                    sender_username: None,
                    message_kind: None,
                    content: Some(result_text),
                    attachments: Vec::new(),
                    command: None,
                    callback_data: None,
                    raw_transport_event: None,
                    error: None,
                    tool_name: Some("routing.reflex.get".into()),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
                })
                .await
            }

            // ── desktop.observe ──────────────────────────────────────────────
            "desktop.observe" => {
                // Observe-only: returns desktop runner metadata. No screenshot or interaction.
                // A real desktop guest is not required — this tool describes what would be observed.
                let result_text = serde_json::json!({
                    "status": "no_desktop_guest",
                    "message": "No desktop guest is currently materialised on this hotel. \
                                Desktop observation requires a desktop runner guest to be active.",
                    "tool": "desktop.observe",
                })
                .to_string();

                self.handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    agent_action: None,
                    handoff_bundle: None,
                    source: Some("agent".into()),
                    session_id: Some(payload.session_id),
                    turn_id: Some(payload.turn_id),
                    transport: None,
                    chat_id: Some(payload.chat_id),
                    thread_id: None,
                    sender_id: None,
                    sender_username: None,
                    message_kind: None,
                    content: Some(result_text),
                    attachments: Vec::new(),
                    command: None,
                    callback_data: None,
                    raw_transport_event: None,
                    error: None,
                    tool_name: Some("desktop.observe".into()),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
                    final_reply_guest_id: payload.final_reply_guest_id,
                    ..Default::default()
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

    /// Executes a shell command via `sh -c`, capturing stdout/stderr and enforcing a timeout.
    async fn execute_bash_command(
        &self,
        command: String,
        working_dir: Option<String>,
        timeout_secs: u64,
    ) -> Result<serde_json::Value> {
        run_bash_command(command, working_dir, timeout_secs).await
    }

    async fn complete_local_command(
        &mut self,
        session_id: String,
        turn_id: String,
        reply_content: String,
    ) -> Result<()> {
        self.complete_local_command_with_markup(session_id, turn_id, reply_content, None)
            .await
    }

    async fn complete_local_command_with_markup(
        &mut self,
        session_id: String,
        turn_id: String,
        reply_content: String,
        reply_markup: Option<serde_json::Value>,
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
            reply_markup,
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

    /// Re-fetches the session snapshot from the hotel on every user turn and merges the latest
    /// effective_toolset, effective_skillset, and component routing into the live session state.
    /// This ensures tool grants and runtime routing changes take effect immediately on the next
    /// message without requiring a session restart or reconnect.
    async fn refresh_bindings_from_snapshot(&mut self, session_id: &str) {
        let response = self
            .ipc_client
            .send_request(IpcRequest::GetConfig {
                key: format!("__session_snapshot__:{session_id}"),
            })
            .await;

        let snapshot = match response {
            Ok(IpcResponse::ConfigData {
                value_json: Some(ref value_json),
                ..
            }) => serde_json::from_str::<serde_json::Value>(value_json).ok(),
            _ => None,
        };

        let Some(state) = self.sessions.get_mut(session_id) else {
            return;
        };
        let Some(snapshot) = snapshot else { return };
        Self::merge_snapshot_bindings(state, &snapshot);
    }

    fn merge_snapshot_bindings(state: &mut SessionState, snapshot: &serde_json::Value) {
        let Some(bindings) = snapshot.get("bindings") else {
            return;
        };
        let new_toolset: Option<Vec<String>> = bindings
            .get("effective_toolset")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let new_skillset: Option<Vec<String>> = bindings
            .get("effective_skillset")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let new_component_routes = snapshot
            .get("component_route_assembly")
            .cloned()
            .and_then(|value| serde_json::from_value::<ComponentRouteAssembly>(value).ok());

        let mut changed = false;
        if let Some(toolset) = new_toolset {
            if toolset != state.bindings.effective_toolset {
                state.bindings.effective_toolset = toolset;
                changed = true;
            }
        }
        if let Some(skillset) = new_skillset {
            if skillset != state.bindings.effective_skillset {
                state.bindings.effective_skillset = skillset;
                changed = true;
            }
        }
        if let Some(component_routes) = new_component_routes {
            if component_routes != state.component_route_assembly {
                state.component_route_assembly = component_routes;
                changed = true;
            }
        }
        if changed {
            state.rebuild_default_tool_assembly();
        }
    }

    async fn ensure_session_loaded(
        &mut self,
        session_id: &str,
        fallback_source: &str,
    ) -> Result<()> {
        if self.sessions.contains_key(session_id) {
            return Ok(());
        }

        // Role processes request a role-scoped snapshot so the hotel returns the
        // role-specific checkpoint rather than the orchestrator's base checkpoint.
        let snapshot_key = {
            let role = std::env::var("PHILOTIC_ROLE_NAME")
                .ok()
                .filter(|r| !r.is_empty());
            match role {
                Some(r) => format!("__session_snapshot__:{session_id}@{r}"),
                None => format!("__session_snapshot__:{session_id}"),
            }
        };
        let response = self
            .ipc_client
            .send_request(IpcRequest::GetConfig { key: snapshot_key })
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
                    if let Some(mut state) = SessionState::from_checkpoint(&checkpoint) {
                        // Preserve runtime voice switches from the checkpoint before
                        // overwriting agent_profile with the live hotel config.
                        let checkpoint_voice_ids =
                            state.agent_profile.voice_response_policy.voice_ids.clone();
                        let checkpoint_provider =
                            state.agent_profile.voice_response_policy.provider.clone();

                        // Overwrite the restored agent_profile with the live default so
                        // voice routing, media policy, and reflex configuration always
                        // reflect the current hotel config rather than a stale snapshot.
                        state.agent_profile = self.default_agent_profile.clone();

                        // Restore any runtime voice provider/ID switches the user made
                        // during the session so they follow the philote across restarts.
                        if !checkpoint_voice_ids.is_empty() {
                            state.agent_profile.voice_response_policy.voice_ids =
                                checkpoint_voice_ids;
                        }
                        if let Some(provider) = checkpoint_provider {
                            state.agent_profile.voice_response_policy.provider = Some(provider);
                        }
                        // Re-apply reflex materialization from the restored profile.
                        state.apply_reflex_materialization();
                        Self::fetch_and_inject_rules(
                            &mut self.ipc_client,
                            &self.agent_id,
                            &mut state,
                        )
                        .await;

                        // If the checkpoint had a stale active turn that from_checkpoint
                        // dropped (e.g. WaitingModel after a crash), immediately re-save
                        // the clean checkpoint so the stale turn is purged from storage
                        // rather than lingering until the next turn completes.
                        let checkpoint_had_active_turn = checkpoint
                            .get("active_turn")
                            .map(|v| !v.is_null())
                            .unwrap_or(false);
                        if checkpoint_had_active_turn && state.active_turn.is_none() {
                            let stale_phase = checkpoint
                                .get("active_turn")
                                .and_then(|t| t.get("phase"))
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or("unknown");
                            warn!(
                                session_id = %session_id,
                                stale_phase = %stale_phase,
                                "Dropped stale active turn on checkpoint restore — persisting clean checkpoint"
                            );
                            let mem_type = state.checkpoint_memory_type();
                            let clean_checkpoint = state.checkpoint_json();
                            if let Err(e) = self
                                .ipc_client
                                .sync_apartment(&self.agent_id, &mem_type, clean_checkpoint)
                                .await
                            {
                                warn!(
                                    "Failed to persist clean checkpoint after stale turn drop: {}",
                                    e
                                );
                            }
                        }

                        self.sessions.insert(session_id.to_string(), state);
                        return Ok(());
                    }
                }
            }
        }

        let mut state = SessionState::new(
            session_id.to_string(),
            self.agent_id.clone(),
            fallback_source.into(),
        );
        state.agent_profile = self.default_agent_profile.clone();
        // Apply reflex materialization now that the profile (including reflex_context) is set.
        state.apply_reflex_materialization();
        Self::fetch_and_inject_rules(&mut self.ipc_client, &self.agent_id, &mut state).await;

        // Auto-activate the agent's default role incarnation on fresh sessions so the
        // correct manifest, toolset, and skill guidance are present from turn zero
        // without requiring an explicit handoff.to_role call.
        if let Some(ref default_role) = self.default_agent_profile.default_role_name.clone() {
            if let Some(activation) = self.fetch_role_activation(default_role).await {
                state.role_activation = Some(activation);
                info!(
                    session_id = %session_id,
                    role = %default_role,
                    "Auto-activated default role on fresh session."
                );
            }
        }

        self.sessions.insert(session_id.to_string(), state);
        Ok(())
    }

    /// Fetches durable rules from the hotel and injects them into the session state.
    /// Called at session init so every cognitive call sees the current rule set.
    /// Failures are non-fatal — rules will simply be absent from context.
    async fn fetch_and_inject_rules(
        ipc_client: &mut PhiloticClient,
        agent_id: &str,
        state: &mut SessionState,
    ) {
        match ipc_client
            .send_request(IpcRequest::ListRules {
                agent_id: agent_id.to_string(),
            })
            .await
        {
            Ok(IpcResponse::RuleList { rules }) => {
                state.rules = rules;
            }
            Ok(_) | Err(_) => {
                // Non-fatal: session proceeds without rules if the hotel is unavailable.
            }
        }
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

fn local_hotel_name() -> Option<String> {
    std::env::var("PHILOTIC_HOTEL_NAME")
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

/// Executes a shell command via `sh -c`, capturing stdout/stderr and enforcing a timeout.
///
/// Returns a JSON object with `stdout`, `stderr`, `exit_code`, and `success` fields.
/// Returns `Err` only on process-spawn failure or timeout — a non-zero exit code is
/// represented as `success: false` within the returned JSON, not as a Rust error.
async fn run_bash_command(
    command: String,
    working_dir: Option<String>,
    timeout_secs: u64,
) -> Result<serde_json::Value> {
    use tokio::process::Command;
    use tokio::time::{Duration, timeout};

    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(&command);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    if let Some(dir) = &working_dir {
        cmd.current_dir(dir);
    }

    let child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("bash.exec: failed to spawn process — {e}"))?;

    let output = timeout(Duration::from_secs(timeout_secs), child.wait_with_output())
        .await
        .map_err(|_| anyhow::anyhow!("bash.exec: command timed out after {timeout_secs}s"))?
        .map_err(|e| anyhow::anyhow!("bash.exec: process error — {e}"))?;

    let exit_code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let success = output.status.success();

    Ok(serde_json::json!({
        "stdout": stdout,
        "stderr": stderr,
        "exit_code": exit_code,
        "success": success,
    }))
}

#[cfg(test)]
mod tests {
    use super::{
        AgentRuntime, DEFAULT_TEXT_MODEL_ROLE, LOCAL_NODE, extract_model_error,
        extract_model_error_payload, format_role_command_reply, format_roles_report,
        media_analysis_attachments, normalized_user_content, parse_memory_candidate,
        resolve_media_routing, resolve_model_execution_target, should_attempt_provider_repair,
    };
    use crate::commands::SlashCommand;
    use crate::r#loop::{ApprovalRequest, ToolCall, TurnPhase};
    use crate::protocol::{
        FinalReplyPayload, InboundTaskPayload, ModelRequestPayload, TransportAttachment,
    };
    use crate::session::{
        ApprovalPolicy, ComponentExecutionRoute, ComponentRouteAssembly, ComponentRouteBinding,
        ResponseRouteMode, SessionState, WorkingTurn,
    };
    use philotic_client::TaskErrorPayload;
    use uuid::Uuid;
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
            response_route: Some("text_only".into()),
            ligand: None,
            provider_options: serde_json::Map::new(),
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
        assert_eq!(json["response_route"], "text_only");
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
    fn response_route_prefers_agent_profile_default_route() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.agent_profile.response_route_policy.default_route =
            ResponseRouteMode::RealtimeWebsocket;

        let route = super::model_response_route(Some(&state), None, &serde_json::Map::new(), &[]);
        assert_eq!(route, "realtime_websocket");
    }

    #[test]
    fn implementation_names_map_to_model_roles() {
        assert_eq!(super::implementation_to_model_role("gemini"), "model");
        assert_eq!(super::implementation_to_model_role("gemini-flash"), "model");
        assert_eq!(
            super::implementation_to_model_role("elevenlabs-v1"),
            "model.elevenlabs"
        );
        assert_eq!(super::implementation_to_model_role("ollama"), "model.ollama");
        assert_eq!(
            super::implementation_to_model_role("ollama-llama3"),
            "model.ollama"
        );
        assert_eq!(super::implementation_to_model_role("mlx"), "model.mlx");
        assert_eq!(
            super::implementation_to_model_role("mlx-community/llama"),
            "model.mlx"
        );
        assert_eq!(
            super::implementation_to_model_role("onnx"),
            "model.local"
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
                    target_capability: None,
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
    fn merge_snapshot_bindings_updates_component_route_assembly() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        let snapshot = serde_json::json!({
            "bindings": {
                "effective_toolset": ["echo"],
                "effective_skillset": ["planning"]
            },
            "component_route_assembly": {
                "execution_routes": {
                    "text.generate": {
                        "target_node": "default-aiua-01",
                        "target_role": "model",
                        "incarnation_id": "default:model-controller-gemini",
                        "hotel_id": "default",
                        "environment_id": null,
                        "execution_mode": "capability",
                        "availability_state": "live",
                        "selection_reason": "live_local_capability"
                    }
                }
            }
        });

        AgentRuntime::merge_snapshot_bindings(&mut state, &snapshot);

        let route = state
            .resolve_component_execution_route("text.generate")
            .expect("text.generate route should refresh from snapshot");
        assert_eq!(route.target_node, "default-aiua-01");
        assert_eq!(route.hotel_id.as_deref(), Some("default"));
        assert_eq!(state.bindings.effective_toolset, vec!["echo"]);
        assert_eq!(state.bindings.effective_skillset, vec!["planning"]);
    }

    #[test]
    fn resolve_dispatch_defaults_to_generate_text() {
        let (action, cap) = super::resolve_dispatch(None, "text.generate");
        assert_eq!(action, "generate_text");
        assert_eq!(cap, "text.generate");
    }

    #[test]
    fn resolve_dispatch_promotes_to_response_generate_when_reflex_set() {
        let mut state =
            SessionState::new("sess-reflex".into(), "agent-01".into(), "telegram".into());
        state.component_route_assembly = ComponentRouteAssembly {
            execution_routes: std::collections::BTreeMap::from([(
                "text.generate".into(),
                ComponentExecutionRoute {
                    target_node: "local".into(),
                    target_role: "model".into(),
                    incarnation_id: None,
                    hotel_id: None,
                    environment_id: None,
                    execution_mode: "capability".into(),
                    availability_state: "live".into(),
                    selection_reason: None,
                    target_capability: Some("response.generate".into()),
                },
            )]),
        };

        let (action, cap) = super::resolve_dispatch(Some(&state), "text.generate");
        assert_eq!(action, "response.generate");
        assert_eq!(cap, "response.generate");
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
            reply_markup: None,
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
            handoff_bundle: None,
            error: Some(TaskErrorPayload {
                kind: "provider_failure".into(),
                message: "Provider invocation failed: missing voice".into(),
                code: Some("ELEVENLABS_MISSING_VOICE".into()),
                component: Some("model-router".into()),
                provider: Some("elevenlabs".into()),
                capability: Some("voice.synthesize".into()),
                retryable: Some(false),
                sub_kind: None,
            }),
            ..Default::default()
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
    fn extract_model_error_payload_preserves_retryable_flag() {
        let payload = InboundTaskPayload {
            agent_action: None,
            handoff_bundle: None,
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
                message:
                    "Provider invocation failed: tool_call.arguments missing from Gemini response"
                        .into(),
                code: Some("MODEL_INVALID_TOOL_CALL".into()),
                component: Some("model-router".into()),
                provider: Some("gemini".into()),
                capability: Some("text.generate".into()),
                retryable: Some(true),
                sub_kind: None,
            }),
            ..Default::default()
        };

        let error =
            extract_model_error_payload(&payload).expect("structured payload should be extracted");
        assert_eq!(error.retryable, Some(true));
        assert_eq!(error.code.as_deref(), Some("MODEL_INVALID_TOOL_CALL"));
    }

    #[test]
    fn retryable_provider_failure_allows_single_repair_attempt() {
        let error = TaskErrorPayload {
            kind: "provider_failure".into(),
            message: "Provider invocation failed: tool_call.arguments missing from Gemini response"
                .into(),
            code: Some("MODEL_INVALID_TOOL_CALL".into()),
            component: Some("model-router".into()),
            provider: Some("gemini".into()),
            capability: Some("text.generate".into()),
            retryable: Some(true),
            sub_kind: None,
        };

        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.start_turn(WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-1".into(),
            chat_id: "123".into(),
            user_content: "say hello".into(),
            final_reply_to: "local-aiua-01".into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: None,
            phase: TurnPhase::WaitingModel,
            iteration: 1,
            pending_tool_call: None,
            pending_approval: None,
            working_tool_history: Vec::new(),
            recalled_memories: Vec::new(),
            active_plan: None,
            consecutive_step_failures: 0,
            provider_repair_note: None,
            provider_repair_attempts: 0,
            pending_text_reply: None,
            had_voice_input: false,
            awaiting_transcription_reentry: false,
            scripted_loop_context: None,
            associated_paracrine_ids: Vec::new(),
            paracrine_origin: None,
            paracrine_reply_session_id: None,
            paracrine_reply_chat_id: None,
            paracrine_merge_completed: false,
            plan_confirmed: false,
            plan_confirm_note: None,
            fallback_tier: 0,
            streaming_retry_attempts: 0,
        });

        assert!(should_attempt_provider_repair(&error, Some(&state)));
        state.increment_provider_repair_attempts();
        assert!(!should_attempt_provider_repair(&error, Some(&state)));
    }

    #[test]
    fn parses_memory_candidate_from_model_result() {
        let candidate = parse_memory_candidate(Some(&serde_json::json!({
            "concept": "ready-for-task-selection",
            "content": "The assistant confirmed readiness and offered the next work options.",
            "tags": ["status", "readiness"]
        })))
        .expect("memory candidate should parse");

        assert_eq!(candidate.concept, "ready-for-task-selection");
        assert_eq!(
            candidate.content,
            "The assistant confirmed readiness and offered the next work options."
        );
        assert_eq!(candidate.tags, vec!["status", "readiness"]);
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
            handoff_bundle: None,
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
                blob_download_url: None,
                transport_error: None,
                ..Default::default()
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
            ..Default::default()
        };

        assert_eq!(normalized_user_content(&task), Some("caption text".into()));
    }

    #[test]
    fn normalized_user_content_summarizes_attachment_only_turns() {
        let task = InboundTaskPayload {
            action: None,
            agent_action: None,
            handoff_bundle: None,
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
                blob_download_url: None,
                transport_error: None,
                ..Default::default()
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
            ..Default::default()
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
            handoff_bundle: None,
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
            ..Default::default()
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
            handoff_bundle: None,
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
                    blob_id: Some("sha256-1".into()),
                    blob_download_url: Some("http://127.0.0.1:9001/download/sha256-1".into()),
                    transport_error: None,
                    ..Default::default()
                },
                TransportAttachment {
                    kind: "sticker".into(),
                    file_id: "sticker-1".into(),
                    mime_type: Some("image/webp".into()),
                    blob_id: Some("sha256-2".into()),
                    blob_download_url: Some("http://127.0.0.1:9001/download/sha256-2".into()),
                    transport_error: None,
                    ..Default::default()
                },
                TransportAttachment {
                    kind: "voice".into(),
                    file_id: "voice-1".into(),
                    mime_type: Some("audio/ogg".into()),
                    blob_id: Some("sha256-3".into()),
                    blob_download_url: None,
                    transport_error: None,
                    ..Default::default()
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
            ..Default::default()
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
            blob_id: Some(format!("sha256-{kind}-1")),
            blob_download_url: Some(format!("http://127.0.0.1:9001/download/sha256-{kind}-1")),
            transport_error: None,
            ..Default::default()
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

    #[test]
    fn command_bypasses_turn_start_for_read_only_commands() {
        assert!(super::command_bypasses_turn_start(&SlashCommand::Ping));
        assert!(super::command_bypasses_turn_start(&SlashCommand::Status));
        assert!(super::command_bypasses_turn_start(&SlashCommand::Context));
        assert!(!super::command_bypasses_turn_start(&SlashCommand::Pause));
        assert!(!super::command_bypasses_turn_start(
            &SlashCommand::Approve { note: None }
        ));
    }

    // ── bash.exec tests ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn bash_exec_captures_stdout_and_exit_zero() {
        let result = super::run_bash_command("echo hello".into(), None, 10)
            .await
            .expect("should succeed");
        assert_eq!(result["stdout"].as_str().unwrap().trim(), "hello");
        assert_eq!(result["stderr"].as_str().unwrap(), "");
        assert_eq!(result["exit_code"].as_i64().unwrap(), 0);
        assert!(result["success"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn bash_exec_captures_stderr_and_nonzero_exit() {
        let result = super::run_bash_command("echo err >&2; exit 2".into(), None, 10)
            .await
            .expect("should not return Err for process failure");
        assert_eq!(result["stdout"].as_str().unwrap(), "");
        assert_eq!(result["stderr"].as_str().unwrap().trim(), "err");
        assert_eq!(result["exit_code"].as_i64().unwrap(), 2);
        assert!(!result["success"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn bash_exec_respects_working_dir() {
        let result = super::run_bash_command("pwd".into(), Some("/tmp".into()), 10)
            .await
            .expect("should succeed");
        let stdout = result["stdout"].as_str().unwrap().trim().to_string();
        // /tmp may be a symlink on macOS — just check it resolves to something under /tmp
        assert!(
            stdout == "/tmp" || stdout.starts_with("/private/tmp"),
            "unexpected pwd: {stdout}"
        );
    }

    #[tokio::test]
    async fn bash_exec_enforces_timeout() {
        let err = super::run_bash_command("sleep 60".into(), None, 1)
            .await
            .expect_err("should time out");
        assert!(
            err.to_string().contains("timed out"),
            "unexpected error: {err}"
        );
    }
}
