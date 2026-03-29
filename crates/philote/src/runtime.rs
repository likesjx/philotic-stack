use crate::commands::{SlashCommand, command_manifest, parse_slash_command};
use crate::r#loop::{
    AgentAction, ApprovalRequest, ToolCall, ToolResult, TurnPhase, interpret_model_payload,
};
use crate::protocol::{
    FinalReplyPayload, InboundTaskPayload, ModelRequestPayload, PartialReplyPayload,
    TaskRunnerOverlay, ToolExecutionPayload, TransportAttachment, TurnEventPayload,
};
use crate::session::{
    ActivePlan, AgentProfile, ApprovalInterruptDisposition, ComponentRouteAssembly,
    MediaRoutingPolicy, RecalledMemoryRecord, RoleActivation, RoutingPreferenceBinding,
    SessionBindings, SessionState, TargetRoleLens, ToolExecutionRoute, TtsMode,
    TurnCapabilityCompositionKind, TurnContextEnvelopeKind, TurnRoutedCapabilityProfile,
    TurnRoutedCapabilitySpecies, TurnRoutingPlan, TurnRoutingStageKind, TurnRoutingStagePlan,
    VoiceResponsePolicy, WorkingTurn, merge_session_index, turn_routed_capability_profile,
};
use anyhow::Result;
use media_prep::{extract_audio_artifact_json, parse_audio_artifact_json};
use memory_core::{
    MemoryScope, MuninnConfig, MuninnRestEngine, RecallContext, RecallTrigger, VaultResolver,
};
use philotic_client::{
    HandoffBundle, IpcRequest, IpcResponse, PhiloticClient, TaskErrorPayload, is_ipc_disconnect,
};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;
use tracing::{error, info, warn};
use uuid::Uuid;

pub const DEFAULT_AGENT_ID: &str = "agent-jane-01";
const DEFAULT_REPLY_ROLE: &str = "membrane";
const DEFAULT_TEXT_MODEL_ROLE: &str = "model";
const DEFAULT_VOICE_MODEL_ROLE: &str = "model.elevenlabs";

#[derive(Debug, Clone)]
struct LearnedReflexWriteback {
    preference_key: String,
    precedence: i32,
    reflexes_json: serde_json::Value,
}

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

fn parse_learned_reflex_writeback(
    args: &serde_json::Value,
) -> std::result::Result<Option<LearnedReflexWriteback>, String> {
    let Some(learned_reflex) = args.get("learned_reflex") else {
        return Ok(None);
    };
    let Some(obj) = learned_reflex.as_object() else {
        return Err("routing.policy.propose: 'learned_reflex' must be an object.".into());
    };
    let preference_key = obj
        .get("preference_key")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if preference_key.is_empty() {
        return Err("routing.policy.propose: learned_reflex.preference_key is required.".into());
    }
    let precedence = obj.get("precedence").and_then(|v| v.as_i64()).unwrap_or(70) as i32;
    let reflexes_json = obj
        .get("reflexes")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    if !reflexes_json.is_object() {
        return Err("routing.policy.propose: learned_reflex.reflexes must be an object.".into());
    }
    Ok(Some(LearnedReflexWriteback {
        preference_key,
        precedence,
        reflexes_json,
    }))
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

fn extract_model_audio_artifact(model_result: Option<&Value>) -> Option<String> {
    extract_audio_artifact_json(model_result)
}

fn extract_native_live_pending_function_call_id(task: &InboundTaskPayload) -> Option<String> {
    task.agent_action
        .as_ref()
        .and_then(|action| action.get("model_result"))
        .and_then(|model_result| model_result.get("native_live"))
        .and_then(|native_live| native_live.get("pending_function_call_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn parse_native_live_tool_response_content(content: &str) -> Value {
    serde_json::from_str::<Value>(content).unwrap_or_else(|_| Value::String(content.to_string()))
}

fn should_attempt_provider_repair(error: &TaskErrorPayload, state: Option<&SessionState>) -> bool {
    error.kind == "provider_failure"
        && error.retryable.unwrap_or(false)
        && error.capability.as_deref() == Some("text.generate")
        && state
            .map(|state| state.provider_repair_attempts() < 1)
            .unwrap_or(false)
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

fn resolve_stage_execution_target(
    state: Option<&SessionState>,
    stage: &TurnRoutingStagePlan,
) -> (String, String, Option<String>) {
    resolve_model_execution_target(state, stage.capability.as_str(), &stage.controller_role)
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

fn role_to_provider_hint(role: &str) -> Option<String> {
    let provider = role
        .strip_prefix("model.")
        .filter(|provider| !provider.is_empty())?;
    Some(provider.to_string())
}

fn cognitive_controller_role(capability: &str) -> &'static str {
    match capability {
        "voice.dialogue" => DEFAULT_TEXT_MODEL_ROLE,
        "response.generate" => DEFAULT_TEXT_MODEL_ROLE,
        _ => DEFAULT_TEXT_MODEL_ROLE,
    }
}

fn cognitive_stage_plan(capability: &str) -> TurnRoutingStagePlan {
    let profile =
        turn_routed_capability_profile(capability).unwrap_or(TurnRoutedCapabilityProfile {
            species: TurnRoutedCapabilitySpecies::TextGenerate,
            capability: "text.generate",
            request_class: "cognitive",
            default_stage_kind: TurnRoutingStageKind::Cognition,
            default_context_envelope: TurnContextEnvelopeKind::Cognitive,
            composition: TurnCapabilityCompositionKind::StageLocal,
            default_streaming: true,
        });
    let controller_role = cognitive_controller_role(profile.capability);
    TurnRoutingStagePlan {
        kind: profile.default_stage_kind,
        capability: profile.capability.into(),
        request_class: profile.request_class.into(),
        context_envelope: profile.default_context_envelope,
        controller_role: controller_role.into(),
        provider_hint: role_to_provider_hint(controller_role),
        model_ref: None,
        streaming: profile.default_streaming,
    }
}

fn voice_turn_supports_native_live(
    media_routing: Option<&MediaRouting>,
    had_voice_input: bool,
) -> bool {
    had_voice_input
        || media_routing
            .map(|routing| routing.capability == "voice.transcribe")
            .unwrap_or(false)
}

fn routing_preference_matches_stage(
    preference: &RoutingPreferenceBinding,
    stage: &TurnRoutingStagePlan,
) -> bool {
    preference
        .stage_kind
        .as_deref()
        .map(|kind| kind == stage_kind_name(stage.kind))
        .unwrap_or(true)
        && preference
            .capability
            .as_deref()
            .map(|capability| capability == stage.capability)
            .unwrap_or(true)
}

fn shared_model_ligand_signal(stage: &TurnRoutingStagePlan, bindings: &SessionBindings) -> i32 {
    bindings
        .shared_model_markers
        .iter()
        .map(|marker| model_marker_stage_signal(marker, stage))
        .max()
        .unwrap_or(0)
}

fn cognitive_receptor_baseline(stage: &TurnRoutingStagePlan, _voice_turn: bool) -> i32 {
    match stage.capability.as_str() {
        "text.generate" => 1,
        _ => 0,
    }
}

fn cognitive_receptor_score(
    stage: &TurnRoutingStagePlan,
    routing_preferences: &[RoutingPreferenceBinding],
    bindings: &SessionBindings,
    voice_turn: bool,
) -> i32 {
    let preference_signal = routing_preferences
        .iter()
        .filter(|pref| pref.preference_level >= 0)
        .filter(|pref| pref.provider_hint.is_some() || pref.model_ref.is_some())
        .filter(|pref| routing_preference_matches_stage(pref, stage))
        .map(|pref| routing_preference_score(pref, stage, bindings))
        .max()
        .unwrap_or(0);

    cognitive_receptor_baseline(stage, voice_turn)
        + preference_signal
        + shared_model_ligand_signal(stage, bindings)
}

fn select_cognitive_receptor_stage(
    media_routing: Option<&MediaRouting>,
    had_voice_input: bool,
    routing_preferences: &[RoutingPreferenceBinding],
    bindings: &SessionBindings,
) -> TurnRoutingStagePlan {
    let voice_turn = voice_turn_supports_native_live(media_routing, had_voice_input);
    let mut candidates = vec![cognitive_stage_plan("text.generate")];
    if voice_turn {
        candidates.push(cognitive_stage_plan("response.generate"));
        candidates.push(cognitive_stage_plan("voice.dialogue"));
    }

    candidates
        .into_iter()
        .max_by(|left, right| {
            cognitive_receptor_score(left, routing_preferences, bindings, voice_turn)
                .cmp(&cognitive_receptor_score(
                    right,
                    routing_preferences,
                    bindings,
                    voice_turn,
                ))
                .then_with(|| left.capability.cmp(&right.capability))
        })
        .unwrap_or_else(|| cognitive_stage_plan("text.generate"))
}

fn active_or_default_cognitive_stage(state: Option<&SessionState>) -> TurnRoutingStagePlan {
    state
        .and_then(|session| session.active_turn_routing_plan())
        .and_then(|plan| stage_plan(plan, TurnRoutingStageKind::Cognition))
        .cloned()
        .unwrap_or_else(|| cognitive_stage_plan("text.generate"))
}

fn native_live_voice_prompt(content: &str, capability: &str) -> String {
    let context = content.trim();
    match capability {
        "voice.dialogue" => {
            if context.is_empty() {
                "Respond helpfully to the user's attached voice input as a native streaming voice dialogue turn.".to_string()
            } else {
                format!(
                    "Respond helpfully to the user's attached voice input as a native streaming voice dialogue turn. User context: {}.",
                    context
                )
            }
        }
        "response.generate" => {
            if context.is_empty() {
                "Generate the best response for the user's attached voice input, using native multimodal response behavior when supported.".to_string()
            } else {
                format!(
                    "Generate the best response for the user's attached voice input, using native multimodal response behavior when supported. User context: {}.",
                    context
                )
            }
        }
        _ => transcription_prompt(content),
    }
}

fn compile_turn_routing_plan(
    media_routing: Option<&MediaRouting>,
    voice_policy: Option<&VoiceResponsePolicy>,
    had_voice_input: bool,
    routing_preferences: &[RoutingPreferenceBinding],
    bindings: &SessionBindings,
) -> TurnRoutingPlan {
    let mut stages = Vec::new();
    let cognition_stage = select_cognitive_receptor_stage(
        media_routing,
        had_voice_input,
        routing_preferences,
        bindings,
    );
    let collapses_ingress = media_routing
        .map(|routing| routing.capability == "voice.transcribe")
        .unwrap_or(false)
        && turn_routed_capability_profile(&cognition_stage.capability)
            .map(|profile| {
                profile.composition == TurnCapabilityCompositionKind::CollapsibleIngressCognition
            })
            .unwrap_or(false);

    if let Some(routing) = media_routing {
        if !collapses_ingress {
            let profile = turn_routed_capability_profile(routing.capability).unwrap_or(
                TurnRoutedCapabilityProfile {
                    species: TurnRoutedCapabilitySpecies::MediaAnalyze,
                    capability: "media.analyze",
                    request_class: "transform",
                    default_stage_kind: TurnRoutingStageKind::Ingress,
                    default_context_envelope: TurnContextEnvelopeKind::Ingress,
                    composition: TurnCapabilityCompositionKind::StageLocal,
                    default_streaming: false,
                },
            );
            let controller_role = match routing.capability {
                "voice.transcribe" => DEFAULT_VOICE_MODEL_ROLE,
                _ => DEFAULT_TEXT_MODEL_ROLE,
            };
            stages.push(TurnRoutingStagePlan {
                kind: profile.default_stage_kind,
                capability: routing.capability.to_string(),
                request_class: profile.request_class.into(),
                context_envelope: profile.default_context_envelope,
                controller_role: controller_role.into(),
                provider_hint: role_to_provider_hint(controller_role),
                model_ref: None,
                streaming: profile.default_streaming,
            });
        }
    }

    stages.push(cognition_stage.clone());

    let tts_mode_enabled = voice_policy
        .map(|policy| match policy.mode {
            TtsMode::Off => false,
            TtsMode::Auto => had_voice_input,
            TtsMode::On => true,
        })
        .unwrap_or(false);
    if tts_mode_enabled {
        let controller_role = DEFAULT_VOICE_MODEL_ROLE;
        let egress_profile = turn_routed_capability_profile("voice.synthesize").unwrap_or(
            TurnRoutedCapabilityProfile {
                species: TurnRoutedCapabilitySpecies::VoiceSynthesize,
                capability: "voice.synthesize",
                request_class: "synthesis",
                default_stage_kind: TurnRoutingStageKind::Egress,
                default_context_envelope: TurnContextEnvelopeKind::Egress,
                composition: TurnCapabilityCompositionKind::StageLocal,
                default_streaming: true,
            },
        );
        stages.push(TurnRoutingStagePlan {
            kind: egress_profile.default_stage_kind,
            capability: egress_profile.capability.into(),
            request_class: egress_profile.request_class.into(),
            context_envelope: egress_profile.default_context_envelope,
            controller_role: controller_role.into(),
            provider_hint: role_to_provider_hint(controller_role),
            model_ref: None,
            streaming: egress_profile.default_streaming,
        });
    }

    let mut plan = TurnRoutingPlan {
        trigger: if collapses_ingress {
            "voice_input_native_live".into()
        } else if media_routing
            .map(|routing| routing.capability == "voice.transcribe")
            .unwrap_or(false)
        {
            "voice_input".into()
        } else if had_voice_input {
            "voice_input_no_transform".into()
        } else {
            "text_input".into()
        },
        stages,
    };
    apply_routing_preferences(&mut plan, routing_preferences, bindings);
    plan
}

fn stage_kind_name(kind: TurnRoutingStageKind) -> &'static str {
    match kind {
        TurnRoutingStageKind::Ingress => "ingress",
        TurnRoutingStageKind::Cognition => "cognition",
        TurnRoutingStageKind::Egress => "egress",
    }
}

fn select_stage_routing_preference<'a>(
    stage: &TurnRoutingStagePlan,
    routing_preferences: &'a [RoutingPreferenceBinding],
    bindings: &SessionBindings,
) -> Option<&'a RoutingPreferenceBinding> {
    routing_preferences
        .iter()
        .filter(|pref| pref.preference_level >= 0)
        .filter(|pref| pref.provider_hint.is_some() || pref.model_ref.is_some())
        .filter(|pref| routing_preference_matches_stage(pref, stage))
        .max_by(|left, right| {
            routing_preference_score(left, stage, bindings)
                .cmp(&routing_preference_score(right, stage, bindings))
                .then_with(|| left.updated_at.cmp(&right.updated_at))
                .then_with(|| left.preference_key.cmp(&right.preference_key))
        })
}

fn routing_preference_score(
    preference: &RoutingPreferenceBinding,
    stage: &TurnRoutingStagePlan,
    bindings: &SessionBindings,
) -> i32 {
    preference.preference_level * 100
        + preference.weight
        + routing_preference_catalog_signal(preference, stage, bindings)
        + routing_preference_reflex_adjustment(preference, stage, bindings)
}

fn routing_preference_catalog_signal(
    preference: &RoutingPreferenceBinding,
    stage: &TurnRoutingStagePlan,
    bindings: &SessionBindings,
) -> i32 {
    bindings
        .shared_model_markers
        .iter()
        .filter(|marker| model_marker_matches_preference(marker, preference))
        .map(|marker| model_marker_stage_signal(marker, stage))
        .max()
        .unwrap_or(0)
}

fn routing_preference_reflex_adjustment(
    preference: &RoutingPreferenceBinding,
    stage: &TurnRoutingStagePlan,
    bindings: &SessionBindings,
) -> i32 {
    let touches_explicit_route =
        preference.provider_hint.is_some() || preference.model_ref.is_some();
    if !touches_explicit_route || stage.kind == TurnRoutingStageKind::Ingress {
        return 0;
    }

    let remote_component_reflex = bindings
        .effective_reflexes
        .get("remote_component_reflex")
        .and_then(|value| value.as_str());
    let reward_bonus = bindings
        .reflex_policy_agent_rewards
        .iter()
        .filter(|marker| reflex_marker_matches_preference(marker, preference))
        .count() as i32
        * 5;
    let immune_penalty = bindings
        .reflex_policy_agent_suppressions
        .iter()
        .filter(|marker| reflex_marker_matches_preference(marker, preference))
        .count() as i32
        * 5;

    match remote_component_reflex {
        Some("allow") => reward_bonus,
        Some("deny") => -immune_penalty,
        _ => 0,
    }
}

fn reflex_marker_matches_preference(
    marker: &serde_json::Value,
    preference: &RoutingPreferenceBinding,
) -> bool {
    let Some(obj) = marker.as_object() else {
        return false;
    };

    let preference_key = obj
        .get("preference_key")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(preference_key) = preference_key {
        return preference_key == preference.preference_key;
    }

    let provider_hint = obj
        .get("provider_hint")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(provider_hint) = provider_hint {
        if preference.provider_hint.as_deref() == Some(provider_hint) {
            return true;
        }
    }

    let model_ref = obj
        .get("model_ref")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(model_ref) = model_ref {
        if preference.model_ref.as_deref() == Some(model_ref) {
            return true;
        }
    }

    false
}

fn model_marker_matches_preference(
    marker: &serde_json::Value,
    preference: &RoutingPreferenceBinding,
) -> bool {
    let Some(obj) = marker.as_object() else {
        return false;
    };
    let marker_model_ref = obj
        .get("model_ref")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let (Some(marker_model_ref), Some(preference_model_ref)) =
        (marker_model_ref, preference.model_ref.as_deref())
    {
        return marker_model_ref == preference_model_ref;
    }

    let marker_provider_hint = obj
        .get("provider_hint")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let (Some(marker_provider_hint), Some(preference_provider_hint)) =
        (marker_provider_hint, preference.provider_hint.as_deref())
    {
        return marker_provider_hint == preference_provider_hint;
    }

    false
}

fn model_marker_stage_signal(marker: &serde_json::Value, stage: &TurnRoutingStagePlan) -> i32 {
    let Some(obj) = marker.as_object() else {
        return 0;
    };
    let capability_markers = obj
        .get("capability_markers")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    let stage_capability_match = capability_markers
        .iter()
        .filter_map(|value| value.as_str())
        .any(|capability| capability == stage.capability);
    if !stage_capability_match {
        return 0;
    }

    let speed_marker = obj
        .get("speed_marker")
        .and_then(|value| value.as_i64())
        .unwrap_or(0) as i32;
    let thinking_marker = obj
        .get("thinking_marker")
        .and_then(|value| value.as_i64())
        .unwrap_or(0) as i32;
    let tool_use_marker = obj
        .get("tool_use_marker")
        .and_then(|value| value.as_i64())
        .unwrap_or(0) as i32;
    let audio_native_marker = obj
        .get("audio_native_marker")
        .and_then(|value| value.as_i64())
        .unwrap_or(0) as i32;

    match stage.kind {
        TurnRoutingStageKind::Ingress => speed_marker + audio_native_marker,
        TurnRoutingStageKind::Cognition => {
            let native_live_bonus = match stage.capability.as_str() {
                "response.generate" => audio_native_marker,
                "voice.dialogue" => audio_native_marker * 2,
                _ => 0,
            };
            speed_marker / 3 + thinking_marker / 2 + tool_use_marker / 2 + native_live_bonus
        }
        TurnRoutingStageKind::Egress => speed_marker + audio_native_marker,
    }
}

fn apply_routing_preferences(
    plan: &mut TurnRoutingPlan,
    routing_preferences: &[RoutingPreferenceBinding],
    bindings: &SessionBindings,
) {
    for stage in &mut plan.stages {
        if let Some(preference) =
            select_stage_routing_preference(stage, routing_preferences, bindings)
        {
            if preference.provider_hint.is_some() {
                stage.provider_hint = preference.provider_hint.clone();
                stage.controller_role = implementation_to_model_role(
                    preference.provider_hint.as_deref().unwrap_or_default(),
                );
            }
            if preference.model_ref.is_some() {
                stage.model_ref = preference.model_ref.clone();
            }
        }
    }
}

fn stage_plan(
    turn_routing_plan: &TurnRoutingPlan,
    kind: TurnRoutingStageKind,
) -> Option<&TurnRoutingStagePlan> {
    turn_routing_plan
        .stages
        .iter()
        .find(|stage| stage.kind == kind)
}

fn stage_routing_hints(stage: &TurnRoutingStagePlan) -> serde_json::Value {
    serde_json::json!({
        "implementation": stage.provider_hint,
        "model_ref": stage.model_ref,
        "controller_role": stage.controller_role,
        "capability": stage.capability,
        "context_envelope": match stage.context_envelope {
            TurnContextEnvelopeKind::Ingress => "ingress",
            TurnContextEnvelopeKind::Cognitive => "cognitive",
            TurnContextEnvelopeKind::Egress => "egress",
        },
        "stage": match stage.kind {
            TurnRoutingStageKind::Ingress => "ingress",
            TurnRoutingStageKind::Cognition => "cognition",
            TurnRoutingStageKind::Egress => "egress",
        },
        "streaming": stage.streaming,
    })
}

fn active_turn_stage_routing_hints(
    state: Option<&SessionState>,
    kind: TurnRoutingStageKind,
) -> Option<serde_json::Value> {
    let turn_routing_plan = state?.active_turn_routing_plan()?;
    let stage = stage_plan(turn_routing_plan, kind)?;
    Some(stage_routing_hints(stage))
}

struct MediaRouting {
    action: String,
    capability: &'static str,
    attachments: Vec<TransportAttachment>,
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

const ROLE_HANDOFF_MAX_ATTEMPTS: usize = 8;
const DEFAULT_ROLE_HANDOFF_RETRY_MS: u64 = 250;

fn is_specific_same_self_role_governance(tool_call: &ToolCall) -> bool {
    matches!(
        tool_call.tool_name.as_str(),
        "role.configure" | "role.create_or_update"
    ) && !tool_call
        .arguments
        .get("is_admin")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        && tool_call
            .arguments
            .get("role_name")
            .and_then(|v| v.as_str())
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
        && tool_call
            .arguments
            .get("toolset_profile")
            .and_then(|v| v.as_str())
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
        && tool_call
            .arguments
            .get("reasoning")
            .and_then(|v| v.as_object())
            .is_some()
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

/// Locally cached role configuration, populated when role configuration succeeds via
/// the prompt-facing `role.create_or_update` workflow surface or the legacy
/// `role.configure` compatibility alias. Used to reconstruct `RoleActivation`
/// on inbound handoff without an IPC round-trip.
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
    guest_id: String,
    sessions: HashMap<String, SessionState>,
    /// MuninnDB config fetched from hotel at startup. None = NullMemoryEngine.
    muninn_config: Option<MuninnConfig>,
    /// Role configurations registered via role configuration workflow/tool execution,
    /// keyed by role_name.
    configured_roles: HashMap<String, CachedRoleConfig>,
    /// Agent profile (identity_text, soul_text, etc.) fetched from hotel at startup.
    /// Applied to every new session so the correct persona is used from the first turn.
    default_agent_profile: AgentProfile,
}

impl AgentRuntime {
    pub fn new(
        ipc_client: PhiloticClient,
        agent_id: impl Into<String>,
        guest_id: impl Into<String>,
    ) -> Self {
        Self {
            ipc_client,
            agent_id: agent_id.into(),
            guest_id: guest_id.into(),
            sessions: HashMap::new(),
            muninn_config: None,
            configured_roles: HashMap::new(),
            default_agent_profile: AgentProfile::default(),
        }
    }

    async fn request_role_handoff_with_backoff(
        &mut self,
        session_id: String,
        role_name: String,
        handoff_bundle: HandoffBundle,
    ) -> Result<IpcResponse> {
        for attempt in 0..ROLE_HANDOFF_MAX_ATTEMPTS {
            let response = self
                .ipc_client
                .send_request(IpcRequest::HandoffToRole {
                    session_id: session_id.clone(),
                    role_name: role_name.clone(),
                    handoff_bundle: handoff_bundle.clone(),
                })
                .await?;
            match response {
                IpcResponse::HandoffPending {
                    role_name,
                    readiness,
                    retry_after_ms,
                } if attempt + 1 < ROLE_HANDOFF_MAX_ATTEMPTS => {
                    let retry_after_ms = retry_after_ms
                        .unwrap_or(DEFAULT_ROLE_HANDOFF_RETRY_MS)
                        .max(25);
                    info!(
                        role_name = %role_name,
                        readiness = %readiness,
                        retry_after_ms,
                        attempt = attempt + 1,
                        "Role handoff still materializing; retrying"
                    );
                    tokio::time::sleep(Duration::from_millis(retry_after_ms)).await;
                }
                other => return Ok(other),
            }
        }

        Ok(IpcResponse::HandoffPending {
            role_name,
            readiness: "materializing".into(),
            retry_after_ms: Some(DEFAULT_ROLE_HANDOFF_RETRY_MS),
        })
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
                Ok(profile) => {
                    info!(agent_id = %self.agent_id, "Agent profile loaded from hotel.");
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
                            info!(
                                session_id = task.session_id.as_deref().unwrap_or(""),
                                turn_id = task.turn_id.as_deref().unwrap_or(""),
                                final_reply_guest_id =
                                    task.final_reply_guest_id.as_deref().unwrap_or(""),
                                "Agent [{}] picked up model_response envelope [{}]",
                                self.agent_id,
                                task_id
                            );
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
        self.refresh_bindings_from_snapshot(&session_id).await;

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

        {
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
                recalled_memories: Vec::new(),
                active_plan: None,
                consecutive_step_failures: 0,
                provider_repair_note: None,
                provider_repair_attempts: 0,
                pending_text_reply: None,
                had_voice_input,
                turn_routing_plan: None,
                awaiting_transcription_reentry: false,
                scripted_loop_context: None,
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

        self.maybe_auto_recall_turn_memory(&session_id).await?;

        let (checkpoint_memory_type, checkpoint_json, index_state) = {
            let state = self
                .sessions
                .get_mut(&session_id)
                .expect("session should exist after ensuring and binding transport target");
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
        let voice_policy = self
            .sessions
            .get(&session_id)
            .map(|s| s.agent_profile.voice_response_policy.clone())
            .unwrap_or_default();
        let routing_preferences = self
            .sessions
            .get(&session_id)
            .map(|s| s.bindings.routing_preferences.clone())
            .unwrap_or_default();
        let routing_bindings = self
            .sessions
            .get(&session_id)
            .map(|s| s.bindings.clone())
            .unwrap_or_default();
        let media_attachments = media_analysis_attachments(&task);
        let media_routing = resolve_media_routing(&media_policy, media_attachments);
        let turn_routing_plan = compile_turn_routing_plan(
            media_routing.as_ref(),
            Some(&voice_policy),
            had_voice_input,
            &routing_preferences,
            &routing_bindings,
        );
        let awaiting_transcription_reentry = media_routing
            .as_ref()
            .map(|routing| routing.action == "transcribe")
            .unwrap_or(false);

        let (checkpoint_memory_type, checkpoint_json, index_state) = {
            let state = self
                .sessions
                .get_mut(&session_id)
                .expect("active turn should still exist after context build");
            state.bump_active_turn_iteration();
            state.set_active_turn_phase(TurnPhase::WaitingModel);
            state.set_active_turn_routing_plan(turn_routing_plan.clone());
            state.set_active_turn_awaiting_transcription_reentry(awaiting_transcription_reentry);
            (
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
                    "session_id": index_state.session_id,
                    "turn_id": turn_id,
                    "chat_id": chat_id,
                    "content": content,
                    "turn_routing_plan": turn_routing_plan,
                }),
            })
            .await?;

        self.ipc_client
            .sync_apartment(&self.agent_id, &checkpoint_memory_type, checkpoint_json)
            .await?;
        self.sync_session_index(&index_state).await?;

        let (action, prompt, context, context_projection, attachments, tools_for_model, stage) =
            if let Some(routing) = media_routing {
                if let Some(stage) =
                    stage_plan(&turn_routing_plan, TurnRoutingStageKind::Ingress).cloned()
                {
                    let effective_tools = self
                        .sessions
                        .get(&session_id)
                        .expect("session should exist while preparing model request")
                        .project_tools_for_envelope(&content, stage.context_envelope);
                    let (_, context, context_projection) = self
                        .sessions
                        .get(&session_id)
                        .expect("session should exist while preparing model request")
                        .model_request_payloads_for_envelope(
                            &content,
                            &effective_tools,
                            stage.context_envelope,
                        );
                    let prompt = if routing.action == "transcribe" {
                        transcription_prompt(&content)
                    } else {
                        media_analysis_prompt(&content, &routing.attachments)
                    };
                    (
                        routing.action,
                        prompt,
                        context,
                        context_projection,
                        routing.attachments,
                        effective_tools,
                        stage,
                    )
                } else {
                    let stage = stage_plan(&turn_routing_plan, TurnRoutingStageKind::Cognition)
                        .expect("cognition stage should exist for native-live media turns")
                        .clone();
                    let effective_tools = self
                        .sessions
                        .get(&session_id)
                        .expect("session should exist while preparing model request")
                        .project_tools_for_envelope(&content, stage.context_envelope);
                    let (default_prompt, context, context_projection) = self
                        .sessions
                        .get(&session_id)
                        .expect("session should exist while preparing model request")
                        .model_request_payloads_for_envelope(
                            &content,
                            &effective_tools,
                            stage.context_envelope,
                        );
                    let prompt = if matches!(
                        stage.capability.as_str(),
                        "response.generate" | "voice.dialogue"
                    ) {
                        native_live_voice_prompt(&content, &stage.capability)
                    } else {
                        default_prompt
                    };
                    (
                        stage.capability.clone(),
                        prompt,
                        context,
                        context_projection,
                        routing.attachments,
                        effective_tools,
                        stage,
                    )
                }
            } else {
                let stage = stage_plan(&turn_routing_plan, TurnRoutingStageKind::Cognition)
                    .expect("cognition stage should exist for every turn")
                    .clone();
                let effective_tools = self
                    .sessions
                    .get(&session_id)
                    .expect("session should exist while preparing model request")
                    .project_tools_for_envelope(&content, stage.context_envelope);
                let (prompt, context, context_projection) = self
                    .sessions
                    .get(&session_id)
                    .expect("session should exist while preparing model request")
                    .model_request_payloads_for_envelope(
                        &content,
                        &effective_tools,
                        stage.context_envelope,
                    );
                (
                    stage.capability.clone(),
                    prompt,
                    context,
                    context_projection,
                    Vec::new(),
                    effective_tools,
                    stage,
                )
            };
        let (target_node, target_role, target_guest_id) =
            resolve_stage_execution_target(self.sessions.get(&session_id), &stage);

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
        let stage_summary = turn_routing_plan
            .stages
            .iter()
            .map(|stage| format!("{:?}:{}", stage.kind, stage.capability))
            .collect::<Vec<_>>()
            .join(" -> ");
        info!(
            "Session [{}] compiled turn routing plan trigger [{}]: {}",
            session_id, turn_routing_plan.trigger, stage_summary
        );

        let model_req = ModelRequestPayload {
            action,
            request_class: Some(stage.request_class.clone()),
            session_id: session_id.clone(),
            turn_id,
            prompt,
            user_content: content.clone(),
            context: Some(context),
            context_projection: Some(context_projection),
            attachments,
            tools_for_model,
            effective_rights: self
                .sessions
                .get(&session_id)
                .expect("session should exist while preparing model request")
                .bindings
                .effective_rights
                .clone(),
            response_contract: Some(if stage.kind == TurnRoutingStageKind::Cognition {
                self.sessions
                    .get(&session_id)
                    .expect("session should exist while preparing response contract")
                    .cognitive_response_contract(
                        &content,
                        stage_plan(&turn_routing_plan, TurnRoutingStageKind::Egress).is_some(),
                    )
            } else {
                serde_json::json!({})
            }),
            routing_hints: Some(stage_routing_hints(&stage)),
            provider_options: None,
            chat_id,
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
        };

        if debug_model_requests_enabled() && stage.kind == TurnRoutingStageKind::Cognition {
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
        let _ = self
            .emit_turn_event(
                &session_id,
                "model_response_received",
                Some(format!("Received model_response for turn {}", turn_id)),
            )
            .await;

        // If the turn is waiting for voice synthesis, this is the audio response — route it
        // directly to the voice handler regardless of the agent_action kind.
        let waiting_voice = self
            .sessions
            .get(&session_id)
            .and_then(|s| s.active_turn.as_ref())
            .map(|t| t.phase == TurnPhase::WaitingVoice)
            .unwrap_or(false);

        if waiting_voice {
            let _ = self
                .emit_turn_event(
                    &session_id,
                    "voice_response_received",
                    Some("Received voice synthesis response".into()),
                )
                .await;
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

        if let Some(error_payload) = extract_model_error_payload(&task) {
            if should_attempt_provider_repair(&error_payload, self.sessions.get(&session_id)) {
                warn!(
                    "Session [{}] retrying model turn after retryable provider failure: {}",
                    session_id,
                    error_payload.display_message()
                );
                return self
                    .retry_active_turn_after_provider_failure(
                        session_id,
                        turn_id,
                        provider_repair_note(&error_payload),
                    )
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
        let audio_artifact = extract_model_audio_artifact(model_result);

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
            let _ = self
                .emit_turn_event(
                    &session_id,
                    "transcription_reentry_received",
                    Some("Received transcription response for re-entry".into()),
                )
                .await;
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
                let _ = self
                    .emit_turn_event(
                        &session_id,
                        "model_response_classified_respond",
                        Some(format!(
                            "Model response classified as respond ({} chars)",
                            content.chars().count()
                        )),
                    )
                    .await;
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
                let _ = self
                    .emit_turn_event(
                        &session_id,
                        "model_response_classified_tool_call",
                        Some(format!("Model requested tool {}", tool_call.tool_name)),
                    )
                    .await;
                if let Some(function_call_id) = extract_native_live_pending_function_call_id(&task)
                {
                    if let Some(state) = self.sessions.get_mut(&session_id) {
                        state.set_pending_native_live_function_call_id(function_call_id);
                    }
                }
                self.handle_tool_call(session_id, turn_id, tool_call).await
            }
            AgentAction::RequestApproval(approval) => {
                let _ = self
                    .emit_turn_event(
                        &session_id,
                        "model_response_classified_approval",
                        Some(format!("Model requested approval: {}", approval.reason)),
                    )
                    .await;
                self.handle_approval_request(session_id, turn_id, approval, false)
                    .await
            }
            AgentAction::Fail { message } => {
                let _ = self
                    .emit_turn_event(
                        &session_id,
                        "model_response_classified_fail",
                        Some(message.clone()),
                    )
                    .await;
                self.fail_active_turn(session_id, turn_id, message).await
            }
        }
    }

    async fn handle_approval_request(
        &mut self,
        session_id: String,
        turn_id: String,
        approval: ApprovalRequest,
        always_require_human: bool,
    ) -> Result<()> {
        let approval = Self::normalize_approval_request(approval);
        let disposition = self
            .sessions
            .get(&session_id)
            .map(|state| state.approval_interrupt_disposition(&approval, always_require_human))
            .unwrap_or(ApprovalInterruptDisposition::Allow);

        match disposition {
            ApprovalInterruptDisposition::Allow => {}
            ApprovalInterruptDisposition::RedirectToDirectResponse { note } => {
                let chat_id = self
                    .sessions
                    .get(&session_id)
                    .and_then(|state| state.active_turn.as_ref())
                    .map(|turn| turn.chat_id.clone())
                    .unwrap_or_default();
                let task_id = self
                    .sessions
                    .get(&session_id)
                    .and_then(|state| state.active_turn.as_ref())
                    .map(|turn| turn.task_id);
                if let Some(task_id) = task_id {
                    let _ = self
                        .ipc_client
                        .send_request(IpcRequest::UpdateTask {
                            task_id,
                            state: "approval_redirected".into(),
                            payload: serde_json::json!({
                                "session_id": session_id,
                                "turn_id": turn_id,
                                "chat_id": chat_id,
                                "approval_request": {
                                    "approval_id": approval.approval_id,
                                    "reason": approval.reason,
                                },
                                "policy_action": "redirect_to_direct_response"
                            }),
                        })
                        .await;
                }
                return self
                    .resume_turn_with_steering(
                        session_id,
                        turn_id,
                        chat_id,
                        note,
                        "approval_redirected",
                        "[Turn policy correction]",
                    )
                    .await;
            }
            ApprovalInterruptDisposition::RejectAsInvalidStage { reason } => {
                return self.fail_active_turn(session_id, turn_id, reason).await;
            }
        }

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
            turn_routing_plan,
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
                state.active_turn_routing_plan().cloned(),
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
                        "turn_routing_plan": turn_routing_plan,
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
                    "turn_routing_plan": turn_routing_plan,
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
            let is_specific_same_self_role_governance =
                !bypass_approval && is_specific_same_self_role_governance(&tool_call);

            let force_approval = if bypass_approval || is_specific_same_self_role_governance {
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
                && matches!(
                    tool_call.tool_name.as_str(),
                    "role.configure" | "role.create_or_update"
                )
                && tool_call
                    .arguments
                    .get("is_admin")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

            // Durable self-governance proposals always require live operator approval — cannot be
            // preapproved or bypassed. This currently covers both general rules and routing-policy
            // refinements, since either one changes future agent behavior.
            let is_durable_governance_proposal = !bypass_approval
                && matches!(
                    tool_call.tool_name.as_str(),
                    "rule.propose" | "routing.policy.propose"
                );

            if is_admin_role_creation || is_durable_governance_proposal || force_approval {
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
                } else if is_durable_governance_proposal {
                    let (reason, approved_response) =
                        if tool_call.tool_name == "routing.policy.propose" {
                            (
                                "Routing policy proposal requires your explicit live approval."
                                    .to_string(),
                                "Routing policy proposal approved.".to_string(),
                            )
                        } else {
                            (
                                "Rule proposal requires your explicit live approval.".to_string(),
                                "Rule proposal approved.".to_string(),
                            )
                        };
                    (reason, approved_response)
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
                        is_admin_role_creation || is_durable_governance_proposal,
                    )
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
            let pending_native_live_function_call_id =
                state.take_pending_native_live_function_call_id();

            state.push_tool_history(tool_call, tool_result.clone());
            state.clear_pending_tool_call();

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
            } else if iteration > iteration_cap {
                Err(format!(
                    "Turn exceeded maximum tool iterations ({iteration_cap}). Aborting."
                ))
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
                            pending_native_live_function_call_id.map(|function_call_id| {
                                serde_json::json!({
                                    "live_tool_response": {
                                        "function_call_id": function_call_id,
                                        "tool_name": tool_result.tool_name,
                                        "tool_response": parse_native_live_tool_response_content(
                                            &tool_result.content,
                                        ),
                                    }
                                })
                            }),
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

        match loop_outcome {
            Err(msg) => {
                if stream_events && !step_failed {
                    // stall/cap hit — emit loop_recovering so observers know we stopped
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
                provider_options,
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

                let cognitive_stage =
                    active_or_default_cognitive_stage(self.sessions.get(&session_id));
                let model_req = ModelRequestPayload {
                    action: cognitive_stage.capability.clone(),
                    request_class: Some(cognitive_stage.request_class.clone()),
                    session_id: session_id.clone(),
                    turn_id,
                    prompt,
                    user_content: user_content.clone(),
                    context: Some(context),
                    context_projection: Some(context_projection),
                    attachments: Vec::new(),
                    tools_for_model,
                    effective_rights: self
                        .sessions
                        .get(&session_id)
                        .expect("session should exist while preparing model request")
                        .bindings
                        .effective_rights
                        .clone(),
                    response_contract: Some(
                        self.sessions
                            .get(&session_id)
                            .expect("session should exist while preparing response contract")
                            .cognitive_response_contract(
                                &user_content,
                                active_turn_stage_routing_hints(
                                    self.sessions.get(&session_id),
                                    TurnRoutingStageKind::Egress,
                                )
                                .is_some(),
                            ),
                    ),
                    routing_hints: active_turn_stage_routing_hints(
                        self.sessions.get(&session_id),
                        TurnRoutingStageKind::Cognition,
                    ),
                    provider_options,
                    chat_id,
                    reply_to: local_node_id(),
                    reply_role: "agent".into(),
                    final_reply_to,
                    final_reply_role,
                    final_reply_guest_id,
                };

                let (target_node, target_role, target_guest_id) = resolve_stage_execution_target(
                    self.sessions.get(&session_id),
                    &cognitive_stage,
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
        note: String,
    ) -> Result<()> {
        let retry_plan = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                return Ok(());
            };
            state.set_provider_repair_note(note);
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

        let cognitive_stage = active_or_default_cognitive_stage(self.sessions.get(&session_id));
        let model_req = ModelRequestPayload {
            action: cognitive_stage.capability.clone(),
            request_class: Some(cognitive_stage.request_class.clone()),
            session_id: session_id.clone(),
            turn_id,
            prompt,
            user_content: user_content.clone(),
            context: Some(context),
            context_projection: Some(context_projection),
            attachments: Vec::new(),
            tools_for_model,
            effective_rights: self
                .sessions
                .get(&session_id)
                .expect("session should exist while preparing model request")
                .bindings
                .effective_rights
                .clone(),
            response_contract: Some(
                self.sessions
                    .get(&session_id)
                    .expect("session should exist while preparing response contract")
                    .cognitive_response_contract(
                        &user_content,
                        active_turn_stage_routing_hints(
                            self.sessions.get(&session_id),
                            TurnRoutingStageKind::Egress,
                        )
                        .is_some(),
                    ),
            ),
            routing_hints: active_turn_stage_routing_hints(
                self.sessions.get(&session_id),
                TurnRoutingStageKind::Cognition,
            ),
            provider_options: None,
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

        let (target_node, target_role, target_guest_id) =
            resolve_stage_execution_target(self.sessions.get(&session_id), &cognitive_stage);

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

        let turn_routing_plan = self
            .sessions
            .get(&session_id)
            .and_then(|state| state.active_turn_routing_plan().cloned());

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
                    "turn_routing_plan": turn_routing_plan,
                }),
            })
            .await?;

        let (context, context_projection) = self
            .sessions
            .get(&session_id)
            .map(|state| {
                let (_, context, context_projection) = state.model_request_payloads_for_envelope(
                    &reentry.user_content,
                    &reentry.tools_for_model,
                    TurnContextEnvelopeKind::Cognitive,
                );
                (Some(context), Some(context_projection))
            })
            .unwrap_or((None, None));
        if let Some(state) = self.sessions.get_mut(&session_id) {
            state.clear_handoff_summary();
        }

        let cognitive_stage = active_or_default_cognitive_stage(self.sessions.get(&session_id));
        let model_req = ModelRequestPayload {
            action: cognitive_stage.capability.clone(),
            request_class: Some(cognitive_stage.request_class.clone()),
            session_id: session_id.clone(),
            turn_id,
            prompt: reentry.prompt,
            user_content: reentry.user_content.clone(),
            context,
            context_projection,
            attachments: Vec::new(),
            tools_for_model: reentry.tools_for_model,
            effective_rights: self
                .sessions
                .get(&session_id)
                .expect("session should exist while preparing model request")
                .bindings
                .effective_rights
                .clone(),
            response_contract: Some(
                self.sessions
                    .get(&session_id)
                    .expect("session should exist while preparing response contract")
                    .cognitive_response_contract(
                        &reentry.user_content,
                        active_turn_stage_routing_hints(
                            self.sessions.get(&session_id),
                            TurnRoutingStageKind::Egress,
                        )
                        .is_some(),
                    ),
            ),
            routing_hints: active_turn_stage_routing_hints(
                self.sessions.get(&session_id),
                TurnRoutingStageKind::Cognition,
            ),
            provider_options: None,
            chat_id: reentry.chat_id,
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            final_reply_to: reentry.final_reply_to,
            final_reply_role: reentry.final_reply_role,
            final_reply_guest_id: reentry.final_reply_guest_id,
        };

        let (target_node, target_role, target_guest_id) = resolve_model_execution_target(
            self.sessions.get(&session_id),
            cognitive_stage.capability.as_str(),
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

    async fn emit_turn_event_to(
        &mut self,
        session_id: String,
        turn_id: String,
        chat_id: String,
        target_node: String,
        target_role: String,
        target_guest_id: Option<String>,
        event: &str,
        partial_content: Option<String>,
    ) -> Result<()> {
        let event_payload = TurnEventPayload {
            action: "turn_event",
            event: event.to_string(),
            session_id,
            turn_id,
            chat_id,
            partial_content,
        };

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node,
                target_role,
                target_guest_id,
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

        let _ = self
            .emit_turn_event(
                &session_id,
                "agent_response_finalizing",
                Some(format!(
                    "Finalizing response voice_input={} audio_artifact={} spoken_text={}",
                    had_voice_input,
                    audio_artifact.is_some(),
                    spoken_text.is_some()
                )),
            )
            .await;

        if let Some(audio_artifact) = audio_artifact {
            if voice_policy.is_active(had_voice_input) || had_voice_input {
                let _ = self
                    .emit_turn_event(
                        &session_id,
                        "reply_delivery_started",
                        Some("Delivering cognitive audio artifact directly".into()),
                    )
                    .await;
                return self
                    .deliver_text_reply(
                        session_id,
                        turn_id,
                        content,
                        Some(audio_artifact),
                        voice_policy.caption_enabled(),
                        memory_concept,
                        memory_candidate,
                    )
                    .await;
            }

            warn!(
                "Session [{}] model returned an audio artifact on a non-voice turn; delivering text only and ignoring the unexpected artifact",
                session_id
            );
        }

        if voice_policy.is_active(had_voice_input) {
            let _ = self
                .emit_turn_event(
                    &session_id,
                    "voice_synthesis_started",
                    Some("Starting voice synthesis for final reply".into()),
                )
                .await;
            return self
                .start_voice_synthesis(session_id, turn_id, content, spoken_text, voice_policy)
                .await;
        }

        let _ = self
            .emit_turn_event(
                &session_id,
                "reply_delivery_started",
                Some("Delivering final text reply".into()),
            )
            .await;
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
            turn_routing_plan,
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
                state.active_turn_routing_plan().cloned(),
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
                    "turn_routing_plan": turn_routing_plan,
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
            "routing_hints": turn_routing_plan
                .as_ref()
                .and_then(|plan| stage_plan(plan, TurnRoutingStageKind::Egress))
                .map(stage_routing_hints),
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
        let audio_artifact = if parse_audio_artifact_json(&raw_audio_content).is_ok() {
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
                warn!(
                    "deliver_text_reply: unknown session {} while delivering turn {}",
                    session_id, turn_id
                );
                return Ok(());
            };

            let Some(completed_turn) = state.complete_active_turn(content.clone()) else {
                warn!(
                    "deliver_text_reply: no active turn for session {} while delivering response turn {}",
                    session_id, turn_id
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
        let final_reply_to = completed_turn.final_reply_to.clone();
        let final_reply_role = completed_turn.final_reply_role.clone();
        let final_reply_guest_id = completed_turn.final_reply_guest_id.clone();

        self.ipc_client
            .send_request(IpcRequest::EmitTask {
                target_node: final_reply_to.clone(),
                target_role: final_reply_role.clone(),
                target_guest_id: final_reply_guest_id.clone(),
                task_json: serde_json::to_string(&reply_payload)?,
            })
            .await?;

        let _ = self
            .emit_turn_event_to(
                reply_payload.session_id.clone(),
                reply_payload.turn_id.clone(),
                reply_payload.chat_id.clone(),
                final_reply_to,
                final_reply_role,
                final_reply_guest_id,
                "reply_delivery_emitted",
                Some(format!(
                    "Reply emitted to membrane audio_artifact={} text_caption={}",
                    reply_payload.audio_artifact.is_some(),
                    reply_payload.send_text_caption
                )),
            )
            .await;

        // Attend hook (Slice E): fire-and-forget autobiographical memory write.
        if let Some(engine) = self.memory_engine_for(&self.agent_id, &self.agent_id) {
            let agent_id = self.agent_id.clone();
            let default_concept =
                memory_concept.unwrap_or_else(|| format!("turn:{}", attend_turn_id));
            let memory_candidate = memory_candidate.unwrap_or_else(|| MemoryCandidate {
                concept: default_concept,
                content: attend_content.clone(),
                tags: Vec::new(),
            });
            let mut tags = vec![
                format!("agent:{}", agent_id),
                format!("session:{}", attend_session_id),
            ];
            tags.extend(memory_candidate.tags);
            let concept = memory_candidate.concept;
            let content_snapshot = memory_candidate.content;
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

        let cognitive_stage = active_or_default_cognitive_stage(self.sessions.get(&session_id));
        let model_req = ModelRequestPayload {
            action: cognitive_stage.capability.clone(),
            request_class: Some(cognitive_stage.request_class.clone()),
            session_id: session_id.clone(),
            turn_id,
            prompt,
            user_content: user_content.clone(),
            context: Some(context),
            context_projection: Some(context_projection),
            attachments: Vec::new(),
            tools_for_model: tools,
            effective_rights: self
                .sessions
                .get(&session_id)
                .expect("session should exist while preparing model request")
                .bindings
                .effective_rights
                .clone(),
            response_contract: Some(
                self.sessions
                    .get(&session_id)
                    .expect("session should exist while preparing response contract")
                    .cognitive_response_contract(
                        &user_content,
                        active_turn_stage_routing_hints(
                            self.sessions.get(&session_id),
                            TurnRoutingStageKind::Egress,
                        )
                        .is_some(),
                    ),
            ),
            routing_hints: active_turn_stage_routing_hints(
                self.sessions.get(&session_id),
                TurnRoutingStageKind::Cognition,
            ),
            provider_options: None,
            chat_id,
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
        };

        let (target_node, target_role, target_guest_id) = resolve_model_execution_target(
            self.sessions.get(&session_id),
            cognitive_stage.capability.as_str(),
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
                    session_id: session_id.clone(),
                    turn_id: original_turn_id.clone(),
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
            | SlashCommand::Abandon { .. }
            | SlashCommand::Tts { .. } => {}
        }

        Ok(())
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
            state.active_incarnation_id = Some(self.guest_id.clone());

            let activation = crate::session::RoleActivation {
                role_name: to_role.clone(),
                active_incarnation_id: Some(self.guest_id.clone()),
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
        let response = match &command {
            SlashCommand::Role { role_name } => {
                let target_role_lens =
                    resolve_target_role_lens(&mut self.ipc_client, &self.agent_id, role_name).await;
                let handoff_bundle = self
                    .sessions
                    .get(&session_id)
                    .map(|state| {
                        state.build_same_identity_handoff_bundle(
                            role_name,
                            &command_turn_id,
                            "manual_role_switch",
                            Some("orchestrator".into()),
                            target_role_lens.as_ref(),
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
                self.request_role_handoff_with_backoff(
                    session_id.clone(),
                    role_name.clone(),
                    handoff_bundle,
                )
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
            IpcResponse::HandoffPending { .. } => (
                format_role_command_reply(&command, false),
                "role_handoff_materializing",
                serde_json::json!({
                    "session_id": session_id,
                    "turn_id": command_turn_id,
                    "chat_id": command_chat_id,
                    "role_command": "handoff_to_role",
                    "became_active": false,
                }),
                None,
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
            .map(|state| {
                state.project_tools_for_envelope(&user_content, TurnContextEnvelopeKind::Cognitive)
            })
            .unwrap_or_default();

        let (context, context_projection) = self
            .sessions
            .get(&session_id)
            .map(|state| {
                let (_, context, context_projection) = state.model_request_payloads_for_envelope(
                    &user_content,
                    &tools_for_model,
                    TurnContextEnvelopeKind::Cognitive,
                );
                (Some(context), Some(context_projection))
            })
            .unwrap_or((None, None));
        if let Some(state) = self.sessions.get_mut(&session_id) {
            state.clear_handoff_summary();
        }

        let cognitive_stage = active_or_default_cognitive_stage(self.sessions.get(&session_id));
        let model_req = ModelRequestPayload {
            action: cognitive_stage.capability.clone(),
            request_class: Some(cognitive_stage.request_class.clone()),
            session_id: session_id.clone(),
            turn_id,
            prompt,
            user_content: user_content.clone(),
            context,
            context_projection,
            attachments: Vec::new(),
            tools_for_model,
            effective_rights: self
                .sessions
                .get(&session_id)
                .expect("session should exist while preparing model request")
                .bindings
                .effective_rights
                .clone(),
            response_contract: Some(
                self.sessions
                    .get(&session_id)
                    .expect("session should exist while preparing response contract")
                    .cognitive_response_contract(
                        &user_content,
                        active_turn_stage_routing_hints(
                            self.sessions.get(&session_id),
                            TurnRoutingStageKind::Egress,
                        )
                        .is_some(),
                    ),
            ),
            routing_hints: active_turn_stage_routing_hints(
                self.sessions.get(&session_id),
                TurnRoutingStageKind::Cognition,
            ),
            provider_options: None,
            chat_id,
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
        };

        let (target_node, target_role, target_guest_id) = resolve_model_execution_target(
            self.sessions.get(&session_id),
            cognitive_stage.capability.as_str(),
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
                })
                .await
            }
            "role.configure" | "role.create_or_update" => {
                let args = &payload.arguments;
                let tool_surface = payload.tool_name.as_str();

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
                                            "{}: missing required argument '{}'",
                                            tool_surface, $key
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
                            format!(
                                "{}: missing required object argument 'reasoning'",
                                tool_surface
                            ),
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

                let req = if tool_surface == "role.create_or_update" {
                    IpcRequest::ExecuteWorkflow {
                        workflow_name: "role.create_or_update".into(),
                        agent_id: self.agent_id.clone(),
                        calling_role,
                        arguments: serde_json::json!({
                            "role_name": role_name.clone(),
                            "guest_id": format!("{}:{}", self.agent_id, role_name),
                            "toolset_profile": toolset_profile,
                            "role_identity_addendum": role_identity_addendum,
                            "role_manifest": role_manifest,
                            "is_admin": is_admin,
                            "inactive_ttl_seconds": inactive_ttl_seconds,
                            "iteration_cap": iteration_cap,
                            "approval_policy": approval_policy,
                            "model_profile": model_profile,
                            "context_window_policy": context_window_policy,
                            "reasoning": args.get("reasoning").cloned().unwrap_or(serde_json::json!({}))
                        }),
                    }
                } else {
                    IpcRequest::ConfigureRole {
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
                    }
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
                        (
                            format!("Successfully configured role incarnation for '{}'.", name),
                            None,
                        )
                    }
                    Ok(IpcResponse::WorkflowExecutionOk {
                        workflow_name: _,
                        result,
                    }) => {
                        let name = result
                            .get("role_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&role_name)
                            .to_string();
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
                            format!("{tool_surface}: unexpected hotel response"),
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("{tool_surface}: IPC transport error — {e}"),
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

                let target_role_lens =
                    resolve_target_role_lens(&mut self.ipc_client, &self.agent_id, &role_name)
                        .await;

                let handoff_bundle = HandoffBundle {
                    goal: active_goal.clone().unwrap_or_else(|| reason.clone()),
                    context_excerpt: if let Some(lens) = target_role_lens.as_ref() {
                        let mut context = context_summary.clone();
                        if let Some(toolset_profile) = lens.toolset_profile.as_deref() {
                            context.push_str(&format!(
                                "\nTarget role toolset profile: {toolset_profile}."
                            ));
                        }
                        if let Some(addendum) = lens.role_identity_addendum.as_deref() {
                            context
                                .push_str(&format!("\nTarget role identity addendum: {addendum}"));
                        }
                        if let Some(manifest) = lens
                            .role_manifest
                            .as_deref()
                            .map(str::trim)
                            .filter(|text| !text.is_empty())
                        {
                            let excerpt: String = manifest.chars().take(240).collect();
                            context.push_str(&format!("\nTarget role manifest excerpt: {excerpt}"));
                        }
                        if !lens.allowed_skills.is_empty() {
                            context.push_str(&format!(
                                "\nTarget role allowed skills: {}",
                                lens.allowed_skills.join(", ")
                            ));
                        }
                        context
                    } else {
                        context_summary
                    },
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
                    .request_role_handoff_with_backoff(
                        payload.session_id.clone(),
                        role_name.clone(),
                        handoff_bundle,
                    )
                    .await
                {
                    Ok(IpcResponse::HandoffAck {
                        handoff_guest_id, ..
                    }) => {
                        if let Err(err) = self
                            .ipc_client
                            .send_request(IpcRequest::RecordRoleHandoffReflexEvidence {
                                agent_id: self.agent_id.clone(),
                                role_name: role_name.clone(),
                                legacy_trigger_class: None,
                                source_turn: Some(payload.turn_id.clone()),
                            })
                            .await
                        {
                            warn!(
                                role_name = %role_name,
                                error = %err,
                                "Failed to record successful same-self role handoff evidence"
                            );
                        }
                        (
                            format!("Handed off to role '{role_name}' (guest {handoff_guest_id})."),
                            None,
                        )
                    }
                    Ok(IpcResponse::HandoffPending { .. }) => (
                        format!("Switching to role '{role_name}' once it finishes materializing."),
                        None,
                    ),
                    Ok(IpcResponse::Error(msg)) => {
                        let e = TaskErrorPayload::tool_execution(
                            "handoff.to_role",
                            msg,
                            Some("HANDOFF_REJECTED"),
                        );
                        (e.display_message(), Some(e))
                    }
                    Ok(_) => {
                        let e = TaskErrorPayload::ipc_failure(
                            "aiua",
                            "UNEXPECTED_RESPONSE",
                            "handoff.to_role: unexpected hotel response",
                        );
                        (e.display_message(), Some(e))
                    }
                    Err(e) => {
                        let err = TaskErrorPayload::transport_error(
                            "philote",
                            format!("handoff.to_role: IPC transport error — {e}"),
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
                })
                .await
            }

            "routing.policy.propose" => {
                let problem = payload
                    .arguments
                    .get("problem")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let proposed_change = payload
                    .arguments
                    .get("proposed_change")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let evidence = payload
                    .arguments
                    .get("evidence")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let affected_stage = payload
                    .arguments
                    .get("affected_stage")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let affected_capability = payload
                    .arguments
                    .get("affected_capability")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let learned_reflex = match parse_learned_reflex_writeback(&payload.arguments) {
                    Ok(value) => value,
                    Err(message) => {
                        return self
                            .fail_active_turn(payload.session_id, payload.turn_id, message)
                            .await;
                    }
                };

                if problem.is_empty() || proposed_change.is_empty() || evidence.is_empty() {
                    return self
                        .fail_active_turn(
                            payload.session_id,
                            payload.turn_id,
                            "routing.policy.propose: 'problem', 'proposed_change', and 'evidence' are required.".into(),
                        )
                        .await;
                }

                let agent_id = self.agent_id.clone();
                let result_text = match self
                    .ipc_client
                    .send_request(IpcRequest::RecordRoutingPolicyProposal {
                        agent_id: agent_id.clone(),
                        problem: problem.clone(),
                        proposed_change: proposed_change.clone(),
                        evidence: evidence.clone(),
                        affected_stage: if affected_stage.is_empty() {
                            None
                        } else {
                            Some(affected_stage.clone())
                        },
                        affected_capability: if affected_capability.is_empty() {
                            None
                        } else {
                            Some(affected_capability.clone())
                        },
                        learned_reflex_preference_key: learned_reflex
                            .as_ref()
                            .map(|reflex| reflex.preference_key.clone()),
                    })
                    .await
                {
                    Ok(IpcResponse::RoutingPolicyRecorded { proposal_id }) => {
                        let mut writeback_note = None;
                        if let Some(reflex) = learned_reflex.as_ref() {
                            let config_json = serde_json::json!({
                                "reason": format!("approved write-back from routing policy proposal {}", proposal_id),
                                "problem": problem.clone(),
                                "proposed_change": proposed_change.clone(),
                                "evidence": evidence.clone(),
                                "affected_stage": affected_stage.clone(),
                                "affected_capability": affected_capability.clone(),
                                "proposal_id": proposal_id.clone(),
                                "source_tool": "routing.policy.propose",
                            });
                            match self
                                .ipc_client
                                .send_request(IpcRequest::UpsertAgentReflexPreference {
                                    agent_id: agent_id.clone(),
                                    preference_key: reflex.preference_key.clone(),
                                    precedence: reflex.precedence,
                                    reflexes_json: reflex.reflexes_json.clone(),
                                    config_json,
                                })
                                .await
                            {
                                Ok(IpcResponse::Standard { ok: true, .. }) => {
                                    let _ = self
                                        .ipc_client
                                        .send_request(IpcRequest::AppendRoutingPolicyEvaluation {
                                            proposal_id: proposal_id.clone(),
                                            evaluation_kind: "learned_reflex_writeback".into(),
                                            decision: "approved_writeback".into(),
                                            reason: format!(
                                                "Learned reflex '{}' was written into the agent graph.",
                                                reflex.preference_key
                                            ),
                                            source_tool: Some("routing.policy.propose".into()),
                                        })
                                        .await;
                                    let _ = self
                                        .ipc_client
                                        .send_request(IpcRequest::UpdateTask {
                                            task_id: Uuid::new_v4(),
                                            state: "routing_reflex_writeback".into(),
                                            payload: serde_json::json!({
                                                "session_id": payload.session_id,
                                                "turn_id": payload.turn_id,
                                                "chat_id": payload.chat_id,
                                                "reflex_evaluations": [{
                                                    "reflex_name": reflex.preference_key,
                                                    "decision": "approved_writeback",
                                                    "reason": format!("approved learned reflex write-back from routing policy proposal {}", proposal_id),
                                                    "source_tool": "routing.policy.propose"
                                                }]
                                            }),
                                        })
                                        .await;
                                    writeback_note = Some(format!(
                                        " Learned reflex '{}' was written into the agent graph.",
                                        reflex.preference_key
                                    ));
                                }
                                Ok(IpcResponse::Standard {
                                    ok: false, message, ..
                                }) => {
                                    let _ = self
                                        .ipc_client
                                        .send_request(IpcRequest::AppendRoutingPolicyEvaluation {
                                            proposal_id: proposal_id.clone(),
                                            evaluation_kind: "learned_reflex_writeback".into(),
                                            decision: "rejected".into(),
                                            reason: format!(
                                                "Hotel rejected learned reflex write-back: {}",
                                                message
                                            ),
                                            source_tool: Some("routing.policy.propose".into()),
                                        })
                                        .await;
                                    writeback_note = Some(format!(
                                        " Routing policy recorded, but learned reflex write-back was rejected — {}.",
                                        message
                                    ));
                                }
                                Ok(other) => {
                                    let _ = self
                                        .ipc_client
                                        .send_request(IpcRequest::AppendRoutingPolicyEvaluation {
                                            proposal_id: proposal_id.clone(),
                                            evaluation_kind: "learned_reflex_writeback".into(),
                                            decision: "unexpected_response".into(),
                                            reason: format!(
                                                "Unexpected response while writing learned reflex: {:?}",
                                                other
                                            ),
                                            source_tool: Some("routing.policy.propose".into()),
                                        })
                                        .await;
                                    writeback_note = Some(format!(
                                        " Routing policy recorded, but learned reflex write-back returned an unexpected response: {:?}.",
                                        other
                                    ));
                                }
                                Err(err) => {
                                    let _ = self
                                        .ipc_client
                                        .send_request(IpcRequest::AppendRoutingPolicyEvaluation {
                                            proposal_id: proposal_id.clone(),
                                            evaluation_kind: "learned_reflex_writeback".into(),
                                            decision: "ipc_error".into(),
                                            reason: format!(
                                                "IPC error while writing learned reflex: {}",
                                                err
                                            ),
                                            source_tool: Some("routing.policy.propose".into()),
                                        })
                                        .await;
                                    writeback_note = Some(format!(
                                        " Routing policy recorded, but learned reflex write-back failed — {}.",
                                        err
                                    ));
                                }
                            }
                        }
                        let mut message = format!(
                            "Routing policy proposal recorded as a first-class routing policy artifact (id: {proposal_id}). Operator disposition is stored as approved, and future reflex outcomes will append to its evaluation history."
                        );
                        if let Some(note) = writeback_note {
                            message.push_str(&note);
                        }
                        message
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
                    tool_name: Some(payload.tool_name),
                    arguments: None,
                    final_reply_to: Some(payload.final_reply_to),
                    final_reply_role: Some(payload.final_reply_role),
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
        let new_rights: Option<Vec<String>> = bindings
            .get("effective_rights")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let new_skillset: Option<Vec<String>> = bindings
            .get("effective_skillset")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let new_routing_preferences = bindings
            .get("routing_preferences")
            .and_then(|v| serde_json::from_value::<Vec<RoutingPreferenceBinding>>(v.clone()).ok());
        let new_effective_reflexes = bindings.get("effective_reflexes").cloned();
        let new_reflex_policy_agent_layers = bindings
            .get("reflex_policy_agent_layers")
            .and_then(|v| serde_json::from_value::<Vec<serde_json::Value>>(v.clone()).ok());
        let new_reflex_policy_agent_rewards = bindings
            .get("reflex_policy_agent_rewards")
            .and_then(|v| serde_json::from_value::<Vec<serde_json::Value>>(v.clone()).ok());
        let new_reflex_policy_agent_suppressions = bindings
            .get("reflex_policy_agent_suppressions")
            .and_then(|v| serde_json::from_value::<Vec<serde_json::Value>>(v.clone()).ok());
        let new_shared_model_markers = bindings
            .get("shared_model_markers")
            .and_then(|v| serde_json::from_value::<Vec<serde_json::Value>>(v.clone()).ok());
        let new_active_incarnation_id = snapshot
            .get("active_incarnation_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let new_role_activation = snapshot
            .get("role_activation")
            .and_then(|v| serde_json::from_value::<RoleActivation>(v.clone()).ok());
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
        if let Some(rights) = new_rights {
            if rights != state.bindings.effective_rights {
                state.bindings.effective_rights = rights;
                changed = true;
            }
        }
        if let Some(skillset) = new_skillset {
            if skillset != state.bindings.effective_skillset {
                state.bindings.effective_skillset = skillset;
                changed = true;
            }
        }
        if let Some(routing_preferences) = new_routing_preferences {
            if routing_preferences != state.bindings.routing_preferences {
                state.bindings.routing_preferences = routing_preferences;
                changed = true;
            }
        }
        if let Some(effective_reflexes) = new_effective_reflexes {
            if effective_reflexes != state.bindings.effective_reflexes {
                state.bindings.effective_reflexes = effective_reflexes;
                changed = true;
            }
        }
        if let Some(layers) = new_reflex_policy_agent_layers {
            if layers != state.bindings.reflex_policy_agent_layers {
                state.bindings.reflex_policy_agent_layers = layers;
                changed = true;
            }
        }
        if let Some(rewards) = new_reflex_policy_agent_rewards {
            if rewards != state.bindings.reflex_policy_agent_rewards {
                state.bindings.reflex_policy_agent_rewards = rewards;
                changed = true;
            }
        }
        if let Some(suppressions) = new_reflex_policy_agent_suppressions {
            if suppressions != state.bindings.reflex_policy_agent_suppressions {
                state.bindings.reflex_policy_agent_suppressions = suppressions;
                changed = true;
            }
        }
        if let Some(shared_model_markers) = new_shared_model_markers {
            if shared_model_markers != state.bindings.shared_model_markers {
                state.bindings.shared_model_markers = shared_model_markers;
                changed = true;
            }
        }
        if new_active_incarnation_id != state.active_incarnation_id {
            state.active_incarnation_id = new_active_incarnation_id;
            changed = true;
        }
        if new_role_activation != state.role_activation {
            state.role_activation = new_role_activation;
            changed = true;
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
                    if let Some(mut state) = SessionState::from_checkpoint(&checkpoint) {
                        Self::fetch_and_inject_rules(
                            &mut self.ipc_client,
                            &self.agent_id,
                            &mut state,
                        )
                        .await;
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
        Self::fetch_and_inject_rules(&mut self.ipc_client, &self.agent_id, &mut state).await;
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

async fn resolve_target_role_lens(
    ipc_client: &mut PhiloticClient,
    agent_id: &str,
    role_name: &str,
) -> Option<TargetRoleLens> {
    let roles = match ipc_client
        .send_request(IpcRequest::ListRoleIncarnations {
            agent_id: agent_id.to_string(),
        })
        .await
    {
        Ok(IpcResponse::Standard {
            ok: true,
            data: Some(data),
            ..
        }) => data
            .get("roles")
            .cloned()
            .and_then(|value| {
                serde_json::from_value::<Vec<ansible_mesh_core::graph::RoleIncarnationRecord>>(
                    value,
                )
                .ok()
            })
            .unwrap_or_default(),
        _ => return None,
    };

    let role = roles
        .into_iter()
        .find(|role| role.role_name.eq_ignore_ascii_case(role_name))?;
    let toolset_profile = role.toolset_profile.clone();
    let toolset = match ipc_client
        .send_request(IpcRequest::GetToolsetProfile {
            profile_name: toolset_profile.clone(),
        })
        .await
    {
        Ok(IpcResponse::Standard {
            ok: true,
            data: Some(data),
            ..
        }) => serde_json::from_value::<ansible_mesh_core::graph::ToolsetProfileRecord>(data).ok(),
        _ => None,
    };

    Some(TargetRoleLens {
        role_name: role.role_name,
        toolset_profile: Some(toolset_profile),
        toolset_description: toolset
            .as_ref()
            .and_then(|profile| profile.description.clone()),
        role_identity_addendum: role.role_identity_addendum,
        role_manifest: role.role_manifest,
        allowed_skills: toolset
            .map(|profile| profile.allowed_skills)
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        AgentRuntime, DEFAULT_TEXT_MODEL_ROLE, DEFAULT_VOICE_MODEL_ROLE, LOCAL_NODE, MediaRouting,
        compile_turn_routing_plan, extract_model_error, extract_model_error_payload,
        format_role_command_reply, format_roles_report, is_specific_same_self_role_governance,
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
        RoleActivation, SessionState, TtsMode, TurnCapabilityCompositionKind,
        TurnRoutedCapabilitySpecies, TurnRoutingStageKind, VoiceResponsePolicy, WorkingTurn,
        turn_routed_capability_profile,
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
            effective_rights: Vec::new(),
            response_contract: None,
            routing_hints: None,
            provider_options: None,
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
        assert_eq!(super::implementation_to_model_role("gemini"), "model");
        assert_eq!(super::implementation_to_model_role("gemini-flash"), "model");
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
    fn merge_snapshot_bindings_updates_component_route_assembly() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        let snapshot = serde_json::json!({
            "bindings": {
                "effective_toolset": ["echo"],
                "effective_rights": ["component.media.analyze", "component.text.generate", "tool.echo"],
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
        assert_eq!(
            state.bindings.effective_rights,
            vec![
                "component.media.analyze".to_string(),
                "component.text.generate".to_string(),
                "tool.echo".to_string(),
            ]
        );
        assert_eq!(state.bindings.effective_skillset, vec!["planning"]);
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
            handoff_bundle: None,
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
            }),
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
            turn_routing_plan: None,
            awaiting_transcription_reentry: false,
            scripted_loop_context: None,
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
    fn parse_learned_reflex_writeback_accepts_valid_payload() {
        let parsed = super::parse_learned_reflex_writeback(&serde_json::json!({
            "learned_reflex": {
                "preference_key": "operator-mesh-trust",
                "precedence": 72,
                "reflexes": {
                    "remote_tool_reflex": "allow",
                    "credential_scope_reflex": "mesh_scoped"
                }
            }
        }))
        .expect("parse should succeed")
        .expect("writeback should be present");

        assert_eq!(parsed.preference_key, "operator-mesh-trust");
        assert_eq!(parsed.precedence, 72);
        assert_eq!(parsed.reflexes_json["remote_tool_reflex"], "allow");
    }

    #[test]
    fn parse_learned_reflex_writeback_rejects_non_object_reflexes() {
        let err = super::parse_learned_reflex_writeback(&serde_json::json!({
            "learned_reflex": {
                "preference_key": "operator-mesh-trust",
                "reflexes": "allow"
            }
        }))
        .expect_err("parse should fail");

        assert!(err.contains("learned_reflex.reflexes must be an object"));
    }

    #[test]
    fn merge_snapshot_bindings_updates_agent_reflex_layers() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        let snapshot = serde_json::json!({
            "bindings": {
                "reflex_policy_agent_layers": [{
                    "policy_scope": "agent_learned",
                    "policy_source": "agent_graph",
                    "origin_class": "agent_learned",
                    "precedence": 70,
                    "preference_key": "same-self-role-handoff:developer",
                    "config": {
                        "reason": "remembered successful same-self handoff to developer",
                        "role_name": "developer",
                        "trigger_class": "implementation"
                    },
                    "reflexes": {
                        "role_handoff_reflex": {
                            "target_role": "developer",
                            "trigger_class": "implementation"
                        }
                    }
                }]
            }
        });

        AgentRuntime::merge_snapshot_bindings(&mut state, &snapshot);

        assert_eq!(state.bindings.reflex_policy_agent_layers.len(), 1);
        assert_eq!(
            state.bindings.reflex_policy_agent_layers[0]["preference_key"],
            serde_json::json!("same-self-role-handoff:developer")
        );
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
    fn compile_turn_routing_plan_for_voice_turn_has_three_stages() {
        use crate::session::{MediaRoutingPolicy, SessionBindings};

        let policy = MediaRoutingPolicy {
            voice_action: Some("transcribe".into()),
            ..Default::default()
        };
        let media_routing =
            resolve_media_routing(&policy, vec![blob_backed_attachment("voice")]).unwrap();
        let voice_policy = VoiceResponsePolicy {
            mode: TtsMode::Auto,
            ..Default::default()
        };

        let plan = compile_turn_routing_plan(
            Some(&media_routing),
            Some(&voice_policy),
            true,
            &[],
            &SessionBindings::default(),
        );

        assert_eq!(plan.trigger, "voice_input");
        assert_eq!(plan.stages.len(), 3);
        assert_eq!(plan.stages[0].kind, TurnRoutingStageKind::Ingress);
        assert_eq!(plan.stages[0].capability, "voice.transcribe");
        assert_eq!(plan.stages[0].controller_role, DEFAULT_VOICE_MODEL_ROLE);
        assert_eq!(plan.stages[0].model_ref, None);
        assert_eq!(plan.stages[1].kind, TurnRoutingStageKind::Cognition);
        assert_eq!(plan.stages[1].capability, "text.generate");
        assert_eq!(plan.stages[1].controller_role, DEFAULT_TEXT_MODEL_ROLE);
        assert_eq!(plan.stages[2].kind, TurnRoutingStageKind::Egress);
        assert_eq!(plan.stages[2].capability, "voice.synthesize");
    }

    #[test]
    fn compile_turn_routing_plan_for_text_turn_only_has_cognition() {
        use crate::session::SessionBindings;

        let plan = compile_turn_routing_plan(None, None, false, &[], &SessionBindings::default());

        assert_eq!(plan.trigger, "text_input");
        assert_eq!(plan.stages.len(), 1);
        assert_eq!(plan.stages[0].kind, TurnRoutingStageKind::Cognition);
        assert_eq!(plan.stages[0].capability, "text.generate");
    }

    #[test]
    fn compile_turn_routing_plan_applies_agent_graph_routing_preferences() {
        use crate::session::{RoutingPreferenceBinding, SessionBindings};

        let routing_preferences = vec![RoutingPreferenceBinding {
            preference_key: "voice-ingress-elevenlabs-scribe".into(),
            stage_kind: Some("ingress".into()),
            capability: Some("voice.transcribe".into()),
            provider_hint: Some("elevenlabs".into()),
            model_ref: Some("scribe_v1".into()),
            preference_level: 1,
            weight: 90,
            updated_at: 123,
        }];
        let media_routing = MediaRouting {
            action: "transcribe".into(),
            capability: "voice.transcribe",
            attachments: vec![blob_backed_attachment("voice")],
        };

        let plan = compile_turn_routing_plan(
            Some(&media_routing),
            None,
            true,
            &routing_preferences,
            &SessionBindings::default(),
        );

        assert_eq!(plan.stages[0].provider_hint.as_deref(), Some("elevenlabs"));
        assert_eq!(plan.stages[0].model_ref.as_deref(), Some("scribe_v1"));
        assert_eq!(plan.stages[0].controller_role, DEFAULT_VOICE_MODEL_ROLE);
    }

    #[test]
    fn stage_preference_promotes_dispatch_to_provider_specific_controller_role() {
        use crate::session::{RoutingPreferenceBinding, SessionBindings};

        let routing_preferences = vec![RoutingPreferenceBinding {
            preference_key: "voice-ingress-elevenlabs-scribe".into(),
            stage_kind: Some("ingress".into()),
            capability: Some("voice.transcribe".into()),
            provider_hint: Some("elevenlabs".into()),
            model_ref: Some("scribe_v1".into()),
            preference_level: 1,
            weight: 90,
            updated_at: 123,
        }];
        let media_routing = MediaRouting {
            action: "transcribe".into(),
            capability: "voice.transcribe",
            attachments: vec![blob_backed_attachment("voice")],
        };

        let plan = compile_turn_routing_plan(
            Some(&media_routing),
            None,
            true,
            &routing_preferences,
            &SessionBindings::default(),
        );
        let target = super::resolve_stage_execution_target(None, &plan.stages[0]);

        assert_eq!(target.0, super::local_node_id());
        assert_eq!(target.1, DEFAULT_VOICE_MODEL_ROLE);
        assert!(target.2.is_none());
    }

    #[test]
    fn compile_turn_routing_plan_rewards_approved_agent_reflexes_for_cognition_selection() {
        use crate::session::{RoutingPreferenceBinding, SessionBindings};

        let routing_preferences = vec![
            RoutingPreferenceBinding {
                preference_key: "cognition-local".into(),
                stage_kind: Some("cognition".into()),
                capability: Some("text.generate".into()),
                provider_hint: Some("mlx".into()),
                model_ref: Some("mlx/qwen".into()),
                preference_level: 1,
                weight: 89,
                updated_at: 10,
            },
            RoutingPreferenceBinding {
                preference_key: "cognition-remote".into(),
                stage_kind: Some("cognition".into()),
                capability: Some("text.generate".into()),
                provider_hint: Some("gemini".into()),
                model_ref: Some("gemini-3.1-flash".into()),
                preference_level: 1,
                weight: 90,
                updated_at: 9,
            },
        ];
        let bindings = SessionBindings {
            effective_reflexes: serde_json::json!({
                "remote_component_reflex": "allow"
            }),
            reflex_policy_agent_rewards: vec![serde_json::json!({
                "preference_key": "cognition-remote",
                "regulatory_system": "reward"
            })],
            ..Default::default()
        };

        let plan = compile_turn_routing_plan(None, None, false, &routing_preferences, &bindings);

        assert_eq!(plan.stages[0].provider_hint.as_deref(), Some("gemini"));
        assert_eq!(
            plan.stages[0].model_ref.as_deref(),
            Some("gemini-3.1-flash")
        );
    }

    #[test]
    fn compile_turn_routing_plan_immune_system_dampens_remote_cognition_selection() {
        use crate::session::{RoutingPreferenceBinding, SessionBindings};

        let routing_preferences = vec![
            RoutingPreferenceBinding {
                preference_key: "cognition-local".into(),
                stage_kind: Some("cognition".into()),
                capability: Some("text.generate".into()),
                provider_hint: Some("mlx".into()),
                model_ref: Some("mlx/qwen".into()),
                preference_level: 1,
                weight: 89,
                updated_at: 10,
            },
            RoutingPreferenceBinding {
                preference_key: "cognition-remote".into(),
                stage_kind: Some("cognition".into()),
                capability: Some("text.generate".into()),
                provider_hint: Some("gemini".into()),
                model_ref: Some("gemini-3.1-flash".into()),
                preference_level: 1,
                weight: 90,
                updated_at: 9,
            },
        ];
        let bindings = SessionBindings {
            effective_reflexes: serde_json::json!({
                "remote_component_reflex": "deny"
            }),
            reflex_policy_agent_suppressions: vec![serde_json::json!({
                "preference_key": "cognition-remote",
                "regulatory_system": "immune"
            })],
            ..Default::default()
        };

        let plan = compile_turn_routing_plan(None, None, false, &routing_preferences, &bindings);

        assert_eq!(plan.stages[0].provider_hint.as_deref(), Some("mlx"));
        assert_eq!(plan.stages[0].model_ref.as_deref(), Some("mlx/qwen"));
    }

    #[test]
    fn compile_turn_routing_plan_uses_shared_model_markers_to_bias_cognition_selection() {
        use crate::session::{RoutingPreferenceBinding, SessionBindings};

        let routing_preferences = vec![
            RoutingPreferenceBinding {
                preference_key: "cognition-local".into(),
                stage_kind: Some("cognition".into()),
                capability: Some("text.generate".into()),
                provider_hint: Some("mlx".into()),
                model_ref: Some("mlx/qwen".into()),
                preference_level: 1,
                weight: 90,
                updated_at: 10,
            },
            RoutingPreferenceBinding {
                preference_key: "cognition-remote".into(),
                stage_kind: Some("cognition".into()),
                capability: Some("text.generate".into()),
                provider_hint: Some("gemini".into()),
                model_ref: Some("gemini-3.1-flash".into()),
                preference_level: 1,
                weight: 90,
                updated_at: 9,
            },
        ];
        let bindings = SessionBindings {
            shared_model_markers: vec![
                serde_json::json!({
                    "model_ref": "mlx/qwen",
                    "provider_hint": "mlx",
                    "capability_markers": ["text.generate"],
                    "speed_marker": 55,
                    "thinking_marker": 55,
                    "tool_use_marker": 45,
                    "audio_native_marker": 0
                }),
                serde_json::json!({
                    "model_ref": "gemini-3.1-flash",
                    "provider_hint": "gemini",
                    "capability_markers": ["text.generate"],
                    "speed_marker": 90,
                    "thinking_marker": 75,
                    "tool_use_marker": 80,
                    "audio_native_marker": 0
                }),
            ],
            ..Default::default()
        };

        let plan = compile_turn_routing_plan(None, None, false, &routing_preferences, &bindings);

        assert_eq!(plan.stages[0].provider_hint.as_deref(), Some("gemini"));
        assert_eq!(
            plan.stages[0].model_ref.as_deref(),
            Some("gemini-3.1-flash")
        );
    }

    #[test]
    fn compile_turn_routing_plan_selects_native_live_voice_dialogue_when_ligands_are_expressed() {
        use crate::session::{RoutingPreferenceBinding, SessionBindings};

        let routing_preferences = vec![
            RoutingPreferenceBinding {
                preference_key: "cognition-text-gemini".into(),
                stage_kind: Some("cognition".into()),
                capability: Some("text.generate".into()),
                provider_hint: Some("gemini".into()),
                model_ref: Some("gemini-3.1-flash".into()),
                preference_level: 1,
                weight: 90,
                updated_at: 10,
            },
            RoutingPreferenceBinding {
                preference_key: "cognition-live-gemini".into(),
                stage_kind: Some("cognition".into()),
                capability: Some("voice.dialogue".into()),
                provider_hint: Some("gemini".into()),
                model_ref: Some("gemini-3.1-flash-live".into()),
                preference_level: 1,
                weight: 90,
                updated_at: 11,
            },
        ];
        let bindings = SessionBindings {
            shared_model_markers: vec![
                serde_json::json!({
                    "model_ref": "gemini-3.1-flash",
                    "provider_hint": "gemini",
                    "capability_markers": ["text.generate"],
                    "speed_marker": 70,
                    "thinking_marker": 72,
                    "tool_use_marker": 75,
                    "audio_native_marker": 0
                }),
                serde_json::json!({
                    "model_ref": "gemini-3.1-flash-live",
                    "provider_hint": "gemini",
                    "capability_markers": ["voice.dialogue", "response.generate"],
                    "speed_marker": 92,
                    "thinking_marker": 74,
                    "tool_use_marker": 78,
                    "audio_native_marker": 95
                }),
            ],
            ..Default::default()
        };
        let media_routing = MediaRouting {
            action: "transcribe".into(),
            capability: "voice.transcribe",
            attachments: vec![blob_backed_attachment("voice")],
        };
        let voice_policy = VoiceResponsePolicy {
            mode: TtsMode::Auto,
            ..Default::default()
        };

        let plan = compile_turn_routing_plan(
            Some(&media_routing),
            Some(&voice_policy),
            true,
            &routing_preferences,
            &bindings,
        );

        assert_eq!(plan.trigger, "voice_input_native_live");
        assert_eq!(plan.stages.len(), 2);
        assert_eq!(plan.stages[0].kind, TurnRoutingStageKind::Cognition);
        assert_eq!(plan.stages[0].capability, "voice.dialogue");
        assert_eq!(plan.stages[0].provider_hint.as_deref(), Some("gemini"));
        assert_eq!(
            plan.stages[0].model_ref.as_deref(),
            Some("gemini-3.1-flash-live")
        );
        assert_eq!(plan.stages[1].kind, TurnRoutingStageKind::Egress);
        assert_eq!(plan.stages[1].capability, "voice.synthesize");
    }

    #[test]
    fn compile_turn_routing_plan_keeps_three_stage_voice_path_without_native_live_ligands() {
        use crate::session::{RoutingPreferenceBinding, SessionBindings};

        let routing_preferences = vec![RoutingPreferenceBinding {
            preference_key: "cognition-text-gemini".into(),
            stage_kind: Some("cognition".into()),
            capability: Some("text.generate".into()),
            provider_hint: Some("gemini".into()),
            model_ref: Some("gemini-3.1-flash".into()),
            preference_level: 1,
            weight: 90,
            updated_at: 10,
        }];
        let bindings = SessionBindings {
            shared_model_markers: vec![serde_json::json!({
                "model_ref": "gemini-3.1-flash",
                "provider_hint": "gemini",
                "capability_markers": ["text.generate"],
                "speed_marker": 90,
                "thinking_marker": 75,
                "tool_use_marker": 80,
                "audio_native_marker": 0
            })],
            ..Default::default()
        };
        let media_routing = MediaRouting {
            action: "transcribe".into(),
            capability: "voice.transcribe",
            attachments: vec![blob_backed_attachment("voice")],
        };
        let voice_policy = VoiceResponsePolicy {
            mode: TtsMode::Auto,
            ..Default::default()
        };

        let plan = compile_turn_routing_plan(
            Some(&media_routing),
            Some(&voice_policy),
            true,
            &routing_preferences,
            &bindings,
        );

        assert_eq!(plan.trigger, "voice_input");
        assert_eq!(plan.stages.len(), 3);
        assert_eq!(plan.stages[0].kind, TurnRoutingStageKind::Ingress);
        assert_eq!(plan.stages[0].capability, "voice.transcribe");
        assert_eq!(plan.stages[1].kind, TurnRoutingStageKind::Cognition);
        assert_eq!(plan.stages[1].capability, "text.generate");
    }

    #[test]
    fn merge_snapshot_bindings_updates_routing_preferences() {
        use crate::session::RoutingPreferenceBinding;

        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        let snapshot = serde_json::json!({
            "bindings": {
                "effective_rights": ["tool.echo"],
                "routing_preferences": [{
                    "preference_key": "cognition-gemini-flash",
                    "stage_kind": "cognition",
                    "capability": "text.generate",
                    "provider_hint": "gemini",
                    "model_ref": "gemini-flash",
                    "preference_level": 1,
                    "weight": 80,
                    "updated_at": 42
                }]
            }
        });

        AgentRuntime::merge_snapshot_bindings(&mut state, &snapshot);

        assert_eq!(
            state.bindings.effective_rights,
            vec!["tool.echo".to_string()]
        );
        assert_eq!(
            state.bindings.routing_preferences,
            vec![RoutingPreferenceBinding {
                preference_key: "cognition-gemini-flash".into(),
                stage_kind: Some("cognition".into()),
                capability: Some("text.generate".into()),
                provider_hint: Some("gemini".into()),
                model_ref: Some("gemini-flash".into()),
                preference_level: 1,
                weight: 80,
                updated_at: 42,
            }]
        );
    }

    #[test]
    fn merge_snapshot_bindings_updates_shared_model_markers() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        let snapshot = serde_json::json!({
            "bindings": {
                "shared_model_markers": [{
                    "model_ref": "gemini-3.1-flash",
                    "provider_hint": "gemini",
                    "capability_markers": ["text.generate", "media.analyze"],
                    "speed_marker": 90,
                    "thinking_marker": 72,
                    "tool_use_marker": 84,
                    "audio_native_marker": 20
                }]
            }
        });

        AgentRuntime::merge_snapshot_bindings(&mut state, &snapshot);

        assert_eq!(state.bindings.shared_model_markers.len(), 1);
        assert_eq!(
            state.bindings.shared_model_markers[0]["model_ref"],
            serde_json::json!("gemini-3.1-flash")
        );
    }

    #[test]
    fn merge_snapshot_bindings_updates_active_role_state_from_snapshot() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.active_incarnation_id = Some("agent-jane:orchestrator".into());
        state.role_activation = Some(RoleActivation {
            role_name: "orchestrator".into(),
            active_incarnation_id: Some("agent-jane:orchestrator".into()),
            activation_reason: "default_identity_posture".into(),
            ..Default::default()
        });

        let snapshot = serde_json::json!({
            "active_incarnation_id": "agent-jane:systems-architect",
            "role_activation": {
                "role_name": "systems-architect",
                "active_incarnation_id": "agent-jane:systems-architect",
                "activation_reason": "session_active_incarnation"
            },
            "bindings": {}
        });

        AgentRuntime::merge_snapshot_bindings(&mut state, &snapshot);

        assert_eq!(
            state.active_incarnation_id.as_deref(),
            Some("agent-jane:systems-architect")
        );
        assert_eq!(
            state
                .role_activation
                .as_ref()
                .map(|role| role.role_name.as_str()),
            Some("systems-architect")
        );
        assert_eq!(
            state
                .role_activation
                .as_ref()
                .and_then(|role| role.active_incarnation_id.as_deref()),
            Some("agent-jane:systems-architect")
        );
    }

    #[test]
    fn turn_routed_capability_taxonomy_marks_native_live_species_as_collapsible() {
        let response_generate =
            turn_routed_capability_profile("response.generate").expect("profile");
        assert_eq!(
            response_generate.species,
            TurnRoutedCapabilitySpecies::ResponseGenerate
        );
        assert_eq!(
            response_generate.composition,
            TurnCapabilityCompositionKind::CollapsibleIngressCognition
        );

        let voice_dialogue = turn_routed_capability_profile("voice.dialogue").expect("profile");
        assert_eq!(
            voice_dialogue.species,
            TurnRoutedCapabilitySpecies::VoiceDialogue
        );
        assert_eq!(
            voice_dialogue.composition,
            TurnCapabilityCompositionKind::CollapsibleIngressCognition
        );
    }

    #[test]
    fn turn_routed_capability_taxonomy_keeps_transcribe_and_synthesize_stage_local() {
        let transcribe = turn_routed_capability_profile("voice.transcribe").expect("profile");
        assert_eq!(transcribe.default_stage_kind, TurnRoutingStageKind::Ingress);
        assert_eq!(
            transcribe.composition,
            TurnCapabilityCompositionKind::StageLocal
        );

        let synth = turn_routed_capability_profile("voice.synthesize").expect("profile");
        assert_eq!(synth.default_stage_kind, TurnRoutingStageKind::Egress);
        assert_eq!(synth.composition, TurnCapabilityCompositionKind::StageLocal);
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
    fn explicit_same_self_role_governance_counts_as_specificity_not_pending_approval() {
        let tool_call = ToolCall {
            tool_name: "role.create_or_update".into(),
            arguments: serde_json::json!({
                "role_name": "virtuosa",
                "toolset_profile": "voice",
                "reasoning": {
                    "purpose": "specialize in performance and voice delivery",
                    "toolset_rationale": "voice tools and performance posture",
                    "handoff_posture_and_limits": "same-self voice specialization only"
                }
            }),
        };

        assert!(is_specific_same_self_role_governance(&tool_call));
    }

    #[test]
    fn admin_role_governance_still_requires_live_operator_gate() {
        let tool_call = ToolCall {
            tool_name: "role.create_or_update".into(),
            arguments: serde_json::json!({
                "role_name": "root-operator",
                "toolset_profile": "admin",
                "is_admin": true,
                "reasoning": {
                    "purpose": "admin mutation",
                    "toolset_rationale": "admin tooling",
                    "handoff_posture_and_limits": "sensitive"
                }
            }),
        };

        assert!(!is_specific_same_self_role_governance(&tool_call));
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
    fn extract_model_audio_artifact_reads_audio_payload_from_artifacts() {
        let model_result = serde_json::json!({
            "artifacts": [{
                "kind": "audio",
                "mime_type": "audio/wav",
                "payload": {
                    "kind": "audio_artifact",
                    "mime_type": "audio/wav",
                    "output_format": "wav",
                    "voice_id": "gemini-live",
                    "model": "gemini-3.1-flash-live",
                    "audio_base64": "AQID"
                }
            }]
        });

        let artifact = super::extract_model_audio_artifact(Some(&model_result))
            .expect("audio artifact should extract");
        let parsed: serde_json::Value =
            serde_json::from_str(&artifact).expect("artifact should stay serialized json");
        assert_eq!(parsed["kind"], "audio_artifact");
        assert_eq!(parsed["mime_type"], "audio/wav");
    }

    #[test]
    fn extract_model_audio_artifact_ignores_non_audio_artifacts() {
        let model_result = serde_json::json!({
            "artifacts": [{
                "kind": "embedding",
                "payload": { "vector": [1, 2, 3] }
            }]
        });

        assert!(super::extract_model_audio_artifact(Some(&model_result)).is_none());
    }

    #[test]
    fn extracts_native_live_pending_function_call_id_from_model_result() {
        let task: InboundTaskPayload = serde_json::from_value(serde_json::json!({
            "action": "model_response",
            "agent_action": {
                "kind": "tool_call",
                "tool_name": "session.status",
                "arguments": {},
                "model_result": {
                    "native_live": {
                        "pending_function_call_id": "call-1"
                    }
                }
            }
        }))
        .expect("payload should parse");

        assert_eq!(
            super::extract_native_live_pending_function_call_id(&task).as_deref(),
            Some("call-1")
        );
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
