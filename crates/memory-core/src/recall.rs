use serde::{Deserialize, Serialize};

use crate::types::{AttentionalLens, Engram, MemoryScope};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecallTrigger {
    UserTurnStart,
    AgentTurnReentry,
    AgentTurnRecovery,
    ExplicitToolCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallContext {
    pub trigger: RecallTrigger,
    pub scope: MemoryScope,
    /// Primary compact semantic seed for deterministic gating and recall query
    /// construction. Callers should provide the most relevant short text for the
    /// trigger rather than an arbitrary raw transcript blob.
    pub recall_seed_text: String,
    pub active_goal: Option<String>,
    pub role_name: Option<String>,
    pub recent_turns: Vec<String>,
    pub local_memory_summaries: Vec<String>,
    pub tool_history_summary: Vec<String>,
    pub lens: Option<AttentionalLens>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecallMode {
    Skip,
    AutoBounded,
    Explicit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecallDecision {
    pub mode: RecallMode,
    pub reason: String,
    pub query: Option<String>,
    pub limit: Option<usize>,
}

impl RecallDecision {
    pub fn should_recall(&self) -> bool {
        !matches!(self.mode, RecallMode::Skip)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRecallResult {
    pub decision: RecallDecision,
    pub engrams: Vec<Engram>,
    pub total: usize,
    /// Engrams the relevance gate removed (weak or superseded).
    #[serde(default)]
    pub dropped_by_gate: usize,
    /// See [`crate::ActivationResult::rejected_vaults`].
    #[serde(default)]
    pub rejected_vaults: Vec<String>,
    /// See [`crate::ActivationResult::failed_vaults`].
    #[serde(default)]
    pub failed_vaults: Vec<String>,
}

impl RecallContext {
    pub fn normalized_recall_seed_text(&self) -> String {
        normalize_whitespace(&self.recall_seed_text)
    }
}

pub fn evaluate_recall(context: &RecallContext) -> RecallDecision {
    let normalized_turn = context.normalized_recall_seed_text();
    if normalized_turn.is_empty() {
        return skip("empty_turn");
    }

    match context.trigger {
        RecallTrigger::ExplicitToolCall => build_decision(
            RecallMode::Explicit,
            "explicit_tool_call",
            build_query(context, &normalized_turn),
            Some(default_limit(context, 5)),
        ),
        RecallTrigger::UserTurnStart => {
            if is_trivial_user_turn(&normalized_turn) {
                return skip("trivial_user_turn");
            }
            build_decision(
                RecallMode::AutoBounded,
                "meaningful_user_turn",
                build_query(context, &normalized_turn),
                Some(default_limit(context, 5)),
            )
        }
        RecallTrigger::AgentTurnRecovery => build_decision(
            RecallMode::AutoBounded,
            "agent_recovery_requires_continuity",
            build_query(context, &normalized_turn),
            Some(default_limit(context, 4)),
        ),
        RecallTrigger::AgentTurnReentry => {
            if !has_recall_cue(&normalized_turn) {
                return skip("agent_reentry_prefers_working_state");
            }
            build_decision(
                RecallMode::AutoBounded,
                "agent_reentry_has_history_cue",
                build_query(context, &normalized_turn),
                Some(default_limit(context, 3)),
            )
        }
    }
}

fn build_decision(
    mode: RecallMode,
    reason: &str,
    query: Option<String>,
    limit: Option<usize>,
) -> RecallDecision {
    RecallDecision {
        mode,
        reason: reason.to_string(),
        query,
        limit,
    }
}

fn skip(reason: &str) -> RecallDecision {
    build_decision(RecallMode::Skip, reason, None, None)
}

fn default_limit(context: &RecallContext, fallback: usize) -> usize {
    context
        .lens
        .as_ref()
        .and_then(|lens| lens.max_results)
        .unwrap_or(fallback)
        .clamp(1, 20)
}

/// Upper bound on the recall query. Long enough for a real request plus a
/// follow-up's antecedent; short enough that the semantic seed stays focused.
const RECALL_QUERY_MAX_CHARS: usize = 400;
/// A turn at or under this many words that also reads as referential
/// ("what about the second one?") is seeded with the previous user turn.
const FOLLOW_UP_MAX_WORDS: usize = 8;
const PLAN_CONTINUATION_PREFIX: &str = "[Plan continuation";

/// Build the semantic recall query.
///
/// Deliberately does NOT include the role name: a `role: <name>` prefix pulled
/// role-themed memories to the top of nearly every recall (2026-09-16 audit:
/// one memory in Beacon's top-3 on 165/178 recalls; removing the prefix moved
/// the relevant memory from #7 to #2 on replay). The user's words lead.
fn build_query(context: &RecallContext, normalized_turn: &str) -> Option<String> {
    let seed =
        plan_continuation_goal(normalized_turn).unwrap_or_else(|| normalized_turn.to_string());
    let mut parts = vec![seed.clone()];

    if is_referential_follow_up(&seed)
        && let Some(previous) = context
            .recent_turns
            .iter()
            .map(|turn| normalize_whitespace(turn))
            .find(|turn| !turn.is_empty() && !turn.starts_with(PLAN_CONTINUATION_PREFIX))
    {
        parts.push(truncate_chars(&previous, 200));
    }

    if let Some(goal) = context
        .active_goal
        .as_deref()
        .map(normalize_whitespace)
        .filter(|goal| !goal.is_empty() && !seed.contains(goal.as_str()))
    {
        parts.push(goal);
    }

    let query = parts.join(" | ");
    if query.trim().is_empty() {
        None
    } else {
        Some(truncate_chars(&query, RECALL_QUERY_MAX_CHARS))
    }
}

/// Plan-continuation turns are seeded with a boilerplate brief
/// ("[Plan continuation n/N] Continue executing your existing plan. Goal: ...").
/// Recalling on the boilerplate returns noise; the goal is the real request.
fn plan_continuation_goal(normalized_turn: &str) -> Option<String> {
    if !normalized_turn.starts_with(PLAN_CONTINUATION_PREFIX) {
        return None;
    }
    let goal = normalized_turn.split_once("Goal:")?.1.trim();
    // The brief may continue past the goal with step listings; keep the goal
    // sentence(s) only.
    let goal = goal
        .split(" Completed steps:")
        .next()
        .unwrap_or(goal)
        .split(" Remaining steps:")
        .next()
        .unwrap_or(goal)
        .trim();
    (!goal.is_empty()).then(|| goal.to_string())
}

fn is_referential_follow_up(text: &str) -> bool {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_ascii_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() || words.len() > FOLLOW_UP_MAX_WORDS {
        return false;
    }
    const REFERENTS: &[&str] = &[
        "it", "that", "this", "those", "these", "them", "they", "one", "ones", "same", "again",
        "there", "then", "above", "previous", "last", "second", "first", "other",
    ];
    words.iter().any(|w| REFERENTS.contains(&w.as_str()))
}

fn normalize_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn is_trivial_user_turn(text: &str) -> bool {
    let normalized = text.to_ascii_lowercase();
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return true;
    }

    let exact_matches = [
        "ok",
        "okay",
        "k",
        "yes",
        "yep",
        "sure",
        "thanks",
        "thank you",
        "sounds good",
        "do it",
        "continue",
        "go on",
    ];

    if exact_matches.contains(&trimmed) {
        return true;
    }

    trimmed.len() <= 24
        && (trimmed.starts_with("thanks")
            || trimmed.starts_with("thank you")
            || trimmed.starts_with("continue")
            || trimmed.starts_with("do it")
            || trimmed.starts_with("go ahead"))
}

fn has_recall_cue(text: &str) -> bool {
    let normalized = text.to_ascii_lowercase();
    [
        "remember",
        "previous",
        "last time",
        "we decided",
        "history",
        "preference",
        "earlier",
        "before",
    ]
    .iter()
    .any(|cue| normalized.contains(cue))
}

/// `Engram.metadata` key under which the REST client records per-activation
/// retrieval statistics (`{"band": ..., "score": ...}`).
pub const RECALL_METADATA_KEY: &str = "recall";

/// The absolute relevance band Muninn assigned this engram for the query that
/// recalled it, when known.
pub fn engram_relevance_band(engram: &Engram) -> Option<&str> {
    engram
        .metadata
        .get(RECALL_METADATA_KEY)?
        .get("band")?
        .as_str()
}

/// The server's per-query score for this engram, when known.
pub fn engram_recall_score(engram: &Engram) -> Option<f64> {
    engram
        .metadata
        .get(RECALL_METADATA_KEY)?
        .get("score")?
        .as_f64()
}

/// Turn-recall relevance gate. Automatic recall injects memories into a prompt
/// the operator never asked for, so it must be conservative:
/// - `weak` matches are dropped (2026-09-16 replay: 76% of auto-recalled rows
///   were weak and off-topic, yet all were injected);
/// - memories explicitly superseded by a newer version are dropped (the newer
///   version is recallable on its own);
/// - unknown bands (older servers) and `uncalibrated` rows pass, so a server
///   that cannot judge relevance degrades to the previous behaviour.
///
/// Returns the number of engrams removed.
pub fn retain_turn_relevant(engrams: &mut Vec<Engram>) -> usize {
    let before = engrams.len();
    engrams.retain(|engram| {
        let weak = engram_relevance_band(engram) == Some("weak");
        let superseded = engram
            .metadata
            .get("annotations")
            .and_then(|ann| ann.get("superseded_by"))
            .and_then(|v| v.as_str())
            .is_some_and(|v| !v.is_empty());
        !weak && !superseded
    });
    before - engrams.len()
}

#[cfg(test)]
mod tests {
    use super::{RecallContext, RecallMode, RecallTrigger, evaluate_recall};
    use crate::{AttentionalLens, MemoryScope};

    fn base_context(trigger: RecallTrigger, turn_text: &str) -> RecallContext {
        RecallContext {
            trigger,
            scope: MemoryScope::SelfOnly,
            recall_seed_text: turn_text.to_string(),
            active_goal: None,
            role_name: None,
            recent_turns: Vec::new(),
            local_memory_summaries: Vec::new(),
            tool_history_summary: Vec::new(),
            lens: None,
        }
    }

    #[test]
    fn user_turn_skips_trivial_acknowledgement() {
        let decision = evaluate_recall(&base_context(RecallTrigger::UserTurnStart, "thanks"));
        assert_eq!(decision.mode, RecallMode::Skip);
        assert_eq!(decision.reason, "trivial_user_turn");
        assert!(!decision.should_recall());
    }

    #[test]
    fn user_turn_recalls_for_meaningful_request() {
        let mut ctx = base_context(
            RecallTrigger::UserTurnStart,
            "Can you continue the memory architecture work from yesterday?",
        );
        ctx.role_name = Some("architect".into());
        let decision = evaluate_recall(&ctx);
        assert_eq!(decision.mode, RecallMode::AutoBounded);
        assert_eq!(decision.limit, Some(5));
        let query = decision.query.unwrap_or_default();
        // The user's words lead and the role name never enters the semantic
        // seed (it dragged role-themed memories into 93% of Beacon's recalls).
        assert!(query.starts_with("Can you continue the memory architecture work"));
        assert!(!query.contains("architect |"), "{query}");
        assert!(!query.contains("role:"), "{query}");
    }

    #[test]
    fn plan_continuation_recalls_on_the_goal_not_the_boilerplate() {
        let brief = "[Plan continuation 2/3] Continue executing your existing plan. Goal: book the \
                     organ practice slots for next week\nCompleted steps:\n1. check calendar\n\
                     Remaining steps:\n2. email the church office\n";
        let decision = evaluate_recall(&base_context(RecallTrigger::UserTurnStart, brief));
        assert_eq!(decision.mode, RecallMode::AutoBounded);
        assert_eq!(
            decision.query.as_deref(),
            Some("book the organ practice slots for next week")
        );
    }

    #[test]
    fn short_referential_follow_up_is_seeded_with_previous_turn() {
        let mut ctx = base_context(RecallTrigger::UserTurnStart, "what about the second one?");
        ctx.recent_turns = vec![
            "compare the two Mendelssohn sonatas for Sunday".into(),
            "older turn".into(),
        ];
        let query = evaluate_recall(&ctx).query.unwrap_or_default();
        assert_eq!(
            query,
            "what about the second one? | compare the two Mendelssohn sonatas for Sunday"
        );

        // A self-contained request is not padded with history.
        let mut ctx = base_context(
            RecallTrigger::UserTurnStart,
            "schedule a dentist appointment for Tuesday afternoon",
        );
        ctx.recent_turns = vec!["compare the two Mendelssohn sonatas".into()];
        assert_eq!(
            evaluate_recall(&ctx).query.as_deref(),
            Some("schedule a dentist appointment for Tuesday afternoon")
        );
    }

    #[test]
    fn goal_is_appended_only_when_not_already_in_the_seed() {
        let mut ctx = base_context(RecallTrigger::UserTurnStart, "draft the weekly review");
        ctx.active_goal = Some("prepare Sunday service music".into());
        assert_eq!(
            evaluate_recall(&ctx).query.as_deref(),
            Some("draft the weekly review | prepare Sunday service music")
        );
        ctx.active_goal = Some("draft the weekly review".into());
        assert_eq!(
            evaluate_recall(&ctx).query.as_deref(),
            Some("draft the weekly review")
        );
    }

    #[test]
    fn query_is_capped() {
        let long = "word ".repeat(200);
        let query = evaluate_recall(&base_context(RecallTrigger::UserTurnStart, &long))
            .query
            .unwrap_or_default();
        assert_eq!(query.chars().count(), super::RECALL_QUERY_MAX_CHARS);
    }

    #[test]
    fn agent_reentry_skips_without_history_cue() {
        let decision = evaluate_recall(&base_context(
            RecallTrigger::AgentTurnReentry,
            "Tool returned success. Continue.",
        ));
        assert_eq!(decision.mode, RecallMode::Skip);
        assert_eq!(decision.reason, "agent_reentry_prefers_working_state");
    }

    #[test]
    fn agent_reentry_recalls_with_history_cue() {
        let decision = evaluate_recall(&base_context(
            RecallTrigger::AgentTurnReentry,
            "Check whether we decided this earlier before responding.",
        ));
        assert_eq!(decision.mode, RecallMode::AutoBounded);
        assert_eq!(decision.limit, Some(3));
    }

    #[test]
    fn explicit_tool_call_always_recalls() {
        let decision = evaluate_recall(&base_context(
            RecallTrigger::ExplicitToolCall,
            "user preference",
        ));
        assert_eq!(decision.mode, RecallMode::Explicit);
        assert_eq!(decision.reason, "explicit_tool_call");
    }

    #[test]
    fn lens_max_results_overrides_default_limit() {
        let mut ctx = base_context(
            RecallTrigger::UserTurnStart,
            "Find memories relevant to the operator's deployment preferences.",
        );
        ctx.lens = Some(AttentionalLens {
            max_results: Some(2),
            ..Default::default()
        });

        let decision = evaluate_recall(&ctx);
        assert_eq!(decision.mode, RecallMode::AutoBounded);
        assert_eq!(decision.limit, Some(2));
    }
}
