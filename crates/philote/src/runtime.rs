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
    ActivePlan, AgentProfile, ComponentRouteAssembly, FallbackOverride, GraphAnchors,
    HANDOFF_CONTEXT_EXCERPT_MAX_CHARS, LifeRecallCacheEntry, MediaRoutingPolicy, MemoryAuthority,
    MemoryShapingContext, MemorySpacetimeFrame, MemorySpatialScope, MemoryTemporalKind,
    MemoryValidationLevel, PARACRINE_MERGE_CONTENT_MAX_CHARS, PARACRINE_WHISPER_PROMPT_MAX_CHARS,
    ParacrineBudgetOutcome, ParacrineThreadStatus, RecalledMemoryRecord, SelectionSource,
    SessionState, ToolDefinition, ToolExecutionRoute, ToolRunnerIncarnationBinding, TtsMode,
    VoiceResponsePolicy, WorkingTurn, charge_paracrine_hop, merge_session_index, truncate_for_wire,
};
use anyhow::{Context, Result};
use memory_core::{
    Engram, MemoryScope, MuninnConfig, MuninnRestEngine, RecallContext, RecallTrigger,
    VaultResolver,
};
use philotic_client::{
    Exosome, HandoffBundle, IpcRequest, IpcResponse, ParacrineRouting, PhiloticClient,
    TaskErrorPayload, UserProfileDataPayload, is_ipc_disconnect, is_ipc_timeout,
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

#[path = "life_capture.rs"]
mod life_capture;
use life_capture::*;

#[path = "memory_explain_tool.rs"]
mod memory_explain_tool;

#[path = "voice_stream.rs"]
mod voice_stream;
use voice_stream::*;

pub const DEFAULT_AGENT_ID: &str = "agent-bjork-01";
const DEFAULT_REPLY_ROLE: &str = "membrane";
const DEFAULT_TEXT_MODEL_ROLE: &str = "model";
const DEFAULT_VOICE_MODEL_ROLE: &str = "model.elevenlabs";
const MEMORY_CANDIDATE_POLICY: &str = "Emit memory_candidate only when this exchange contains \
durable future-useful context: a user preference, explicit decision, stable fact, validation \
outcome, reality gap, next seam, or recurring pattern. Omit it for greetings, acknowledgments, \
readiness/status chatter, transient task progress, tool logs, transcripts, or routine task-list \
churn. The candidate must be atomic: concept is a specific short slug, content is 1-3 sentences \
and 24-700 characters, tags are optional with 10 or fewer short tags.";

fn cognitive_response_contract(channels: &[&str]) -> Value {
    json!({
        "channels": channels,
        "memory_candidate_policy": MEMORY_CANDIDATE_POLICY,
    })
}

fn local_node_id() -> String {
    std::env::var("PHILOTIC_NODE_ID").unwrap_or_else(|_| "local-aiua-01".to_string())
}

/// The guest identity a philote registers under and stamps as `reply_guest_id`
/// on its dispatches: the bare `agent_id` for a base philote, or
/// `"{agent_id}:{role_name}"` for a role incarnation. Keeping the registration
/// shape and the reply-address shape in one place is what lets the hotel deliver
/// a response back to the subscription owned by the runtime that holds the turn
/// (DEF-051).
pub fn compose_guest_identity(agent_id: &str, role_name: Option<&str>) -> String {
    match role_name {
        Some(role_name) => format!("{agent_id}:{role_name}"),
        None => agent_id.to_string(),
    }
}

/// Best-effort chat_id recovery for out-of-turn dispatch, when there is no
/// active `WorkingTurn` to read `chat_id` from directly. Mirrors the
/// session_id encoding from `InboundTaskPayload::session_id_or_default`
/// (`"{source}:{chat_id}:..."`), the same convention `handle_paracrine_response`
/// relies on to route to an idle session.
fn chat_id_from_session_id(session_id: &str, source: &str) -> Option<String> {
    session_id
        .strip_prefix(&format!("{source}:"))
        .and_then(|rest| rest.split(':').next())
        .filter(|c| !c.is_empty())
        .map(str::to_string)
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

/// Reads the `context_pressure_pct` field out of a serialized
/// `ContextProjection` (as produced by `SessionState::model_request_payloads`)
/// and clamps it to `u8::MAX`-safe 0..=100, matching the clamp already
/// applied at assembly time in `session/mod.rs`. This is the literal
/// extraction logic used at the `handle_user_message` turn-assembly call
/// site to drive the live `ReflexEvent::ContextPressure` producer — pulled
/// out as a pure function so a rename or removal of the field is caught by
/// a plain unit test instead of requiring a full IPC-backed runtime harness.
fn context_pressure_pct_from_projection(context_projection: &Value) -> Option<u8> {
    context_projection
        .get("context_pressure_pct")
        .and_then(Value::as_u64)
        .map(|pct| pct.min(100) as u8)
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

/// Escalation class for a provider failure — decides what the turn loop does
/// with the error signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderErrorClass {
    /// Transient (network / 5xx / streaming stall): a retry may succeed —
    /// same-tier retry where the sub_kind allows it, otherwise next tier.
    RetrySameProvider,
    /// The request will fail identically on the same provider (4xx contract
    /// errors, INVALID_ARGUMENT, refusals, rate limits): advance to the next
    /// fallback tier, skipping tiers that dispatch to the failed provider.
    SwitchProvider,
    /// Auth/key misconfiguration: no retry against this provider can help
    /// until an operator intervenes — surface to the user fast (plus a heal
    /// event so the outage becomes an A3 work item).
    Fatal,
    /// The provider's own content/safety filter blocked the response (Gemini
    /// `finishReason=SAFETY` / `promptFeedback.blockReason`, etc.) — a
    /// deliberately DISTINCT outcome from `SwitchProvider`. Switching to a
    /// different-behaving model mid-conversation is the jarring failover this
    /// class exists to prevent (2026-07-09 operator report); for an
    /// `unrestricted` agent this should essentially never fire since
    /// `safetySettings` disables the filter, but if it does, fail the turn
    /// cleanly with a clear message rather than silently hopping providers.
    ContentBlocked,
    /// Not a model-provider escalation signal (tool/transport/voice errors):
    /// fall through to the generic fail path.
    Unclassified,
}

/// Classify a provider failure into an escalation class.
///
/// Prefers the machine-readable `error_class` stamped by newer model-router
/// controllers; falls back to `sub_kind` / HTTP `status` / kind heuristics for
/// older controllers that predate the field. The final fallback maps any
/// otherwise-unclassified `provider_failure` on the text path to
/// `SwitchProvider`: an unrecognized provider error must engage the fallback
/// ladder, not insta-fail the turn (forensic 2026-07-08 — nine Gemini 400s
/// died as MODEL_EMPTY_RESPONSE while a healthy model.ollama tier sat idle).
pub(crate) fn classify_provider_error(error: &TaskErrorPayload) -> ProviderErrorClass {
    // 1. Machine-readable class from the controller wins.
    match error.error_class.as_deref() {
        Some("fatal") => return ProviderErrorClass::Fatal,
        Some("content_blocked") => return ProviderErrorClass::ContentBlocked,
        Some("switch_provider") => return ProviderErrorClass::SwitchProvider,
        Some("retry_same_provider") => return ProviderErrorClass::RetrySameProvider,
        _ => {}
    }

    // 2. sub_kind fallback (older controllers).
    match error.sub_kind.as_deref() {
        Some("provider_auth") => return ProviderErrorClass::Fatal,
        Some("content_policy_block") => return ProviderErrorClass::ContentBlocked,
        Some("network_error") | Some("streaming_timeout") | Some("provider_error") => {
            return ProviderErrorClass::RetrySameProvider;
        }
        Some("rate_limit") | Some("invalid_request") => {
            return ProviderErrorClass::SwitchProvider;
        }
        _ => {}
    }

    if error.kind != "provider_failure" {
        return ProviderErrorClass::Unclassified;
    }

    // 3. HTTP status fallback.
    if let Some(status) = error.status {
        return match status {
            401 | 403 => ProviderErrorClass::Fatal,
            400..=499 => ProviderErrorClass::SwitchProvider,
            _ => ProviderErrorClass::RetrySameProvider,
        };
    }

    // 4. Un-annotated provider_failure. Only the text-generation path may
    // engage the fallback ladder — voice/embedding failures have their own
    // handling and must not re-dispatch generate_text. Content errors reach
    // here only after the repair attempt is spent; switching providers is
    // then the productive move.
    let text_capability = matches!(error.capability.as_deref(), None | Some("text.generate"));
    if text_capability {
        ProviderErrorClass::SwitchProvider
    } else {
        ProviderErrorClass::Unclassified
    }
}

/// Default tier ordering when none is configured in TurnLoopConfig.
///
/// Single source of truth in core so the hotel's config-time ladder validation
/// checks the exact ladder philote runs.
use ansible_mesh_core::model_routing::DEFAULT_FALLBACK_TIERS;

/// Why a turn is being treated as "the model did not answer". Carried into the
/// shared policy so the escalation path and the watchdog can never pick
/// divergent escalate-vs-evict thresholds for the same underlying event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoResponseClass {
    /// The provider returned an explicit retriable failure (network / timeout /
    /// provider error) — fired immediately on the error signal.
    ProviderFailure,
    /// The provider rejected the request contract (4xx / INVALID_ARGUMENT /
    /// refusal / rate limit): the same request fails identically on the same
    /// provider, so the ladder walk skips tiers that dispatch to it.
    ProviderContractFailure,
    /// No signal arrived; the stuck-turn watchdog fired after the WaitingModel
    /// deadline elapsed.
    WatchdogTimeout,
}

/// What to do when the model did not produce a usable answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoResponseAction {
    /// Re-dispatch the turn to the next fallback tier.
    EscalateTier,
    /// Give up on this turn (fail it with a user-visible notice).
    EvictTurn,
}

/// The single "model didn't answer" decision table, consulted by BOTH the
/// provider-failure escalation path and the WaitingModel watchdog (both funnel
/// through `advance_turn_to_next_fallback_tier`). Keeping one table means their
/// thresholds cannot silently diverge.
///
/// Initial values preserve pre-slice behavior exactly: escalate while a live
/// fallback tier remains, otherwise evict. Failure class is threaded so a future
/// slice can diverge (e.g. skip escalation on a hard auth failure) without
/// re-introducing two uncoordinated code paths — it does not change the action
/// today. The elapsed deadline is encoded by *which* trigger invoked the policy
/// (the watchdog only fires past its deadline; provider failures fire on signal).
pub(crate) fn decide_no_response_action(
    class: NoResponseClass,
    tiers_remaining: bool,
) -> NoResponseAction {
    match class {
        NoResponseClass::ProviderFailure
        | NoResponseClass::ProviderContractFailure
        | NoResponseClass::WatchdogTimeout => {
            if tiers_remaining {
                NoResponseAction::EscalateTier
            } else {
                NoResponseAction::EvictTurn
            }
        }
    }
}

/// Short cause string for a `NoResponseClass`, used both for the user-visible
/// `provider_switch` turn event and as the `FallbackOverride.reason` recorded
/// by `advance_turn_to_next_fallback_tier` on a successful escalation.
pub(crate) fn no_response_reason_str(class: NoResponseClass) -> &'static str {
    match class {
        NoResponseClass::ProviderFailure => "provider_failure",
        NoResponseClass::ProviderContractFailure => "provider_contract_failure",
        NoResponseClass::WatchdogTimeout => "model_timeout",
    }
}

/// The next ladder tier to dispatch after the currently active dispatch fails.
///
/// `last_ladder_tier` is the index of the last ladder tier that was actually
/// *dispatched* — `None` when the ladder hasn't been consulted yet (the
/// primary dispatch used a hotel route, an explicit binding, or the plain
/// default role instead of `configured_tiers[0]`; see
/// `primary_dispatch_used_ladder`). This is the off-by-one fix: without it, a
/// turn whose primary dispatch bypassed the ladder would jump straight to
/// tier 1 on failure and a single-tier ladder would never be tried at all.
/// `Some(tier)` walks forward from `tier + 1` as before.
///
/// On a contract failure (`skip_failed_provider = true` with a known failed
/// provider) tiers whose role dispatches to that same provider are skipped —
/// the request would fail identically there. Returns a tier > `max_tier` when
/// the ladder is exhausted (possibly *by* the skip). Pure so the skip contract
/// is unit-testable without IPC.
fn next_ladder_tier(
    configured_tiers: &[String],
    last_ladder_tier: Option<u8>,
    max_tier: u8,
    failed_provider: Option<&str>,
    skip_failed_provider: bool,
) -> u8 {
    let mut next = last_ladder_tier.map_or(0, |tier| tier.saturating_add(1));
    if skip_failed_provider {
        if let Some(failed) = failed_provider {
            while next <= max_tier
                && provider_for_role(role_for_tier(configured_tiers, next)).as_deref()
                    == Some(failed)
            {
                next = next.saturating_add(1);
            }
        }
    }
    next
}

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
        // Out-of-range clamp mirrors the last tier of DEFAULT_FALLBACK_TIERS
        // (the local last resort).
        DEFAULT_FALLBACK_TIERS
            .get(idx)
            .copied()
            .unwrap_or("model.ollama")
    }
}

/// Maximum extra dispatch attempts the routing oracle may add beneath an
/// exhausted fallback ladder before the turn is evicted. Keeps a pathological
/// oracle loop bounded (the 600 s CatchAll turn budget is the hard ceiling).
pub(crate) const MAX_ORACLE_EXTRA_TIERS: u8 = 2;

/// Inverse of the hotel's controller-role seeding: which provider a ladder
/// tier role dispatches to. Used to build the oracle's exclude list from the
/// tiers a turn has already burned through. Unknown roles map to `None`
/// (nothing to exclude).
fn provider_for_role(role: &str) -> Option<String> {
    match role {
        "model" => Some("gemini".to_string()),
        "model.local" => Some("onnx".to_string()),
        _ => role.strip_prefix("model.").map(str::to_string),
    }
}

/// Capability guard for shadow-mode (`PHILOTIC_SHADOW_ORACLE`) oracle-vs-ladder
/// logging at the FIRST-TURN dispatch (Model Oracle Primary Authority, slice 2).
///
/// The first-turn dispatch in `handle_user_message` multiplexes the cognitive
/// text-generation ladder with aux `transform` tasks (voice transcription, media
/// analysis / description / summarization). The shadow oracle comparison must
/// fire ONLY for the cognitive text case — the same class of dispatch slice 1
/// instruments in `turn_loop` — and NEVER for aux tasks (they don't ride the
/// text fallback ladder, so an oracle-vs-ladder comparison is meaningless there).
///
/// Returns `true` only for `"text.generate"` / `"response.generate"`, the exact
/// pair that also flags this dispatch's `request_class` as `"cognitive"`. Pure so
/// the cognitive-eligible / aux-excluded contract is unit-testable without IPC.
fn shadow_eligible_capability(capability: &str) -> bool {
    matches!(capability, "text.generate" | "response.generate")
}

/// Pick the first oracle-ranked entry whose role is not already in the
/// turn's ladder. Returns `(role, provider)`. Pure so the skip-tried-roles
/// contract is unit-testable without IPC.
fn pick_oracle_role(
    data: &serde_json::Value,
    tried_roles: &std::collections::HashSet<String>,
) -> Option<(String, String)> {
    data.get("ranked")?.as_array()?.iter().find_map(|entry| {
        let role = entry.get("role")?.as_str()?;
        if tried_roles.contains(role) {
            return None;
        }
        let provider = entry
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        Some((role.to_string(), provider.to_string()))
    })
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

/// Absolute ceiling on the effective iteration cap — the same ceiling the
/// plan-scaled cap enforces (`turn_loop.rs` plan handling). The earned-streak
/// extension can never push a turn past this.
const STREAK_CAP_CEILING: u32 = 50;

/// Effective iteration cap for a turn: the configured (possibly plan-scaled)
/// cap plus iterations earned by the turn's productive streak, bounded by
/// [`STREAK_CAP_CEILING`]. A configured cap already at or above the ceiling
/// is honoured unchanged — the extension only ever adds, never shrinks.
fn effective_iteration_cap(configured_cap: u32, turn: &WorkingTurn) -> u32 {
    configured_cap
        .saturating_add(turn.streak_extension)
        .min(STREAK_CAP_CEILING.max(configured_cap))
}

/// True when a completed tool step earns the turn one extra iteration
/// (cognitive-loop-streak-extension seam): the step succeeded, was not a
/// diagnostic/status call, was not a skipped duplicate, and is NOVEL — no
/// prior call this turn used the same tool with the same arguments. Judged
/// against the history BEFORE the step is pushed onto it.
fn tool_step_earns_streak(
    turn: &WorkingTurn,
    call: &ToolCall,
    result: &ToolResult,
    step_failed: bool,
) -> bool {
    if step_failed || low_progress_tool_name(&call.tool_name) || duplicate_tool_skip(result) {
        return false;
    }
    !turn
        .working_tool_history
        .iter()
        .any(|(prior, _)| prior.tool_name == call.tool_name && prior.arguments == call.arguments)
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

/// Maps an `effective_model_controller` / component-route `implementation`
/// string to the model-router role that dispatches it. Mirrors the hotel-side
/// `component_implementation_to_role` (`crates/aiua/src/service/ipc.rs`):
/// already-role-shaped values (`"model.openrouter"`) pass through unchanged,
/// `"gemini"` maps to the default `"model"` role, and every other known
/// provider (`"openrouter"`, `"openai"`, `"ollama"`, `"mlx"`, ...) gets its
/// own dedicated `"model.<provider>"` role instead of collapsing onto the
/// gemini default. `"onnx"` / `"kokoro"` / `"local"` remain a special case
/// mapping to the shared local-backend role.
fn implementation_to_model_role(implementation: &str) -> String {
    let normalized = implementation.trim().to_ascii_lowercase();

    if normalized.starts_with("model.") {
        return normalized;
    }

    let segment = normalized
        .split(['.', '-', '@', '/'])
        .find(|segment| !segment.is_empty())
        .unwrap_or("gemini");

    match segment {
        "gemini" => "model".into(),
        "elevenlabs" => "model.elevenlabs".into(),
        "onnx" | "kokoro" | "local" => "model.local".into(),
        other => format!("model.{other}"),
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

/// Threads the turn's effective content-filtering posture into the
/// `provider_options` bag the same way `voice_response_provider_options`
/// threads voice settings — the model-router's `ControllerTask::provider_options`
/// is the existing generic per-turn knob channel (see `response_modalities`,
/// `voice_id`, `model` above). `"standard"` omits the key entirely so the
/// wire payload — and downstream provider behavior — is byte-for-byte
/// unchanged for every agent that hasn't opted into this feature.
fn content_policy_provider_options(effective_content_policy: &str) -> Map<String, Value> {
    let mut options = Map::new();
    if effective_content_policy != "standard" {
        options.insert("content_policy".into(), json!(effective_content_policy));
    }
    options
}

/// Session-lookup wrapper matching the `Option<&SessionState>` convention used
/// by `model_response_route` / `planning_ligand` / `model_affordances` below —
/// call sites already have `self.sessions.get(&session_id)` in scope for those,
/// so this drops in alongside them at every `action: "generate_text"`
/// `ModelRequestPayload` construction. A missing session (shouldn't happen on
/// this path, but mirrors the other helpers' fail-open behavior) resolves to
/// `"standard"` — i.e. no `provider_options` change, current behavior.
pub(crate) fn resolve_content_policy_provider_options(
    state: Option<&SessionState>,
) -> Map<String, Value> {
    content_policy_provider_options(
        state
            .map(|s| s.effective_content_policy())
            .unwrap_or("standard"),
    )
}

fn voice_response_contract(policy: &VoiceResponsePolicy) -> Value {
    if policy.delivery_mode.is_native_audio() {
        let mut contract = cognitive_response_contract(&[
            "spoken_text",
            "memory_candidate",
            "active_plan",
            "memory_concept",
        ]);
        contract["modalities"] = json!(["text", "audio"]);
        contract
    } else {
        cognitive_response_contract(&[
            "spoken_text",
            "memory_candidate",
            "active_plan",
            "memory_concept",
        ])
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

/// Resolves the model-router dispatch target for `capability`.
///
/// Precedence (highest wins):
///   1. The operator pin (`/model <tier>`, `SessionState.pinned_tier_role`) —
///      explicit operator intent for text-generation dispatch, wins over
///      everything, including the fallback override below.
///   2. The persisted fallback override (`SessionState.fallback_override`,
///      Slice 2) — a session degraded by a prior escalation stays on the
///      tier that last worked until the origin-tier probe clears it.
///   3. The hotel-computed execution route (`resolve_component_execution_route`)
///      — but for ladder-governed capabilities (`fallback_role ==
///      DEFAULT_TEXT_MODEL_ROLE`), ONLY when it reflects genuine explicit
///      routing intent: a remote/cross-hotel placement (`target_node` !=
///      local — e.g. cross-hotel park, an oracle remote pick), or an
///      operator/reflex `component_routes` pin (`route.explicit_pin`).
///      Non-ladder capabilities (voice synthesis, ...) always trust the
///      hotel route, as before.
///   4. An explicit per-session `component_routes` pin
///      (`SessionState.component_route_for_capability`) — an operator/reflex
///      binding to a specific implementation. More specific than a role-wide
///      default, so it outranks the ladder too. This does NOT include the
///      legacy `effective_model_controller` fallback (step 6 below).
///   5. The active role's configured fallback ladder primary
///      (`role_activation.turn_loop_config.fallback_tiers[0]`) — only consulted
///      for text-generation dispatch (`fallback_role == DEFAULT_TEXT_MODEL_ROLE`)
///      and only when neither of the above is set. This is what lets a
///      configured ladder (e.g. `["model.openrouter", "model.ollama"]`) govern
///      the *initial* dispatch, not just failure escalation.
///   6. The legacy `preferred_component_implementation` fallback (e.g.
///      `bindings.effective_model_controller` for text-generation, or the
///      agent-profile voice provider policy for voice capabilities) — a last
///      resort *below* the ladder for agents with no ladder configured.
///   7. `fallback_role` (`DEFAULT_TEXT_MODEL_ROLE` for text turns) — the very
///      last resort when nothing else is configured.
///
/// Routing drill 2026-07-09: steps 3 and 6 used to sit *above* the ladder
/// unconditionally. In production, aiua's `compose_component_route_assembly`
/// always populates a `text.generate` execution route (`declared_component_
/// capabilities` unconditionally includes it), defaulting to plain "model"
/// (gemini) via `default_component_role` when nothing is explicitly pinned —
/// so step 3 silently outranked every agent's ladder for the *primary*
/// dispatch, and a stale legacy `effective_model_controller` (step 6) did
/// too. Both are now demoted below the ladder unless they represent genuine
/// explicit routing intent, so a role's `fallback_tiers[0]` (Layer 1's
/// per-agent model binding surface) is actually authoritative.
fn resolve_model_execution_target(
    state: Option<&SessionState>,
    capability: &str,
    fallback_role: &str,
) -> (String, String, Option<String>) {
    // Operator pin (`/model <tier>`) wins over everything else for text
    // generation dispatch — it is the single choke point every initial model
    // dispatch funnels through, so slotting the pin in here (rather than at
    // each of the 8+ call sites) is enough to make it actually route, not
    // just gate fallback in advance_turn_to_next_fallback_tier.
    if matches!(capability, "text.generate" | "response.generate") {
        if let Some(pinned) = state.and_then(|state| state.pinned_tier_role.as_deref()) {
            return (local_node_id(), pinned.to_string(), None);
        }

        // Sticky fallback (Slice 2): a session degraded by a prior escalation
        // stays on the tier that last worked until the periodic origin-tier
        // probe (`turn_loop::probe_degraded_sessions`) clears the override —
        // otherwise every new turn would re-probe a known-bad primary at full
        // latency/failure cost. Beneath the operator pin, above everything
        // else (hotel route, component binding, ladder primary).
        if let Some(active) = state
            .and_then(|state| state.fallback_override.as_ref())
            .map(|ov| ov.active_tier_role.clone())
        {
            return (local_node_id(), active, None);
        }
    }

    if let Some(target) = pre_ladder_dispatch_target(state, capability, fallback_role) {
        return target;
    }

    // The text fallback ladder serves TEXT capabilities only. Media
    // capabilities (voice.transcribe, image.describe, media.analyze, …) must
    // fall through: ladder tiers are text controllers (openrouter/ollama/…)
    // with no media providers, and routing a voice note there yields
    // "no provider registered for voice.transcribe".
    if matches!(capability, "text.generate" | "response.generate") {
        if let Some(role) = ladder_primary_role(state, fallback_role) {
            return (local_node_id(), role, None);
        }
    }

    if let Some(implementation) =
        state.and_then(|state| state.preferred_component_implementation(capability))
    {
        return (
            local_node_id(),
            implementation_to_model_role(implementation),
            None,
        );
    }

    (local_node_id(), fallback_role.into(), None)
}

/// Steps 3–4 of [`resolve_model_execution_target`]'s precedence (operator
/// pin and sticky fallback override excluded — those are checked separately
/// by callers that need them). Returns `Some` when a pre-ladder step
/// resolved a target; `None` means the ladder (step 5, if configured) or the
/// legacy fallback (step 6) governs. Shared by `resolve_model_execution_target`
/// and `primary_dispatch_used_ladder` so the two precedence computations can
/// never drift out of sync (see the off-by-one defect their docs reference).
fn pre_ladder_dispatch_target(
    state: Option<&SessionState>,
    capability: &str,
    fallback_role: &str,
) -> Option<(String, String, Option<String>)> {
    let ladder_governed = fallback_role == DEFAULT_TEXT_MODEL_ROLE;

    if let Some(route) = state.and_then(|state| state.resolve_component_execution_route(capability))
    {
        if !ladder_governed || route.target_node != local_node_id() || route.explicit_pin {
            return Some((
                route.target_node.clone(),
                route.target_role.clone(),
                route.incarnation_id.clone(),
            ));
        }
        // Ladder-governed capability, local target, no explicit pin: this is
        // the hotel's implicit default (e.g. `default_component_role`) —
        // fall through so the ladder gets a chance to govern instead.
    }

    if let Some(role) = state
        .and_then(|state| state.component_route_for_capability(capability))
        .and_then(|route| route.implementation.as_deref())
        .map(implementation_to_model_role)
    {
        return Some((local_node_id(), role, None));
    }

    None
}

/// The active role's configured fallback ladder (`turn_loop_config.fallback_tiers`),
/// when the session has a role active with a non-empty custom ladder. `None`
/// when no role is active, the role has no `turn_loop_config`, or its ladder is
/// empty (the `DEFAULT_FALLBACK_TIERS` constant governs those cases elsewhere).
fn role_ladder_tiers(state: Option<&SessionState>) -> Option<&[String]> {
    state
        .and_then(|state| state.role_activation.as_ref())
        .and_then(|ra| ra.turn_loop_config.as_ref())
        .map(|tlc| tlc.fallback_tiers.as_slice())
        .filter(|tiers| !tiers.is_empty())
}

/// Resolves the per-agent model NAME bound to `target_role` (a provider
/// role such as `"model.openrouter"`) from the active role's
/// `turn_loop_config.model_bindings` (Layer 1 — see
/// `TurnLoopConfig::model_bindings`). `None` when no role is active, the
/// role has no `turn_loop_config`, or the role has no binding for this
/// specific provider role — callers then leave `ModelRequestPayload.model`
/// unset, so model-router falls back to the provider's own global default
/// (`openrouter_default_model`, etc). Called with whichever `target_role`
/// `resolve_model_execution_target` (or the fallback-tier advance walk)
/// resolved, so each fallback tier gets its own binding, not just the
/// primary.
fn role_model_binding(state: Option<&SessionState>, target_role: &str) -> Option<String> {
    state
        .and_then(|state| state.role_activation.as_ref())
        .and_then(|ra| ra.turn_loop_config.as_ref())
        .and_then(|tlc| tlc.model_bindings.get(target_role))
        .cloned()
}

/// `role_ladder_tiers(state)[0]`, gated to text-generation dispatch. The
/// ladder is a text-model routing construct (`"model"`, `"model.openrouter"`,
/// ...); non-text capabilities (voice synthesis, media analysis, ...) pass a
/// different `fallback_role` and never consult it.
fn ladder_primary_role(state: Option<&SessionState>, fallback_role: &str) -> Option<String> {
    if fallback_role != DEFAULT_TEXT_MODEL_ROLE {
        return None;
    }
    role_ladder_tiers(state).and_then(|tiers| tiers.first().cloned())
}

/// True when, given the current session state, the *primary* dispatch for
/// text generation would resolve to the role ladder's `tiers[0]` (per
/// [`resolve_model_execution_target`]'s precedence) rather than an explicit
/// hotel route or component binding. Lets
/// [`turn_loop::advance_turn_to_next_fallback_tier`] tell whether ladder tier
/// 0 was already attempted as the primary dispatch (skip it) or never
/// attempted (try it before advancing further) — see DEF: off-by-one on
/// single-tier ladders.
fn primary_dispatch_used_ladder(state: Option<&SessionState>, capability: &str) -> bool {
    // Media capabilities never dispatch via the ladder (see the gating in
    // resolve_model_execution_target) — keep the two computations in sync.
    if !matches!(capability, "text.generate" | "response.generate") {
        return false;
    }
    if pre_ladder_dispatch_target(state, capability, DEFAULT_TEXT_MODEL_ROLE).is_some() {
        return false;
    }
    role_ladder_tiers(state).is_some()
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
    /// Content-filtering posture for this role — `"unrestricted"` | `"standard"`
    /// | `"strict"`. Defaults to `"standard"` (current behavior) so caches built
    /// before this field existed, or roles that never set it, behave exactly as
    /// before.
    content_policy: String,
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
    /// Cached OpenRouter catalog snapshot for `/model` display: the set of
    /// model ids whose endpoints accept tool calls, plus the set of all known
    /// ids (absent-from-catalog models stay unannotated). Refreshed lazily
    /// when older than [`OPENROUTER_CATALOG_TTL`]; `None` until first fetch or
    /// when the last fetch failed (annotation is best-effort — dispatch-time
    /// tools handling lives in model-router, not here).
    openrouter_tools_catalog: Option<OpenRouterToolsCatalog>,
    /// Agent profile (identity_text, soul_text, etc.) fetched from hotel at startup.
    /// Applied to every new session so the correct persona is used from the first turn.
    default_agent_profile: AgentProfile,
    /// Projected upstream MCP tools (`mcp:<upstream>.<tool>`) this agent owns
    /// or is granted, cached from `GetMcpUpstreams`. Copied into each session's
    /// bindings so the tool assembly projects them (proposal mcp-client-fabric).
    mcp_upstream_tools: Vec<crate::session::McpUpstreamToolBinding>,
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
    /// Dedup + budget ledger for the LifeGraph auto-capture lane (Slice E2).
    /// Live-only (never checkpointed), mirroring the prefetch-dispatched flag.
    life_capture_ledger: LifeCaptureLedger,
    /// Correlation id of an in-flight origin-tier probe per session (Slice 2
    /// fallback-override auto-recovery — see `turn_loop::probe_degraded_sessions`).
    /// Bounds "at most one probe in flight per session": a session_id present
    /// here has an outstanding probe and is skipped by the next eligible tick.
    /// Live-only — never checkpointed. A probe lost across a restart is not a
    /// correctness issue: the next eligible tick simply sends a fresh one.
    pending_fallback_probes: HashMap<String, String>,
    /// Per-session sentence-pipelined TTS state for operator-chat voice turns.
    /// Live-only (never checkpointed): armed at turn start when the transport
    /// is `operator_chat` and the voice policy allows streaming, dropped when
    /// the turn completes/fails/evicts. See `voice_stream.rs`.
    voice_chunk_pipelines: HashMap<String, VoiceChunkPipeline>,
}

/// One model row from the hotel's compact catalog (config node
/// `model_catalog.openrouter`, written by aiua's model-catalog-sync job) or
/// from the direct-OpenRouter fallback fetch. Backs both the `/models`
/// drill-down and the `/model` tool badges.
#[derive(Debug, Clone)]
struct CatalogModelEntry {
    id: String,
    name: Option<String>,
    tools: Option<bool>,
    ctx: Option<u32>,
}

/// OpenRouter catalog snapshot. See the field docs on
/// `AgentRuntime::openrouter_tools_catalog`.
struct OpenRouterToolsCatalog {
    fetched_at: std::time::Instant,
    entries: Vec<CatalogModelEntry>,
}

impl OpenRouterToolsCatalog {
    /// `Some(bool)` when the catalog lists the model AND reports its tool
    /// capability; `None` for unknown models or unreported capability.
    fn supports_tools(&self, model_id: &str) -> Option<bool> {
        self.entries
            .iter()
            .find(|e| e.id == model_id)
            .and_then(|e| e.tools)
    }
}

/// Refresh cadence for the `/model` tool-capability annotation catalog.
const OPENROUTER_CATALOG_TTL: Duration = Duration::from_secs(600);
/// Buttons per drill-down page in `/models` (Telegram keyboards stay usable
/// around this size; more matches get a "refine the search" hint instead).
const MODELS_PAGE_SIZE: usize = 10;
/// Telegram rejects `callback_data` over 64 bytes — a model whose `/model
/// <id>` callback would exceed that is listed as text instead of a button.
const TELEGRAM_CALLBACK_LIMIT: usize = 64;

impl AgentRuntime {
    /// Live merged `/model` preset list: the hotel config key `model_presets`
    /// (JSON array of `{alias, label, tier, model, description}`) merged over
    /// the compiled-in defaults. Config edits apply on the next `/model` —
    /// no restart, no redeploy.
    async fn load_model_presets(&mut self) -> Vec<crate::commands::ResolvedModelPreset> {
        let config_json = match self
            .ipc_client
            .send_request_with_timeout(
                IpcRequest::GetConfig {
                    key: "model_presets".into(),
                },
                Duration::from_secs(5),
            )
            .await
        {
            Ok(IpcResponse::ConfigData { value_json, .. }) => value_json,
            _ => None,
        };
        crate::commands::merge_config_model_presets(config_json.as_deref())
    }

    /// Best-effort tool-capability lookup for `/model` display. `Some(bool)`
    /// when the catalog knows the model; `None` when the catalog is
    /// unreachable or the model isn't listed (annotation is skipped — the
    /// dispatch-time no-tools handling lives in model-router, not here).
    async fn openrouter_model_supports_tools(&mut self, model_id: &str) -> Option<bool> {
        self.ensure_openrouter_catalog().await;
        self.openrouter_tools_catalog
            .as_ref()
            .and_then(|c| c.supports_tools(model_id))
    }

    /// Refresh the cached catalog if stale. HOTEL-FIRST: reads the compact
    /// catalog aiua's model-catalog-sync job persists to the config node
    /// `model_catalog.openrouter` (one fetch per hotel, mesh-consistent);
    /// falls back to a direct OpenRouter fetch when the hotel hasn't run
    /// discovery yet. A failed refresh keeps the previous snapshot.
    async fn ensure_openrouter_catalog(&mut self) {
        let stale = self
            .openrouter_tools_catalog
            .as_ref()
            .map(|c| c.fetched_at.elapsed() > OPENROUTER_CATALOG_TTL)
            .unwrap_or(true);
        if !stale {
            return;
        }
        let fresh = match self.fetch_hotel_catalog().await {
            Some(entries) => Some(entries),
            None => self.fetch_openrouter_catalog_direct().await,
        };
        if let Some(entries) = fresh {
            self.openrouter_tools_catalog = Some(OpenRouterToolsCatalog {
                fetched_at: std::time::Instant::now(),
                entries,
            });
        }
    }

    /// Read the hotel's compact model catalog (config node
    /// `model_catalog.openrouter`): a JSON array of
    /// `{"id","name"?,"tools"?,"ctx"?,...}` objects.
    async fn fetch_hotel_catalog(&mut self) -> Option<Vec<CatalogModelEntry>> {
        let raw = match self
            .ipc_client
            .send_request_with_timeout(
                IpcRequest::GetConfig {
                    key: "model_catalog.openrouter".into(),
                },
                Duration::from_secs(5),
            )
            .await
        {
            Ok(IpcResponse::ConfigData {
                value_json: Some(v),
                ..
            }) => v,
            _ => return None,
        };
        let rows: Vec<serde_json::Value> = serde_json::from_str(&raw).ok()?;
        let entries: Vec<CatalogModelEntry> = rows
            .iter()
            .filter_map(|row| {
                Some(CatalogModelEntry {
                    id: row.get("id")?.as_str()?.to_string(),
                    name: row.get("name").and_then(|n| n.as_str()).map(str::to_string),
                    tools: row.get("tools").and_then(|t| t.as_bool()),
                    ctx: row.get("ctx").and_then(|c| c.as_u64()).map(|c| c as u32),
                })
            })
            .collect();
        if entries.is_empty() {
            None
        } else {
            Some(entries)
        }
    }

    /// Direct OpenRouter fallback for hotels that haven't run catalog
    /// discovery yet (public endpoint, no key).
    async fn fetch_openrouter_catalog_direct(&mut self) -> Option<Vec<CatalogModelEntry>> {
        let base = match self
            .ipc_client
            .send_request_with_timeout(
                IpcRequest::GetConfig {
                    key: "openrouter_base_url".into(),
                },
                Duration::from_secs(3),
            )
            .await
        {
            Ok(IpcResponse::ConfigData {
                value_json: Some(v),
                ..
            }) => v,
            _ => "https://openrouter.ai/api".to_string(),
        };
        let base = base
            .trim()
            .trim_matches('"')
            .trim_end_matches('/')
            .trim_end_matches("/v1")
            .trim_end_matches('/')
            .to_string();
        let url = format!("{base}/v1/models");
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(4))
            .build()
            .ok()?;
        let body: serde_json::Value = client.get(&url).send().await.ok()?.json().await.ok()?;
        let entries: Vec<CatalogModelEntry> = body
            .get("data")
            .and_then(|d| d.as_array())?
            .iter()
            .filter_map(|model| {
                let id = model.get("id").and_then(|i| i.as_str())?.to_string();
                let tools = model
                    .get("supported_parameters")
                    .and_then(|p| p.as_array())
                    .map(|params| {
                        params
                            .iter()
                            .filter_map(|p| p.as_str())
                            .any(|p| p == "tools")
                    });
                Some(CatalogModelEntry {
                    id,
                    name: model
                        .get("name")
                        .and_then(|n| n.as_str())
                        .map(str::to_string),
                    tools,
                    ctx: model
                        .get("context_length")
                        .and_then(|c| c.as_u64())
                        .map(|c| c as u32),
                })
            })
            .collect();
        if entries.is_empty() {
            None
        } else {
            Some(entries)
        }
    }

    /// Build the `/models` drill-down reply: bare → vendor buttons; with a
    /// query → matching-model buttons whose taps fire `/model <id>`.
    async fn build_models_browse_reply(
        &mut self,
        query: Option<&str>,
    ) -> (String, Option<serde_json::Value>) {
        self.ensure_openrouter_catalog().await;
        let Some(catalog) = self.openrouter_tools_catalog.as_ref() else {
            return (
                "Model catalog unavailable — the hotel's discovery job hasn't run yet and \
                 OpenRouter is unreachable. You can still bind directly: /model <vendor/model>."
                    .to_string(),
                None,
            );
        };

        let query = query.map(str::trim).filter(|q| !q.is_empty());
        match query {
            None => {
                // Vendor page: group by the id's vendor prefix, largest first.
                let mut counts: std::collections::BTreeMap<&str, usize> =
                    std::collections::BTreeMap::new();
                for entry in &catalog.entries {
                    let vendor = entry.id.split('/').next().unwrap_or(&entry.id);
                    *counts.entry(vendor).or_default() += 1;
                }
                let mut vendors: Vec<(&str, usize)> = counts.into_iter().collect();
                vendors.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
                let shown = vendors.len().min(24);
                let rows: Vec<Vec<serde_json::Value>> = vendors[..shown]
                    .chunks(3)
                    .map(|chunk| {
                        chunk
                            .iter()
                            .map(|(vendor, count)| {
                                serde_json::json!({
                                    "text": format!("{vendor} ({count})"),
                                    "callback_data": format!("/models {vendor}"),
                                })
                            })
                            .collect()
                    })
                    .collect();
                let text = format!(
                    "🗂 {} models from {} vendors. Tap a vendor to drill down, or search with \
                     /models <text>.",
                    catalog.entries.len(),
                    vendors.len(),
                );
                (text, Some(serde_json::json!({ "inline_keyboard": rows })))
            }
            Some(q) => {
                let lowered = q.to_lowercase();
                let matches: Vec<&CatalogModelEntry> = catalog
                    .entries
                    .iter()
                    .filter(|e| {
                        let vendor = e.id.split('/').next().unwrap_or("");
                        vendor.eq_ignore_ascii_case(&lowered)
                            || e.id.to_lowercase().contains(&lowered)
                            || e.name
                                .as_deref()
                                .map(|n| n.to_lowercase().contains(&lowered))
                                .unwrap_or(false)
                    })
                    .collect();
                if matches.is_empty() {
                    return (
                        format!(
                            "No catalog models match `{q}`. Try /models for the vendor list, or \
                             bind directly with /model <vendor/model>."
                        ),
                        None,
                    );
                }
                let total = matches.len();
                let mut rows: Vec<Vec<serde_json::Value>> = Vec::new();
                for entry in matches.iter().take(MODELS_PAGE_SIZE) {
                    let callback = format!("/model {}", entry.id);
                    if callback.len() > TELEGRAM_CALLBACK_LIMIT {
                        continue;
                    }
                    let badge = match entry.tools {
                        Some(true) => " 🔧",
                        Some(false) => " 💬",
                        None => "",
                    };
                    let ctx = entry
                        .ctx
                        .map(|c| format!(" · {}k", c / 1000))
                        .unwrap_or_default();
                    rows.push(vec![serde_json::json!({
                        "text": format!("{}{}{}", entry.id, badge, ctx),
                        "callback_data": callback,
                    })]);
                }
                let mut text = format!(
                    "🎯 {total} match(es) for `{q}` — tap to bind (🔧 tools · 💬 chat-only):"
                );
                if total > MODELS_PAGE_SIZE {
                    text.push_str(&format!(
                        "\nShowing {MODELS_PAGE_SIZE} — refine with /models <more specific>."
                    ));
                }
                let keyboard = if rows.is_empty() {
                    None
                } else {
                    Some(serde_json::json!({ "inline_keyboard": rows }))
                };
                (text, keyboard)
            }
        }
    }

    pub fn new(ipc_client: PhiloticClient, agent_id: impl Into<String>) -> Self {
        Self {
            ipc_client,
            agent_id: agent_id.into(),
            sessions: HashMap::new(),
            muninn_config: None,
            muninn_available: true,
            configured_roles: HashMap::new(),
            openrouter_tools_catalog: None,
            default_agent_profile: AgentProfile::default(),
            mcp_upstream_tools: Vec::new(),
            pending_drains: std::collections::VecDeque::new(),
            stuck_turn_first_seen: HashMap::new(),
            stuck_turn_signature: HashMap::new(),
            total_active_since: HashMap::new(),
            role_switch_history: HashMap::new(),
            network_offline: false,
            role_name: None,
            life_capture_ledger: LifeCaptureLedger::default(),
            pending_fallback_probes: HashMap::new(),
            voice_chunk_pipelines: HashMap::new(),
        }
    }

    pub fn set_role_name(&mut self, rn: impl Into<String>) {
        self.role_name = Some(rn.into());
    }

    /// This philote's own registered guest identity — the value stamped as
    /// `reply_guest_id` on every model/tool/life dispatch so the hotel delivers
    /// the response back to the subscription owned by THIS runtime (the one
    /// holding the active turn).
    ///
    /// For a role-incarnation philote (a separate process running as
    /// `{agent_id}:{role_name}`, e.g. a whisper specialist) this is its own
    /// incarnation id, so the reply returns to THIS process — which subscribes
    /// to role `agent` under that incarnation guest id. For the base philote it
    /// is the bare `agent_id`, matching its own registration. Without stamping
    /// it, `ReturnRoute::from_task` falls back to the bare `agent_id` and a role
    /// specialist's reply is addressed to the base agent — the wrong process —
    /// hanging the specialist's turn (DEF-051).
    pub(super) fn own_guest_id(&self) -> String {
        compose_guest_identity(&self.agent_id, self.role_name.as_deref())
    }

    /// The concrete guest identity THIS runtime registered with the hotel —
    /// `model_reply_guest_id()` for role incarnations, the bare `agent_id`
    /// for base philotes. Used by the tool / life.* dispatch paths (DEF-051
    /// part 2), which need an explicit return guest rather than the
    /// Option-and-infer contract the model dispatch uses.
    pub(crate) fn own_guest_id(&self) -> String {
        self.model_reply_guest_id()
            .unwrap_or_else(|| self.agent_id.clone())
    }

    /// Fetch this agent's identity bundle from the hotel and store it as the default profile.
    /// Applied to every new session so the correct persona is used from the first message.
    async fn fetch_agent_profile(&mut self) {
        let key = format!("__agent_bundle__:{}", self.agent_id);
        match self
            .ipc_client
            .send_request_with_timeout(IpcRequest::GetConfig { key }, Duration::from_secs(5))
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
            Ok(_) => {
                warn!("Unexpected response to agent bundle fetch — using default profile.");
            }
            Err(_) => {
                warn!(agent_id = %self.agent_id, "Agent bundle fetch timed out — using default profile.");
            }
        }

        // Fetch hotel-level user profile and inject into agent profile when the
        // agent-specific profile doesn't already override the field.
        if let Some(hotel_name) = local_hotel_name() {
            match self
                .ipc_client
                .send_request_with_timeout(
                    IpcRequest::GetUserProfile {
                        hotel_name: hotel_name.clone(),
                    },
                    Duration::from_secs(5),
                )
                .await
            {
                Ok(IpcResponse::UserProfileData(p)) => {
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
                Ok(_) => {
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
        if let Ok(IpcResponse::ConfigData {
            value_json: Some(ref json),
            ..
        }) = self
            .ipc_client
            .send_request_with_timeout(
                IpcRequest::GetConfig {
                    key: "config:voice_response_policy".into(),
                },
                Duration::from_secs(5),
            )
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
            .send_request_with_timeout(
                IpcRequest::GetConfig {
                    key: "config:media_routing_policy".into(),
                },
                Duration::from_secs(5),
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
        let result = self
            .ipc_client
            .send_request_with_timeout(
                IpcRequest::ListRoleIncarnations {
                    agent_id: self.agent_id.clone(),
                },
                std::time::Duration::from_secs(5),
            )
            .await;
        match result {
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
            Err(_) => {
                warn!(agent_id = %self.agent_id, "fetch_role_names timed out (startup race) — continuing with empty roster");
            }
            _ => {
                info!(agent_id = %self.agent_id, "No role incarnations found for delegation roster.");
            }
        }
    }

    /// Fetch operator-stored MCP route overrides (key `__mcp_routes__:<agent_id>`).
    ///
    /// These represent an explicit operator provisioning decision, so they publish
    /// regardless of the `mcp_auto_publish` gate.
    async fn operator_mcp_route_overrides(
        &mut self,
    ) -> Option<Vec<ansible_mesh_core::mcp_route::McpRouteRecord>> {
        use ansible_mesh_core::mcp_route::McpRouteRecord;

        let key = format!("__mcp_routes__:{}", self.agent_id);
        let mcp_override = self
            .ipc_client
            .send_request_with_timeout(
                IpcRequest::GetConfig { key: key.clone() },
                std::time::Duration::from_secs(5),
            )
            .await
            .ok();
        if let Some(IpcResponse::ConfigData {
            value_json: Some(json),
            ..
        }) = mcp_override
        {
            match serde_json::from_str::<Vec<McpRouteRecord>>(&json) {
                Ok(r) if !r.is_empty() => {
                    info!(agent_id = %self.agent_id, count = r.len(), "Using operator-stored MCP route overrides.");
                    return Some(r);
                }
                Ok(_) => {}
                Err(e) => {
                    warn!(key = %key, err = %e, "Ignoring malformed operator MCP route override.")
                }
            }
        }
        None
    }

    /// Build `McpRouteRecord`s from the agent's effective toolset.
    ///
    /// Every tool in `default_toolset` that has a real catalog entry is projected
    /// as a self-targeting `Philote` route. Tools with no catalog entry are
    /// skipped — they are model-internal only. All derived routes are published
    /// with `require_approval` forced on: startup publication is a convenience,
    /// not an authorization event.
    fn derived_mcp_routes(&self) -> Vec<ansible_mesh_core::mcp_route::McpRouteRecord> {
        use ansible_mesh_core::mcp_route::{
            McpAuthScheme, McpRouteRecord, McpRouteSecurity, McpRouteTarget,
        };

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
                        require_approval: true,
                    },
                    updated_at: now,
                })
            })
            .collect()
    }

    /// At startup, publish this philote's MCP routes to the hotel.
    ///
    /// Publication policy (MCP membrane hardening, S2):
    /// - Operator-stored overrides always publish — they are an explicit decision.
    /// - Toolset-derived routes publish only when the agent profile opts in via
    ///   `mcp_auto_publish`; they carry `require_approval = true`.
    /// - With the gate off, any previously auto-published route set is revoked
    ///   so stale publications from earlier releases do not stay live.
    async fn register_mcp_routes(&mut self) {
        if let Some(routes) = self.operator_mcp_route_overrides().await {
            self.push_mcp_routes(routes).await;
            return;
        }

        let derived = self.derived_mcp_routes();
        if !self.default_agent_profile.mcp_auto_publish {
            if !derived.is_empty() {
                warn!(
                    agent_id = %self.agent_id,
                    suppressed = derived.len(),
                    "MCP startup auto-publication is opt-in and OFF for this agent — \
                     publishing nothing and revoking any previously auto-published routes. \
                     Set `mcp_auto_publish: true` on the agent profile to re-enable."
                );
            }
            // Fail-closed cleanup: remove any route set a previous release
            // auto-published for this agent.
            let result = self
                .ipc_client
                .send_request_with_timeout(
                    IpcRequest::RevokeMcpRoutes {
                        agent_id: self.agent_id.clone(),
                    },
                    std::time::Duration::from_secs(5),
                )
                .await;
            if let Err(e) = result {
                if is_ipc_timeout(&e) {
                    warn!(agent_id = %self.agent_id, "MCP route revoke timed out (startup race) — continuing");
                } else {
                    warn!(agent_id = %self.agent_id, err = %e, "Failed to revoke stale MCP routes");
                }
            }
            return;
        }

        if derived.is_empty() {
            info!(agent_id = %self.agent_id, "No MCP routes to register (empty default_toolset or no catalog matches).");
            return;
        }
        self.push_mcp_routes(derived).await;
    }

    async fn push_mcp_routes(&mut self, routes: Vec<ansible_mesh_core::mcp_route::McpRouteRecord>) {
        let count = routes.len();
        let result = self
            .ipc_client
            .send_request_with_timeout(
                IpcRequest::UpdateMcpRoutes {
                    agent_id: self.agent_id.clone(),
                    routes,
                    vault_ref: None,
                },
                std::time::Duration::from_secs(5),
            )
            .await;
        match result {
            Ok(_) => {
                info!(agent_id = %self.agent_id, count, "MCP routes registered with hotel.")
            }
            Err(e) if is_ipc_timeout(&e) => {
                warn!(agent_id = %self.agent_id, "MCP route registration timed out (startup race) — continuing")
            }
            Err(e) => {
                warn!(agent_id = %self.agent_id, err = %e, "Failed to register MCP routes")
            }
        }
    }

    /// At startup, enumerate all session apartments for this agent and purge any
    /// stale active turns left over from an unclean shutdown. Cleans the DB so
    /// sessions are not blocked before the first inbound message arrives.
    async fn sweep_stale_session_turns(&mut self) {
        let list_key = format!("__session_apartments__:{}", self.agent_id);
        let memory_types: Vec<String> = match self
            .ipc_client
            .send_request_with_timeout(
                IpcRequest::GetConfig { key: list_key },
                std::time::Duration::from_secs(5),
            )
            .await
        {
            Ok(IpcResponse::ConfigData {
                value_json: Some(json),
                ..
            }) => serde_json::from_str::<Vec<String>>(&json).unwrap_or_default(),
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
            let checkpoint = match self
                .ipc_client
                .send_request_with_timeout(
                    IpcRequest::GetConfig { key: snapshot_key },
                    std::time::Duration::from_secs(5),
                )
                .await
            {
                Ok(IpcResponse::ConfigData {
                    value_json: Some(json),
                    ..
                }) => match serde_json::from_str::<serde_json::Value>(&json) {
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
        // LifeGraph auto-recall lane: prime the cache once per session load so
        // the first turn can already inject graph context (fire-and-forget).
        self.dispatch_life_recall_prefetch_once(&session_id, &content)
            .await;

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

        // Plan-continuation hygiene: a synthesized continuation is only valid
        // while its carryover still exists (it may have been cleared via
        // `/plan drop`) and while no new plan proposal is parked awaiting the
        // operator — a pending proposal is a redirect and wins over the old plan.
        if task.action.as_deref() == Some("plan_continuation") {
            let valid = self
                .sessions
                .get(&session_id)
                .map(|s| s.carryover_plan.is_some() && !s.has_parked_plan_turn())
                .unwrap_or(false);
            if !valid {
                info!(
                    session_id = %session_id,
                    "Dropping orphaned plan continuation (carryover cleared or a new plan is parked)."
                );
                return Ok(());
            }
        }

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
                SlashCommand::Model { .. } => {}
                SlashCommand::ModelPreset { .. } => {}
                SlashCommand::Models { .. } => {}
                SlashCommand::Dirty | SlashCommand::Sfw => {}
                SlashCommand::Abandon { .. } => {}
                SlashCommand::Plan { drop } => {
                    // Resolved without starting a turn so it works mid-turn too.
                    return self
                        .handle_plan_command(
                            task_id,
                            session_id,
                            turn_id,
                            chat_id,
                            final_reply_to,
                            final_reply_role,
                            final_reply_guest_id,
                            drop,
                        )
                        .await;
                }
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

        if let Some(life_observe) = direct_life_observe_command_for_task(&task, &content) {
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
                let response_contract = Some(cognitive_response_contract(&[
                    "spoken_text",
                    "memory_candidate",
                    "active_plan",
                ]));
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
                let mut model_req = ModelRequestPayload {
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
                    model: None,
                    provider_options: resolve_content_policy_provider_options(
                        self.sessions.get(&session_id),
                    ),
                    chat_id: restored_chat_id,
                    reply_to: local_node_id(),
                    reply_role: "agent".into(),
                    reply_guest_id: Some(self.own_guest_id()),
                    final_reply_to: restored_reply_to,
                    final_reply_role: restored_reply_role,
                    final_reply_guest_id: restored_reply_guest_id,
                    agent_id: Some(self.agent_id.clone()),
                    oracle_pick: None,
                    oracle_agreement: None,
                };
                let (target_node, target_role, target_guest_id) = resolve_model_execution_target(
                    self.sessions.get(&session_id),
                    "text.generate",
                    DEFAULT_TEXT_MODEL_ROLE,
                );
                model_req.model = role_model_binding(self.sessions.get(&session_id), &target_role);
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

            // Plan continuation turns are seeded with the carried-over plan
            // (completed steps marked done) so the model sees exactly what is
            // left, and enter pre-confirmed so the plan gate does not re-park.
            let is_plan_continuation = task.action.as_deref() == Some("plan_continuation");
            let (seeded_plan, seeded_plan_confirmed) = if is_plan_continuation {
                match state.carryover_plan.as_ref() {
                    Some(carry) => {
                        let mut plan = carry.plan.clone();
                        for (i, step) in plan.steps.iter_mut().enumerate() {
                            if carry.steps_done.get(i).copied().unwrap_or(false) {
                                step.status = "done".into();
                            }
                        }
                        if plan.status == "planning" {
                            plan.status = "executing".into();
                        }
                        (Some(plan), true)
                    }
                    None => (None, false),
                }
            } else {
                (None, false)
            };

            // Cron-triggered turns get CronPrimary (the honest marker is
            // `cron_job_id`, not corr_id/source string-sniffing); an operator
            // pin on the session takes precedence over the configured default.
            // A session with an active fallback override (Slice 2) is dispatched
            // to `active_tier_role` at the `resolve_model_execution_target`
            // choke point, so the turn is tagged AutoFallback to match — it is
            // not the session's configured default, even though the operator
            // never explicitly pinned it.
            // Operator-authored cron jobs carry a narrow standing tool
            // preapproval (`cron_preapproved_tools` — aiua forwards it only
            // for jobs with created_by=Operator, see
            // CronTicker::build_cron_task_json). Seed it into this session's
            // approval policy so the unattended fire can execute the tools
            // its instruction names instead of parking WaitingApproval with
            // no operator awake and riding the watchdog to eviction.
            if task.cron_job_id.is_some() {
                for tool in &task.cron_preapproved_tools {
                    if !state.approval_policy.preapproved_tools.contains(tool) {
                        state.approval_policy.preapproved_tools.push(tool.clone());
                    }
                }
            }

            let selection_source = if task.cron_job_id.is_some() {
                SelectionSource::CronPrimary
            } else if state.pinned_tier_role.is_some() {
                SelectionSource::OperatorExplicit
            } else if state.fallback_override.is_some() {
                SelectionSource::AutoFallback
            } else {
                SelectionSource::ConfiguredDefault
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
                active_plan: seeded_plan,
                consecutive_step_failures: 0,
                streak_extension: 0,
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
                plan_confirmed: seeded_plan_confirmed,
                plan_confirm_note: None,
                fallback_tier: if self.network_offline { 1 } else { 0 },
                ladder_tier0_dispatched: false,
                streaming_retry_attempts: 0,
                streamed_content: String::new(),
                paracrine_hop_count: 0,
                paracrine_chain_started_at: None,
                selection_source,
            });
            state.set_active_turn_phase(TurnPhase::LoadingContext);

            // Sentence-pipelined streaming TTS (operator_chat only): arm the
            // per-turn voice chunk pipeline so streamed tokens are synthesized
            // per sentence while generation continues. Any pipeline left over
            // from a previous turn in this session is stale either way.
            if streaming_voice_eligible(
                task.transport.as_deref(),
                task.source.as_deref(),
                &state.agent_profile.voice_response_policy,
                had_voice_input,
            ) {
                // NOTE: direct field insert (not a helper method) — `state`
                // still mutably borrows `self.sessions` in this scope, so only
                // a disjoint field access compiles here.
                self.voice_chunk_pipelines
                    .insert(session_id.clone(), VoiceChunkPipeline::new(turn_id.clone()));
            } else {
                self.voice_chunk_pipelines.remove(&session_id);
            }

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
        // LifeGraph auto-recall lane: inject cached graph context beside the
        // Muninn lane. Cache-only — never blocks the turn on the runner.
        self.maybe_inject_life_graph_context(&session_id).await;

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
            // First live producer of ReflexEvent::ContextPressure (reflex.rs:309/460
            // has handled it, unfired, since the reflex engine was introduced).
            // context_pressure_pct_from_projection is a pure wrapper around this
            // exact field read, unit-tested below so a rename/removal fails a
            // plain test instead of only manifesting as a silent no-op here.
            if let Some(used_pct) = context_pressure_pct_from_projection(&context_projection) {
                state.fire_reflex_event(ReflexEvent::ContextPressure { used_pct });
            }
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
                // Plan always returns from the pre-turn gate above; unreachable here.
                SlashCommand::Plan { .. } => Ok(()),
                SlashCommand::Tts { .. }
                | SlashCommand::Voice { .. }
                | SlashCommand::Model { .. }
                | SlashCommand::ModelPreset { .. }
                | SlashCommand::Models { .. } => {
                    self.handle_session_control_command(
                        task_id, session_id, turn_id, chat_id, command,
                    )
                    .await
                }
                SlashCommand::Role { .. } | SlashCommand::Roles | SlashCommand::Back => {
                    self.handle_role_command(task_id, session_id, turn_id, chat_id, command)
                        .await
                }
                SlashCommand::Dirty | SlashCommand::Sfw => {
                    self.handle_dirty_command(task_id, session_id, turn_id, chat_id, command)
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
        let (response_contract, mut provider_options) = voice_delivery_envelope(
            self.sessions.get(&session_id),
            Some(cognitive_response_contract(&[
                "spoken_text",
                "memory_candidate",
                "active_plan",
                "memory_concept",
            ])),
        );
        // Thread the turn's effective content-filtering posture into the same
        // generic provider_options bag voice settings already use — this is the
        // primary turn-dispatch path (handles most user messages), so it must
        // not be skipped even though it shares an envelope with voice/transform
        // capabilities that don't need it.
        if matches!(capability.as_str(), "text.generate" | "response.generate") {
            provider_options.extend(resolve_content_policy_provider_options(
                self.sessions.get(&session_id),
            ));
        }
        let (target_node, target_role, target_guest_id) = {
            let (node, role, guest_id) = if capability == "voice.transcribe"
                && media_policy
                    .transcription_provider
                    .as_deref()
                    .map(|p| !p.trim().is_empty())
                    .unwrap_or(false)
            {
                // The agent's media policy names an STT provider — dispatch
                // straight to that provider's controller role (e.g.
                // "elevenlabs" → model.elevenlabs / Scribe). Transcription
                // must never ride the text fallback ladder: its tiers are
                // text controllers with no media providers.
                let provider = media_policy
                    .transcription_provider
                    .as_deref()
                    .unwrap()
                    .trim()
                    .to_ascii_lowercase();
                let role = if provider == "gemini" {
                    // Gemini's controller registers the plain "model" role.
                    DEFAULT_TEXT_MODEL_ROLE.to_string()
                } else {
                    format!("model.{provider}")
                };
                (local_node_id(), role, None)
            } else {
                resolve_model_execution_target(
                    self.sessions.get(&session_id),
                    &capability,
                    DEFAULT_TEXT_MODEL_ROLE,
                )
            };
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
        let mut model_req = ModelRequestPayload {
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
            model: role_model_binding(self.sessions.get(&session_id), &target_role),
            provider_options,
            chat_id,
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            reply_guest_id: Some(self.own_guest_id()),
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
            agent_id: Some(self.agent_id.clone()),
            oracle_pick: None,
            oracle_agreement: None,
        };

        // Shadow-mode (PHILOTIC_SHADOW_ORACLE, default OFF): log-only oracle-vs-
        // ladder annotation on the FIRST-TURN dispatch (Model Oracle Primary
        // Authority, slice 2). Capability-guarded so it fires ONLY for the
        // cognitive text-generation case — the same dispatch class slice 1
        // instruments in `turn_loop` — and NEVER for the aux/transform tasks
        // this site also multiplexes (voice.transcribe, media.analyze, ...).
        // `shadow_oracle_pick` is itself flag-gated (returns before any
        // ipc_client access when off), so aux tasks stay completely untouched
        // and the cognitive OFF path costs nothing beyond this string compare.
        // Never alters the dispatch target (target_node/role/guest_id unchanged).
        if shadow_eligible_capability(&capability) {
            let (shadow_pick, shadow_agreement) = self.shadow_oracle_pick(&target_role).await;
            model_req.oracle_pick = shadow_pick;
            model_req.oracle_agreement = shadow_agreement;
        }

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
            // A new plan proposal is a redirect: it replaces any plan the
            // plan-eval-repeat loop was still carrying over.
            if state.carryover_plan.take().is_some() {
                info!(
                    session_id = %session_id,
                    "New plan proposal replaces the existing plan carryover."
                );
            }
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

        self.ipc_client
            .send_request_with_timeout(
                IpcRequest::EmitTask {
                    target_node: final_reply_to,
                    target_role: final_reply_role,
                    target_guest_id: final_reply_guest_id,
                    task_json: serde_json::to_string(&payload)?,
                },
                Duration::from_secs(10),
            )
            .await
            .context("emit_turn_status: ipc ack failed or timed out after 10s")?;

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

        let response_contract = Some(cognitive_response_contract(&[
            "spoken_text",
            "memory_candidate",
            "active_plan",
            "memory_concept",
        ]));
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
        let mut model_req = ModelRequestPayload {
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
            model: None,
            provider_options: resolve_content_policy_provider_options(
                self.sessions.get(&session_id),
            ),
            chat_id: reentry.chat_id,
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            reply_guest_id: Some(self.own_guest_id()),
            final_reply_to: reentry.final_reply_to,
            final_reply_role: reentry.final_reply_role,
            final_reply_guest_id: reentry.final_reply_guest_id,
            agent_id: Some(self.agent_id.clone()),
            oracle_pick: None,
            oracle_agreement: None,
        };

        // Bound once so the routing resolution below and the shadow-mode
        // capability guard can never drift apart.
        let capability = "text.generate";
        let (target_node, target_role, target_guest_id) = resolve_model_execution_target(
            self.sessions.get(&session_id),
            capability,
            DEFAULT_TEXT_MODEL_ROLE,
        );
        model_req.model = role_model_binding(self.sessions.get(&session_id), &target_role);

        // Shadow-mode (PHILOTIC_SHADOW_ORACLE, default OFF): log-only oracle-vs-
        // ladder annotation on the TRANSCRIPTION-REENTRY dispatch (Model Oracle
        // Primary Authority, slice 3).
        //
        // This is the cognitive leg of a voice-originated turn. The preceding
        // `voice.transcribe` leg was dispatched by `handle_user_message`, whose
        // slice-2 hook excludes it via the same `shadow_eligible_capability`
        // guard; this function then emits its OWN `EmitTask` rather than
        // re-entering `handle_user_message` or the two instrumented `turn_loop`
        // sites — so without this hook a healthy voice turn logged ZERO shadow
        // rows for the leg that actually rides the text fallback ladder.
        //
        // Unlike the slice-2 site this dispatch is unconditionally cognitive
        // (`request_class` is hardcoded "cognitive"), so the guard is trivially
        // true here; it is applied for uniformity and to stay correct if this
        // site ever starts multiplexing aux capabilities. `shadow_oracle_pick`
        // is itself flag-gated — its first statement returns (None, None)
        // before any ipc_client access — so the OFF path performs no oracle IPC.
        // Never alters the dispatch target (target_node/role/guest_id unchanged).
        if shadow_eligible_capability(capability) {
            let (shadow_pick, shadow_agreement) = self.shadow_oracle_pick(&target_role).await;
            model_req.oracle_pick = shadow_pick;
            model_req.oracle_agreement = shadow_agreement;
        }

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
            match state.active_turn.as_ref() {
                Some(active_turn) => (
                    active_turn.turn_id.clone(),
                    active_turn.chat_id.clone(),
                    active_turn.final_reply_to.clone(),
                    active_turn.final_reply_role.clone(),
                    active_turn.final_reply_guest_id.clone(),
                ),
                // No active turn (e.g. a fallback-recovery notice fired from the
                // origin-tier probe in `handle_fallback_probe_response`, which by
                // construction only runs on idle sessions — see
                // `probe_degraded_sessions`'s eligibility filter). Fall back to
                // the session's persisted transport target and a chat_id inferred
                // from the session_id encoding, mirroring the idle-session
                // routing `handle_paracrine_response` already uses to reach a
                // session with no in-flight turn.
                None => {
                    let Some(chat_id) = chat_id_from_session_id(session_id, &state.source) else {
                        return Ok(());
                    };
                    let target =
                        state.resolved_transport_reply_target(local_node_id(), "membrane", None);
                    (
                        String::new(),
                        chat_id,
                        target.target_node,
                        target.target_role,
                        target.target_guest_id,
                    )
                }
            }
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

        let response_contract = Some(cognitive_response_contract(&[
            "spoken_text",
            "memory_candidate",
            "active_plan",
        ]));
        let response_route = Some(model_response_route(
            self.sessions.get(&session_id),
            response_contract.as_ref(),
            &Map::new(),
            &Vec::new(),
        ));
        let ligand = planning_ligand(self.sessions.get(&session_id), &prompt, &tools);
        let affordances = model_affordances(self.sessions.get(&session_id), &user_content, &tools);
        let mut model_req = ModelRequestPayload {
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
            model: None,
            provider_options: resolve_content_policy_provider_options(
                self.sessions.get(&session_id),
            ),
            chat_id,
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            reply_guest_id: Some(self.own_guest_id()),
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
            agent_id: Some(self.agent_id.clone()),
            oracle_pick: None,
            oracle_agreement: None,
        };

        let (target_node, target_role, target_guest_id) = resolve_model_execution_target(
            self.sessions.get(&session_id),
            "text.generate",
            DEFAULT_TEXT_MODEL_ROLE,
        );
        model_req.model = role_model_binding(self.sessions.get(&session_id), &target_role);

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

        // Sentence-pipelined TTS: feed the voice chunk pipeline (no-op unless
        // one is armed for this turn). Synthesis trouble never fails the
        // streaming path — text delivery always wins.
        if let Err(err) = self.ingest_streaming_tokens_for_voice(&session_id).await {
            warn!(
                session_id = %session_id,
                "voice chunk pipeline ingest failed (non-fatal): {err}"
            );
        }

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
        // LifeGraph auto-recall prefetch responses carry a sentinel turn id and
        // are pure cache refreshes — they must never be routed as tool results.
        if task.turn_id.as_deref() == Some(LIFE_AUTORECALL_PREFETCH_TURN_ID) {
            self.handle_life_recall_prefetch_response(&task);
            return Ok(());
        }

        // LifeGraph auto-capture acks carry their own sentinel turn id and are
        // pure observability — they must never be routed as tool results.
        if task.turn_id.as_deref() == Some(LIFE_AUTOCAPTURE_TURN_ID) {
            self.handle_life_autocapture_response(&task);
            return Ok(());
        }

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
        let mut model_req = ModelRequestPayload {
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
            model: None,
            provider_options: resolve_content_policy_provider_options(
                self.sessions.get(&session_id),
            ),
            chat_id,
            reply_to: local_node_id(),
            reply_role: "agent".into(),
            reply_guest_id: Some(self.own_guest_id()),
            final_reply_to,
            final_reply_role,
            final_reply_guest_id,
            agent_id: Some(self.agent_id.clone()),
            oracle_pick: None,
            oracle_agreement: None,
        };

        let (target_node, target_role, target_guest_id) = resolve_model_execution_target(
            self.sessions.get(&session_id),
            "text.generate",
            DEFAULT_TEXT_MODEL_ROLE,
        );
        model_req.model = role_model_binding(self.sessions.get(&session_id), &target_role);

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

        // Handle /model — pins this session to a model tier role, or with no
        // argument reports (and clears) the current pin. A pin tags new turns
        // OperatorExplicit (see handle_user_message), which disables automatic
        // fallback escalation in advance_turn_to_next_fallback_tier, and is
        // routed at the resolve_model_execution_target choke point.
        // Handle /models — catalog drill-down. Bare: vendor buttons; with a
        // query: matching-model buttons that fire `/model <id>` on tap
        // (membrane passes inline-button callback_data through as a slash
        // command, same path as the /roles keyboard).
        if let SlashCommand::Models { ref query } = command {
            let (reply, keyboard) = self.build_models_browse_reply(query.as_deref()).await;
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
                .complete_local_command_with_markup(session_id, command_turn_id, reply, keyboard)
                .await;
        }

        if let SlashCommand::ModelPreset { ref alias } = command {
            // Resolve against the LIVE merged preset list (hotel config over
            // built-ins) — or any direct `vendor/model` OpenRouter slug — so
            // the swappable model set is operator-editable without a redeploy.
            let presets = self.load_model_presets().await;
            let reply = match crate::commands::resolve_model_preset(alias, &presets) {
                None => format!(
                    "Unknown model preset `{alias}`. Send /model to see the presets, \
                     use any OpenRouter id directly (e.g. /model sao10k/l3.1-euryale-70b), \
                     or /model model.<tier> to pin a session tier."
                ),
                Some(preset) => {
                    // Read the active role + its current turn-loop config so we
                    // change ONLY the model routing and preserve everything else
                    // (toolset, manifest, content policy). This keeps the
                    // ConfigureRole a "model-selection-only" change, which the
                    // hotel gate lets any role (orchestrator or not) apply to
                    // its own record (see role_materialization.rs).
                    let (role_name, mut tlc, toolset_profile) = {
                        match self.sessions.get(&session_id) {
                            Some(state) => {
                                let ra = state.role_activation.as_ref();
                                let role_name = ra
                                    .map(|r| r.role_name.clone())
                                    .unwrap_or_else(|| "orchestrator".to_string());
                                let tlc = ra
                                    .and_then(|r| r.turn_loop_config.clone())
                                    .or_else(|| {
                                        self.configured_roles
                                            .get(&role_name)
                                            .map(|c| c.turn_loop_config.clone())
                                    })
                                    .unwrap_or_default();
                                let toolset_profile = ra
                                    .and_then(|r| r.toolset_profile_ref.clone())
                                    .or_else(|| {
                                        self.configured_roles
                                            .get(&role_name)
                                            .map(|c| c.toolset_profile.clone())
                                    })
                                    .unwrap_or_else(|| "default".to_string());
                                (role_name, tlc, toolset_profile)
                            }
                            None => (
                                "orchestrator".to_string(),
                                ansible_mesh_core::graph::TurnLoopConfig::default(),
                                "default".to_string(),
                            ),
                        }
                    };

                    // Make the preset's provider tier primary, then bind the
                    // concrete model name to that tier (Layer 1 model_bindings).
                    let tier = preset.tier_role.clone();
                    let mut tiers: Vec<String> = tlc.fallback_tiers.clone();
                    tiers.retain(|t| t != &tier);
                    tiers.insert(0, tier.clone());
                    if let Some(model_id) = preset.model_id.as_ref() {
                        tlc.model_bindings.insert(tier.clone(), model_id.clone());
                    }
                    tlc.fallback_tiers = tiers.clone();

                    let req = IpcRequest::ConfigureRole {
                        agent_id: self.agent_id.clone(),
                        role_name: role_name.clone(),
                        guest_id: format!("{}:{}", self.agent_id, role_name),
                        calling_role: role_name.clone(),
                        toolset_profile,
                        role_identity_addendum: None,
                        role_manifest: None,
                        is_admin: false,
                        inactive_ttl_seconds: None,
                        iteration_cap: None,
                        approval_policy: None,
                        model_profile: None,
                        context_window_policy: None,
                        fallback_tiers: Some(tiers),
                        model_bindings: Some(tlc.model_bindings.clone()),
                        content_policy: None,
                    };

                    match self.ipc_client.send_request(req).await {
                        Ok(IpcResponse::ConfigureRoleOk { .. }) => {
                            // Live: update the active session's role config so the
                            // very next turn routes through the new model — no
                            // restart, mirroring role.configure's cache update.
                            if let Some(state) = self.sessions.get_mut(&session_id) {
                                if let Some(ra) = state.role_activation.as_mut() {
                                    ra.turn_loop_config = Some(tlc.clone());
                                }
                            }
                            if let Some(cached) = self.configured_roles.get_mut(&role_name) {
                                cached.turn_loop_config = tlc;
                            }
                            format!("✅ Switched to {}. (live — no restart)", preset.label)
                        }
                        Ok(IpcResponse::Standard {
                            ok: false, message, ..
                        }) => {
                            warn!("/model preset swap denied: {message}");
                            format!("Couldn't switch to {}: {message}", preset.label)
                        }
                        Ok(other) => {
                            warn!("/model preset swap unexpected response: {other:?}");
                            format!(
                                "Couldn't switch to {} — unexpected hotel response.",
                                preset.label
                            )
                        }
                        Err(e) => {
                            warn!("/model preset swap failed: {e}");
                            format!("Couldn't switch to {}: {e}", preset.label)
                        }
                    }
                }
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

        if let SlashCommand::Model { ref tier } = command {
            // Bare `/model` lists the live merged presets with best-effort
            // tool-capability notes. Both need `&mut self` (IPC + catalog
            // fetch), so gather them BEFORE borrowing the session state.
            let (presets, tool_notes) = if tier.is_none() {
                let presets = self.load_model_presets().await;
                let mut notes: HashMap<String, bool> = HashMap::new();
                for preset in &presets {
                    if preset.tier_role != "model.openrouter" {
                        continue;
                    }
                    let Some(model_id) = preset.model_id.clone() else {
                        continue;
                    };
                    if let Some(supports) = self.openrouter_model_supports_tools(&model_id).await {
                        notes.insert(preset.alias.clone(), supports);
                    }
                }
                (presets, notes)
            } else {
                (Vec::new(), HashMap::new())
            };
            // One-tap swap keyboard for the bare listing: preset buttons fire
            // `/model <alias>`, plus a drill-down into the full catalog.
            let preset_keyboard: Option<serde_json::Value> = if tier.is_none() {
                let mut rows: Vec<Vec<serde_json::Value>> = presets
                    .chunks(3)
                    .map(|chunk| {
                        chunk
                            .iter()
                            .map(|p| {
                                let badge = match tool_notes.get(&p.alias) {
                                    Some(true) => " 🔧",
                                    Some(false) => " 💬",
                                    None => "",
                                };
                                serde_json::json!({
                                    "text": format!("{}{}", p.alias, badge),
                                    "callback_data": format!("/model {}", p.alias),
                                })
                            })
                            .collect()
                    })
                    .collect();
                rows.push(vec![serde_json::json!({
                    "text": "📚 Browse all models",
                    "callback_data": "/models",
                })]);
                Some(serde_json::json!({ "inline_keyboard": rows }))
            } else {
                None
            };
            let reply = if let Some(state) = self.sessions.get_mut(&session_id) {
                // Any `/model` invocation (pin or clear) resets the persisted
                // fallback override (Slice 2) — the operator is taking explicit
                // control of tier selection, so a stale auto-fallback record
                // from a prior escalation must not linger underneath it.
                state.fallback_override = None;
                match tier.as_deref() {
                    Some(t) => {
                        state.pinned_tier_role = Some(t.to_string());
                        format!(
                            "Pinned this session to model tier `{t}`. Fallback escalation is disabled until you clear the pin with a bare `/model`."
                        )
                    }
                    None => {
                        // Bare `/model`: clear any session tier pin and show the
                        // current model plus the swappable presets.
                        let cleared = state.pinned_tier_role.take();
                        let current = state
                            .role_activation
                            .as_ref()
                            .and_then(|ra| ra.turn_loop_config.as_ref())
                            .and_then(|tlc| {
                                tlc.fallback_tiers.first().map(|primary| {
                                    tlc.model_bindings
                                        .get(primary)
                                        .cloned()
                                        .unwrap_or_else(|| primary.clone())
                                })
                            });
                        let current_label = current
                            .as_deref()
                            .map(|id| {
                                presets
                                    .iter()
                                    .find(|p| p.model_id.as_deref() == Some(id))
                                    .map(|p| p.label.clone())
                                    .unwrap_or_else(|| id.to_string())
                            })
                            .unwrap_or_else(|| "provider default".to_string());
                        let mut list = String::new();
                        if let Some(prev) = cleared {
                            list.push_str(&format!("Cleared session tier pin (was `{prev}`).\n"));
                        }
                        list.push_str(&format!(
                            "🧠 Current: {current_label}\nSwap with /model <name>:"
                        ));
                        for p in &presets {
                            let note = match tool_notes.get(&p.alias) {
                                Some(true) => " 🔧",
                                Some(false) => " 💬 chat-only (no tools)",
                                None => "",
                            };
                            list.push_str(&format!("\n • {} — {}{}", p.alias, p.description, note));
                        }
                        list.push_str(
                            "\n • <vendor/model> — any OpenRouter id \
                             (e.g. /model sao10k/l3.1-euryale-70b)",
                        );
                        list
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
                .complete_local_command_with_markup(
                    session_id,
                    command_turn_id,
                    reply,
                    preset_keyboard,
                )
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
                | SlashCommand::Model { .. }
                | SlashCommand::ModelPreset { .. }
                | SlashCommand::Models { .. }
                | SlashCommand::Dirty
                | SlashCommand::Sfw
                | SlashCommand::Role { .. }
                | SlashCommand::Roles
                | SlashCommand::Back
                | SlashCommand::Approve { .. }
                | SlashCommand::Deny { .. }
                | SlashCommand::ApprovalClear { .. }
                | SlashCommand::Abandon { .. }
                | SlashCommand::Correct { .. }
                | SlashCommand::Plan { .. } => (
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

    /// `/plan` — show plan/carryover status; `/plan drop` — clear the carryover.
    /// Resolved without starting a turn so the command works while a turn (or a
    /// synthesized plan continuation) is in flight.
    #[allow(clippy::too_many_arguments)]
    async fn handle_plan_command(
        &mut self,
        task_id: Uuid,
        session_id: String,
        turn_id: String,
        chat_id: String,
        reply_to: String,
        reply_role: String,
        reply_guest_id: Option<String>,
        drop: bool,
    ) -> Result<()> {
        let (reply, dropped) = {
            let Some(state) = self.sessions.get_mut(&session_id) else {
                warn!("handle_plan_command: unknown session {}", session_id);
                return Ok(());
            };
            if drop {
                if state.carryover_plan.take().is_some() {
                    (
                        "Plan carryover dropped — no further auto-continuations.".to_string(),
                        true,
                    )
                } else {
                    ("No plan carryover to drop.".to_string(), false)
                }
            } else {
                (state.plan_status_text(), false)
            }
        };
        if dropped {
            // Persist immediately so the drop survives a restart.
            self.persist_session_checkpoint(&session_id).await?;
        }
        self.complete_command_without_turn(
            task_id,
            session_id,
            turn_id,
            chat_id,
            reply_to,
            reply_role,
            reply_guest_id,
            reply,
            None,
            None,
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
    /// Refresh the projected upstream MCP tool cache from the hotel and apply
    /// it to every live session (proposal mcp-client-fabric). Sessions whose
    /// projection changed get their tool assembly rebuilt, so revoked
    /// upstreams disappear and newly reported catalogs appear.
    pub(crate) async fn refresh_mcp_upstream_projection(&mut self) {
        let entries = match self
            .ipc_client
            .send_request_with_timeout(IpcRequest::GetMcpUpstreams {}, Duration::from_secs(5))
            .await
        {
            Ok(IpcResponse::McpUpstreamsState { mcp_upstreams }) => mcp_upstreams,
            Ok(_) => return,
            Err(e) => {
                warn!("refresh_mcp_upstream_projection: GetMcpUpstreams failed: {e}");
                return;
            }
        };

        let mut projected = Vec::new();
        for entry in entries {
            let cfg = entry.config;
            let granted = cfg.owner_agent_id == self.agent_id
                || cfg.grant_agents.iter().any(|a| a == &self.agent_id);
            if !granted {
                continue;
            }
            let Some(catalog) = entry.catalog else {
                continue;
            };
            for tool in catalog.tools {
                projected.push(crate::session::McpUpstreamToolBinding {
                    upstream_id: cfg.upstream_id.clone(),
                    remote_name: tool.remote_name,
                    description: tool.description,
                    input_schema: tool.input_schema,
                });
            }
        }

        self.mcp_upstream_tools = projected;
        let cache = self.mcp_upstream_tools.clone();
        for state in self.sessions.values_mut() {
            if state.bindings.mcp_upstream_tools != cache {
                state.bindings.mcp_upstream_tools = cache.clone();
                state.rebuild_default_tool_assembly();
            }
        }
    }

    /// Copy the cached upstream projection into one session's bindings if it
    /// drifted (e.g. a session restored from a pre-projection checkpoint).
    fn apply_mcp_upstream_projection(&mut self, session_id: &str) {
        let cache = self.mcp_upstream_tools.clone();
        if let Some(state) = self.sessions.get_mut(session_id) {
            if state.bindings.mcp_upstream_tools != cache {
                state.bindings.mcp_upstream_tools = cache;
                state.rebuild_default_tool_assembly();
            }
        }
    }

    async fn refresh_bindings_from_snapshot(&mut self, session_id: &str) {
        let response = self
            .ipc_client
            .send_request_with_timeout(
                IpcRequest::GetConfig {
                    key: format!("__session_snapshot__:{session_id}"),
                },
                Duration::from_secs(10),
            )
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
            self.apply_mcp_upstream_projection(session_id);
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
        let response = match self
            .ipc_client
            .send_request_with_timeout(
                IpcRequest::GetConfig { key: snapshot_key },
                Duration::from_secs(15),
            )
            .await
        {
            Ok(r) => r,
            Err(e) if is_ipc_timeout(&e) => {
                warn!(
                    session_id = %session_id,
                    "ensure_session_loaded: GetConfig timed out after 15s — starting fresh session"
                );
                IpcResponse::ConfigData {
                    key: String::new(),
                    value_json: None,
                }
            }
            Err(e) => return Err(e),
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
                        if let Some(tlc) = state
                            .role_activation
                            .as_ref()
                            .and_then(|ra| ra.turn_loop_config.clone())
                        {
                            state.settings.execution.apply_paracrine_overrides(&tlc);
                            // Load-time re-derivation: apply the restored role's
                            // context-window overrides directly (no baseline
                            // snapshot — this is base derivation, not a return).
                            if let Some(ov) = tlc.context_window.as_ref() {
                                state.settings.context_window.apply_overrides(ov);
                            }
                        }

                        self.sessions.insert(session_id.to_string(), state);
                        self.apply_mcp_upstream_projection(session_id);
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
                if let Some(tlc) = activation.turn_loop_config.as_ref() {
                    state.settings.execution.apply_paracrine_overrides(tlc);
                }
                // Fresh-session default-role activation is base derivation: apply
                // the default role's context-window overrides directly, without a
                // baseline snapshot, so they define the session's own default.
                if let Some(ov) = activation
                    .turn_loop_config
                    .as_ref()
                    .and_then(|c| c.context_window.as_ref())
                {
                    state.settings.context_window.apply_overrides(ov);
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
        self.apply_mcp_upstream_projection(session_id);
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
            .send_request_with_timeout(
                IpcRequest::ListRules {
                    agent_id: agent_id.to_string(),
                },
                Duration::from_secs(5),
            )
            .await
        {
            Ok(IpcResponse::RuleList { rules }) => {
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

    /// Best-effort push of a turn-level failure event into the hotel's
    /// self-heal queue (turn-failure heal intake). Used for failures only the
    /// agent loop can see: watchdog evictions (`stuck_turn_evicted:{phase}`),
    /// fallback-ladder exhaustion (`fallback_exhausted:{last_provider}`), and
    /// paracrine budget breaches (`paracrine_budget_exhausted`).
    ///
    /// Never fails the caller — heal intake is diagnostics, not control flow.
    /// An older hotel that does not know `PushHealEvent` answers with an
    /// error (or drops the frame); both are swallowed here.
    pub(crate) async fn push_heal_event(&mut self, pattern_tag: &str, detail: &str) {
        let request = IpcRequest::PushHealEvent {
            guest_id: self.agent_id.clone(),
            severity: "medium".into(),
            pattern_tag: pattern_tag.to_string(),
            detail: detail.to_string(),
        };
        if let Err(e) = self.ipc_client.send_request(request).await {
            warn!(
                pattern_tag = %pattern_tag,
                "heal event push failed (best-effort): {e}"
            );
        }
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

    // L0 execution safety floor: compiled-in, non-configurable, evaluated
    // before any approval/session state is consulted and before the shell
    // is ever spawned. No policy record or `auto_approve_all` can reach
    // this — see crates/exec-guard for the blocklist and rationale.
    if let Some(hardline) = exec_guard::detect_hardline(&command) {
        return Ok(serde_json::json!({
            "stdout": "",
            "stderr": hardline.denial_message(),
            "exit_code": 126,
            "success": false,
        }));
    }

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
        AgentRuntime, CachedRoleConfig, DEFAULT_TEXT_MODEL_ROLE, LOCAL_NODE,
        MAX_ORACLE_EXTRA_TIERS, NoResponseAction, NoResponseClass, ProviderErrorClass,
        STREAK_CAP_CEILING, classify_provider_error, cognitive_response_contract,
        context_pressure_pct_from_projection, decide_no_response_action, effective_iteration_cap,
        extract_model_error, extract_model_error_payload, format_role_command_reply,
        format_roles_report, loop_stop_fallback_reply, loop_stop_reason,
        media_analysis_attachments, next_ladder_tier, normalized_user_content,
        parse_memory_candidate, pick_oracle_role, primary_dispatch_used_ladder, provider_for_role,
        resolve_media_routing, resolve_model_execution_target, role_model_binding,
        shadow_eligible_capability, should_attempt_provider_repair, tool_step_earns_streak,
    };
    use crate::commands::SlashCommand;
    use crate::r#loop::{ApprovalRequest, PlanProposalAction, ToolCall, ToolResult, TurnPhase};
    use crate::protocol::{
        FinalReplyPayload, InboundTaskPayload, ModelRequestPayload, TransportAttachment,
    };
    use crate::reflex::ReflexEvent;
    use crate::session::{
        ActivePlan, ApprovalPolicy, CarryoverPlan, ComponentExecutionRoute, ComponentRouteAssembly,
        ComponentRouteBinding, FallbackOverride, PlanStep, ResponseRouteMode, RoleActivation,
        SelectionSource, SessionState, WorkingTurn,
    };
    use philotic_client::{TaskErrorPayload, UserProfileDataPayload};
    use uuid::Uuid;

    // ── Routing-oracle helpers ────────────────────────────────────────────

    #[test]
    fn provider_for_role_inverts_hotel_seeding() {
        assert_eq!(provider_for_role("model").as_deref(), Some("gemini"));
        assert_eq!(provider_for_role("model.local").as_deref(), Some("onnx"));
        assert_eq!(
            provider_for_role("model.anthropic").as_deref(),
            Some("anthropic")
        );
        assert_eq!(
            provider_for_role("model.openrouter").as_deref(),
            Some("openrouter")
        );
        assert_eq!(provider_for_role("tool"), None);
    }

    #[test]
    fn pick_oracle_role_skips_roles_already_in_the_ladder() {
        let data = serde_json::json!({
            "ranked": [
                { "role": "model", "provider": "gemini", "score": 0.9 },
                { "role": "model.anthropic", "provider": "anthropic", "score": 0.8 },
                { "role": "model.openrouter", "provider": "openrouter", "score": 0.7 }
            ]
        });
        let tried: std::collections::HashSet<String> =
            ["model".to_string(), "model.local".to_string()].into();
        let (role, provider) = pick_oracle_role(&data, &tried).expect("pick");
        assert_eq!(role, "model.anthropic");
        assert_eq!(provider, "anthropic");
    }

    #[test]
    fn pick_oracle_role_none_when_all_options_tried_or_empty() {
        let tried: std::collections::HashSet<String> = ["model.anthropic".to_string()].into();
        let data = serde_json::json!({
            "ranked": [ { "role": "model.anthropic", "provider": "anthropic", "score": 0.8 } ]
        });
        assert!(pick_oracle_role(&data, &tried).is_none());
        assert!(pick_oracle_role(&serde_json::json!({ "ranked": [] }), &tried).is_none());
        assert!(pick_oracle_role(&serde_json::json!({}), &tried).is_none());
    }

    #[test]
    fn pick_oracle_role_tolerates_malformed_entries() {
        // Entries missing role are skipped, not fatal; missing provider
        // degrades to "unknown".
        let data = serde_json::json!({
            "ranked": [
                { "provider": "gemini", "score": 0.9 },
                { "role": "model.openai", "score": 0.7 }
            ]
        });
        let tried = std::collections::HashSet::new();
        let (role, provider) = pick_oracle_role(&data, &tried).expect("pick");
        assert_eq!(role, "model.openai");
        assert_eq!(provider, "unknown");
    }

    #[test]
    fn shadow_eligible_capability_fires_only_on_cognitive_text() {
        // (a) The cognitive text-generation dispatch — the same class slice 1
        // instruments in turn_loop — is shadow-eligible. Firing here makes a
        // first-turn cognitive dispatch stamp oracle_pick/agreement onto the
        // outgoing task (recorded downstream by the model-router trace store,
        // proven by from_task_threads_agent_id_and_shadow_into_recorded_trace).
        assert!(shadow_eligible_capability("text.generate"));
        assert!(shadow_eligible_capability("response.generate"));
    }

    #[test]
    fn shadow_eligible_capability_excludes_aux_transform_tasks() {
        // (b) The aux/transform tasks the first-turn dispatch also multiplexes
        // must NEVER be compared against the oracle — they don't ride the text
        // fallback ladder. These are the exact capability strings produced by
        // resolve_media_routing / action_to_capability at this site.
        for aux in [
            "voice.transcribe",
            "media.analyze",
            "image.describe",
            "document.summarize",
            "voice.synthesize",
        ] {
            assert!(
                !shadow_eligible_capability(aux),
                "aux capability {aux} must be excluded from shadow-oracle logging"
            );
        }
    }

    #[test]
    fn oracle_extra_tier_budget_bounds_reroutes() {
        // The consult gate: current_tier >= max_tier + MAX_ORACLE_EXTRA_TIERS
        // stops further oracle dispatches. With a 1-tier ladder (max_tier 0)
        // and the default budget of 2, tiers 0 and 1 may consult; tier 2 may
        // not.
        let max_tier: u8 = 0;
        assert!(0 < max_tier.saturating_add(MAX_ORACLE_EXTRA_TIERS));
        assert!(1 < max_tier.saturating_add(MAX_ORACLE_EXTRA_TIERS));
        assert!(2 >= max_tier.saturating_add(MAX_ORACLE_EXTRA_TIERS));
    }

    #[test]
    fn no_response_table_matches_pre_slice_behavior() {
        // Pre-slice: both the provider-failure path and the WaitingModel watchdog
        // funnel through advance_turn_to_next_fallback_tier, which escalates while
        // a tier remains and fails (evicts) only when the last tier is exhausted —
        // identical for both failure classes.
        for class in [
            NoResponseClass::ProviderFailure,
            NoResponseClass::WatchdogTimeout,
        ] {
            assert_eq!(
                decide_no_response_action(class, true),
                NoResponseAction::EscalateTier,
                "{class:?} with a tier remaining must escalate"
            );
            assert_eq!(
                decide_no_response_action(class, false),
                NoResponseAction::EvictTurn,
                "{class:?} with no tier remaining must evict"
            );
        }
    }

    #[test]
    fn cognitive_response_contract_carries_memory_candidate_policy() {
        let contract = cognitive_response_contract(&["spoken_text", "memory_candidate"]);

        assert_eq!(
            contract["channels"],
            serde_json::json!(["spoken_text", "memory_candidate"])
        );
        let policy = contract["memory_candidate_policy"]
            .as_str()
            .expect("memory candidate policy should be present");
        assert!(policy.contains("durable future-useful context"));
        assert!(policy.contains("readiness/status chatter"));
        assert!(policy.contains("24-700 characters"));
    }

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
            streak_extension: 0,
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
            ladder_tier0_dispatched: false,
            streaming_retry_attempts: 0,
            streamed_content: String::new(),
            paracrine_hop_count: 0,
            paracrine_chain_started_at: None,
            selection_source: SelectionSource::default(),
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
    fn tool_step_earns_streak_only_for_novel_successful_work() {
        let mut turn = test_working_turn(TurnPhase::WaitingModel);
        let call = |name: &str, args: serde_json::Value| ToolCall {
            tool_name: name.into(),
            arguments: args,
        };
        let ok = |name: &str| ToolResult {
            tool_name: name.into(),
            content: "ok".into(),
        };
        let observe_a = call("life.observe", serde_json::json!({"observation_id": "a"}));

        // Novel successful work earns.
        assert!(tool_step_earns_streak(
            &turn,
            &observe_a,
            &ok("life.observe"),
            false
        ));
        // A failed step never earns.
        assert!(!tool_step_earns_streak(
            &turn,
            &observe_a,
            &ok("life.observe"),
            true
        ));
        // Diagnostic/status tools never earn — they are the loop signature.
        assert!(!tool_step_earns_streak(
            &turn,
            &call("hotel.status", serde_json::json!({})),
            &ok("hotel.status"),
            false
        ));
        // Skipped duplicates never earn.
        let dup = ToolResult {
            tool_name: "life.observe".into(),
            content: "[Duplicate call skipped] identical call already ran".into(),
        };
        assert!(!tool_step_earns_streak(&turn, &observe_a, &dup, false));

        // An identical repeat of a prior call never earns; new arguments do.
        turn.working_tool_history
            .push((observe_a.clone(), ok("life.observe")));
        assert!(!tool_step_earns_streak(
            &turn,
            &observe_a,
            &ok("life.observe"),
            false
        ));
        assert!(tool_step_earns_streak(
            &turn,
            &call("life.observe", serde_json::json!({"observation_id": "b"})),
            &ok("life.observe"),
            false
        ));
    }

    #[test]
    fn effective_iteration_cap_adds_streak_and_respects_ceiling() {
        let mut turn = test_working_turn(TurnPhase::WaitingModel);
        assert_eq!(effective_iteration_cap(10, &turn), 10);
        turn.streak_extension = 5;
        assert_eq!(effective_iteration_cap(10, &turn), 15);
        turn.streak_extension = 100;
        assert_eq!(effective_iteration_cap(10, &turn), STREAK_CAP_CEILING);
        // A configured cap already at the ceiling is honoured unchanged —
        // the extension only adds headroom below it, never shrinks.
        assert_eq!(effective_iteration_cap(50, &turn), 50);
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
            model: None,
            provider_options: serde_json::Map::new(),
            chat_id: "123".into(),
            reply_to: LOCAL_NODE.into(),
            reply_role: "agent".into(),
            reply_guest_id: None,
            final_reply_to: LOCAL_NODE.into(),
            final_reply_role: "membrane".into(),
            final_reply_guest_id: None,
            agent_id: Some("jane".into()),
            oracle_pick: None,
            oracle_agreement: None,
        };

        let json = serde_json::to_value(&request).expect("serialize request");
        assert_eq!(json["reply_role"], "agent");
        assert_eq!(json["agent_id"], "jane");
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
    fn context_pressure_pct_from_projection_reads_the_field_runtime_depends_on() {
        // Pins the exact JSON field name + clamping behavior that
        // handle_user_message's turn-assembly call site depends on to fire
        // the live ReflexEvent::ContextPressure producer. If the field is
        // renamed or removed on the session-side producer, this fails
        // instead of the runtime call silently becoming a permanent no-op.
        let present = serde_json::json!({"context_pressure_pct": 87});
        assert_eq!(context_pressure_pct_from_projection(&present), Some(87));

        let missing = serde_json::json!({"other_field": 1});
        assert_eq!(context_pressure_pct_from_projection(&missing), None);

        let wrong_type = serde_json::json!({"context_pressure_pct": "87"});
        assert_eq!(context_pressure_pct_from_projection(&wrong_type), None);

        // Assembly already clamps to 100 (session/mod.rs), but the runtime-side
        // read clamps too, defense in depth against a future producer regression.
        let over_100 = serde_json::json!({"context_pressure_pct": 150});
        assert_eq!(context_pressure_pct_from_projection(&over_100), Some(100));
    }

    #[test]
    fn context_pressure_projection_to_emit_path_fires_reflex_through_the_runtime_extractor() {
        // Integration-style coverage of the exact path handle_user_message
        // drives at its turn-assembly call site: build a real
        // ContextProjection through SessionState, serialize it the same way
        // model_request_payloads does, extract used_pct with the same
        // runtime.rs helper the live call site uses, then fire the reflex
        // event and assert the known downstream effect. This fails if either
        // the field name drifts or the runtime extraction/emit wiring is
        // removed — not just a data-structure assertion.
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state
            .agent_profile
            .media_routing_policy
            .strip_tools_on_media = false;
        state.settings.injection_budget.total_envelope_chars = 1;

        let (_, _, context_projection) = state.model_request_payloads("hello", &[]);
        let used_pct = context_pressure_pct_from_projection(&context_projection)
            .expect("runtime.rs call site expects this field to be present");
        assert_eq!(used_pct, 100);

        state.fire_reflex_event(ReflexEvent::ContextPressure { used_pct });
        assert!(
            state
                .agent_profile
                .media_routing_policy
                .strip_tools_on_media,
            "the runtime.rs extractor -> fire_reflex_event path should trip the reflex.rs:460 media-strip handler exactly like the live call site does"
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

    /// Implementation-mapping matrix (defect 3): openrouter (and other
    /// non-enumerated providers) must get their own dedicated role instead of
    /// collapsing onto the gemini default — mirrors the hotel-side
    /// `component_implementation_to_role` (ipc.rs).
    #[test]
    fn implementation_mapping_matrix_covers_openrouter_and_passthrough() {
        assert_eq!(
            super::implementation_to_model_role("openrouter"),
            "model.openrouter"
        );
        assert_eq!(
            super::implementation_to_model_role("openrouter/anthropic/claude-3"),
            "model.openrouter"
        );
        // Already role-shaped values pass through unchanged (case-insensitively).
        assert_eq!(
            super::implementation_to_model_role("model.openrouter"),
            "model.openrouter"
        );
        assert_eq!(
            super::implementation_to_model_role("MODEL.OpenRouter"),
            "model.openrouter"
        );
        // Any other known provider name also gets a dedicated role.
        assert_eq!(
            super::implementation_to_model_role("openai"),
            "model.openai"
        );
        assert_eq!(
            super::implementation_to_model_role("anthropic"),
            "model.anthropic"
        );
        // gemini still maps to the plain default role.
        assert_eq!(super::implementation_to_model_role("gemini"), "model");
    }

    // ── Ladder-primary precedence matrix (defect 1) ──────────────────────────

    fn role_activation_with_ladder(tiers: &[&str]) -> RoleActivation {
        RoleActivation {
            role_name: "researcher".into(),
            activation_reason: "test".into(),
            turn_loop_config: Some(ansible_mesh_core::graph::TurnLoopConfig {
                fallback_tiers: tiers.iter().map(|t| t.to_string()).collect(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// Explicit hotel execution route always wins, even with a ladder
    /// configured.
    #[test]
    fn ladder_precedence_hotel_route_wins_over_ladder() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.role_activation = Some(role_activation_with_ladder(&["model.openrouter"]));
        state.component_route_assembly = ComponentRouteAssembly {
            execution_routes: std::collections::BTreeMap::from([(
                "text.generate".into(),
                ComponentExecutionRoute {
                    target_node: "aria-node".into(),
                    target_role: "model".into(),
                    incarnation_id: None,
                    hotel_id: Some("aria-architect-hotel".into()),
                    environment_id: None,
                    execution_mode: "capability".into(),
                    availability_state: "live".into(),
                    selection_reason: Some("remote_latency_capacity".into()),
                    target_capability: None,
                    explicit_pin: false,
                },
            )]),
        };

        let target =
            resolve_model_execution_target(Some(&state), "text.generate", DEFAULT_TEXT_MODEL_ROLE);
        assert_eq!(target.0, "aria-node");
        assert_eq!(target.1, "model");
    }

    /// Reversed by the routing drill 2026-07-09 fix: `effective_model_controller`
    /// is a legacy fallback (see its `[legacy]` label in
    /// `component_route_summary`), not a genuine per-session pin. A
    /// configured role ladder is the intended per-agent primary-model lever
    /// and must now win over a stale `effective_model_controller`, not sit
    /// below it — this was proven live on mbp-jane (Jane's
    /// `fallback_tiers[0]=model.openrouter` was silently losing to a stale
    /// controller binding). Previously this test asserted the opposite
    /// ("model.elevenlabs" wins); see `ladder_precedence_ladder_wins_over_hotel_default_route`
    /// and `ladder_precedence_explicit_pin_hotel_route_still_wins_locally`
    /// below for the full precedence matrix this change preserves.
    #[test]
    fn ladder_precedence_ladder_beats_legacy_effective_model_controller() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.role_activation = Some(role_activation_with_ladder(&["model.openrouter"]));
        state.bindings.effective_model_controller = Some("elevenlabs".into());

        let target =
            resolve_model_execution_target(Some(&state), "text.generate", DEFAULT_TEXT_MODEL_ROLE);
        assert_eq!(target.0, LOCAL_NODE.to_string());
        assert_eq!(
            target.1, "model.openrouter",
            "a configured ladder must beat the legacy effective_model_controller fallback"
        );
    }

    /// A role with NO configured ladder still honors `effective_model_controller`
    /// as a legacy fallback — this mechanism isn't removed, only demoted
    /// below the ladder when one is configured.
    #[test]
    fn ladder_precedence_legacy_effective_model_controller_still_applies_without_ladder() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.bindings.effective_model_controller = Some("elevenlabs".into());

        let target =
            resolve_model_execution_target(Some(&state), "text.generate", DEFAULT_TEXT_MODEL_ROLE);
        assert_eq!(target.0, LOCAL_NODE.to_string());
        assert_eq!(target.1, "model.elevenlabs");
    }

    /// Regression (routing drill 2026-07-09): production always populates a
    /// hotel-computed `text.generate` execution route (aiua's
    /// `compose_component_route_assembly` — `declared_component_capabilities`
    /// unconditionally includes "text.generate", and `select_component_route`
    /// always returns a route, defaulting to "model"/gemini when nothing is
    /// explicitly pinned). Before this fix, that implicit-default route —
    /// `selection_reason: "live_local_fallback"`, `explicit_pin: false` —
    /// silently outranked every agent's ladder for the *primary* dispatch,
    /// making `fallback_tiers[0]` dead in production even though the bare
    /// `ladder_precedence_tiers_zero_used_when_set` fixture (no hotel route
    /// at all) passed.
    #[test]
    fn ladder_precedence_ladder_wins_over_hotel_default_route() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.role_activation = Some(role_activation_with_ladder(&["model.openrouter"]));
        state.component_route_assembly = ComponentRouteAssembly {
            execution_routes: std::collections::BTreeMap::from([(
                "text.generate".into(),
                ComponentExecutionRoute {
                    target_node: LOCAL_NODE.into(),
                    target_role: "model".into(),
                    incarnation_id: None,
                    hotel_id: Some(LOCAL_NODE.into()),
                    environment_id: None,
                    execution_mode: "capability".into(),
                    availability_state: "live".into(),
                    selection_reason: Some("live_local_fallback".into()),
                    target_capability: None,
                    explicit_pin: false,
                },
            )]),
        };

        let target =
            resolve_model_execution_target(Some(&state), "text.generate", DEFAULT_TEXT_MODEL_ROLE);
        assert_eq!(
            target.1, "model.openrouter",
            "the role's ladder must govern the primary dispatch when the hotel \
             route is only its implicit local default, not an explicit pin"
        );
    }

    /// A genuine explicit `component_routes` pin (`explicit_pin: true` —
    /// operator/reflex-set, not the hotel's implicit default) still wins
    /// over the ladder even when it resolves to a local live guest.
    #[test]
    fn ladder_precedence_explicit_pin_hotel_route_still_wins_locally() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.role_activation = Some(role_activation_with_ladder(&["model.openrouter"]));
        state.component_route_assembly = ComponentRouteAssembly {
            execution_routes: std::collections::BTreeMap::from([(
                "text.generate".into(),
                ComponentExecutionRoute {
                    target_node: LOCAL_NODE.into(),
                    target_role: "model.elevenlabs".into(),
                    incarnation_id: None,
                    hotel_id: Some(LOCAL_NODE.into()),
                    environment_id: None,
                    execution_mode: "capability".into(),
                    availability_state: "live".into(),
                    selection_reason: Some("live_local_capability".into()),
                    target_capability: None,
                    explicit_pin: true,
                },
            )]),
        };

        let target =
            resolve_model_execution_target(Some(&state), "text.generate", DEFAULT_TEXT_MODEL_ROLE);
        assert_eq!(target.1, "model.elevenlabs");
    }

    /// With no hotel route and no explicit binding, the role ladder's
    /// `tiers[0]` governs the *primary* dispatch.
    #[test]
    fn ladder_precedence_tiers_zero_used_when_set() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.role_activation = Some(role_activation_with_ladder(&[
            "model.openrouter",
            "model.ollama",
        ]));

        let target =
            resolve_model_execution_target(Some(&state), "text.generate", DEFAULT_TEXT_MODEL_ROLE);
        assert_eq!(target.1, "model.openrouter");

        assert!(primary_dispatch_used_ladder(Some(&state), "text.generate"));
    }

    /// With no hotel route, no explicit binding, and no configured ladder,
    /// the plain default text-model role is used.
    #[test]
    fn ladder_precedence_default_when_nothing_configured() {
        let state = SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());

        let target =
            resolve_model_execution_target(Some(&state), "text.generate", DEFAULT_TEXT_MODEL_ROLE);
        assert_eq!(target.1, DEFAULT_TEXT_MODEL_ROLE);
        assert!(!primary_dispatch_used_ladder(Some(&state), "text.generate"));
    }

    // ── Layer 1: per-agent model NAME binding ─────────────────────────────

    fn role_activation_with_ladder_and_bindings(
        tiers: &[&str],
        bindings: &[(&str, &str)],
    ) -> RoleActivation {
        RoleActivation {
            role_name: "researcher".into(),
            activation_reason: "test".into(),
            turn_loop_config: Some(ansible_mesh_core::graph::TurnLoopConfig {
                fallback_tiers: tiers.iter().map(|t| t.to_string()).collect(),
                model_bindings: bindings
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    /// `role_model_binding` resolves independently per provider role — the
    /// core requirement that per-agent model selection covers both the
    /// primary dispatch AND every fallback tier, not just tier 0.
    #[test]
    fn role_model_binding_resolves_per_provider_role() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.role_activation = Some(role_activation_with_ladder_and_bindings(
            &["model.openrouter", "model"],
            &[
                ("model.openrouter", "z-ai/glm-5.2"),
                ("model", "gemini-flash-latest"),
            ],
        ));

        assert_eq!(
            role_model_binding(Some(&state), "model.openrouter").as_deref(),
            Some("z-ai/glm-5.2")
        );
        assert_eq!(
            role_model_binding(Some(&state), "model").as_deref(),
            Some("gemini-flash-latest")
        );
        // No binding configured for this role — falls through to None so the
        // caller leaves ModelRequestPayload.model unset (provider global default).
        assert_eq!(role_model_binding(Some(&state), "model.ollama"), None);
    }

    #[test]
    fn role_model_binding_none_without_role_or_bindings() {
        assert_eq!(role_model_binding(None, "model.openrouter"), None);

        let state = SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        assert_eq!(role_model_binding(Some(&state), "model.openrouter"), None);
    }

    /// End-to-end wiring: `resolve_model_execution_target` resolves the
    /// provider ROLE, and `role_model_binding` resolves the model NAME for
    /// that same role — the two compose correctly for the primary dispatch.
    #[test]
    fn role_model_binding_composes_with_ladder_primary_dispatch() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.role_activation = Some(role_activation_with_ladder_and_bindings(
            &["model.openrouter"],
            &[("model.openrouter", "z-ai/glm-5.2")],
        ));

        let target =
            resolve_model_execution_target(Some(&state), "text.generate", DEFAULT_TEXT_MODEL_ROLE);
        assert_eq!(target.1, "model.openrouter");
        assert_eq!(
            role_model_binding(Some(&state), &target.1).as_deref(),
            Some("z-ai/glm-5.2")
        );
    }

    /// The ladder is a text-generation construct: a non-text `fallback_role`
    /// (e.g. voice synthesis) must never consult it, even when one is
    /// configured.
    #[test]
    fn ladder_precedence_does_not_apply_to_non_text_capability() {
        let mut state =
            SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());
        state.role_activation = Some(role_activation_with_ladder(&["model.openrouter"]));

        let target =
            resolve_model_execution_target(Some(&state), "voice.synthesize", "model.elevenlabs");
        assert_eq!(target.1, "model.elevenlabs");
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
                    explicit_pin: false,
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

    /// The headline Slice 1 routing behavior: `/model <tier>` must not just
    /// gate fallback escalation, it must actually dispatch there. Verifies
    /// the `resolve_model_execution_target` choke point directly rather than
    /// only indirectly through a full turn.
    #[test]
    fn operator_pin_routes_text_generation_to_pinned_tier() {
        let mut state = SessionState::new("s".into(), "a".into(), "telegram".into());
        state.pinned_tier_role = Some("model.ollama".into());

        let (node, role, incarnation) =
            resolve_model_execution_target(Some(&state), "text.generate", DEFAULT_TEXT_MODEL_ROLE);
        assert_eq!(node, LOCAL_NODE);
        assert_eq!(role, "model.ollama");
        assert!(incarnation.is_none());

        let (_, role, _) = resolve_model_execution_target(
            Some(&state),
            "response.generate",
            DEFAULT_TEXT_MODEL_ROLE,
        );
        assert_eq!(
            role, "model.ollama",
            "Gemini Live dispatch honors the pin too"
        );

        // Scoping guard: a model pin must not hijack unrelated capabilities.
        state.agent_profile.voice_response_policy.provider = Some("elevenlabs".into());
        let (_, voice_role, _) =
            resolve_model_execution_target(Some(&state), "voice.synthesize", "model.local");
        assert_ne!(
            voice_role, "model.ollama",
            "a model tier pin must not affect voice provider routing"
        );
    }

    fn test_fallback_override(origin: &str, active: &str) -> FallbackOverride {
        FallbackOverride {
            origin_tier_role: origin.into(),
            active_tier_role: active.into(),
            reason: "provider_failure".into(),
            since_epoch_ms: 1_000,
            last_probe_epoch_ms: 1_000,
            notice_sent: false,
        }
    }

    /// The headline Slice 2 routing behavior: a session degraded by a prior
    /// escalation must start NEW turns on `active_tier_role`, not re-probe the
    /// known-bad primary — and clearing the override restores the normal
    /// resolution chain (ladder primary).
    #[test]
    fn fallback_override_routes_new_dispatch_to_active_tier() {
        let mut state = SessionState::new("s".into(), "a".into(), "telegram".into());
        state.role_activation = Some(role_activation_with_ladder(&["model", "model.openrouter"]));
        state.fallback_override = Some(test_fallback_override("model", "model.openrouter"));

        let (node, role, incarnation) =
            resolve_model_execution_target(Some(&state), "text.generate", DEFAULT_TEXT_MODEL_ROLE);
        assert_eq!(node, LOCAL_NODE);
        assert_eq!(
            role, "model.openrouter",
            "sticky dispatch to the fallback tier"
        );
        assert!(incarnation.is_none());

        let (_, role, _) = resolve_model_execution_target(
            Some(&state),
            "response.generate",
            DEFAULT_TEXT_MODEL_ROLE,
        );
        assert_eq!(
            role, "model.openrouter",
            "Gemini Live dispatch honors the override too"
        );

        // Scoping guard: the override must not hijack unrelated capabilities.
        let (_, voice_role, _) =
            resolve_model_execution_target(Some(&state), "voice.synthesize", "model.local");
        assert_ne!(
            voice_role, "model.openrouter",
            "a fallback override must not affect voice provider routing"
        );

        // Cleared override: the next turn resolves back to the origin (the
        // ladder primary here) through the normal chain.
        state.fallback_override = None;
        let (_, role, _) =
            resolve_model_execution_target(Some(&state), "text.generate", DEFAULT_TEXT_MODEL_ROLE);
        assert_eq!(
            role, "model",
            "clearing the override restores primary resolution"
        );
    }

    /// Precedence at the choke point: the operator pin (`/model <tier>`) is
    /// explicit intent and must beat the automatic fallback override.
    #[test]
    fn operator_pin_beats_fallback_override_at_choke_point() {
        let mut state = SessionState::new("s".into(), "a".into(), "telegram".into());
        state.fallback_override = Some(test_fallback_override("model", "model.openrouter"));
        state.pinned_tier_role = Some("model.ollama".into());

        let (_, role, _) =
            resolve_model_execution_target(Some(&state), "text.generate", DEFAULT_TEXT_MODEL_ROLE);
        assert_eq!(
            role, "model.ollama",
            "the operator pin outranks the fallback override"
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
                    explicit_pin: false,
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
                status: None,
                error_class: None,
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
                status: None,
                error_class: None,
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
            status: None,
            error_class: None,
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
            streak_extension: 0,
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
            ladder_tier0_dispatched: false,
            streaming_retry_attempts: 0,
            streamed_content: String::new(),
            paracrine_hop_count: 0,
            paracrine_chain_started_at: None,
            selection_source: SelectionSource::default(),
        });

        assert!(should_attempt_provider_repair(&error, Some(&state)));
        state.increment_provider_repair_attempts();
        assert!(!should_attempt_provider_repair(&error, Some(&state)));
    }

    #[test]
    fn provider_auth_failure_is_fatal_without_same_provider_repair() {
        let error = TaskErrorPayload {
            kind: "provider_failure".into(),
            message: "Gemini API error (400): API key expired. Please renew the API key.".into(),
            code: None,
            component: Some("model-router".into()),
            provider: Some("gemini".into()),
            capability: Some("text.generate".into()),
            retryable: Some(false),
            sub_kind: Some("provider_auth".into()),
            status: None,
            error_class: None,
        };

        let state = SessionState::new("sess-1".into(), "agent-jane-01".into(), "telegram".into());

        assert!(!should_attempt_provider_repair(&error, Some(&state)));
        assert_eq!(classify_provider_error(&error), ProviderErrorClass::Fatal);
    }

    // ── Provider-error classification matrix ─────────────────────────────

    fn provider_error(sub_kind: Option<&str>, status: Option<u16>) -> TaskErrorPayload {
        TaskErrorPayload {
            kind: "provider_failure".into(),
            message: "Provider invocation failed".into(),
            component: Some("model-router".into()),
            provider: Some("gemini".into()),
            capability: Some("text.generate".into()),
            sub_kind: sub_kind.map(str::to_string),
            status,
            ..Default::default()
        }
    }

    /// The forensic 2026-07-08 gap: a Gemini 400 with no sub_kind and no
    /// error_class (old-controller envelope) must engage the fallback ladder
    /// — never insta-fail the turn as MODEL_EMPTY_RESPONSE.
    #[test]
    fn classification_matrix_routes_each_error_class() {
        // 4xx contract errors → switch provider.
        assert_eq!(
            classify_provider_error(&provider_error(Some("invalid_request"), Some(400))),
            ProviderErrorClass::SwitchProvider
        );
        assert_eq!(
            classify_provider_error(&provider_error(None, Some(400))),
            ProviderErrorClass::SwitchProvider,
            "status-only 400 must switch providers"
        );
        assert_eq!(
            classify_provider_error(&provider_error(Some("rate_limit"), Some(429))),
            ProviderErrorClass::SwitchProvider
        );
        // The exact forensic shape: kind=provider_failure, nothing else set.
        assert_eq!(
            classify_provider_error(&provider_error(None, None)),
            ProviderErrorClass::SwitchProvider,
            "un-annotated provider_failure on the text path must engage the ladder"
        );

        // 5xx / timeout / network → retryable (existing escalate behavior).
        assert_eq!(
            classify_provider_error(&provider_error(Some("provider_error"), Some(503))),
            ProviderErrorClass::RetrySameProvider
        );
        assert_eq!(
            classify_provider_error(&provider_error(Some("streaming_timeout"), None)),
            ProviderErrorClass::RetrySameProvider
        );
        assert_eq!(
            classify_provider_error(&provider_error(Some("network_error"), None)),
            ProviderErrorClass::RetrySameProvider
        );
        assert_eq!(
            classify_provider_error(&provider_error(None, Some(500))),
            ProviderErrorClass::RetrySameProvider,
            "status-only 5xx is transient"
        );

        // Auth → fatal.
        assert_eq!(
            classify_provider_error(&provider_error(Some("provider_auth"), Some(401))),
            ProviderErrorClass::Fatal
        );
        assert_eq!(
            classify_provider_error(&provider_error(None, Some(401))),
            ProviderErrorClass::Fatal,
            "status-only 401 is fatal"
        );

        // Machine-readable error_class from a new controller wins over everything.
        let mut annotated = provider_error(Some("provider_error"), Some(503));
        annotated.error_class = Some("switch_provider".into());
        assert_eq!(
            classify_provider_error(&annotated),
            ProviderErrorClass::SwitchProvider
        );
        let mut fatal_annotated = provider_error(None, None);
        fatal_annotated.error_class = Some("fatal".into());
        assert_eq!(
            classify_provider_error(&fatal_annotated),
            ProviderErrorClass::Fatal
        );

        // Content/safety block (the second fix): a DISTINCT outcome from
        // SwitchProvider — the whole point is that a blocked turn does NOT
        // silently hop to a different-behaving provider mid-conversation.
        let mut content_blocked_annotated = provider_error(Some("content_policy_block"), None);
        content_blocked_annotated.error_class = Some("content_blocked".into());
        assert_eq!(
            classify_provider_error(&content_blocked_annotated),
            ProviderErrorClass::ContentBlocked
        );
        assert_ne!(
            classify_provider_error(&content_blocked_annotated),
            ProviderErrorClass::SwitchProvider
        );
        // sub_kind fallback (older/un-annotated controller envelope) must also
        // classify as ContentBlocked, not fall through to the un-annotated
        // provider_failure → SwitchProvider default.
        assert_eq!(
            classify_provider_error(&provider_error(Some("content_policy_block"), None)),
            ProviderErrorClass::ContentBlocked
        );

        // Non-text capabilities and non-provider kinds fall through to the
        // generic fail path.
        let mut voice = provider_error(None, None);
        voice.capability = Some("voice.synthesize".into());
        assert_eq!(
            classify_provider_error(&voice),
            ProviderErrorClass::Unclassified
        );
        let transport = TaskErrorPayload::transport_error("philote", "socket closed");
        assert_eq!(
            classify_provider_error(&transport),
            ProviderErrorClass::Unclassified
        );
    }

    /// A contract failure must skip remaining ladder tiers that dispatch to
    /// the failed provider — the same request fails identically there.
    #[test]
    fn next_ladder_tier_skips_failed_provider_tiers_on_contract_failure() {
        let ladder: Vec<String> = vec![
            "model".into(),        // gemini
            "model.gemini".into(), // gemini again — must be skipped
            "model.ollama".into(), // ollama
        ];

        // Contract failure on gemini at tier 0 → tier 2 (model.ollama).
        assert_eq!(
            next_ladder_tier(&ladder, Some(0), 2, Some("gemini"), true),
            2
        );
        // Transient failure keeps the plain +1 walk (no skip).
        assert_eq!(
            next_ladder_tier(&ladder, Some(0), 2, Some("gemini"), false),
            1
        );
        // Skip can exhaust the ladder: gemini fails at tier 0 of an all-gemini tail.
        let gemini_only: Vec<String> = vec!["model".into(), "model.gemini".into()];
        assert_eq!(
            next_ladder_tier(&gemini_only, Some(0), 1, Some("gemini"), true),
            2,
            "skipping past the end signals exhaustion (oracle next)"
        );
        // Unknown failed provider → no skip.
        assert_eq!(next_ladder_tier(&ladder, Some(0), 2, None, true), 1);
    }

    /// The off-by-one fix: `last_ladder_tier = None` means the ladder hasn't
    /// been consulted yet (primary dispatch bypassed it), so the walk must
    /// start at tier 0 — not tier 1.
    #[test]
    fn next_ladder_tier_starts_at_zero_when_ladder_not_yet_engaged() {
        let ladder: Vec<String> = vec!["model.openrouter".into()];
        assert_eq!(
            next_ladder_tier(&ladder, None, 0, None, false),
            0,
            "a single-tier ladder must be reachable when the primary dispatch \
             didn't come from the ladder"
        );

        // Once engaged (Some(tier)), the walk resumes as a plain +1 as before.
        assert_eq!(next_ladder_tier(&ladder, Some(0), 0, None, false), 1);
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
    fn media_capabilities_never_ride_the_text_fallback_ladder() {
        // No session state at all: text capability may consult the (absent)
        // ladder and still lands on the fallback role; media capabilities
        // must land on the fallback role WITHOUT ladder consultation. The
        // regression this guards: a session whose text ladder starts at
        // model.openrouter sent voice.transcribe to a text-only controller
        // ("no provider registered for voice.transcribe").
        let (_, role, _) = resolve_model_execution_target(None, "voice.transcribe", "model");
        assert_eq!(role, "model");
        let (_, role, _) = resolve_model_execution_target(None, "media.analyze", "model");
        assert_eq!(role, "model");
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

    // ── L0 execution safety floor (exec-guard) ─────────────────────────────
    //
    // The floor is checked at the top of `run_bash_command` itself — the
    // function that a parked-turn `/approve` (or `auto_approve_all`)
    // ultimately calls to spawn `sh -c`. There is no approval/session
    // parameter this function accepts that could route around the check,
    // so a passing test here demonstrates the floor sits below approvals:
    // no caller of this function, however it got here, can avoid it.

    #[tokio::test]
    async fn bash_exec_hardline_blocks_root_delete_without_spawning() {
        let result = super::run_bash_command("rm -rf /".into(), None, 10)
            .await
            .expect("denial is a tool result, not an Err");
        assert!(!result["success"].as_bool().unwrap());
        assert_eq!(result["stdout"].as_str().unwrap(), "");
        let stderr = result["stderr"].as_str().unwrap();
        assert!(
            stderr.contains("Do not retry"),
            "unexpected stderr: {stderr}"
        );
        assert!(
            stderr.contains("recursive delete of root filesystem"),
            "unexpected stderr: {stderr}"
        );
    }

    #[tokio::test]
    async fn bash_exec_hardline_blocks_even_when_command_would_otherwise_run() {
        // A hardline shutdown command must be denied rather than executed —
        // if the floor were bypassed this would attempt to power off the
        // test host, which is exactly what must never happen.
        let result = super::run_bash_command("shutdown -h now".into(), None, 10)
            .await
            .expect("denial is a tool result, not an Err");
        assert!(!result["success"].as_bool().unwrap());
        assert_eq!(result["exit_code"].as_i64().unwrap(), 126);
    }

    #[tokio::test]
    async fn bash_exec_hardline_does_not_block_ordinary_commands() {
        let result = super::run_bash_command("echo hello".into(), None, 10)
            .await
            .expect("should succeed");
        assert!(result["success"].as_bool().unwrap());
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
                philotic_client::IpcRequest::PushHealEvent {
                    guest_id,
                    severity,
                    pattern_tag,
                    detail,
                } => {
                    emitted.lock().unwrap().push(serde_json::json!({
                        "heal_event": {
                            "guest_id": guest_id,
                            "severity": severity,
                            "pattern_tag": pattern_tag,
                            "detail": detail,
                        },
                    }));
                    serde_json::to_vec(&philotic_client::IpcResponse::success("ok", None)).unwrap()
                }
                philotic_client::IpcRequest::ConfigureRole {
                    role_name,
                    fallback_tiers,
                    ..
                } => {
                    emitted.lock().unwrap().push(serde_json::json!({
                        "configure_role": {
                            "role_name": role_name,
                            "fallback_tiers": fallback_tiers,
                        },
                    }));
                    serde_json::to_vec(&philotic_client::IpcResponse::ConfigureRoleOk {
                        role_name: role_name.clone(),
                    })
                    .unwrap()
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

    /// DEF-051: `own_guest_id` is both the hotel registration identity AND the
    /// `reply_guest_id` stamped on every dispatch — the bare agent id for a base
    /// philote, `{agent_id}:{role_name}` for a role incarnation. A role
    /// specialist must stamp its incarnation id so its model reply returns to
    /// THIS process instead of the base agent's (which would hang its turn).
    #[test]
    fn compose_guest_identity_matches_registration_shapes() {
        use super::compose_guest_identity;
        assert_eq!(compose_guest_identity("agent-bjork-01", None), "agent-bjork-01");
        assert_eq!(
            compose_guest_identity("agent-bjork-01", Some("theoretician")),
            "agent-bjork-01:theoretician"
        );
    }

    /// End-to-end shape of the reply address a role-incarnation runtime stamps.
    #[tokio::test]
    async fn own_guest_id_targets_own_incarnation() {
        let socket_path = format!("/tmp/philote-mrg-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-bjork-01".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-bjork-01");

        // Base philote → bare agent id (matches its own registration).
        assert_eq!(runtime.own_guest_id(), "agent-bjork-01");

        // Role incarnation → its own "{agent_id}:{role_name}" id.
        runtime.set_role_name("theoretician");
        assert_eq!(runtime.own_guest_id(), "agent-bjork-01:theoretician");

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
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

    // ── life.observe contract-error retry (2026-07-10 LifeGraph forensic) ────
    //
    // A model-invoked `life.observe` call whose payload fails datasource's
    // pre-write contract validation (CONTRACT_ERROR_MARKER, tagged
    // `sub_kind: "invalid_request"` — see FIX 1 in datasource/runtime.rs and
    // data-memorygraphrag/provider.rs) must get exactly one bounded model
    // retry with the cause surfaced, instead of being silently apologized
    // away. If the retry also fails — or the payload came from philote's own
    // direct-command parser, which has no model in the loop to act on a
    // retry — the failure must escalate to heal_queue (`life_observe_parse_failed`)
    // and still reach the user. Non-contract errors (transport/routing) must
    // not retry at all; that class already has its own handling.

    fn life_observe_arguments(claim_summary: &str, direct_origin: bool) -> serde_json::Value {
        let mut evidence = serde_json::json!({
            "packet_id": "pkt-1",
            "claim_ref": { "id": "life:openloop:1", "label": "OpenLoop", "datasource": "life-graph" },
            "claim_summary": claim_summary,
            "source_refs": [{
                "source_id": "membrane:telegram",
                "source_kind": "runtime_observation",
                "reliability": { "score": 0.9, "basis": "direct_observation" },
            }],
            "passage_refs": [],
            "confidence": 0.8,
            "validation_state": "proposed",
            "source_reliability": 0.9,
            "conflict_ids": [],
            "adjudication_status": "not_needed",
        });
        if direct_origin {
            evidence["metadata"] = serde_json::json!({
                "route": "philote_direct_life_observe",
                "session_id": "sess-lifeobs",
                "turn_id": "turn-lifeobs",
                "chat_id": "555",
                "agent_id": "agent-lifeobs",
            });
        }
        serde_json::json!({
            "observation_id": "obs-1",
            "evidence": evidence,
            "proposed_graph_refs": [],
        })
    }

    fn life_observe_working_turn(turn_id: &str, direct_origin: bool) -> WorkingTurn {
        let mut turn = test_working_turn(TurnPhase::WaitingTool);
        turn.turn_id = turn_id.into();
        turn.chat_id = "555".into();
        turn.user_content = "Toastmasters OpenLoop capture".into();
        turn.final_reply_to = "membrane-node-01".into();
        turn.final_reply_role = "membrane".into();
        turn.final_reply_guest_id = Some("membrane-seat-1".into());
        turn.pending_tool_call = Some(ToolCall {
            tool_name: "life.observe".into(),
            arguments: life_observe_arguments(
                "Toastmasters: own the Toastmaster role next meeting",
                direct_origin,
            ),
        });
        turn
    }

    fn life_observe_error_task(
        session_id: &str,
        turn_id: &str,
        sub_kind: Option<&str>,
        message: &str,
    ) -> InboundTaskPayload {
        InboundTaskPayload {
            action: Some("tool_result".into()),
            session_id: Some(session_id.into()),
            turn_id: Some(turn_id.into()),
            tool_name: Some("life.observe".into()),
            content: Some(format!(
                "Tool call failed: {message} (provider: life-graph-runner, capability: life.observe)"
            )),
            error: Some(philotic_client::TaskErrorPayload {
                kind: "provider_failure".into(),
                message: message.into(),
                code: None,
                component: Some("datasource_controller".into()),
                provider: Some("life-graph-runner".into()),
                capability: Some("life.observe".into()),
                retryable: None,
                sub_kind: sub_kind.map(str::to_string),
                status: None,
                error_class: None,
            }),
            ..Default::default()
        }
    }

    /// FIX 2, case 1: a model-invoked life.observe call fails with a
    /// contract error → exactly one model re-entry with the cause surfaced,
    /// no user-facing reply yet, no heal event yet.
    #[tokio::test]
    async fn life_observe_contract_error_retries_model_once_with_cause() {
        let socket_path = format!(
            "/tmp/philote-lifeobs-retry-{}.sock",
            Uuid::new_v4().simple()
        );
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-lifeobs-retry".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-lifeobs-retry");

        let session_id = "sess-lifeobs-retry";
        let turn_id = "turn-lifeobs-retry";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(life_observe_working_turn(turn_id, false));

        runtime
            .handle_tool_result(life_observe_error_task(
                session_id,
                turn_id,
                Some("invalid_request"),
                "provider failed: contract_error: failed to parse life.observe parameters as \
                 LifeObserveInput: invalid type: string \"maybe\", expected f64 at line 1 column 42",
            ))
            .await
            .expect("tool result handled");

        {
            let state = runtime.sessions.get(session_id).expect("session");
            let turn = state
                .active_turn
                .as_ref()
                .expect("turn must still be active for a retry, not finalized to the user");
            assert_eq!(turn.phase, TurnPhase::WaitingModel);
            assert_eq!(
                turn.provider_repair_attempts, 1,
                "one retry attempt must be recorded"
            );
            assert_eq!(
                turn.iteration, 1,
                "the retried round-trip must count as exactly one iteration \
                 against iteration_cap, not two"
            );
        }

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        let reentries: Vec<_> = emitted
            .iter()
            .filter(|e| e["task"]["action"] == "generate_text")
            .collect();
        assert_eq!(
            reentries.len(),
            1,
            "exactly one model re-entry expected: {:#?}",
            *emitted
        );
        let reentry_json = serde_json::to_string(&reentries[0]["task"]).unwrap();
        assert!(
            reentry_json.contains("Your life.observe call failed"),
            "reentry must surface the life.observe repair note: {reentry_json}"
        );
        assert!(
            reentry_json.contains("invalid type: string"),
            "reentry must surface the actual cause, not a generic message: {reentry_json}"
        );

        assert!(
            emitted.iter().all(|e| e.get("heal_event").is_none()),
            "no heal event should fire on the first, retried failure: {:#?}",
            *emitted
        );
        assert!(
            emitted.iter().all(|e| e["task"]["action"] != "send_reply"),
            "no user-facing reply should fire before the retry is exhausted: {:#?}",
            *emitted
        );
    }

    /// FIX 2, case 2: the model's one retry also fails → surfaced to the
    /// user AND a `life_observe_parse_failed` heal event is filed; no second
    /// retry is attempted.
    #[tokio::test]
    async fn life_observe_contract_error_after_retry_exhausted_heals_and_tells_user() {
        let socket_path = format!(
            "/tmp/philote-lifeobs-exhausted-{}.sock",
            Uuid::new_v4().simple()
        );
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-lifeobs-exhausted".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-lifeobs-exhausted");

        let session_id = "sess-lifeobs-exhausted";
        let turn_id = "turn-lifeobs-exhausted";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        // Simulate the retry budget already being spent (as it would be
        // after case 1's one bounded retry came back with a second failure).
        let mut turn = life_observe_working_turn(turn_id, false);
        turn.provider_repair_attempts = 1;
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(turn);

        runtime
            .handle_tool_result(life_observe_error_task(
                session_id,
                turn_id,
                Some("invalid_request"),
                "provider failed: contract_error: life.observe plan validation failed: \
                 observation_id must not be empty",
            ))
            .await
            .expect("tool result handled");

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();

        let reentries: Vec<_> = emitted
            .iter()
            .filter(|e| e["task"]["action"] == "generate_text")
            .collect();
        assert!(
            reentries.is_empty(),
            "must not attempt a second model retry: {:#?}",
            *emitted
        );

        let heal_events: Vec<_> = emitted
            .iter()
            .filter(|e| e.get("heal_event").is_some())
            .collect();
        assert_eq!(
            heal_events.len(),
            1,
            "exactly one heal event expected: {:#?}",
            *emitted
        );
        assert_eq!(
            heal_events[0]["heal_event"]["pattern_tag"],
            "life_observe_parse_failed"
        );
        assert!(
            heal_events[0]["heal_event"]["detail"]
                .as_str()
                .unwrap()
                .contains("retry exhausted")
        );

        let send_replies: Vec<_> = emitted
            .iter()
            .filter(|e| e["task"]["action"] == "send_reply")
            .collect();
        assert_eq!(
            send_replies.len(),
            1,
            "the user must still be told, not left with a silent drop: {:#?}",
            *emitted
        );
        assert!(
            send_replies[0]["task"]["content"]
                .as_str()
                .unwrap()
                .contains("I tried to record this")
        );
    }

    /// FIX 2, case 3: a non-contract error (transport/routing — unmarked
    /// `sub_kind`) must never trigger the retry or the new heal path; that
    /// class already has its own handling elsewhere.
    #[tokio::test]
    async fn life_observe_non_contract_error_does_not_retry_or_heal() {
        let socket_path = format!(
            "/tmp/philote-lifeobs-transport-{}.sock",
            Uuid::new_v4().simple()
        );
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-lifeobs-transport".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-lifeobs-transport");

        let session_id = "sess-lifeobs-transport";
        let turn_id = "turn-lifeobs-transport";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(life_observe_working_turn(turn_id, false));

        runtime
            .handle_tool_result(life_observe_error_task(
                session_id,
                turn_id,
                None, // no sub_kind: e.g. a Memgraph connection failure, not a contract error
                "provider failed: Memgraph connection refused",
            ))
            .await
            .expect("tool result handled");

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        assert!(
            emitted
                .iter()
                .all(|e| e["task"]["action"] != "generate_text"),
            "a non-contract error must never trigger the life.observe retry: {:#?}",
            *emitted
        );
        assert!(
            emitted.iter().all(|e| e.get("heal_event").is_none()),
            "a non-contract error must not use the new life.observe heal path: {:#?}",
            *emitted
        );
        let send_replies: Vec<_> = emitted
            .iter()
            .filter(|e| e["task"]["action"] == "send_reply")
            .collect();
        assert_eq!(
            send_replies.len(),
            1,
            "existing apology-to-user behavior must be unchanged: {:#?}",
            *emitted
        );
    }

    /// FIX 2, case 4: a contract error on a payload built by philote's own
    /// direct-command parser (no model in the loop) must skip the retry
    /// entirely and go straight to heal_queue + user notification.
    #[tokio::test]
    async fn life_observe_direct_command_origin_contract_error_skips_retry() {
        let socket_path = format!(
            "/tmp/philote-lifeobs-direct-{}.sock",
            Uuid::new_v4().simple()
        );
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-lifeobs-direct".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-lifeobs-direct");

        let session_id = "sess-lifeobs-direct";
        let turn_id = "turn-lifeobs-direct";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(life_observe_working_turn(turn_id, true));

        runtime
            .handle_tool_result(life_observe_error_task(
                session_id,
                turn_id,
                Some("invalid_request"),
                "provider failed: contract_error: failed to parse life.observe parameters as \
                 LifeObserveInput: missing field `claim_summary`",
            ))
            .await
            .expect("tool result handled");

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        assert!(
            emitted
                .iter()
                .all(|e| e["task"]["action"] != "generate_text"),
            "direct-command origin must never get a model retry: {:#?}",
            *emitted
        );

        let heal_events: Vec<_> = emitted
            .iter()
            .filter(|e| e.get("heal_event").is_some())
            .collect();
        assert_eq!(
            heal_events.len(),
            1,
            "expected one heal event: {:#?}",
            *emitted
        );
        assert_eq!(
            heal_events[0]["heal_event"]["pattern_tag"],
            "life_observe_parse_failed"
        );
        assert!(
            heal_events[0]["heal_event"]["detail"]
                .as_str()
                .unwrap()
                .contains("direct-command origin")
        );

        let send_replies: Vec<_> = emitted
            .iter()
            .filter(|e| e["task"]["action"] == "send_reply")
            .collect();
        assert_eq!(
            send_replies.len(),
            1,
            "user must still be told: {:#?}",
            *emitted
        );
    }

    // ── Turn-failure heal intake (self-heal) ─────────────────────────────────

    /// A watchdog eviction must push a `stuck_turn_evicted:{phase}` heal
    /// event to the hotel (turn-failure heal intake) in addition to failing
    /// the task and unblocking the session.
    #[tokio::test]
    async fn watchdog_eviction_pushes_stuck_turn_heal_event() {
        let socket_path = format!("/tmp/philote-healevict-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-heal-evict".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-heal-evict");

        let session_id = "sess-heal-evict";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        // Seed an active turn stuck waiting on a tool.
        let turn = def004_working_turn("turn-heal-evict", "hotel.status");
        let signature = format!(
            "active:{}:{:?}:{}:{}",
            turn.turn_id, turn.phase, turn.iteration, "hotel.status"
        );
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(turn);

        // Backdate the watchdog bookkeeping past the WaitingTool deadline,
        // with the matching wait signature so reconcile keeps the timestamp.
        let past = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_secs(120))
            .expect("backdate instant");
        runtime
            .stuck_turn_first_seen
            .insert(session_id.to_string(), past);
        runtime
            .stuck_turn_signature
            .insert(session_id.to_string(), signature);

        runtime.evict_timed_out_turns().await;

        assert!(
            runtime
                .sessions
                .get(session_id)
                .expect("session")
                .active_turn
                .is_none(),
            "eviction must clear the active turn"
        );

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        let heal_events: Vec<_> = emitted
            .iter()
            .filter(|e| !e["heal_event"].is_null())
            .collect();
        assert_eq!(
            heal_events.len(),
            1,
            "exactly one heal event must be pushed: {:#?}",
            *emitted
        );
        let heal = &heal_events[0]["heal_event"];
        assert_eq!(heal["guest_id"], "agent-heal-evict");
        assert_eq!(heal["severity"], "medium");
        assert_eq!(heal["pattern_tag"], "stuck_turn_evicted:WaitingTool");
        assert!(
            heal["detail"]
                .as_str()
                .expect("detail")
                .contains("watchdog evicted stuck turn"),
            "detail must carry the eviction reason: {heal:#?}"
        );

        // The unblock notice still reaches the user.
        assert!(
            emitted.iter().any(|e| e["task"]["action"] == "send_reply"),
            "eviction must still emit the unblock send_reply: {:#?}",
            *emitted
        );
    }

    /// Fallback-ladder + oracle exhaustion must push a
    /// `fallback_exhausted:{last_provider}` heal event before failing the turn.
    #[tokio::test]
    async fn fallback_exhaustion_pushes_heal_event() {
        let socket_path = format!("/tmp/philote-healfb-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-heal-fb".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-heal-fb");

        let session_id = "sess-heal-fb";
        let turn_id = "turn-heal-fb";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        // Seed a WaitingModel turn already past every static tier AND past
        // the oracle extra-tier budget, so exhaustion is immediate (no
        // QueryModelRoute round trip).
        let mut turn = test_working_turn(TurnPhase::WaitingModel);
        turn.turn_id = turn_id.into();
        turn.fallback_tier = u8::MAX - 1;
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(turn);

        runtime
            .advance_turn_to_next_fallback_tier(
                session_id.to_string(),
                turn_id.to_string(),
                NoResponseClass::ProviderFailure,
                Some("gemini".into()),
                "provider failed".into(),
            )
            .await
            .expect("advance");

        assert!(
            runtime
                .sessions
                .get(session_id)
                .expect("session")
                .active_turn
                .is_none(),
            "exhaustion must fail (clear) the active turn"
        );

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        let heal_events: Vec<_> = emitted
            .iter()
            .filter(|e| !e["heal_event"].is_null())
            .collect();
        assert_eq!(
            heal_events.len(),
            1,
            "exactly one heal event must be pushed: {:#?}",
            *emitted
        );
        let heal = &heal_events[0]["heal_event"];
        assert_eq!(heal["pattern_tag"], "fallback_exhausted:gemini");
        assert_eq!(heal["severity"], "medium");
        assert!(
            heal["detail"]
                .as_str()
                .expect("detail")
                .contains("All model providers failed"),
            "detail must describe the exhaustion: {heal:#?}"
        );
    }

    /// An operator-pinned session (`SelectionSource::OperatorExplicit`) must
    /// never silently escalate to the next fallback tier — that would violate
    /// the operator's explicit `/model <tier>` intent. Instead the turn fails
    /// fast with the real trigger message, and `fallback_tier` is left
    /// untouched (no ladder/oracle consultation, no heal event).
    #[tokio::test]
    async fn operator_pinned_turn_fails_fast_instead_of_escalating() {
        let socket_path = format!("/tmp/philote-pinfail-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-pin-fail".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-pin-fail");

        let session_id = "sess-pin-fail";
        let turn_id = "turn-pin-fail";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        let mut turn = test_working_turn(TurnPhase::WaitingModel);
        turn.turn_id = turn_id.into();
        turn.fallback_tier = 0;
        turn.selection_source = SelectionSource::OperatorExplicit;
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(turn);
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .pinned_tier_role = Some("model.ollama".into());

        runtime
            .advance_turn_to_next_fallback_tier(
                session_id.to_string(),
                turn_id.to_string(),
                NoResponseClass::ProviderFailure,
                Some("gemini".into()),
                "the pinned provider did not respond".into(),
            )
            .await
            .expect("advance");

        assert!(
            runtime
                .sessions
                .get(session_id)
                .expect("session")
                .active_turn
                .is_none(),
            "an operator-pinned session must fail fast, not stay parked mid-escalation"
        );

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        assert!(
            !emitted.iter().any(|e| !e["heal_event"].is_null()),
            "operator-pinned fail-fast must not push a fallback_exhausted heal event: {:#?}",
            *emitted
        );
        assert!(
            emitted.iter().any(|e| e["task"]["action"] == "send_reply"
                && e["task"]["content"] == "the pinned provider did not respond"),
            "must fail with the real trigger message, not a generic exhaustion notice: {:#?}",
            *emitted
        );
    }

    // ── Slice 2: persisted FallbackOverride + origin probe ───────────────────

    /// A successful escalation must write the session's `FallbackOverride`
    /// (origin = the ladder primary, active = the newly dispatched tier,
    /// reason = the failure class); a SECOND escalation on the same degraded
    /// session must update `active_tier_role`/`reason` only, keeping the
    /// original `origin_tier_role` and `since_epoch_ms`.
    #[tokio::test]
    async fn escalation_writes_fallback_override_then_updates_active_only() {
        let socket_path = format!("/tmp/philote-ovwrite-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-ov-write".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-ov-write");

        let session_id = "sess-ov-write";
        let turn_id = "turn-ov-write";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");
        {
            let state = runtime.sessions.get_mut(session_id).expect("session");
            state.role_activation = Some(role_activation_with_ladder(&[
                "model",
                "model.openrouter",
                "model.ollama",
            ]));
        }

        let mut turn = test_working_turn(TurnPhase::WaitingModel);
        turn.turn_id = turn_id.into();
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(turn);

        runtime
            .advance_turn_to_next_fallback_tier(
                session_id.to_string(),
                turn_id.to_string(),
                NoResponseClass::ProviderFailure,
                Some("gemini".into()),
                "gemini did not respond".into(),
            )
            .await
            .expect("first escalation");

        let (first_since, first_probe_ms) = {
            let state = runtime.sessions.get(session_id).expect("session");
            let ov = state
                .fallback_override
                .as_ref()
                .expect("escalation must write the fallback override");
            assert_eq!(ov.origin_tier_role, "model", "origin = ladder primary");
            assert_eq!(
                ov.active_tier_role, "model.openrouter",
                "active = the newly dispatched tier"
            );
            assert_eq!(ov.reason, "provider_failure");
            assert!(ov.since_epoch_ms > 0);
            assert_eq!(ov.since_epoch_ms, ov.last_probe_epoch_ms);
            // Slice 3: the first escalation on a fresh override also fires
            // (and latches) the `model_fallback` operational notice — see
            // `escalation_emits_model_fallback_notice_once_then_latches_silent`.
            assert!(ov.notice_sent);
            (ov.since_epoch_ms, ov.last_probe_epoch_ms)
        };

        // Second escalation on the same degraded session (tier 1 → tier 2).
        runtime
            .advance_turn_to_next_fallback_tier(
                session_id.to_string(),
                turn_id.to_string(),
                NoResponseClass::WatchdogTimeout,
                None,
                "openrouter timed out".into(),
            )
            .await
            .expect("second escalation");

        {
            let state = runtime.sessions.get(session_id).expect("session");
            let ov = state
                .fallback_override
                .as_ref()
                .expect("override survives the second escalation");
            assert_eq!(
                ov.origin_tier_role, "model",
                "origin must stay the original primary"
            );
            assert_eq!(
                ov.active_tier_role, "model.ollama",
                "active tracks the newest dispatched tier"
            );
            assert_eq!(
                ov.reason, "model_timeout",
                "reason tracks the newest failure"
            );
            assert_eq!(
                ov.since_epoch_ms, first_since,
                "since must not reset on re-escalation"
            );
            assert_eq!(
                ov.last_probe_epoch_ms, first_probe_ms,
                "re-escalation must not consume the probe cadence"
            );
        }

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
    }

    /// Helper: pull every `turn_event` EmitTask recorded by `run_recording_hotel`
    /// whose `event` field matches `event_name`.
    fn recorded_turn_events<'a>(
        emitted: &'a [serde_json::Value],
        event_name: &str,
    ) -> Vec<&'a serde_json::Value> {
        emitted
            .iter()
            .filter(|e| e["task"]["action"] == "turn_event" && e["task"]["event"] == event_name)
            .collect()
    }

    /// Slice 3: the first escalation on a fresh session (no override yet, so
    /// `notice_sent` starts false) must emit exactly one `model_fallback`
    /// turn_event and latch `notice_sent`; a second escalation on the same
    /// already-notified session (sticky-turn / further tier walk) must emit
    /// nothing more.
    #[tokio::test]
    async fn escalation_emits_model_fallback_notice_once_then_latches_silent() {
        let socket_path = format!("/tmp/philote-ovnotice-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-ov-notice".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-ov-notice");

        let session_id = "sess-ov-notice";
        let turn_id = "turn-ov-notice";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");
        {
            let state = runtime.sessions.get_mut(session_id).expect("session");
            state.role_activation = Some(role_activation_with_ladder(&[
                "model",
                "model.openrouter",
                "model.ollama",
            ]));
        }

        let mut turn = test_working_turn(TurnPhase::WaitingModel);
        turn.turn_id = turn_id.into();
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(turn);

        runtime
            .advance_turn_to_next_fallback_tier(
                session_id.to_string(),
                turn_id.to_string(),
                NoResponseClass::ProviderFailure,
                Some("gemini".into()),
                "gemini did not respond".into(),
            )
            .await
            .expect("first escalation");

        {
            let recorded = emitted.lock().unwrap().clone();
            let notices = recorded_turn_events(&recorded, "model_fallback");
            assert_eq!(
                notices.len(),
                1,
                "first escalation (notice_sent=false) must emit exactly one model_fallback event: {recorded:#?}"
            );
            let message = notices[0]["task"]["partial_content"]
                .as_str()
                .expect("partial_content");
            assert_eq!(
                message,
                "\u{21aa}\u{fe0f} Model fallback: model.openrouter (was model; provider_failure)"
            );
        }
        assert!(
            runtime
                .sessions
                .get(session_id)
                .expect("session")
                .fallback_override
                .as_ref()
                .expect("override")
                .notice_sent,
            "notice_sent must latch true after the first emission"
        );

        // Second escalation on the same already-notified session — sticky
        // turn / further tier walk. Must not emit a second notice.
        runtime
            .advance_turn_to_next_fallback_tier(
                session_id.to_string(),
                turn_id.to_string(),
                NoResponseClass::WatchdogTimeout,
                None,
                "openrouter timed out".into(),
            )
            .await
            .expect("second escalation");

        {
            let recorded = emitted.lock().unwrap().clone();
            let notices = recorded_turn_events(&recorded, "model_fallback");
            assert_eq!(
                notices.len(),
                1,
                "an already-latched session must not emit a second model_fallback notice: {recorded:#?}"
            );
        }

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
    }

    /// Probe eligibility: `probe_degraded_sessions` fires only for a session
    /// that has an override, no active turn, and a stale `last_probe_epoch_ms`
    /// (>= 300s). A fresh override is not probed; a degraded session with an
    /// active turn is not probed; a fired probe is a degenerate one-shot
    /// `generate_text` ("ping") at the ORIGIN tier, and the in-flight marker
    /// plus the send-time stamp prevent a second probe on the next tick.
    #[tokio::test]
    async fn probe_fires_only_past_cadence_and_with_no_active_turn() {
        let socket_path = format!("/tmp/philote-ovprobe-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-ov-probe".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-ov-probe");

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        // Session A: degraded, idle, fresh probe stamp — must NOT probe yet.
        runtime
            .ensure_session_loaded("sess-probe-a", "telegram")
            .await
            .expect("session load");
        runtime
            .sessions
            .get_mut("sess-probe-a")
            .expect("session")
            .fallback_override = Some(test_fallback_override("model", "model.openrouter"));
        runtime
            .sessions
            .get_mut("sess-probe-a")
            .expect("session")
            .fallback_override
            .as_mut()
            .expect("override")
            .last_probe_epoch_ms = now_ms;

        // Session B: degraded, stale stamp, but an ACTIVE turn — must not probe.
        runtime
            .ensure_session_loaded("sess-probe-b", "telegram")
            .await
            .expect("session load");
        {
            let state = runtime.sessions.get_mut("sess-probe-b").expect("session");
            state.fallback_override = Some(test_fallback_override("model", "model.openrouter"));
            state.start_turn(test_working_turn(TurnPhase::WaitingModel));
        }

        runtime.probe_degraded_sessions().await;
        assert!(
            emitted
                .lock()
                .unwrap()
                .iter()
                .all(|e| e["task"]["action"] != "generate_text"),
            "no probe may fire before the 300s cadence or while a turn is active: {:#?}",
            *emitted.lock().unwrap()
        );

        // Age session A past the cadence — exactly one probe must fire.
        runtime
            .sessions
            .get_mut("sess-probe-a")
            .expect("session")
            .fallback_override
            .as_mut()
            .expect("override")
            .last_probe_epoch_ms = now_ms.saturating_sub(301_000);

        runtime.probe_degraded_sessions().await;
        // Second tick immediately after: in-flight marker + send-time stamp
        // must prevent a duplicate probe.
        runtime.probe_degraded_sessions().await;

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        let probes: Vec<_> = emitted
            .iter()
            .filter(|e| e["task"]["action"] == "generate_text")
            .collect();
        assert_eq!(probes.len(), 1, "exactly one probe: {:#?}", *emitted);
        let probe = &probes[0];
        assert_eq!(
            probe["target_role"], "model",
            "the probe must target the ORIGIN tier, not the active fallback tier"
        );
        assert_eq!(probe["task"]["session_id"], "sess-probe-a");
        assert!(
            probe["task"]["turn_id"]
                .as_str()
                .expect("probe turn_id")
                .starts_with("probe-"),
            "probe turns carry a dedicated probe- turn_id: {probe:#?}"
        );
        assert_eq!(probe["task"]["user_content"], "ping");
        assert_eq!(
            probe["task"]["provider_options"]["probe"], true,
            "the payload carries the probe marker"
        );
    }

    /// Probe outcome handling: an error response correlated to the probe
    /// leaves the session degraded (only the in-flight marker clears); a
    /// successful response clears the override, after which dispatch resolves
    /// back to the origin tier. Neither outcome touches the (absent) active
    /// turn.
    #[tokio::test]
    async fn probe_success_clears_override_and_probe_failure_keeps_it() {
        let socket_path = format!("/tmp/philote-ovclear-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-ov-clear".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-ov-clear");

        let session_id = "sess-ov-clear";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");
        {
            let state = runtime.sessions.get_mut(session_id).expect("session");
            state.role_activation =
                Some(role_activation_with_ladder(&["model", "model.openrouter"]));
            let mut ov = test_fallback_override("model", "model.openrouter");
            ov.last_probe_epoch_ms = 0; // long past the cadence
            state.fallback_override = Some(ov);
        }

        // First probe → origin still failing.
        runtime.probe_degraded_sessions().await;
        let probe_id = runtime
            .pending_fallback_probes
            .get(session_id)
            .cloned()
            .expect("probe in flight");
        runtime
            .handle_model_response(InboundTaskPayload {
                action: Some("model_response".into()),
                session_id: Some(session_id.into()),
                turn_id: Some(probe_id),
                error: Some(TaskErrorPayload {
                    kind: "provider_failure".into(),
                    message: "still down".into(),
                    provider: Some("gemini".into()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .expect("probe failure handled");
        assert!(
            runtime
                .sessions
                .get(session_id)
                .expect("session")
                .fallback_override
                .is_some(),
            "a failed probe must leave the session degraded"
        );
        assert!(
            !runtime.pending_fallback_probes.contains_key(session_id),
            "the in-flight marker must clear on any probe outcome"
        );

        // Second probe → origin recovered.
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session")
            .fallback_override
            .as_mut()
            .expect("override")
            .last_probe_epoch_ms = 0;
        runtime.probe_degraded_sessions().await;
        let probe_id = runtime
            .pending_fallback_probes
            .get(session_id)
            .cloned()
            .expect("second probe in flight");
        runtime
            .handle_model_response(InboundTaskPayload {
                action: Some("model_response".into()),
                session_id: Some(session_id.into()),
                turn_id: Some(probe_id),
                agent_action: Some(serde_json::json!({
                    "kind": "respond",
                    "content": "pong",
                })),
                content: Some("pong".into()),
                ..Default::default()
            })
            .await
            .expect("probe success handled");

        {
            let state = runtime.sessions.get(session_id).expect("session");
            assert!(
                state.fallback_override.is_none(),
                "a successful probe must clear the override"
            );
            assert!(
                state.active_turn.is_none(),
                "probes must never materialize a WorkingTurn"
            );
            let (_, role, _) = resolve_model_execution_target(
                Some(state),
                "text.generate",
                DEFAULT_TEXT_MODEL_ROLE,
            );
            assert_eq!(
                role, "model",
                "after recovery the next turn resolves to the origin tier"
            );
        }
        assert!(!runtime.pending_fallback_probes.contains_key(session_id));

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
    }

    /// Slice 3: the origin-tier probe clearing the override must announce
    /// recovery via `model_fallback_cleared` — but ONLY when the degradation
    /// was actually announced in the first place (`notice_sent == true`). A
    /// fallback the user never heard about doesn't need a recovery
    /// announcement. `session_id` follows the `"{source}:{chat_id}:{...}"`
    /// encoding so `emit_turn_event`'s no-active-turn fallback (there is no
    /// `WorkingTurn` on a probe-cleared, idle session) can recover a chat_id.
    #[tokio::test]
    async fn probe_clear_emits_notice_only_when_previously_notified() {
        async fn run_probe_clear_case(
            session_id: &str,
            notice_sent: bool,
        ) -> Vec<serde_json::Value> {
            let socket_path = format!("/tmp/philote-ovclearn-{}.sock", Uuid::new_v4().simple());
            let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
            let emitted =
                std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
            let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

            let identity = philotic_client::GuestIdentity {
                guest_id: "agent-ov-clear-notice".into(),
                role: "agent".into(),
                supported_tools: Vec::new(),
            };
            let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
                .await
                .expect("connect to stub hotel");
            let mut runtime = AgentRuntime::new(client, "agent-ov-clear-notice");

            runtime
                .ensure_session_loaded(session_id, "telegram")
                .await
                .expect("session load");
            {
                let state = runtime.sessions.get_mut(session_id).expect("session");
                state.role_activation =
                    Some(role_activation_with_ladder(&["model", "model.openrouter"]));
                let mut ov = test_fallback_override("model", "model.openrouter");
                ov.notice_sent = notice_sent;
                ov.last_probe_epoch_ms = 0; // long past the cadence
                state.fallback_override = Some(ov);
            }

            runtime.probe_degraded_sessions().await;
            let probe_id = runtime
                .pending_fallback_probes
                .get(session_id)
                .cloned()
                .expect("probe in flight");
            runtime
                .handle_model_response(InboundTaskPayload {
                    action: Some("model_response".into()),
                    session_id: Some(session_id.into()),
                    turn_id: Some(probe_id),
                    agent_action: Some(serde_json::json!({
                        "kind": "respond",
                        "content": "pong",
                    })),
                    content: Some("pong".into()),
                    ..Default::default()
                })
                .await
                .expect("probe success handled");

            assert!(
                runtime
                    .sessions
                    .get(session_id)
                    .expect("session")
                    .fallback_override
                    .is_none(),
                "a successful probe must clear the override"
            );

            let recorded = emitted.lock().unwrap().clone();
            drop(runtime);
            let _ = server.await;
            let _ = std::fs::remove_file(&socket_path);
            recorded
        }

        let with_notice = run_probe_clear_case("telegram:98765:agent-ov-clear-a", true).await;
        let notices = recorded_turn_events(&with_notice, "model_fallback_cleared");
        assert_eq!(
            notices.len(),
            1,
            "a previously-notified session must emit exactly one model_fallback_cleared event: {with_notice:#?}"
        );
        let message = notices[0]["task"]["partial_content"]
            .as_str()
            .expect("partial_content");
        assert_eq!(
            message,
            "\u{21aa}\u{fe0f} Model fallback cleared: model (was model.openrouter)"
        );

        let without_notice = run_probe_clear_case("telegram:98766:agent-ov-clear-b", false).await;
        let notices = recorded_turn_events(&without_notice, "model_fallback_cleared");
        assert!(
            notices.is_empty(),
            "a session whose degradation was never announced must not emit a recovery notice: {without_notice:#?}"
        );
    }

    /// A role change (inbound handoff_bundle — the application path of
    /// `/role <name>`) must clear the fallback override and ONLY that field.
    #[tokio::test]
    async fn role_change_clears_fallback_override() {
        let socket_path = format!("/tmp/philote-ovrole-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-ov-role".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-ov-role");

        let session_id = "sess-ov-role";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");
        {
            let state = runtime.sessions.get_mut(session_id).expect("session");
            state.fallback_override = Some(test_fallback_override("model", "model.openrouter"));
            state.pinned_tier_role = Some("model.ollama".into());
        }

        runtime
            .handle_handoff_bundle(
                InboundTaskPayload {
                    action: Some("handoff_bundle".into()),
                    session_id: Some(session_id.into()),
                    handoff_bundle: Some(philotic_client::HandoffBundle {
                        to_role: Some("developer".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                Uuid::new_v4(),
            )
            .await
            .expect("handoff bundle");

        let state = runtime.sessions.get(session_id).expect("session");
        assert!(
            state.fallback_override.is_none(),
            "a role change must clear the fallback override"
        );
        assert_eq!(
            state.pinned_tier_role.as_deref(),
            Some("model.ollama"),
            "clearing the override must not touch other session state"
        );

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
    }

    /// Any `/model` invocation — setting a pin or clearing it — resets the
    /// fallback override: the operator is taking explicit control of tier
    /// selection. This is an operator-driven clear, so — unlike the
    /// origin-tier probe path — it must never emit `model_fallback_cleared`
    /// even when `notice_sent` was true (the operator caused it; they know).
    #[tokio::test]
    async fn model_pin_command_clears_fallback_override() {
        let socket_path = format!("/tmp/philote-ovpin-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-ov-pin".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-ov-pin");

        let session_id = "sess-ov-pin";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");
        let mut notified_override = test_fallback_override("model", "model.openrouter");
        notified_override.notice_sent = true;
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session")
            .fallback_override = Some(notified_override);

        // Setting a pin clears the override.
        runtime
            .handle_session_control_command(
                Uuid::new_v4(),
                session_id.to_string(),
                "turn-ov-pin-1".into(),
                "123".into(),
                SlashCommand::Model {
                    tier: Some("model.ollama".into()),
                },
            )
            .await
            .expect("pin command");
        {
            let state = runtime.sessions.get(session_id).expect("session");
            assert!(
                state.fallback_override.is_none(),
                "setting a pin must clear the fallback override"
            );
            assert_eq!(state.pinned_tier_role.as_deref(), Some("model.ollama"));
        }

        // A bare /model (pin clear) also clears a lingering override.
        let mut notified_override = test_fallback_override("model", "model.openrouter");
        notified_override.notice_sent = true;
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session")
            .fallback_override = Some(notified_override);
        runtime
            .handle_session_control_command(
                Uuid::new_v4(),
                session_id.to_string(),
                "turn-ov-pin-2".into(),
                "123".into(),
                SlashCommand::Model { tier: None },
            )
            .await
            .expect("pin clear command");
        {
            let state = runtime.sessions.get(session_id).expect("session");
            assert!(
                state.fallback_override.is_none(),
                "clearing the pin must also clear the fallback override"
            );
            assert!(state.pinned_tier_role.is_none());
        }

        let recorded = emitted.lock().unwrap().clone();
        assert!(
            recorded_turn_events(&recorded, "model_fallback_cleared").is_empty(),
            "operator-driven /model clears must never emit model_fallback_cleared, \
             even when notice_sent was true: {recorded:#?}"
        );

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
    }

    // ── Provider 4xx mid-turn switch (forensic 2026-07-08) ───────────────────

    /// The forensic gap end-to-end: a Gemini 400 arriving as an old-controller
    /// envelope (kind=provider_failure, no sub_kind, no error_class) must NOT
    /// fail the turn — the SAME turn must be re-dispatched to the next ladder
    /// tier (model.openrouter under the post-#175 default ladder), a
    /// `provider_switch` turn event must surface, and the fallback reply must
    /// be delivered to the user.
    #[tokio::test]
    async fn provider_400_switches_provider_mid_turn_and_delivers_reply() {
        let socket_path = format!("/tmp/philote-4xxswitch-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-4xx-switch".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-4xx-switch");

        let session_id = "sess-4xx-switch";
        let turn_id = "turn-4xx-switch";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        // Seed an in-flight turn waiting on the tier-0 (gemini) model, bound
        // to a membrane transport target.
        let mut turn = def004_working_turn(turn_id, "hotel.status");
        turn.phase = TurnPhase::WaitingModel;
        turn.pending_tool_call = None;
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(turn);

        // Gemini 400 arrives — old-controller shape: no sub_kind, no
        // error_class, no status. Exactly what died as MODEL_EMPTY_RESPONSE.
        runtime
            .handle_model_response(InboundTaskPayload {
                action: Some("model_response".into()),
                session_id: Some(session_id.into()),
                turn_id: Some(turn_id.into()),
                error: Some(TaskErrorPayload {
                    kind: "provider_failure".into(),
                    message: "Gemini API error (400): Request contains an invalid argument.".into(),
                    component: Some("model-router".into()),
                    provider: Some("gemini".into()),
                    capability: Some("text.generate".into()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .expect("400 must not error the loop");

        // The turn survives, advanced to tier 1 (model.openrouter), still WaitingModel.
        {
            let state = runtime.sessions.get(session_id).expect("session");
            let turn = state
                .active_turn
                .as_ref()
                .expect("turn must survive the 400 — it switches providers, not fails");
            assert_eq!(
                turn.fallback_tier, 1,
                "must advance to the next ladder tier"
            );
            assert_eq!(turn.phase, TurnPhase::WaitingModel);
        }

        // The fallback tier answers on the same turn → reply delivered.
        let final_text = "Openrouter fallback answer.";
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
            .expect("fallback respond");

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();

        // The re-dispatch went to the model.openrouter controller role.
        let redispatches: Vec<_> = emitted
            .iter()
            .filter(|e| e["task"]["action"] == "generate_text")
            .collect();
        assert_eq!(
            redispatches.len(),
            1,
            "exactly one fallback re-dispatch: {:#?}",
            *emitted
        );
        assert_eq!(
            redispatches[0]["target_role"], "model.openrouter",
            "the 400 must engage the static ladder's next tier"
        );

        // A provider_switch turn event surfaced from → to → reason.
        let switches: Vec<_> = emitted
            .iter()
            .filter(|e| {
                e["task"]["action"] == "turn_event" && e["task"]["event"] == "provider_switch"
            })
            .collect();
        assert_eq!(
            switches.len(),
            1,
            "one provider_switch event: {:#?}",
            *emitted
        );
        let detail = switches[0]["task"]["partial_content"]
            .as_str()
            .expect("switch detail");
        assert!(
            detail.contains("gemini") && detail.contains("openrouter"),
            "switch detail must carry from/to providers: {detail}"
        );

        // No failure surfaced — the user got the fallback answer.
        let replies: Vec<_> = emitted
            .iter()
            .filter(|e| e["task"]["action"] == "send_reply")
            .collect();
        assert_eq!(replies.len(), 1, "one final reply: {:#?}", *emitted);
        assert!(
            replies[0]["task"]["content"]
                .as_str()
                .expect("reply content")
                .contains("Openrouter fallback answer"),
            "user must see the fallback answer, not an error: {:#?}",
            replies[0]
        );
        assert!(
            emitted.iter().all(|e| e["heal_event"].is_null()),
            "a successful mid-turn switch must not push heal events: {:#?}",
            *emitted
        );
    }

    // ── Single-tier ladder reachability (defect 2: off-by-one) ───────────────

    /// Regression for the off-by-one: a role with a single-tier ladder whose
    /// *primary* dispatch bypassed the ladder (an explicit per-session
    /// `component_routes` pin won precedence — see
    /// `resolve_model_execution_target`; routing drill 2026-07-09 demoted
    /// the legacy `effective_model_controller` fallback this test used to
    /// use for the bypass below the ladder, so a genuine explicit pin is
    /// used here instead to keep exercising the same bypass class) must
    /// still get a shot at that one ladder tier on failure. Before the fix,
    /// `fallback_tier` started at 0 unconditionally and the walk jumped
    /// straight to tier 1, which immediately exceeded a single-tier
    /// ladder's `max_tier` (0) and skipped
    /// the ladder entirely.
    #[tokio::test]
    async fn single_tier_ladder_reachable_when_primary_bypassed_it() {
        let socket_path = format!("/tmp/philote-1tier-bypass-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-1tier-bypass".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-1tier-bypass");

        let session_id = "sess-1tier-bypass";
        let turn_id = "turn-1tier-bypass";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        // A 1-tier ladder is configured, but an explicit per-session
        // `component_routes` pin (higher precedence than the ladder) is what
        // actually drove the primary dispatch to gemini — the
        // "default-primary role with a 1-tier ladder" case.
        {
            let state = runtime.sessions.get_mut(session_id).expect("session");
            state.role_activation = Some(role_activation_with_ladder(&["model.openrouter"]));
            state.bindings.component_routes.push(ComponentRouteBinding {
                capability: "text.generate".into(),
                selection_mode: "preferred".into(),
                implementation: Some("gemini".into()),
                incarnation: None,
                preferred_hotel_id: None,
                preferred_environment_id: None,
            });
        }

        let mut turn = def004_working_turn(turn_id, "hotel.status");
        turn.phase = TurnPhase::WaitingModel;
        turn.pending_tool_call = None;
        turn.fallback_tier = 0;
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(turn);

        runtime
            .handle_model_response(InboundTaskPayload {
                action: Some("model_response".into()),
                session_id: Some(session_id.into()),
                turn_id: Some(turn_id.into()),
                error: Some(TaskErrorPayload {
                    kind: "provider_failure".into(),
                    message: "Gemini API error (400): Request contains an invalid argument.".into(),
                    component: Some("model-router".into()),
                    provider: Some("gemini".into()),
                    capability: Some("text.generate".into()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .expect("400 must not error the loop");

        // The turn survives and the single ladder tier gets tried, not skipped.
        {
            let state = runtime.sessions.get(session_id).expect("session");
            let turn = state
                .active_turn
                .as_ref()
                .expect("the single ladder tier must be reachable, not skipped to exhaustion");
            assert_eq!(turn.phase, TurnPhase::WaitingModel);
        }

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        let redispatches: Vec<_> = emitted
            .iter()
            .filter(|e| e["task"]["action"] == "generate_text")
            .collect();
        assert_eq!(
            redispatches.len(),
            1,
            "exactly one fallback re-dispatch: {:#?}",
            *emitted
        );
        assert_eq!(
            redispatches[0]["target_role"], "model.openrouter",
            "the single configured ladder tier must be tried on failure: {:#?}",
            *emitted
        );
        assert!(
            emitted.iter().all(|e| e["heal_event"].is_null()),
            "the ladder must not be exhausted after only one failure: {:#?}",
            *emitted
        );
    }

    /// Deeper regression on the same bypass case: `primary_dispatch_used_ladder`
    /// is re-derived from *static* session config on every call, so on its own
    /// it can't distinguish "virgin primary" from "tier 0 already dispatched
    /// via this same bypass path" — both look identical (`current_tier == 0`,
    /// primary still bypasses the ladder per session config). Without a
    /// per-turn marker, a second failure would re-derive the same "None"
    /// verdict and redispatch tier 0 forever, leaking every later tier
    /// (`model.ollama` here) and the routing oracle. `WatchdogTimeout` is used
    /// deliberately — it carries no `failed_provider`, so the contract-failure
    /// skip (which happens to rescue the single-tier case) cannot mask this.
    #[tokio::test]
    async fn multi_tier_bypassed_ladder_walks_forward_across_repeated_failures() {
        let socket_path = format!("/tmp/philote-2tier-bypass-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-2tier-bypass".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-2tier-bypass");

        let session_id = "sess-2tier-bypass";
        let turn_id = "turn-2tier-bypass";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        // Two-tier ladder configured, but an explicit per-session
        // `component_routes` pin wins precedence for the primary dispatch
        // (bypasses the ladder entirely) — see the single-tier variant above
        // for why this uses a genuine explicit pin rather than the legacy
        // `effective_model_controller` fallback this test used pre-routing-drill.
        {
            let state = runtime.sessions.get_mut(session_id).expect("session");
            state.role_activation = Some(role_activation_with_ladder(&[
                "model.openrouter",
                "model.ollama",
            ]));
            state.bindings.component_routes.push(ComponentRouteBinding {
                capability: "text.generate".into(),
                selection_mode: "preferred".into(),
                implementation: Some("gemini".into()),
                incarnation: None,
                preferred_hotel_id: None,
                preferred_environment_id: None,
            });
        }

        let mut turn = def004_working_turn(turn_id, "hotel.status");
        turn.phase = TurnPhase::WaitingModel;
        turn.pending_tool_call = None;
        turn.fallback_tier = 0;
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(turn);

        // First WatchdogTimeout: the bypassed ladder's tiers[0] must be tried.
        runtime
            .advance_turn_to_next_fallback_tier(
                session_id.to_string(),
                turn_id.to_string(),
                NoResponseClass::WatchdogTimeout,
                None,
                "watchdog timeout".into(),
            )
            .await
            .expect("first escalation");
        {
            let state = runtime.sessions.get(session_id).expect("session");
            let turn = state.active_turn.as_ref().expect("turn survives tier 0");
            assert_eq!(turn.fallback_tier, 0, "lands on ladder tier 0");
            assert!(
                turn.ladder_tier0_dispatched,
                "the ladder-engaged marker must flip once tier 0 is dispatched"
            );
        }

        // Second WatchdogTimeout: must advance to tiers[1] ("model.ollama"),
        // not redispatch tiers[0] again.
        runtime
            .advance_turn_to_next_fallback_tier(
                session_id.to_string(),
                turn_id.to_string(),
                NoResponseClass::WatchdogTimeout,
                None,
                "watchdog timeout".into(),
            )
            .await
            .expect("second escalation");
        {
            let state = runtime.sessions.get(session_id).expect("session");
            let turn = state.active_turn.as_ref().expect("turn survives tier 1");
            assert_eq!(
                turn.fallback_tier, 1,
                "must advance past tier 0, not redispatch it"
            );
        }

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        let redispatches: Vec<_> = emitted
            .iter()
            .filter(|e| e["task"]["action"] == "generate_text")
            .map(|e| e["target_role"].as_str().unwrap_or("").to_string())
            .collect();
        assert_eq!(
            redispatches,
            vec!["model.openrouter".to_string(), "model.ollama".to_string()],
            "the walk must cover each ladder entry exactly once, in order: {:#?}",
            redispatches
        );
    }

    /// Layer 1 isolation, driven through the real fallback-tier *advance*
    /// dispatch (`turn_loop.rs`'s `configured_model_bindings`, not just the
    /// primary-dispatch helper covered by the `role_model_binding_*` tests
    /// above): "Aria" has her OWN `model_bindings`, distinct per tier and
    /// distinct from any other agent's or the provider's global default.
    /// Proves each fallback tier resolves its own bound model — the routing
    /// drill's headline complaint was Aria's openrouter *fallback* silently
    /// inheriting Jane's GLM-5.2 via the shared global
    /// `openrouter_default_model`; per-agent `model_bindings` is the fix.
    #[tokio::test]
    async fn aria_fallback_tier_uses_her_own_model_binding_not_a_shared_default() {
        let socket_path = format!("/tmp/philote-aria-binding-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-aria".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-aria");

        let session_id = "sess-aria-binding";
        let turn_id = "turn-aria-binding";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        // Two-tier ladder, primary dispatch bypassed (explicit component_routes
        // pin, same harness pattern as the off-by-one tests above) so both
        // tier 0 and tier 1 get a real dispatch through
        // `advance_turn_to_next_fallback_tier` in this synthetic test. Aria's
        // bindings are deliberately NOT "z-ai/glm-5.2" (Jane's model from the
        // routing drill) to prove isolation, not just presence.
        {
            let state = runtime.sessions.get_mut(session_id).expect("session");
            state.role_activation = Some(role_activation_with_ladder_and_bindings(
                &["model.openrouter", "model.ollama"],
                &[
                    ("model.openrouter", "aria/openrouter-preferred"),
                    ("model.ollama", "aria/ollama-preferred"),
                ],
            ));
            state.bindings.component_routes.push(ComponentRouteBinding {
                capability: "text.generate".into(),
                selection_mode: "preferred".into(),
                implementation: Some("gemini".into()),
                incarnation: None,
                preferred_hotel_id: None,
                preferred_environment_id: None,
            });
        }

        let mut turn = def004_working_turn(turn_id, "hotel.status");
        turn.phase = TurnPhase::WaitingModel;
        turn.pending_tool_call = None;
        turn.fallback_tier = 0;
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(turn);

        // Tier 0 (bypassed primary gets its shot): must carry Aria's
        // model.openrouter binding, not Jane's glm-5.2 or an unset default.
        runtime
            .advance_turn_to_next_fallback_tier(
                session_id.to_string(),
                turn_id.to_string(),
                NoResponseClass::WatchdogTimeout,
                None,
                "watchdog timeout".into(),
            )
            .await
            .expect("first escalation");

        // Tier 1: must carry Aria's OWN model.ollama binding — a distinct
        // value per tier, not a single agent-wide model pinned once.
        runtime
            .advance_turn_to_next_fallback_tier(
                session_id.to_string(),
                turn_id.to_string(),
                NoResponseClass::WatchdogTimeout,
                None,
                "watchdog timeout".into(),
            )
            .await
            .expect("second escalation");

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        let dispatches: Vec<(String, Option<String>)> = emitted
            .iter()
            .filter(|e| e["task"]["action"] == "generate_text")
            .map(|e| {
                (
                    e["target_role"].as_str().unwrap_or("").to_string(),
                    e["task"]["model"].as_str().map(str::to_string),
                )
            })
            .collect();

        assert_eq!(
            dispatches,
            vec![
                (
                    "model.openrouter".to_string(),
                    Some("aria/openrouter-preferred".to_string())
                ),
                (
                    "model.ollama".to_string(),
                    Some("aria/ollama-preferred".to_string())
                ),
            ],
            "each fallback tier must carry Aria's OWN per-tier bound model, \
             not Jane's z-ai/glm-5.2 or a shared global default: {:#?}",
            dispatches
        );
        assert!(
            dispatches
                .iter()
                .all(|(_, model)| model.as_deref() != Some("z-ai/glm-5.2")),
            "Aria's dispatches must never carry Jane's bound model: {:#?}",
            dispatches
        );
    }

    /// Counterpart: when the primary dispatch *did* come from the ladder
    /// (tiers[0], per defect-1's fixed precedence), a single-tier ladder is
    /// correctly exhausted on the next failure — there is nothing left to
    /// retry, so the turn falls through to the routing oracle (which the stub
    /// hotel answers with no ranked data) and fails cleanly instead of
    /// looping back onto the tier it already tried.
    #[tokio::test]
    async fn single_tier_ladder_engaged_as_primary_exhausts_cleanly_on_failure() {
        let socket_path = format!(
            "/tmp/philote-1tier-engaged-{}.sock",
            Uuid::new_v4().simple()
        );
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-1tier-engaged".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-1tier-engaged");

        let session_id = "sess-1tier-engaged";
        let turn_id = "turn-1tier-engaged";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        // No hotel route, no explicit binding: per resolve_model_execution_target's
        // precedence the 1-tier ladder's tiers[0] ("model.openrouter") IS the
        // primary dispatch.
        {
            let state = runtime.sessions.get_mut(session_id).expect("session");
            state.role_activation = Some(role_activation_with_ladder(&["model.openrouter"]));
        }
        assert!(
            primary_dispatch_used_ladder(runtime.sessions.get(session_id), "text.generate"),
            "sanity: the ladder must be the primary source in this fixture"
        );

        let mut turn = def004_working_turn(turn_id, "hotel.status");
        turn.phase = TurnPhase::WaitingModel;
        turn.pending_tool_call = None;
        turn.fallback_tier = 0;
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(turn);

        runtime
            .handle_model_response(InboundTaskPayload {
                action: Some("model_response".into()),
                session_id: Some(session_id.into()),
                turn_id: Some(turn_id.into()),
                error: Some(TaskErrorPayload {
                    kind: "provider_failure".into(),
                    message: "OpenRouter API error (500): upstream unavailable.".into(),
                    component: Some("model-router".into()),
                    provider: Some("openrouter".into()),
                    capability: Some("text.generate".into()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .expect("failure must not error the loop");

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        // No second ladder dispatch — the single tier already served as
        // primary, so there was nothing left in the ladder to retry.
        let redispatches: Vec<_> = emitted
            .iter()
            .filter(|e| e["task"]["action"] == "generate_text")
            .collect();
        assert!(
            redispatches.is_empty(),
            "no ladder tier remains to redispatch to: {:#?}",
            *emitted
        );
        let heal_events: Vec<_> = emitted
            .iter()
            .filter(|e| !e["heal_event"].is_null())
            .collect();
        assert_eq!(
            heal_events.len(),
            1,
            "exhaustion must push exactly one heal event: {:#?}",
            *emitted
        );
    }

    /// Fatal auth failures fail the turn fast (no pointless provider ladder
    /// walk with a dead key) and flag the heal queue so the outage becomes an
    /// operator work item.
    #[tokio::test]
    async fn fatal_auth_failure_fails_fast_and_pushes_heal_event() {
        let socket_path = format!("/tmp/philote-authfatal-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-auth-fatal".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-auth-fatal");

        let session_id = "sess-auth-fatal";
        let turn_id = "turn-auth-fatal";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        let mut turn = def004_working_turn(turn_id, "hotel.status");
        turn.phase = TurnPhase::WaitingModel;
        turn.pending_tool_call = None;
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .start_turn(turn);

        runtime
            .handle_model_response(InboundTaskPayload {
                action: Some("model_response".into()),
                session_id: Some(session_id.into()),
                turn_id: Some(turn_id.into()),
                error: Some(TaskErrorPayload {
                    kind: "provider_failure".into(),
                    message: "Gemini API error (400): API key expired.".into(),
                    provider: Some("gemini".into()),
                    capability: Some("text.generate".into()),
                    retryable: Some(false),
                    sub_kind: Some("provider_auth".into()),
                    error_class: Some("fatal".into()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .await
            .expect("auth failure handled");

        assert!(
            runtime
                .sessions
                .get(session_id)
                .expect("session")
                .active_turn
                .is_none(),
            "fatal auth must fail the turn fast"
        );

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        assert!(
            !emitted
                .iter()
                .any(|e| e["task"]["action"] == "generate_text"),
            "no ladder walk with a dead key: {:#?}",
            *emitted
        );
        let heal_events: Vec<_> = emitted
            .iter()
            .filter(|e| !e["heal_event"].is_null())
            .collect();
        assert_eq!(heal_events.len(), 1, "one heal event: {:#?}", *emitted);
        assert_eq!(
            heal_events[0]["heal_event"]["pattern_tag"],
            "provider_auth:gemini"
        );
        assert!(
            emitted.iter().any(|e| {
                e["task"]["action"] == "send_reply"
                    && e["task"]["content"]
                        .as_str()
                        .is_some_and(|c| c.contains("rejected its credentials"))
            }),
            "user must get a fast, clear auth notice: {:#?}",
            *emitted
        );
    }

    /// Wiring guard: a role whose `TurnLoopConfig` carries paracrine-budget
    /// overrides must have them applied to the live session's `ExecutionPolicy`
    /// when the role is activated via a handoff bundle — not merely stored on the
    /// role record. This is the assertion that survives refactors of the
    /// activation sites.
    #[tokio::test]
    async fn role_activation_applies_paracrine_budget_override() {
        let socket_path = format!("/tmp/philote-roleovr-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-roleovr".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-roleovr");

        // Register a role whose TurnLoopConfig tightens the paracrine budgets.
        runtime.configured_roles.insert(
            "specialist".to_string(),
            CachedRoleConfig {
                toolset_profile: "default".into(),
                role_identity_addendum: None,
                role_manifest: None,
                iteration_cap: None,
                approval_policy: None,
                turn_loop_config: ansible_mesh_core::graph::TurnLoopConfig {
                    paracrine_hop_budget: Some(9),
                    paracrine_chain_budget_secs: Some(120),
                    ..Default::default()
                },
                content_policy: "standard".into(),
            },
        );

        let session_id = "sess-roleovr";
        let task_id = Uuid::new_v4();
        let bundle = philotic_client::HandoffBundle {
            to_role: Some("specialist".into()),
            from_role: Some("orchestrator".into()),
            handoff_reason: Some("test".into()),
            working_summary: Some("do the thing".into()),
            ..Default::default()
        };
        runtime
            .handle_handoff_bundle(
                InboundTaskPayload {
                    action: Some("handoff_bundle".into()),
                    session_id: Some(session_id.into()),
                    turn_id: Some("turn-roleovr".into()),
                    handoff_bundle: Some(bundle),
                    ..Default::default()
                },
                task_id,
            )
            .await
            .expect("handoff bundle");

        let exec = &runtime
            .session(session_id)
            .expect("session exists")
            .settings
            .execution;
        assert_eq!(
            exec.paracrine_hop_budget, 9,
            "role TurnLoopConfig hop budget must be applied at activation"
        );
        assert_eq!(
            exec.paracrine_chain_budget_secs, 120,
            "role TurnLoopConfig time budget must be applied at activation"
        );

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
    }

    /// Wiring guard: a role whose `TurnLoopConfig` carries context-window
    /// overrides must have them applied to the live session's `ContextWindowPolicy`
    /// at handoff-bundle activation, and reverted to the session baseline on
    /// handoff-return. This is the assertion that survives refactors of the
    /// activation/return sites — a helper-only test would miss the wire-up.
    #[tokio::test]
    async fn role_activation_applies_and_restores_context_window() {
        let socket_path = format!("/tmp/philote-ctxwin-{}.sock", Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));

        let identity = philotic_client::GuestIdentity {
            guest_id: "agent-ctxwin".into(),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let mut runtime = AgentRuntime::new(client, "agent-ctxwin");

        // Register a terse specialist that tightens the dialogue-window budgets.
        runtime.configured_roles.insert(
            "specialist".to_string(),
            CachedRoleConfig {
                toolset_profile: "default".into(),
                role_identity_addendum: None,
                role_manifest: None,
                iteration_cap: None,
                approval_policy: None,
                turn_loop_config: ansible_mesh_core::graph::TurnLoopConfig {
                    context_window: Some(ansible_mesh_core::graph::ContextWindowOverrides {
                        dialogue_window_chars: Some(2_000),
                        max_tool_history_entries: Some(5),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                content_policy: "standard".into(),
            },
        );

        let session_id = "sess-ctxwin";
        let bundle = philotic_client::HandoffBundle {
            to_role: Some("specialist".into()),
            from_role: Some("orchestrator".into()),
            handoff_reason: Some("test".into()),
            working_summary: Some("do the thing".into()),
            ..Default::default()
        };
        runtime
            .handle_handoff_bundle(
                InboundTaskPayload {
                    action: Some("handoff_bundle".into()),
                    session_id: Some(session_id.into()),
                    turn_id: Some("turn-ctxwin".into()),
                    handoff_bundle: Some(bundle),
                    ..Default::default()
                },
                Uuid::new_v4(),
            )
            .await
            .expect("handoff bundle");

        {
            let cw = &runtime
                .session(session_id)
                .expect("session exists")
                .settings
                .context_window;
            assert_eq!(
                cw.dialogue_window_chars, 2_000,
                "role context-window override must be applied at activation"
            );
            assert_eq!(
                cw.max_tool_history_entries, 5,
                "role tool-history override must be applied at activation"
            );
        }

        // Now hand control back to the orchestrator; the baseline must return.
        runtime
            .handle_handoff_return(
                InboundTaskPayload {
                    action: Some("handoff_return".into()),
                    session_id: Some(session_id.into()),
                    turn_id: Some("turn-ctxwin-return".into()),
                    ..Default::default()
                },
                Uuid::new_v4(),
            )
            .await
            .expect("handoff return");

        {
            let state = runtime.session(session_id).expect("session exists");
            let defaults = crate::session::ContextWindowPolicy::default();
            assert_eq!(
                state.settings.context_window, defaults,
                "context-window policy must be restored to the session baseline on return"
            );
            assert!(
                state.base_context_window.is_none(),
                "baseline snapshot must be cleared on return"
            );
        }

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
    }

    // ── Plan-eval-repeat loop ───────────────────────────────────────────────

    /// Serializes tests that read or mutate PHILOTIC_DISABLE_PLAN_CONTINUATION,
    /// since env vars are process-global and tests run concurrently.
    static PLAN_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn plan_env_guard() -> std::sync::MutexGuard<'static, ()> {
        PLAN_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn plan_with_statuses(goal: &str, statuses: &[&str]) -> ActivePlan {
        ActivePlan {
            goal: goal.into(),
            steps: statuses
                .iter()
                .enumerate()
                .map(|(i, st)| PlanStep {
                    id: i as u32 + 1,
                    description: format!("work item {}", i + 1),
                    tool_name: None,
                    status: (*st).to_string(),
                })
                .collect(),
            status: "executing".into(),
            context_1_advisory: None,
        }
    }

    async fn plan_test_runtime(
        tag: &str,
    ) -> (
        AgentRuntime,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        tokio::task::JoinHandle<()>,
        String,
    ) {
        let socket_path = format!("/tmp/philote-{}-{}.sock", tag, Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_recording_hotel(listener, emitted.clone()));
        let identity = philotic_client::GuestIdentity {
            guest_id: format!("agent-{tag}"),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let runtime = AgentRuntime::new(client, format!("agent-{tag}"));
        (runtime, emitted, server, socket_path)
    }

    fn respond_payload(session_id: &str, turn_id: &str, text: &str) -> InboundTaskPayload {
        InboundTaskPayload {
            action: Some("model_response".into()),
            session_id: Some(session_id.into()),
            turn_id: Some(turn_id.into()),
            agent_action: Some(serde_json::json!({
                "kind": "respond",
                "content": text,
            })),
            content: Some(text.into()),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn plan_continuation_synthesized_after_partial_plan_respond() {
        let _guard = plan_env_guard();
        unsafe {
            std::env::remove_var("PHILOTIC_DISABLE_PLAN_CONTINUATION");
        }
        let (mut runtime, emitted, server, socket_path) = plan_test_runtime("plancont").await;
        let session_id = "sess-plancont";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        // Turn completing with a 3-step plan, only step 1 model-reported done.
        {
            let mut turn = test_working_turn(TurnPhase::WaitingModel);
            turn.turn_id = "turn-plan-1".into();
            turn.active_plan = Some(plan_with_statuses(
                "ship it",
                &["done", "pending", "pending"],
            ));
            runtime
                .sessions
                .get_mut(session_id)
                .expect("session")
                .start_turn(turn);
        }

        runtime
            .handle_model_response(respond_payload(
                session_id,
                "turn-plan-1",
                "progress so far",
            ))
            .await
            .expect("respond");

        // Carryover persisted with the eval flags; one continuation charged.
        {
            let state = runtime.session(session_id).expect("session");
            let carry = state.carryover_plan.as_ref().expect("carryover persisted");
            assert_eq!(carry.steps_done, vec![true, false, false]);
            assert_eq!(carry.continuations_used, 1);
            assert_eq!(carry.created_turn_id, "turn-plan-1");
        }

        // Exactly one continuation task queued through pending_drains.
        assert_eq!(runtime.pending_drains.len(), 1, "one continuation queued");
        let (drain_id, drain_task) = runtime.pending_drains.pop_front().expect("drain");
        assert_eq!(drain_task.action.as_deref(), Some("plan_continuation"));
        let brief = drain_task.content.clone().expect("brief content");
        assert!(brief.contains("[Plan continuation 1/3]"), "{brief}");
        assert!(brief.contains("work item 2"), "{brief}");
        assert!(brief.contains("work item 3"), "{brief}");
        assert!(
            !brief.contains("Completed steps:\n- step 2"),
            "done list must not include pending steps: {brief}"
        );

        // Dispatching the continuation seeds the new turn with the carried-over
        // plan (done steps marked) and enters pre-confirmed.
        runtime
            .handle_user_message(drain_task, drain_id)
            .await
            .expect("continuation turn starts");
        {
            let state = runtime.session(session_id).expect("session");
            let turn = state.active_turn.as_ref().expect("continuation turn");
            assert!(turn.plan_confirmed, "continuation must be pre-confirmed");
            let plan = turn.active_plan.as_ref().expect("plan seeded");
            assert_eq!(plan.steps[0].status, "done");
            assert_eq!(plan.steps[1].status, "pending");
        }

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        let plan_evals: Vec<_> = emitted
            .iter()
            .filter(|e| e["task"]["action"] == "turn_event" && e["task"]["event"] == "plan_eval")
            .collect();
        assert_eq!(plan_evals.len(), 1, "one plan_eval event: {:#?}", *emitted);
        let detail: serde_json::Value = serde_json::from_str(
            plan_evals[0]["task"]["partial_content"]
                .as_str()
                .expect("plan_eval detail"),
        )
        .expect("plan_eval detail is JSON");
        assert_eq!(detail["verdict"], "continue");
        assert_eq!(detail["basis"], "model_reported");
        assert_eq!(detail["steps_done"], 1);
        assert_eq!(detail["steps_total"], 3);
        assert!(
            emitted.iter().any(|e| e["task"]["action"] == "turn_event"
                && e["task"]["event"] == "plan_continuation"),
            "plan_continuation event must be emitted: {:#?}",
            *emitted
        );
        assert!(
            emitted
                .iter()
                .any(|e| e["task"]["action"] == "generate_text"),
            "continuation turn must re-enter the model"
        );
    }

    #[tokio::test]
    async fn plan_continuation_budget_exhaustion_notifies_and_clears() {
        let _guard = plan_env_guard();
        unsafe {
            std::env::remove_var("PHILOTIC_DISABLE_PLAN_CONTINUATION");
        }
        let (mut runtime, emitted, server, socket_path) = plan_test_runtime("planbudget").await;
        let session_id = "sess-planbudget";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        {
            let state = runtime.sessions.get_mut(session_id).expect("session");
            // Carryover already spent the full default budget (3).
            state.carryover_plan = Some(CarryoverPlan {
                plan: plan_with_statuses("ship it", &["done", "pending", "pending"]),
                steps_done: vec![true, false, false],
                continuations_used: 3,
                created_turn_id: "turn-origin".into(),
            });
            // This continuation made progress (step 2 done) but one step remains.
            let mut turn = test_working_turn(TurnPhase::WaitingModel);
            turn.turn_id = "turn-plan-4".into();
            turn.active_plan = Some(plan_with_statuses("ship it", &["done", "done", "pending"]));
            state.start_turn(turn);
        }

        runtime
            .handle_model_response(respond_payload(session_id, "turn-plan-4", "more progress"))
            .await
            .expect("respond");

        let state = runtime.session(session_id).expect("session");
        assert!(
            state.carryover_plan.is_none(),
            "carryover must be cleared when the budget is exhausted"
        );
        assert!(
            runtime.pending_drains.is_empty(),
            "no further continuation may be synthesized"
        );

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        let notices: Vec<_> = emitted
            .iter()
            .filter(|e| {
                e["task"]["action"] == "send_reply"
                    && e["task"]["content"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("budget")
            })
            .collect();
        assert_eq!(notices.len(), 1, "one stop notice: {:#?}", *emitted);
        let notice = notices[0]["task"]["content"].as_str().unwrap();
        assert!(notice.contains("2/3 steps done"), "{notice}");
        assert!(notice.contains("work item 3"), "{notice}");
        assert!(notice.contains("/plan drop"), "{notice}");
    }

    #[tokio::test]
    async fn plan_continuation_kill_switch_disables_carryover() {
        let _guard = plan_env_guard();
        unsafe {
            std::env::set_var("PHILOTIC_DISABLE_PLAN_CONTINUATION", "1");
        }
        let (mut runtime, emitted, server, socket_path) = plan_test_runtime("plankill").await;
        let session_id = "sess-plankill";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        {
            let mut turn = test_working_turn(TurnPhase::WaitingModel);
            turn.turn_id = "turn-plan-k".into();
            turn.active_plan = Some(plan_with_statuses("ship it", &["done", "pending"]));
            runtime
                .sessions
                .get_mut(session_id)
                .expect("session")
                .start_turn(turn);
        }

        let result = runtime
            .handle_model_response(respond_payload(session_id, "turn-plan-k", "partial"))
            .await;
        unsafe {
            std::env::remove_var("PHILOTIC_DISABLE_PLAN_CONTINUATION");
        }
        result.expect("respond");

        let state = runtime.session(session_id).expect("session");
        assert!(
            state.carryover_plan.is_none(),
            "kill switch must prevent carryover persistence"
        );
        assert!(
            runtime.pending_drains.is_empty(),
            "kill switch must prevent continuation synthesis"
        );

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        assert!(
            emitted
                .iter()
                .any(|e| e["task"]["action"] == "turn_event" && e["task"]["event"] == "plan_eval"),
            "plan_eval event still emitted for observability"
        );
        assert!(
            !emitted.iter().any(|e| e["task"]["action"] == "turn_event"
                && e["task"]["event"] == "plan_continuation"),
            "no continuation event under kill switch"
        );
    }

    #[tokio::test]
    async fn new_plan_proposal_replaces_carryover() {
        let (mut runtime, _emitted, server, socket_path) = plan_test_runtime("planredir").await;
        let session_id = "sess-planredir";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        {
            let state = runtime.sessions.get_mut(session_id).expect("session");
            state.carryover_plan = Some(CarryoverPlan {
                plan: plan_with_statuses("old goal", &["done", "pending"]),
                steps_done: vec![true, false],
                continuations_used: 1,
                created_turn_id: "turn-old".into(),
            });
            let mut turn = test_working_turn(TurnPhase::Thinking);
            turn.turn_id = "turn-redirect".into();
            state.start_turn(turn);
        }

        runtime
            .handle_plan_proposal(
                session_id.to_string(),
                "turn-redirect".to_string(),
                PlanProposalAction {
                    summary: "brand new direction".into(),
                    steps: vec![serde_json::json!({"description": "step one"})],
                    approval_risk_hint: None,
                },
            )
            .await
            .expect("plan proposal");

        let state = runtime.session(session_id).expect("session");
        assert!(
            state.carryover_plan.is_none(),
            "a new plan proposal must replace the carried-over plan"
        );
        assert!(
            state.parked_plan_turn.is_some(),
            "proposal parks the turn for operator confirmation"
        );

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
    }

    #[tokio::test]
    async fn queued_user_task_defers_plan_continuation() {
        let _guard = plan_env_guard();
        unsafe {
            std::env::remove_var("PHILOTIC_DISABLE_PLAN_CONTINUATION");
        }
        let (mut runtime, _emitted, server, socket_path) = plan_test_runtime("plandefer").await;
        let session_id = "sess-plandefer";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        {
            let state = runtime.sessions.get_mut(session_id).expect("session");
            let mut turn = test_working_turn(TurnPhase::WaitingModel);
            turn.turn_id = "turn-plan-d".into();
            turn.active_plan = Some(plan_with_statuses("ship it", &["done", "pending"]));
            state.start_turn(turn);
            // A user message arrived mid-turn and was queued.
            state.enqueue_user_task(
                Uuid::new_v4(),
                InboundTaskPayload {
                    session_id: Some(session_id.into()),
                    content: Some("quick interrupt question".into()),
                    ..Default::default()
                },
            );
        }

        runtime
            .handle_model_response(respond_payload(session_id, "turn-plan-d", "partial"))
            .await
            .expect("respond");

        // The drained task must be the user's, not a continuation.
        assert_eq!(runtime.pending_drains.len(), 1);
        let (_, drained) = runtime.pending_drains.pop_front().expect("drain");
        assert_ne!(drained.action.as_deref(), Some("plan_continuation"));
        assert_eq!(drained.content.as_deref(), Some("quick interrupt question"));
        // Carryover retained with the budget uncharged — resumes later.
        {
            let carry = runtime
                .session(session_id)
                .expect("session")
                .carryover_plan
                .clone()
                .expect("carryover retained during deferral");
            assert_eq!(carry.continuations_used, 0);
        }

        // The interleaved user turn completes without a plan of its own →
        // the deferred carryover resumes with a synthesized continuation.
        {
            let state = runtime.sessions.get_mut(session_id).expect("session");
            let mut turn = test_working_turn(TurnPhase::WaitingModel);
            turn.turn_id = "turn-user-1".into();
            state.start_turn(turn);
        }
        runtime
            .handle_model_response(respond_payload(session_id, "turn-user-1", "answered"))
            .await
            .expect("user turn respond");

        assert_eq!(runtime.pending_drains.len(), 1, "carryover resumed");
        let (_, resumed) = runtime.pending_drains.pop_front().expect("drain");
        assert_eq!(resumed.action.as_deref(), Some("plan_continuation"));
        assert_eq!(
            runtime
                .session(session_id)
                .expect("session")
                .carryover_plan
                .as_ref()
                .expect("carryover still present")
                .continuations_used,
            1
        );

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
    }

    #[tokio::test]
    async fn plan_command_shows_and_drops_carryover() {
        let (mut runtime, emitted, server, socket_path) = plan_test_runtime("plancmd").await;
        let session_id = "sess-plancmd";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        runtime
            .sessions
            .get_mut(session_id)
            .expect("session")
            .carryover_plan = Some(CarryoverPlan {
            plan: plan_with_statuses("ship it", &["done", "pending"]),
            steps_done: vec![true, false],
            continuations_used: 1,
            created_turn_id: "turn-origin".into(),
        });

        // /plan → status report.
        runtime
            .handle_plan_command(
                Uuid::new_v4(),
                session_id.to_string(),
                "turn-cmd-1".to_string(),
                "555".to_string(),
                "membrane-node-01".to_string(),
                "membrane".to_string(),
                None,
                false,
            )
            .await
            .expect("plan status");
        assert!(
            runtime
                .session(session_id)
                .expect("session")
                .carryover_plan
                .is_some(),
            "status must not clear the carryover"
        );

        // /plan drop → cleared.
        runtime
            .handle_plan_command(
                Uuid::new_v4(),
                session_id.to_string(),
                "turn-cmd-2".to_string(),
                "555".to_string(),
                "membrane-node-01".to_string(),
                "membrane".to_string(),
                None,
                true,
            )
            .await
            .expect("plan drop");
        assert!(
            runtime
                .session(session_id)
                .expect("session")
                .carryover_plan
                .is_none(),
            "drop must clear the carryover"
        );

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);

        let emitted = emitted.lock().unwrap();
        let replies: Vec<&str> = emitted
            .iter()
            .filter(|e| e["task"]["action"] == "send_reply")
            .filter_map(|e| e["task"]["content"].as_str())
            .collect();
        assert!(
            replies.iter().any(|r| r.contains("ship it")
                && r.contains("1/2 steps done")
                && r.contains("work item 2")),
            "status reply must describe the carryover: {replies:#?}"
        );
        assert!(
            replies.iter().any(|r| r.contains("dropped")),
            "drop reply must confirm: {replies:#?}"
        );
    }

    #[tokio::test]
    async fn orphaned_plan_continuation_is_dropped() {
        let (mut runtime, _emitted, server, socket_path) = plan_test_runtime("planorphan").await;
        let session_id = "sess-planorphan";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        // No carryover exists (e.g. operator ran /plan drop) — a stale
        // continuation task must be dropped without starting a turn.
        runtime
            .handle_user_message(
                InboundTaskPayload {
                    action: Some("plan_continuation".into()),
                    session_id: Some(session_id.into()),
                    content: Some("[Plan continuation 1/3] Continue executing".into()),
                    ..Default::default()
                },
                Uuid::new_v4(),
            )
            .await
            .expect("orphan handled");

        assert!(
            runtime
                .session(session_id)
                .expect("session")
                .active_turn
                .is_none(),
            "orphaned continuation must not start a turn"
        );

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
    }

    /// A session with an operator pin (`/model <tier>`) must tag new turns
    /// `OperatorExplicit` — this is what makes `advance_turn_to_next_fallback_tier`
    /// refuse to auto-escalate a pinned session.
    #[tokio::test]
    async fn pinned_session_new_turn_gets_operator_explicit_selection_source() {
        let (mut runtime, _emitted, server, socket_path) = plan_test_runtime("pinselect").await;
        let session_id = "sess-pinselect";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");
        runtime
            .sessions
            .get_mut(session_id)
            .expect("session exists")
            .pinned_tier_role = Some("model.ollama".into());

        runtime
            .handle_user_message(
                InboundTaskPayload {
                    session_id: Some(session_id.into()),
                    content: Some("hello".into()),
                    ..Default::default()
                },
                Uuid::new_v4(),
            )
            .await
            .expect("turn started");

        let turn = runtime
            .session(session_id)
            .expect("session")
            .active_turn
            .as_ref()
            .expect("turn started");
        assert_eq!(turn.selection_source, SelectionSource::OperatorExplicit);

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
    }

    /// A task delivered by the aiua CronTicker (`cron_job_id` set) must tag
    /// its turn `CronPrimary` — the honest marker per `InboundTaskPayload`,
    /// not string-sniffing corr_id/source.
    #[tokio::test]
    async fn cron_task_new_turn_gets_cron_primary_selection_source() {
        let (mut runtime, _emitted, server, socket_path) = plan_test_runtime("cronselect").await;
        let session_id = "sess-cronselect";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        runtime
            .handle_user_message(
                InboundTaskPayload {
                    session_id: Some(session_id.into()),
                    content: Some("scheduled heartbeat".into()),
                    cron_job_id: Some("job-1".into()),
                    ..Default::default()
                },
                Uuid::new_v4(),
            )
            .await
            .expect("turn started");

        let turn = runtime
            .session(session_id)
            .expect("session")
            .active_turn
            .as_ref()
            .expect("turn started");
        assert_eq!(turn.selection_source, SelectionSource::CronPrimary);

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
    }

    /// Operator-authored cron preapproval: `cron_preapproved_tools` on a
    /// cron-delivered task must seed the session's approval policy so the
    /// unattended fire can execute its named tools instead of parking
    /// WaitingApproval. (aiua only forwards the field for operator-created
    /// jobs — see CronTicker tests.)
    #[tokio::test]
    async fn cron_preapproved_tools_seed_session_approval_policy() {
        let (mut runtime, _emitted, server, socket_path) = plan_test_runtime("cronpreapp").await;
        let session_id = "cron:ephemeral:agent-cronpreapp";
        runtime
            .ensure_session_loaded(session_id, "cron")
            .await
            .expect("session load");

        runtime
            .handle_user_message(
                InboundTaskPayload {
                    session_id: Some(session_id.into()),
                    content: Some("run the nightly backup".into()),
                    cron_job_id: Some("job-backup".into()),
                    cron_preapproved_tools: vec!["bash.exec".into(), "bash.exec".into()],
                    ..Default::default()
                },
                Uuid::new_v4(),
            )
            .await
            .expect("turn started");

        let policy = &runtime
            .session(session_id)
            .expect("session")
            .approval_policy;
        assert_eq!(
            policy.preapproved_tools,
            vec!["bash.exec".to_string()],
            "cron preapproval must seed the session policy exactly once"
        );

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
    }

    /// A non-cron task must ignore `cron_preapproved_tools` even if present —
    /// only the CronTicker delivery path is trusted to carry it.
    #[tokio::test]
    async fn non_cron_task_ignores_preapproved_tools() {
        let (mut runtime, _emitted, server, socket_path) = plan_test_runtime("nocronpre").await;
        let session_id = "sess-nocronpre";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        runtime
            .handle_user_message(
                InboundTaskPayload {
                    session_id: Some(session_id.into()),
                    content: Some("hello".into()),
                    cron_preapproved_tools: vec!["bash.exec".into()],
                    ..Default::default()
                },
                Uuid::new_v4(),
            )
            .await
            .expect("turn started");

        assert!(
            runtime
                .session(session_id)
                .expect("session")
                .approval_policy
                .preapproved_tools
                .is_empty(),
            "non-cron tasks must not seed approval policy"
        );

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
    }

    /// DEF-051: the model request must carry the dispatching philote's exact
    /// IPC guest identity as `reply_guest_id`. With a roster philote and a
    /// role-incarnation philote both subscribed under role="agent" for the
    /// same agent, aiua's `agent_id` routing fallback is ambiguous — the
    /// response can be consumed (and dropped as "no active turn") by the
    /// sibling process, stranding the real turn in WaitingModel until the
    /// 600s watchdog eviction. `reply_guest_id` feeds aiua's
    /// `explicit_response_guest_from_payload`, which routes the response back
    /// to the instance that owns the turn.
    #[tokio::test]
    async fn model_request_carries_incarnation_guest_identity() {
        let (mut runtime, emitted, server, socket_path) = plan_test_runtime("guestroute").await;
        runtime.set_role_name("orchestrator");
        let session_id = "cron:ephemeral:agent-guestroute";
        runtime
            .ensure_session_loaded(session_id, "cron")
            .await
            .expect("session load");

        runtime
            .handle_user_message(
                InboundTaskPayload {
                    session_id: Some(session_id.into()),
                    content: Some("run the nightly backup".into()),
                    cron_job_id: Some("job-backup".into()),
                    ..Default::default()
                },
                Uuid::new_v4(),
            )
            .await
            .expect("turn started");

        {
            let emitted = emitted.lock().unwrap();
            let model_req = emitted
                .iter()
                .find(|e| e["task"]["action"] == "generate_text")
                .expect("model request emitted");
            assert_eq!(
                model_req["task"]["reply_guest_id"], "agent-guestroute:orchestrator",
                "model request must carry the incarnation's exact guest identity"
            );
        }

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
    }

    /// Roster philote (no role name set): `reply_guest_id` is the bare agent
    /// id, matching its IPC registration in `main.rs`.
    #[tokio::test]
    async fn model_request_carries_roster_guest_identity() {
        let (mut runtime, emitted, server, socket_path) = plan_test_runtime("rosterroute").await;
        let session_id = "sess-rosterroute";
        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");

        runtime
            .handle_user_message(
                InboundTaskPayload {
                    session_id: Some(session_id.into()),
                    content: Some("hello".into()),
                    ..Default::default()
                },
                Uuid::new_v4(),
            )
            .await
            .expect("turn started");

        {
            let emitted = emitted.lock().unwrap();
            let model_req = emitted
                .iter()
                .find(|e| e["task"]["action"] == "generate_text")
                .expect("model request emitted");
            // Converged contract (develop's `model_reply_guest_id`): the base
            // philote sends NO reply_guest_id — aiua's `agent_id` fallback is
            // its own registration, so omission routes correctly.
            assert!(
                model_req["task"].get("reply_guest_id").is_none()
                    || model_req["task"]["reply_guest_id"].is_null(),
                "base philote must omit reply_guest_id (agent_id fallback is correct)"
            );
        }

        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
    }

    // ── Model Oracle Primary Authority, slice 3 ─────────────────────────────
    //
    // Wiring proof for the TRANSCRIPTION-REENTRY shadow hook. A pure predicate
    // test would pass even if the hook were absent, so this drives both legs of
    // a real voice turn over a stub hotel and asserts on the emitted envelopes.

    /// Serializes tests that mutate PHILOTIC_SHADOW_ORACLE, since env vars are
    /// process-global and tests run concurrently.
    static SHADOW_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn shadow_env_guard() -> std::sync::MutexGuard<'static, ()> {
        SHADOW_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Stub hotel that records every EmitTask envelope and answers the shadow
    /// oracle's `QueryModelRoute` with a fixed ranking topped by
    /// `model.anthropic` — deliberately DIFFERENT from the text ladder's
    /// default role, so an `agreement: false` annotation is unambiguous
    /// evidence that the shadow path actually executed (rather than a field
    /// that happened to match).
    async fn run_shadow_oracle_hotel(
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
                philotic_client::IpcRequest::QueryModelRoute {
                    exclude_providers, ..
                } => {
                    // Shadow queries must use an EMPTY exclude list so the hotel
                    // stays on its read-only path (no heal-queue reroute write).
                    emitted.lock().unwrap().push(serde_json::json!({
                        "query_model_route": { "exclude_providers": exclude_providers },
                    }));
                    serde_json::to_vec(&philotic_client::IpcResponse::success(
                        "ok",
                        Some(serde_json::json!({
                            "ranked": [
                                { "role": "model.anthropic", "provider": "anthropic", "score": 0.9 },
                                { "role": "model", "provider": "gemini", "score": 0.5 }
                            ]
                        })),
                    ))
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

    async fn shadow_test_runtime(
        tag: &str,
    ) -> (
        AgentRuntime,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        tokio::task::JoinHandle<()>,
        String,
    ) {
        let socket_path = format!("/tmp/philote-{}-{}.sock", tag, Uuid::new_v4().simple());
        let listener = tokio::net::UnixListener::bind(&socket_path).expect("bind");
        let emitted = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let server = tokio::spawn(run_shadow_oracle_hotel(listener, emitted.clone()));
        let identity = philotic_client::GuestIdentity {
            guest_id: format!("agent-{tag}"),
            role: "agent".into(),
            supported_tools: Vec::new(),
        };
        let client = philotic_client::PhiloticClient::connect_at(&socket_path, identity)
            .await
            .expect("connect to stub hotel");
        let runtime = AgentRuntime::new(client, format!("agent-{tag}"));
        (runtime, emitted, server, socket_path)
    }

    /// A blob-backed voice attachment, the shape `media_analysis_attachments`
    /// accepts and `resolve_media_routing` routes to `voice.transcribe`.
    fn voice_attachment() -> TransportAttachment {
        TransportAttachment {
            kind: "voice".into(),
            file_id: "voice-1".into(),
            mime_type: Some("audio/ogg".into()),
            blob_id: Some("sha256-voice".into()),
            blob_download_url: Some("http://127.0.0.1:9001/download/sha256-voice".into()),
            transport_error: None,
            ..Default::default()
        }
    }

    /// Drives BOTH legs of a voice-originated turn with the shadow flag ON:
    ///
    ///   leg 1 — `handle_user_message` dispatches `voice.transcribe` (aux).
    ///   leg 2 — the transcript comes back and `reenter_turn_after_transcription`
    ///           dispatches the cognitive `generate_text` follow-on.
    ///
    /// Asserts leg 1 carries NO shadow fields (the slice-2 capability guard
    /// excludes aux transforms) and leg 2 DOES (the slice-3 hook). Because both
    /// legs run in the same process under the same flag, this proves the guard
    /// discriminates rather than the flag simply being off — and it fails if
    /// the slice-3 hook is removed.
    #[tokio::test]
    async fn transcription_reentry_records_shadow_fields_while_transcribe_leg_does_not() {
        let _guard = shadow_env_guard();
        unsafe {
            std::env::set_var("PHILOTIC_SHADOW_ORACLE", "1");
        }

        let (mut runtime, emitted, server, socket_path) = shadow_test_runtime("shadow3").await;
        let session_id = "sess-shadow3";
        let turn_id = "turn-shadow3";

        let result = async {
            runtime
                .ensure_session_loaded(session_id, "telegram")
                .await?;

            // Route voice attachments to transcription (the path that arms
            // awaiting_transcription_reentry).
            {
                let state = runtime.sessions.get_mut(session_id).expect("session");
                state
                    .agent_profile
                    .media_routing_policy
                    .forward_media_to_model = true;
                state.agent_profile.media_routing_policy.voice_action = Some("transcribe".into());
            }

            // ── Leg 1: the voice.transcribe dispatch ──
            runtime
                .handle_user_message(
                    InboundTaskPayload {
                        source: Some("telegram".into()),
                        transport: Some("telegram".into()),
                        session_id: Some(session_id.into()),
                        turn_id: Some(turn_id.into()),
                        chat_id: Some("123".into()),
                        message_kind: Some("voice".into()),
                        content: Some("voice message".into()),
                        attachments: vec![voice_attachment()],
                        ..Default::default()
                    },
                    Uuid::new_v4(),
                )
                .await?;

            // ── Leg 2: transcript returns → cognitive re-entry ──
            runtime
                .handle_model_response(respond_payload(
                    session_id,
                    turn_id,
                    "what is the weather tomorrow",
                ))
                .await
        }
        .await;

        unsafe {
            std::env::remove_var("PHILOTIC_SHADOW_ORACLE");
        }
        result.expect("both legs dispatch");

        let emitted = emitted.lock().unwrap();

        // Leg 1 must be an aux voice.transcribe task with NO shadow annotation.
        let transcribe: Vec<_> = emitted
            .iter()
            .filter(|e| {
                e["task"]["action"] == "transcribe" || e["task"]["kind"] == "voice.transcribe"
            })
            .collect();
        assert_eq!(
            transcribe.len(),
            1,
            "exactly one voice.transcribe dispatch: {:#?}",
            *emitted
        );
        assert!(
            transcribe[0]["task"]["oracle_pick"].is_null(),
            "aux voice.transcribe leg must carry NO oracle_pick even with the flag ON: {:#?}",
            transcribe[0]
        );
        assert!(
            transcribe[0]["task"]["oracle_agreement"].is_null(),
            "aux voice.transcribe leg must carry NO oracle_agreement: {:#?}",
            transcribe[0]
        );

        // Leg 2 — the transcription re-entry — is the cognitive dispatch and
        // MUST carry the shadow annotation. This assertion is what fails if the
        // slice-3 hook is missing.
        let cognitive: Vec<_> = emitted
            .iter()
            .filter(|e| {
                e["task"]["action"] == "generate_text" && e["task"]["request_class"] == "cognitive"
            })
            .collect();
        assert_eq!(
            cognitive.len(),
            1,
            "exactly one cognitive re-entry dispatch: {:#?}",
            *emitted
        );
        assert_eq!(
            cognitive[0]["task"]["oracle_pick"], "model.anthropic:anthropic",
            "re-entry must record the oracle's top pick: {:#?}",
            cognitive[0]
        );
        assert_eq!(
            cognitive[0]["task"]["oracle_agreement"], false,
            "oracle ranked model.anthropic but the ladder resolved its default \
             role — divergence must be recorded as agreement=false: {:#?}",
            cognitive[0]
        );

        // Routing is UNCHANGED: the re-entry still goes to the ladder's role,
        // never the oracle's pick. This is the log-only invariant.
        assert_ne!(
            cognitive[0]["target_role"], "model.anthropic",
            "shadow mode must NEVER redirect the dispatch to the oracle pick: {:#?}",
            cognitive[0]
        );

        // The shadow query must use an empty exclude list (read-only path).
        let queries: Vec<_> = emitted
            .iter()
            .filter(|e| e.get("query_model_route").is_some())
            .collect();
        assert_eq!(
            queries.len(),
            1,
            "exactly one shadow oracle query — the aux leg must not query: {:#?}",
            *emitted
        );
        assert_eq!(
            queries[0]["query_model_route"]["exclude_providers"],
            serde_json::json!([]),
            "shadow query must keep the hotel on its read-only path"
        );

        drop(emitted);
        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
    }

    /// OFF path (the default): the same re-entry emits NO shadow fields and
    /// issues NO oracle query at all.
    #[tokio::test]
    async fn transcription_reentry_records_nothing_when_shadow_flag_is_off() {
        let _guard = shadow_env_guard();
        unsafe {
            std::env::remove_var("PHILOTIC_SHADOW_ORACLE");
        }

        let (mut runtime, emitted, server, socket_path) = shadow_test_runtime("shadow3off").await;
        let session_id = "sess-shadow3off";
        let turn_id = "turn-shadow3off";

        runtime
            .ensure_session_loaded(session_id, "telegram")
            .await
            .expect("session load");
        {
            let state = runtime.sessions.get_mut(session_id).expect("session");
            state
                .agent_profile
                .media_routing_policy
                .forward_media_to_model = true;
            state.agent_profile.media_routing_policy.voice_action = Some("transcribe".into());
        }
        runtime
            .handle_user_message(
                InboundTaskPayload {
                    source: Some("telegram".into()),
                    transport: Some("telegram".into()),
                    session_id: Some(session_id.into()),
                    turn_id: Some(turn_id.into()),
                    chat_id: Some("123".into()),
                    message_kind: Some("voice".into()),
                    content: Some("voice message".into()),
                    attachments: vec![voice_attachment()],
                    ..Default::default()
                },
                Uuid::new_v4(),
            )
            .await
            .expect("transcribe leg");
        runtime
            .handle_model_response(respond_payload(session_id, turn_id, "hello there"))
            .await
            .expect("re-entry leg");

        let emitted = emitted.lock().unwrap();
        let cognitive: Vec<_> = emitted
            .iter()
            .filter(|e| {
                e["task"]["action"] == "generate_text" && e["task"]["request_class"] == "cognitive"
            })
            .collect();
        assert_eq!(cognitive.len(), 1, "one re-entry dispatch: {:#?}", *emitted);
        assert!(
            cognitive[0]["task"]["oracle_pick"].is_null()
                && cognitive[0]["task"]["oracle_agreement"].is_null(),
            "flag OFF must leave the re-entry unannotated: {:#?}",
            cognitive[0]
        );
        assert!(
            !emitted.iter().any(|e| e.get("query_model_route").is_some()),
            "flag OFF must issue ZERO oracle IPC: {:#?}",
            *emitted
        );

        drop(emitted);
        drop(runtime);
        let _ = server.await;
        let _ = std::fs::remove_file(&socket_path);
    }
}
