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

fn build_query(context: &RecallContext, normalized_turn: &str) -> Option<String> {
    let mut parts = Vec::new();

    if let Some(role_name) = context
        .role_name
        .as_deref()
        .map(normalize_whitespace)
        .filter(|value| !value.is_empty())
    {
        parts.push(format!("role: {role_name}"));
    }

    parts.push(normalized_turn.to_string());

    if let Some(goal) = context
        .active_goal
        .as_deref()
        .map(normalize_whitespace)
        .filter(|value| !value.is_empty())
    {
        parts.push(format!("goal: {goal}"));
    }

    let query = parts.join(" | ");
    if query.is_empty() {
        None
    } else {
        Some(truncate_chars(&query, 240))
    }
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
        assert!(
            decision
                .query
                .as_deref()
                .unwrap_or_default()
                .contains("role: architect")
        );
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
        let decision =
            evaluate_recall(&base_context(RecallTrigger::ExplicitToolCall, "user preference"));
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
