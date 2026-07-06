use crate::commands::{SlashCommand, command_manifest, parse_slash_command};
use crate::r#loop::{
    AgentAction, ApprovalRequest, PlanProposalAction, ToolCall, ToolResult, TurnPhase,
    interpret_model_payload,
};
use crate::protocol::{
    FinalReplyPayload, InboundTaskPayload, LigandEnvelope, ModelRequestPayload,
    PartialReplyPayload, TaskRunnerOverlay, ToolExecutionPayload, TransportAttachment,
    TurnEventPayload, TurnStatusPayload,
};
use crate::reflex::{IngressAction, ReflexEvent};
use crate::session::{
    ActivePlan, AgentProfile, ComponentRouteAssembly, GraphAnchors, MediaRoutingPolicy,
    MemoryAuthority, MemoryShapingContext, MemorySpacetimeFrame, MemorySpatialScope,
    MemoryTemporalKind, MemoryValidationLevel, ParacrineBudgetOutcome, ParacrineThreadStatus,
    RecalledMemoryRecord, SessionState, ToolDefinition, ToolExecutionRoute,
    ToolRunnerIncarnationBinding, TtsMode, VoiceResponsePolicy, WorkingTurn, charge_paracrine_hop,
    merge_session_index,
};
use anyhow::Result;
use memory_core::{
    Engram, MemoryScope, MuninnConfig, MuninnRestEngine, RecallContext, RecallTrigger,
    VaultResolver,
};
use philotic_client::{
    Exosome, HandoffBundle, IpcRequest, IpcResponse, ParacrineRouting, PhiloticClient,
    TaskErrorPayload, UserProfileDataPayload, is_ipc_disconnect,
};
use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, HashMap};
use std::time::Duration;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

#[path = "turn_loop.rs"]
mod turn_loop;

#[path = "roles.rs"]
mod roles;

#[path = "paracrine.rs"]
mod paracrine;

#[path = "tool_exec.rs"]
mod tool_exec;

#[path = "memory_integration.rs"]
mod memory_integration;
use memory_integration::*;

pub const DEFAULT_AGENT_ID: &str = "agent-bjork-01";
const DEFAULT_REPLY_ROLE: &str = "membrane";
const DEFAULT_TEXT_MODEL_ROLE: &str = "model";
const DEFAULT_VOICE_MODEL_ROLE: &str = "model.elevenlabs";

fn local_node_id() -> String {
    std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string())
}

fn graph_datasource_node_id() -> String {
    std::env::var("PHILOTIC_GRAPH_DATASOURCE_HOME_NODE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("PHILOTIC_GRAPH_DATASOURCE_HOME_HOTEL")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .map(|hotel| format!("{hotel}-aiua-01"))
        })
        .unwrap_or_else(|| "vps-jane-aiua-01".to_string())
}

fn life_graph_runner_node_id() -> String {
    std::env::var("PHILOTIC_LIFE_GRAPH_RUNNER_HOME_NODE")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("PHILOTIC_LIFE_GRAPH_RUNNER_HOME_HOTEL")
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
                .map(|hotel| format!("{hotel}-aiua-01"))
        })
        .unwrap_or_else(|| "vps-jane-aiua-01".to_string())
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
            | Some("provider_auth")
            | Some("provider_error")
    ) || (error.kind == "provider_failure"
        && error.retryable.unwrap_or(false)
        && !is_content_error(error))
}

/// Default tier ordering when none is configured in TurnLoopConfig.
const DEFAULT_FALLBACK_TIERS: &[&str] = &["model", "model.ollama", "model.local"];

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

fn low_progress_tool_name(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "agent.graph.read"
            | "hotel.logs"
            | "hotel.status"
            | "mcp.status"
            | "memory.status"
            | "role.list"
            | "session.status"
            | "skill.list"
    )
}

fn duplicate_tool_skip(result: &ToolResult) -> bool {
    result.content.starts_with("[Duplicate call skipped]")
}

fn recent_low_progress_tool_run(turn: &WorkingTurn) -> usize {
    turn.working_tool_history
        .iter()
        .rev()
        .take_while(|(call, result)| {
            low_progress_tool_name(&call.tool_name) || duplicate_tool_skip(result)
        })
        .count()
}

fn loop_stop_reason(turn: &WorkingTurn, iteration_cap: u32) -> Option<&'static str> {
    let recent_low_progress = recent_low_progress_tool_run(turn);
    if turn.iteration >= 4 && recent_low_progress >= 4 {
        return Some("the last several tool calls only inspected status or repeated previous work");
    }

    if iteration_cap.saturating_sub(turn.iteration) <= 1 && recent_low_progress >= 2 {
        return Some("the turn is close to its iteration limit without new forward progress");
    }

    None
}

fn loop_stop_fallback_reply(
    user_content: &str,
    history: &[(ToolCall, ToolResult)],
    reason: &str,
) -> String {
    let mut tools = Vec::new();
    for (call, _) in history.iter().rev() {
        if !tools.iter().any(|name: &String| name == &call.tool_name) {
            tools.push(call.tool_name.clone());
        }
        if tools.len() >= 5 {
            break;
        }
    }
    tools.reverse();

    let tool_text = if tools.is_empty() {
        "no completed tool calls".to_string()
    } else {
        tools.join(", ")
    };
    let request = user_content.trim();
    let request_text = if request.is_empty() {
        "this turn".to_string()
    } else {
        format!("\"{}\"", request.chars().take(160).collect::<String>())
    };

    format!(
        "I'm going to stop this turn instead of looping: {reason}. I had been working on {request_text} and the recent tool path was: {tool_text}.\n\nI can keep going from here, but the next step needs to be a more specific action rather than another status check."
    )
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

fn model_affordances(
    state: Option<&SessionState>,
    user_content: &str,
    tools_for_model: &[ToolDefinition],
) -> Option<Value> {
    let value = state
        .map(|state| state.model_affordances_for_turn(user_content, tools_for_model))
        .unwrap_or_else(|| {
            let tools = tools_for_model
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.tool_name,
                        "class": tool.class,
                    })
                })
                .collect::<Vec<_>>();
            json!({
                "skills": [],
                "tools": tools,
            })
        });
    let is_empty = value
        .get("skills")
        .and_then(Value::as_array)
        .map(Vec::is_empty)
        .unwrap_or(true)
        && value
            .get("tools")
            .and_then(Value::as_array)
            .map(Vec::is_empty)
            .unwrap_or(true);
    if is_empty { None } else { Some(value) }
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

fn tool_status_label(tool_name: &str) -> String {
    let label = if tool_name.contains("search") || tool_name.contains("web") {
        "Searching the web..."
    } else if tool_name.starts_with("memory.") || tool_name.contains("recall") {
        "Recalling memory..."
    } else if tool_name == "bash.exec" || tool_name == "bash" {
        "Running command..."
    } else if tool_name.starts_with("file") || tool_name.contains("read") {
        "Reading files..."
    } else if tool_name.starts_with("graph") {
        "Checking context..."
    } else {
        return format!("Running {}...", tool_name);
    };
    label.to_string()
}

/// Stage 2 (prepare_inputs) + Stage 3 (merge_context) for the philote capability pipeline.
///
/// Converts a tool call's flat argument map into a normalized `CapabilityRequest`,
/// extracting image/text inputs and injecting conversation + agent identity.
fn build_capability_request(
    tool_name: &str,
    arguments: &Value,
    session_id: &str,
    agent_id: &str,
) -> ansible_mesh_core::capability::CapabilityRequest {
    use ansible_mesh_core::capability::{CapabilityContext, CapabilityInput, CapabilityRequest};

    let mut req = CapabilityRequest::new(tool_name);

    // Inject image input from image_url or image_base64.
    if let Some(url) = arguments.get("image_url").and_then(Value::as_str) {
        req = req.with_image_url(url);
    } else if let Some(b64) = arguments.get("image_base64").and_then(Value::as_str) {
        let mime = arguments
            .get("mime_type")
            .and_then(Value::as_str)
            .map(str::to_string);
        req = req.with_image_base64(b64, mime);
    }

    // For audio primitives: inject audio input.
    if tool_name.starts_with("audio.") {
        if let Some(url) = arguments.get("audio_url").and_then(Value::as_str) {
            req.inputs.push(CapabilityInput::Audio {
                url: Some(url.to_string()),
                base64: None,
                mime_type: arguments
                    .get("audio_mime_type")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
    }

    // Inject text prompt (query / hint).
    let text_prompt = arguments
        .get("query")
        .or_else(|| arguments.get("hint"))
        .or_else(|| arguments.get("prompt"))
        .and_then(Value::as_str);
    if let Some(text) = text_prompt {
        req = req.with_text(text);
    }

    // Stage 3 — merge context.
    req.context = CapabilityContext {
        conversation_id: Some(session_id.to_string()),
        agent_id: Some(agent_id.to_string()),
        identity: Value::Null,
    };

    req
}

fn command_bypasses_turn_start(command: &SlashCommand) -> bool {
    matches!(
        command,
        SlashCommand::Ping | SlashCommand::Status | SlashCommand::Context
    )
}

#[allow(dead_code)]
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
    /// Tracks hotel-broadcast MuninnDB reachability. False = hotel reported endpoint down.
    /// When false, `memory_engine_for` returns None even if `muninn_config` is set.
    muninn_available: bool,
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
    /// Signature of the exact wait state currently being timed per session.
    /// Includes turn id, phase, iteration, and pending tool name so productive
    /// model/tool progress resets the watchdog timer.
    stuck_turn_signature: HashMap<String, String>,
    /// Tracks when any active turn started, regardless of phase. Used as a
    /// catch-all eviction budget — a turn that stays active for too long in any
    /// phase (including InProgress) will be forcibly evicted. Separate from
    /// stuck_turn_first_seen so per-phase timers are not disturbed.
    total_active_since: HashMap<String, std::time::Instant>,
    /// Hotel-wide network reachability flag. Set true when the hotel broadcasts
    /// NetworkState { online: false }. When true, text.generate is routed directly
    /// to the local model tier without attempting cloud providers.
    network_offline: bool,
    /// Role name when this runtime is a role-incarnation philote (e.g. "orchestrator",
    /// "brain"). None for the default agent philote. Used to set reply_to_guest_id on
    /// paracrine whispers so the specialist's response routes back to this role instance
    /// rather than to the membrane seat that initiated the user turn.
    role_name: Option<String>,
    /// Per-session recent role-switch timestamps (epoch millis). Used to throttle a
    /// burst of `/role`/`/back` commands (e.g. a membrane redelivery storm) so a
    /// rapid switch storm cannot drive an endless role-handoff ping-pong.
    role_switch_history: HashMap<String, std::collections::VecDeque<i64>>,
}

impl AgentRuntime {
    pub fn new(ipc_client: PhiloticClient, agent_id: impl Into<String>) -> Self {
        Self {
            ipc_client,
            agent_id: agent_id.into(),
            sessions: HashMap::new(),
            muninn_config: None,
            muninn_available: true,
            configured_roles: HashMap::new(),
            default_agent_profile: AgentProfile::default(),
            pending_drains: std::collections::VecDeque::new(),
            stuck_turn_first_seen: HashMap::new(),
            stuck_turn_signature: HashMap::new(),
            total_active_since: HashMap::new(),
            role_switch_history: HashMap::new(),
            network_offline: false,
            role_name: None,
        }
    }

    pub fn set_role_name(&mut self, rn: impl Into<String>) {
        self.role_name = Some(rn.into());
    }

    /// Fetch this agent's identity bundle from the hotel and store it as the default profile.
    /// Applied to every new session so the correct persona is used from the first message.
    async fn fetch_agent_profile(&mut self) {
        let key = format!("__agent_bundle__:{}", self.agent_id);
        match tokio::time::timeout(
            Duration::from_secs(5),
            self.ipc_client.send_request(IpcRequest::GetConfig { key }),
        )
        .await
        {
            Ok(Ok(IpcResponse::ConfigData {
                value_json: Some(json),
                ..
            })) => match serde_json::from_str::<AgentProfile>(&json) {
                Ok(mut profile) => {
                    info!(agent_id = %self.agent_id, "Agent profile loaded from hotel.");
                    profile.voice_response_policy.seed_voice_ids();
                    self.default_agent_profile = profile;
                }
                Err(e) => warn!("Failed to parse agent profile bundle: {}", e),
            },
            Ok(Ok(IpcResponse::ConfigData {
                value_json: None, ..
            })) => {
                info!(agent_id = %self.agent_id, "No agent identity bundle found in hotel — using default profile.");
            }
            Ok(Ok(_)) | Ok(Err(_)) => {
                warn!("Unexpected response to agent bundle fetch — using default profile.");
            }
            Err(_) => {
                warn!(agent_id = %self.agent_id, "Agent bundle fetch timed out — using default profile.");
            }
        }

        // Fetch hotel-level user profile and inject into agent profile when the
        // agent-specific profile doesn't already override the field.
        if let Some(hotel_name) = local_hotel_name() {
            match tokio::time::timeout(
                Duration::from_secs(5),
                self.ipc_client.send_request(IpcRequest::GetUserProfile {
                    hotel_name: hotel_name.clone(),
                }),
            )
            .await
            {
                Ok(Ok(IpcResponse::UserProfileData(p))) => {
                    if self.default_agent_profile.user_timezone.is_none() {
                        if let Some(tz) = p.timezone.clone() {
                            info!(hotel = %hotel_name, tz = %tz, "Injecting user timezone from hotel user profile.");
                            self.default_agent_profile.user_timezone = Some(tz);
                        }
                    }
                    if self.default_agent_profile.user_principal_id.is_none() {
                        self.default_agent_profile.user_principal_id = p.principal_id.clone();
                    }
                    if self.default_agent_profile.user_preferred_name.is_none() {
                        self.default_agent_profile.user_preferred_name = p.preferred_name.clone();
                    }
                    if self.default_agent_profile.user_primary_email.is_none() {
                        self.default_agent_profile.user_primary_email = p.primary_email.clone();
                    }
                    if self.default_agent_profile.user_linked_providers.is_empty() {
                        self.default_agent_profile.user_linked_providers =
                            p.linked_providers.clone();
                    }
                    if self.default_agent_profile.user_context_text.is_none() {
                        if let Some(context) = projected_user_context_from_profile(&p) {
                            info!(hotel = %hotel_name, "Injecting bounded projected user context from hotel identity.");
                            self.default_agent_profile.user_context_text = Some(context);
                        }
                    }
                }
                Ok(Ok(_)) | Ok(Err(_)) => {
                    // Non-fatal — hotel may not have a user profile configured yet.
                }
                Err(_) => {
                    warn!(agent_id = %self.agent_id, "GetUserProfile timed out at startup — continuing without user profile injection.");
                }
            }
        }

        // Apply operator-persisted policy overrides from hotel config keys.
        // These take precedence over the bundle so /voice and agent.configure persist
        // correctly across restarts without requiring a bundle rebuild.
        if let Ok(Ok(IpcResponse::ConfigData {
            value_json: Some(ref json),
            ..
        })) = tokio::time::timeout(
            Duration::from_secs(5),
            self.ipc_client.send_request(IpcRequest::GetConfig {
                key: "config:voice_response_policy".into(),
            }),
        )
        .await
        {
            if let Ok(policy) = serde_json::from_str::<VoiceResponsePolicy>(json) {
                self.default_agent_profile.voice_response_policy = policy;
            }
        }
        if let Ok(Ok(IpcResponse::ConfigData {
            value_json: Some(ref json),
            ..
        })) = tokio::time::timeout(
            Duration::from_secs(5),
            self.ipc_client.send_request(IpcRequest::GetConfig {
                key: "config:media_routing_policy".into(),
            }),
        )
        .await
        {
            if let Ok(policy) = serde_json::from_str::<MediaRoutingPolicy>(json) {
                self.default_agent_profile.media_routing_policy = policy;
            }
        }
    }

    /// Fetch all role incarnation names for this agent from the hotel and store them
    /// on the default agent profile. Called once at startup so every session gets
    /// the authoritative list injected into its system prompt.
    async fn fetch_role_names(&mut self) {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.ipc_client
                .send_request(IpcRequest::ListRoleIncarnations {
                    agent_id: self.agent_id.clone(),
                }),
        )
        .await;
        match result {
            Ok(Ok(IpcResponse::Standard {
                ok: true,
                data: Some(data),
                ..
            })) => {
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
            Err(_) => {
                warn!(agent_id = %self.agent_id, "fetch_role_names timed out (startup race) — continuing with empty roster");
            }
            _ => {
                info!(agent_id = %self.agent_id, "No role incarnations found for delegation roster.");
            }
        }
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
        let mcp_override = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.ipc_client
                .send_request(IpcRequest::GetConfig { key: key.clone() }),
        )
        .await
        .ok()
        .and_then(|r| r.ok());
        if let Some(IpcResponse::ConfigData {
            value_json: Some(json),
            ..
        }) = mcp_override
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
                        target_node: None,
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
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.ipc_client.send_request(IpcRequest::UpdateMcpRoutes {
                agent_id: self.agent_id.clone(),
                routes,
                vault_ref: None,
            }),
        )
        .await;
        match result {
            Ok(Ok(_)) => {
                info!(agent_id = %self.agent_id, count, "MCP routes registered with hotel.")
            }
            Ok(Err(e)) => {
                warn!(agent_id = %self.agent_id, err = %e, "Failed to register MCP routes")
            }
            Err(_) => {
                warn!(agent_id = %self.agent_id, "MCP route registration timed out (startup race) — continuing")
            }
        }
    }

    /// At startup, enumerate all session apartments for this agent and purge any
    /// stale active turns left over from an unclean shutdown. Cleans the DB so
    /// sessions are not blocked before the first inbound message arrives.
    async fn sweep_stale_session_turns(&mut self) {
        let list_key = format!("__session_apartments__:{}", self.agent_id);
        let memory_types: Vec<String> = match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.ipc_client
                .send_request(IpcRequest::GetConfig { key: list_key }),
        )
        .await
        {
            Ok(Ok(IpcResponse::ConfigData {
                value_json: Some(json),
                ..
            })) => serde_json::from_str::<Vec<String>>(&json).unwrap_or_default(),
            Err(_) => {
                warn!("sweep_stale_session_turns: apartment list fetch timed out — skipping sweep");
                return;
            }
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
            let checkpoint = match tokio::time::timeout(
                std::time::Duration::from_secs(5),
                self.ipc_client
                    .send_request(IpcRequest::GetConfig { key: snapshot_key }),
            )
            .await
            {
                Ok(Ok(IpcResponse::ConfigData {
                    value_json: Some(json),
                    ..
                })) => match serde_json::from_str::<serde_json::Value>(&json) {
                    Ok(v) => v,
                    Err(_) => continue,
                },
                Err(_) => {
                    warn!(
                        "sweep_stale_session_turns: snapshot fetch timed out for session {session_id} — skipping"
                    );
                    continue;
                }
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
                // from_checkpoint keeps WaitingTool turns as "resumable", but after a
                // process restart the tool runner connection is gone — the response will
                // never arrive. Evict the turn now so the checkpoint gets cleaned below
                // instead of leaving this session outside eviction coverage forever.
                state.active_turn = None;
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

    async fn persist_session_checkpoint(&mut self, session_id: &str) -> Result<()> {
        let Some(state) = self.sessions.get(session_id) else {
            return Ok(());
        };
        let checkpoint_memory_type = state.checkpoint_memory_type();
        let checkpoint_json = state.checkpoint_json();
        let index_state = state.clone();
        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await
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
        self.dispatch_graph_preload_once(&session_id).await;

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
                    if self
                        .sessions
                        .get(&session_id)
                        .map(Self::should_defer_parked_approval_command)
                        .unwrap_or(false)
                    {
                        if let Some(state) = self.sessions.get_mut(&session_id) {
                            state.prepend_user_task(task_id, task);
                        }
                        info!(
                            session_id = %session_id,
                            "Approval command arrived while another turn is active; deferring until the parked approval turn can resume"
                        );
                        return Ok(());
                    }

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

        if let Some(life_observe) = parse_direct_life_observe_command(&content) {
            return self
                .handle_direct_life_observe_command(
                    task_id,
                    session_id,
                    turn_id,
                    chat_id,
                    final_reply_to,
                    final_reply_role,
                    final_reply_guest_id,
                    life_observe,
                    inbound_primary_user_id(&task),
                )
                .await;
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

                        return Ok(());
                    }
                };

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
                let affordances = model_affordances(
                    self.sessions.get(&session_id),
                    &restored_user_content,
                    &tools,
                );
                let model_req = ModelRequestPayload {
                    action: "generate_text".to_string(),
                    request_class: Some("cognitive".to_string()),
                    session_id: session_id.clone(),
                    turn_id: restored_task_id.to_string(),
                    prompt,
                    user_content: restored_user_content,
                    context: Some(context),
                    context_projection: Some(context_projection),
                    affordances,
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
                                reply_to_guest_id: None,
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
            let (
                paracrine_origin,
                paracrine_reply_session_id,
                paracrine_reply_chat_id,
                paracrine_response_routing,
            ) = {
                let exosome = task
                    .exosome
                    .as_ref()
                    .and_then(|v| serde_json::from_value::<Exosome>(v.clone()).ok());
                (
                    exosome.as_ref().and_then(|e| e.paracrine_id.clone()),
                    exosome.as_ref().and_then(|e| e.source_session_id.clone()),
                    exosome.as_ref().and_then(|e| e.source_chat_id.clone()),
                    exosome.as_ref().and_then(|e| e.response_routing.clone()),
                )
            };

            state.start_turn(WorkingTurn {
                task_id,
                turn_id: turn_id.clone(),
                chat_id: chat_id.clone(),
                primary_user_id: inbound_primary_user_id(&task),
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
                paracrine_origin: paracrine_origin.clone(),
                paracrine_reply_session_id,
                paracrine_reply_chat_id,
                paracrine_response_routing,
                paracrine_merge_completed: false,
                plan_confirmed: false,
                plan_confirm_note: None,
                fallback_tier: if self.network_offline { 1 } else { 0 },
                streaming_retry_attempts: 0,
                streamed_content: String::new(),
                paracrine_hop_count: 0,
                paracrine_chain_started_at: None,
            });
            state.set_active_turn_phase(TurnPhase::LoadingContext);

            // Paracrine context: inject delegate.merge into execution_routes so the specialist
            // can call it without needing it in her toolset profile. The tool is already injected
            // into tools_for_model by project_tools_for_turn; this adds the matching route so
            // execute_bound_tool can resolve it.
            if paracrine_origin.is_some()
                && !state
                    .tool_assembly
                    .execution_routes
                    .contains_key("delegate.merge")
            {
                state.tool_assembly.execution_routes.insert(
                    "delegate.merge".into(),
                    ToolExecutionRoute {
                        target_node: local_node_id(),
                        target_role: "agent".into(),
                        runner_id: None,
                        incarnation_id: None,
                        hotel_id: None,
                        environment_id: None,
                        task_runner_kind: None,
                        task_runner_config: None,
                        execution_mode: "local_agent".into(),
                        availability_state: "live".into(),
                        selection_reason: Some("paracrine_auto_inject".into()),
                    },
                );
            }

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
            if self.network_offline
                && matches!(capability.as_str(), "text.generate" | "response.generate")
            {
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
        let affordances =
            model_affordances(self.sessions.get(&session_id), &content, &tools_for_model);
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
            affordances,
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

        if debug_model_requests_enabled()
            && matches!(capability.as_str(), "text.generate" | "response.generate")
        {
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

        // Persist the plan as a durable UserTask in the hotel context graph.
        // Best-effort — a storage failure here does not abort the planning flow.
        let user_task_id = Uuid::new_v4().to_string();
        let steps_json: Vec<serde_json::Value> = proposal
            .steps
            .iter()
            .enumerate()
            .map(|(idx, step)| {
                let description = step
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(step)")
                    .to_string();
                let risk = step
                    .get("risk")
                    .and_then(|v| v.as_str())
                    .or(proposal.approval_risk_hint.as_deref())
                    .unwrap_or("safe")
                    .to_string();
                json!({
                    "idx": idx,
                    "description": description,
                    "risk": risk,
                    "status": "pending",
                    "output": null,
                    "error": null,
                    "started_at": null,
                    "completed_at": null,
                })
            })
            .collect();
        let approved_risk_ceiling = proposal
            .approval_risk_hint
            .as_deref()
            .unwrap_or("safe")
            .to_string();

        let create_result = self
            .ipc_client
            .send_request(IpcRequest::CreateUserTask {
                task_id: user_task_id.clone(),
                session_id: session_id.clone(),
                agent_id: self.agent_id.clone(),
                chat_id: chat_id.clone(),
                goal: proposal.summary.clone(),
                approved_risk_ceiling,
                planning_model_tier: 0,
                quiet: false,
            })
            .await;

        match create_result {
            Ok(IpcResponse::UserTaskCreated { .. }) => {
                // Patch in the plan steps now that the node exists.
                let steps_str = serde_json::to_string(&steps_json).unwrap_or_default();
                let _ = self
                    .ipc_client
                    .send_request(IpcRequest::UpdateUserTask {
                        task_id: user_task_id.clone(),
                        status: "awaiting_approval".into(),
                        steps_json: Some(steps_str),
                        next_step_idx: Some(0),
                        approval_note: None,
                    })
                    .await;

                if let Some(state) = self.sessions.get_mut(&session_id) {
                    state.active_user_task_id = Some(user_task_id.clone());
                }
                info!(
                    user_task_id,
                    "UserTask created and steps set for plan proposal"
                );
            }
            Ok(other) => {
                warn!(?other, "CreateUserTask returned unexpected response");
            }
            Err(e) => {
                warn!(error = %e, "CreateUserTask failed — plan persists in-memory only");
            }
        }

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

    async fn emit_turn_status(&mut self, session_id: &str, status: String) -> Result<()> {
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

        let payload = TurnStatusPayload {
            action: "turn_status",
            session_id: session_id.to_string(),
            turn_id,
            chat_id,
            status,
        };

        tokio::time::timeout(
            Duration::from_secs(10),
            self.ipc_client.send_request(IpcRequest::EmitTask {
                target_node: final_reply_to,
                target_role: final_reply_role,
                target_guest_id: final_reply_guest_id,
                task_json: serde_json::to_string(&payload)?,
            }),
        )
        .await
        .map_err(|_| anyhow::anyhow!("emit_turn_status: ipc ack timeout after 10s"))??;

        Ok(())
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
        let affordances = model_affordances(
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
            affordances,
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
        let affordances = model_affordances(self.sessions.get(&session_id), &user_content, &tools);
        let model_req = ModelRequestPayload {
            action: "generate_text".to_string(),
            request_class: Some("cognitive".to_string()),
            session_id: session_id.clone(),
            turn_id,
            prompt,
            user_content,
            context: Some(context),
            context_projection: Some(context_projection),
            affordances,
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

        // Accumulate the delta into the active turn and resolve routing. The
        // model-router emits one `streaming_token` per SSE delta, but membrane's
        // draft edit replaces the message with `content` — so we must emit the
        // *cumulative* text built up so far, not the isolated token, or the
        // Telegram message flickers between fragments instead of growing.
        let Some(state) = self.sessions.get_mut(&session_id) else {
            // No active turn — token arrived after turn completed; drop silently.
            return Ok(());
        };
        let Some(turn) = state.active_turn.as_mut() else {
            return Ok(());
        };
        turn.streamed_content.push_str(&token);
        let accumulated = turn.streamed_content.clone();
        let reply_to = turn.final_reply_to.clone();
        let reply_role = turn.final_reply_role.clone();
        let reply_guest_id = turn.final_reply_guest_id.clone();
        let turn_id = turn.turn_id.clone();
        let chat_id = turn.chat_id.clone();
        let is_paracrine_turn = turn.paracrine_origin.is_some();

        if is_paracrine_turn {
            // Paracrine specialist output is private until it is wrapped as a
            // paracrine_response at final completion. Streaming it through the
            // ordinary reply route leaks aside tokens and can target a role with
            // no subscriber, leaving the response ledger-only.
            return Ok(());
        }

        let task_json = serde_json::to_string(&serde_json::json!({
            "action": "partial_reply",
            "session_id": session_id,
            "turn_id": turn_id,
            "chat_id": chat_id,
            "content": accumulated,
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

    /// model-router emits `action = "model_dispatch_status"` during its retry loop so users
    /// see transient state ("retrying...", "switching model...") rather than silence.
    /// The label is pre-formatted by the controller into `content`; philote forwards it
    /// as a partial_reply to membrane. Dropped silently if no active turn.
    async fn handle_model_dispatch_status(&mut self, task: InboundTaskPayload) -> Result<()> {
        let session_id = match &task.session_id {
            Some(s) => s.clone(),
            None => return Ok(()),
        };

        let label = match &task.content {
            Some(c) if !c.is_empty() => c.clone(),
            _ => return Ok(()),
        };

        let routing = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .filter(|t| t.paracrine_origin.is_none())
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
            return Ok(());
        };

        let task_json = serde_json::to_string(&serde_json::json!({
            "action": "partial_reply",
            "session_id": session_id,
            "turn_id": turn_id,
            "chat_id": chat_id,
            "content": label,
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

        // graph.query with no pending tool call = background preload; update the snapshot.
        // All other datasource responses (user-initiated tool calls) must be routed back to
        // the model as tool results so the turn doesn't hang waiting for a reply that never
        // comes. Previously these were silently dropped, causing the watchdog to fire.
        let is_preload = task.capability.as_deref() == Some("graph.query")
            && self
                .sessions
                .get(&session_id)
                .and_then(|s| s.active_turn.as_ref())
                .map(|t| t.pending_tool_call.is_none())
                .unwrap_or(true);

        if !is_preload {
            let active_turn = self
                .sessions
                .get(&session_id)
                .and_then(|state| state.active_turn.as_ref());
            let pending_tool_name = active_turn
                .and_then(|turn| turn.pending_tool_call.as_ref())
                .map(|tool| tool.tool_name.clone());

            // Drop error responses that have no routing context (empty turn_id + chat_id)
            // and could not identify the originating tool (capability="unknown").
            // These come from fire-and-forget datasource calls that fail asynchronously;
            // routing them to the active turn would corrupt the pending tool result.
            let has_no_context = task.turn_id.as_deref().filter(|s| !s.is_empty()).is_none()
                && task.chat_id.as_deref().filter(|s| !s.is_empty()).is_none();
            let is_unattributable_error =
                task.error.is_some() && task.capability.as_deref().is_none_or(|c| c == "unknown");
            if has_no_context && is_unattributable_error {
                return Ok(());
            }

            let turn_id = task
                .turn_id
                .clone()
                .filter(|id| !id.is_empty())
                .or_else(|| active_turn.map(|turn| turn.turn_id.clone()));
            let chat_id = task
                .chat_id
                .clone()
                .filter(|id| !id.is_empty())
                .or_else(|| active_turn.map(|turn| turn.chat_id.clone()));
            let tool_name = task
                .tool_name
                .clone()
                .filter(|name| !name.is_empty())
                .or(pending_tool_name)
                .or_else(|| task.capability.clone().filter(|name| !name.is_empty()));

            // Convert datasource success/failure into a tool_result the model can read.
            let content = if let Some(ref err) = task.error {
                format!(
                    "Tool call failed: {} (provider: {}, capability: {})",
                    err.message,
                    err.provider.as_deref().unwrap_or("unknown"),
                    task.capability.as_deref().unwrap_or("unknown"),
                )
            } else {
                task.result
                    .as_ref()
                    .map(|r| serde_json::to_string_pretty(r).unwrap_or_else(|_| r.to_string()))
                    .unwrap_or_else(|| "(empty result)".into())
            };
            return self
                .handle_tool_result(InboundTaskPayload {
                    action: Some("tool_result".into()),
                    content: Some(content),
                    session_id: task.session_id.clone(),
                    turn_id,
                    chat_id,
                    tool_name,
                    error: task.error.clone(),
                    ..Default::default()
                })
                .await;
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
                session_id: None,
                turn_id: None,
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
        let affordances = model_affordances(
            self.sessions.get(&session_id),
            &user_content,
            &tools_for_model,
        );
        let model_req = ModelRequestPayload {
            action: "generate_text".to_string(),
            request_class: Some("cognitive".to_string()),
            session_id: session_id.clone(),
            turn_id,
            prompt,
            user_content,
            context,
            context_projection,
            affordances,
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
        let response = match tokio::time::timeout(
            Duration::from_secs(10),
            self.ipc_client.send_request(IpcRequest::GetConfig {
                key: format!("__session_snapshot__:{session_id}"),
            }),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => return, // timeout — skip the bindings refresh
        };

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
        let new_allowed_classes: Option<Vec<String>> = bindings
            .get("allowed_classes")
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
        if let Some(allowed_classes) = new_allowed_classes {
            if allowed_classes != state.bindings.allowed_classes {
                state.bindings.allowed_classes = allowed_classes;
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
        let response = match tokio::time::timeout(
            Duration::from_secs(15),
            self.ipc_client
                .send_request(IpcRequest::GetConfig { key: snapshot_key }),
        )
        .await
        {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                warn!(
                    session_id = %session_id,
                    "ensure_session_loaded: GetConfig timed out after 15s — starting fresh session"
                );
                IpcResponse::ConfigData {
                    key: String::new(),
                    value_json: None,
                }
            }
        };

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

                        // Apply iteration_cap from the restored role's turn_loop_config.
                        // settings are not persisted in the checkpoint, so this must be
                        // re-applied every time we restore.
                        if let Some(cap) = state
                            .role_activation
                            .as_ref()
                            .and_then(|ra| ra.turn_loop_config.as_ref())
                            .and_then(|c| c.iteration_cap)
                        {
                            state.settings.execution.iteration_cap = cap.clamp(1, 50);
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
        let default_role = self
            .default_agent_profile
            .default_role_name
            .clone()
            .filter(|role| !role.trim().is_empty())
            .unwrap_or_else(|| "orchestrator".into());
        {
            if let Some(activation) = self.fetch_role_activation(&default_role).await {
                let toolset_profile_ref = activation.toolset_profile_ref.clone();
                if let Some(cap) = activation
                    .turn_loop_config
                    .as_ref()
                    .and_then(|c| c.iteration_cap)
                {
                    state.settings.execution.iteration_cap = cap.clamp(1, 50);
                }
                state.role_activation = Some(activation);
                if let Some(profile_name) = toolset_profile_ref.as_deref() {
                    self.hydrate_bindings_from_toolset_profile(&mut state, profile_name)
                        .await;
                }
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
        match tokio::time::timeout(
            Duration::from_secs(5),
            ipc_client.send_request(IpcRequest::ListRules {
                agent_id: agent_id.to_string(),
            }),
        )
        .await
        {
            Ok(Ok(IpcResponse::RuleList { rules })) => {
                state.rules = rules;
            }
            Ok(_) | Err(_) => {
                // Non-fatal: session proceeds without rules if the hotel is unavailable or times out.
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

fn projected_user_context_from_profile(profile: &UserProfileDataPayload) -> Option<String> {
    let display_name = profile
        .preferred_name
        .as_deref()
        .or(profile.display_name.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let principal_id = profile
        .principal_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let home_hotel = profile
        .home_hotel
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let provider_summary = if profile.linked_providers.is_empty() {
        None
    } else {
        Some(profile.linked_providers.join(", "))
    };

    if display_name.is_none()
        && principal_id.is_none()
        && home_hotel.is_none()
        && provider_summary.is_none()
    {
        return None;
    }

    let mut lines = Vec::new();
    if let Some(name) = display_name {
        lines.push(format!("Current operator: {name}."));
    }
    if let Some(principal_id) = principal_id {
        lines.push(format!("Stable operator principal: {principal_id}."));
    }
    if let Some(home_hotel) = home_hotel {
        lines.push(format!("Operator identity home hotel: {home_hotel}."));
    }
    if let Some(provider_summary) = provider_summary {
        lines.push(format!(
            "Recognized login providers for this operator: {provider_summary}."
        ));
    }
    Some(lines.join(" "))
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
        loop_stop_fallback_reply, loop_stop_reason, media_analysis_attachments,
        normalized_user_content, resolve_media_routing, resolve_model_execution_target,
        should_attempt_provider_repair,
    };
    use crate::commands::SlashCommand;
    use crate::r#loop::{ApprovalRequest, ToolCall, ToolResult, TurnPhase};
    use crate::protocol::{
        FinalReplyPayload, InboundTaskPayload, ModelRequestPayload, TransportAttachment,
    };
    use crate::session::{
        ApprovalPolicy, ComponentExecutionRoute, ComponentRouteAssembly, ComponentRouteBinding,
        ResponseRouteMode, SessionState, WorkingTurn,
    };
    use philotic_client::{TaskErrorPayload, UserProfileDataPayload};
    use uuid::Uuid;

    pub(super) fn test_working_turn(phase: TurnPhase) -> WorkingTurn {
        WorkingTurn {
            task_id: Uuid::nil(),
            turn_id: "turn-1".into(),
            chat_id: "123".into(),
            primary_user_id: None,
            user_content: "test".into(),
            final_reply_to: "local-aiua-01".into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: None,
            phase,
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
            had_voice_input: false,
            awaiting_transcription_reentry: false,
            scripted_loop_context: None,
            associated_paracrine_ids: Vec::new(),
            paracrine_origin: None,
            paracrine_reply_session_id: None,
            paracrine_reply_chat_id: None,
            paracrine_response_routing: None,
            paracrine_merge_completed: false,
            plan_confirmed: false,
            plan_confirm_note: None,
            fallback_tier: 0,
            streaming_retry_attempts: 0,
            streamed_content: String::new(),
            paracrine_hop_count: 0,
            paracrine_chain_started_at: None,
        }
    }

    fn push_test_tool(turn: &mut WorkingTurn, tool_name: &str, content: &str) {
        turn.working_tool_history.push((
            ToolCall {
                tool_name: tool_name.into(),
                arguments: serde_json::json!({}),
            },
            ToolResult {
                tool_name: tool_name.into(),
                content: content.into(),
            },
        ));
        turn.iteration += 1;
    }

    #[test]
    fn loop_stop_reason_detects_low_progress_diagnostic_run() {
        let mut turn = test_working_turn(TurnPhase::WaitingModel);
        for tool_name in ["hotel.status", "role.list", "skill.list", "session.status"] {
            push_test_tool(&mut turn, tool_name, "ok");
        }

        let reason = loop_stop_reason(&turn, 10).expect("diagnostic run should stop");
        assert!(reason.contains("status"));
    }

    #[test]
    fn loop_stop_reason_allows_non_diagnostic_progress() {
        let mut turn = test_working_turn(TurnPhase::WaitingModel);
        for tool_name in [
            "hotel.status",
            "life.recall",
            "memory.recall",
            "life.observe",
        ] {
            push_test_tool(&mut turn, tool_name, "ok");
        }

        assert!(loop_stop_reason(&turn, 10).is_none());
    }

    #[test]
    fn loop_stop_fallback_names_recent_tool_path() {
        let mut turn = test_working_turn(TurnPhase::WaitingModel);
        turn.user_content = "let's try that again".into();
        push_test_tool(&mut turn, "hotel.status", "ok");
        push_test_tool(&mut turn, "session.status", "ok");

        let reply = loop_stop_fallback_reply(
            &turn.user_content,
            &turn.working_tool_history,
            "the turn reached its maximum tool-iteration limit",
        );

        assert!(reply.contains("instead of looping"));
        assert!(reply.contains("hotel.status, session.status"));
        assert!(reply.contains("let's try that again"));
    }

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
            affordances: None,
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
        assert_eq!(
            super::implementation_to_model_role("ollama"),
            "model.ollama"
        );
        assert_eq!(
            super::implementation_to_model_role("ollama-llama3"),
            "model.ollama"
        );
        assert_eq!(super::implementation_to_model_role("mlx"), "model.mlx");
        assert_eq!(
            super::implementation_to_model_role("mlx-community/llama"),
            "model.mlx"
        );
        assert_eq!(super::implementation_to_model_role("onnx"), "model.local");
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
            primary_user_id: None,
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
            paracrine_response_routing: None,
            paracrine_merge_completed: false,
            plan_confirmed: false,
            plan_confirm_note: None,
            fallback_tier: 0,
            streaming_retry_attempts: 0,
            streamed_content: String::new(),
            paracrine_hop_count: 0,
            paracrine_chain_started_at: None,
        });

        assert!(should_attempt_provider_repair(&error, Some(&state)));
        state.increment_provider_repair_attempts();
        assert!(!should_attempt_provider_repair(&error, Some(&state)));
    }

    #[test]
    fn provider_auth_failure_escalates_fallback_tier_without_same_provider_repair() {
        let error = TaskErrorPayload {
            kind: "provider_failure".into(),
            message: "Gemini API error (400): API key expired. Please renew the API key.".into(),
            code: None,
            component: Some("model-router".into()),
            provider: Some("gemini".into()),
            capability: Some("text.generate".into()),
            retryable: Some(false),
            sub_kind: Some("provider_auth".into()),
        };

        let state = SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());

        assert!(!should_attempt_provider_repair(&error, Some(&state)));
        assert!(super::should_escalate_tier(&error));
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

    #[test]
    fn projected_user_context_from_profile_formats_bounded_identity_summary() {
        let profile = UserProfileDataPayload {
            timezone: Some("America/New_York".into()),
            display_name: Some("Jared Likes".into()),
            principal_id: Some("user:google:subject-123".into()),
            preferred_name: Some("Jared".into()),
            primary_email: Some("jared@example.com".into()),
            home_hotel: Some("vps-jane".into()),
            linked_providers: vec!["google".into(), "github".into()],
        };

        let summary = super::projected_user_context_from_profile(&profile)
            .expect("bounded user context should be present");
        assert!(summary.contains("Current operator: Jared."));
        assert!(summary.contains("Stable operator principal: user:google:subject-123."));
        assert!(summary.contains("Operator identity home hotel: vps-jane."));
        assert!(summary.contains("Recognized login providers for this operator: google, github."));
        assert!(!summary.contains("jared@example.com"));
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

    // ── DEF-004 regression: multi-tool re-entry must surface the final reply ──
    //
    // Drives a turn through two tool_result re-entries and a final model Respond,
    // over a recording stub hotel, and asserts exactly one `send_reply` task is
    // emitted to the turn's final_reply_* transport target with the synthesized text.

    /// Minimal hotel harness that records every EmitTask envelope it receives
    /// (target fields + parsed task_json) and acks everything else generically.
    pub(super) async fn run_recording_hotel(
        listener: tokio::net::UnixListener,
        emitted: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut stream, _) = listener.accept().await.expect("accept");
        loop {
            let buf = match async {
                let mut len_buf = [0u8; 4];
                stream.read_exact(&mut len_buf).await?;
                let len = u32::from_be_bytes(len_buf) as usize;
                let mut buf = vec![0u8; len];
                stream.read_exact(&mut buf).await?;
                Ok::<_, std::io::Error>(buf)
            }
            .await
            {
                Ok(b) => b,
                Err(_) => return, // client disconnected
            };

            let req: philotic_client::IpcRequest =
                serde_json::from_slice(&buf).expect("decode request");
            let reply = match &req {
                philotic_client::IpcRequest::GetConfig { key } => {
                    serde_json::to_vec(&philotic_client::IpcResponse::ConfigData {
                        key: key.clone(),
                        value_json: None,
                    })
                    .unwrap()
                }
                philotic_client::IpcRequest::EmitTask {
                    target_node,
                    target_role,
                    target_guest_id,
                    task_json,
                } => {
                    let task: serde_json::Value =
                        serde_json::from_str(task_json).unwrap_or(serde_json::Value::Null);
                    emitted.lock().unwrap().push(serde_json::json!({
                        "target_node": target_node,
                        "target_role": target_role,
                        "target_guest_id": target_guest_id,
                        "task": task,
                    }));
                    serde_json::to_vec(&philotic_client::IpcResponse::success("ok", None)).unwrap()
                }
                _ => {
                    serde_json::to_vec(&philotic_client::IpcResponse::success("ok", None)).unwrap()
                }
            };

            let len = u32::try_from(reply.len()).expect("frame length fits u32");
            stream
                .write_all(&len.to_be_bytes())
                .await
                .expect("write header");
            stream.write_all(&reply).await.expect("write payload");
        }
    }

    pub(super) fn def004_working_turn(turn_id: &str, first_tool: &str) -> WorkingTurn {
        let mut turn = test_working_turn(TurnPhase::WaitingTool);
        turn.turn_id = turn_id.into();
        turn.chat_id = "555".into();
        turn.user_content = "compare hotel and session status".into();
        turn.final_reply_to = "membrane-node-01".into();
        turn.final_reply_role = "membrane".into();
        turn.final_reply_guest_id = Some("membrane-seat-1".into());
        turn.pending_tool_call = Some(ToolCall {
            tool_name: first_tool.into(),
            arguments: serde_json::json!({}),
        });
        turn
    }

    #[tokio::test]
    async fn multi_tool_reentry_surfaces_final_send_reply() {
        let socket_path = format!("/tmp/philote-def004-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-def004".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-def004");

        let session_id = "sess-def004";
        let turn_id = "turn-def004";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        // Seed an active turn waiting on its first tool result, bound to a
        // membrane transport target (the real final_reply_* shape).
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(def004_working_turn(turn_id, "hotel.status"));

        // Tool result #1 → first model re-entry (iteration 1).
        runtime
            .handle_tool_result(InboundTaskPayload {
                action: Some("tool_result".into()),
                session_id: Some(session_id.into()),
                turn_id: Some(turn_id.into()),
                tool_name: Some("hotel.status".into()),
                content: Some("hotel green".into()),
                ..Default::default()
            })
            .await
            .expect("tool result 1");

        // Simulate the model dispatching a second tool call.
        {
            let state = runtime.sessions.get_mut(session_id).expect("session");
            state.set_pending_tool_call(ToolCall {
                tool_name: "session.status".into(),
                arguments: serde_json::json!({}),
            });
            state.set_active_turn_phase(TurnPhase::WaitingTool);
        }

        // Tool result #2 → second model re-entry (iteration 2).
        runtime
            .handle_tool_result(InboundTaskPayload {
                action: Some("tool_result".into()),
                session_id: Some(session_id.into()),
                turn_id: Some(turn_id.into()),
                tool_name: Some("session.status".into()),
                content: Some("session green".into()),
                ..Default::default()
            })
            .await
            .expect("tool result 2");

        // Sanity: both results are in the working tool history and iteration advanced.
        {
            let state = runtime.sessions.get(session_id).expect("session");
            let turn = state.active_turn.as_ref().expect("turn still active");
            assert_eq!(turn.iteration, 2, "two re-entries must advance iteration");
            assert_eq!(turn.working_tool_history.len(), 2);
        }

        // Final model Respond with the synthesized summary.
        let final_text = "Final synthesized summary: hotel green, session green.";
        runtime
            .handle_model_response(InboundTaskPayload {
                action: Some("model_response".into()),
                session_id: Some(session_id.into()),
                turn_id: Some(turn_id.into()),
                agent_action: Some(serde_json::json!({
                    "kind": "respond",
                    "content": final_text,
                })),
                content: Some(final_text.into()),
                ..Default::default()
            })
            .await
            .expect("final model respond");

        // The turn must be completed and out of the active slot.
        {
            let state = runtime.sessions.get(session_id).expect("session");
            assert!(
                state.active_turn.is_none(),
                "turn must complete after final respond"
            );
            let last = state.recent_turns.last().expect("turn recorded");
            assert_eq!(last.assistant_content.as_deref(), Some(final_text));
        }

        drop(runtime); // closes socket → server exits
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();

        let reentries: Vec<_> = emitted
            .iter()
            .filter(|e| e["task"]["action"] == "generate_text")
            .collect();
        assert!(
            reentries.len() >= 2,
            "expected >=2 model re-entries, got {}: {:#?}",
            reentries.len(),
            *emitted
        );

        let send_replies: Vec<_> = emitted
            .iter()
            .filter(|e| e["task"]["action"] == "send_reply")
            .collect();
        assert_eq!(
            send_replies.len(),
            1,
            "exactly one send_reply must surface: {:#?}",
            *emitted
        );
        let reply = send_replies[0];
        assert_eq!(reply["task"]["session_id"], session_id);
        assert_eq!(reply["task"]["turn_id"], turn_id);
        assert!(
            reply["task"]["content"]
                .as_str()
                .expect("send_reply content")
                .contains("Final synthesized summary"),
            "send_reply must carry the synthesized text: {reply:#?}"
        );
        // The reply must go to the turn's bound transport target, not fan out.
        assert_eq!(reply["target_node"], "membrane-node-01");
        assert_eq!(reply["target_role"], "membrane");
        assert_eq!(reply["target_guest_id"], "membrane-seat-1");
    }
}
