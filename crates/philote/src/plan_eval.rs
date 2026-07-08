//! Plan-eval-repeat support: the "eval" and "repeat" stages of the agent-level
//! plan → execute → eval → repeat cycle.
//!
//! After a turn's final Respond, [`evaluate_plan`] derives a completion verdict
//! for the turn's `ActivePlan` without a second model round-trip: it trusts the
//! model's own per-step status claims when the response contract carried them
//! (`basis = model_reported`), and falls back to a cheap heuristic that matches
//! step text / bound tools against the turn's tool history successes
//! (`basis = heuristic`), flagging heuristic completions as uncertain.
//!
//! The runtime consumes the outcome to persist a
//! [`crate::session::CarryoverPlan`], emit `plan_eval` / `plan_continuation`
//! turn events, and synthesize budgeted continuation turns through the
//! `pending_drains` mechanism.

use crate::r#loop::{ToolCall, ToolResult};
use crate::session::{ActivePlan, CarryoverPlan, PlanStep};
use serde_json::json;

/// Default number of auto-continuation turns per carried-over plan.
/// Role-overridable via `TurnLoopConfig.plan_continuation_budget`.
pub const DEFAULT_PLAN_CONTINUATION_BUDGET: u32 = 3;

/// Operator kill switch: when set, the plan-eval-repeat loop never persists a
/// carryover and never synthesizes continuation turns.
pub fn plan_continuation_disabled() -> bool {
    std::env::var("PHILOTIC_DISABLE_PLAN_CONTINUATION")
        .map(|v| {
            let v = v.trim();
            v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanEvalVerdict {
    /// Every step is done (or the model declared the plan done).
    Complete,
    /// Unfinished steps remain and forward progress is still plausible.
    Continue,
    /// The plan cannot proceed: it failed, every remaining step failed, or a
    /// continuation turn made no forward progress.
    Blocked,
}

impl PlanEvalVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Continue => "continue",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanEvalBasis {
    /// The model's response contract carried per-step (or whole-plan) status
    /// claims — the eval trusts them.
    ModelReported,
    /// The contract was silent on completion; steps were matched against the
    /// turn's tool-history successes.
    Heuristic,
}

impl PlanEvalBasis {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ModelReported => "model_reported",
            Self::Heuristic => "heuristic",
        }
    }
}

/// Result of a plan eval over a completed turn.
#[derive(Debug, Clone)]
pub struct PlanEvalOutcome {
    pub steps_total: usize,
    pub steps_done: usize,
    /// Index-aligned per-step completion flags (merged with any prior flags).
    pub steps_done_flags: Vec<bool>,
    /// Step ids marked done by the heuristic rather than a model claim.
    pub uncertain_step_ids: Vec<u32>,
    pub verdict: PlanEvalVerdict,
    pub basis: PlanEvalBasis,
}

impl PlanEvalOutcome {
    /// Compact JSON record for the `plan_eval` turn event.
    pub fn event_json(&self) -> serde_json::Value {
        json!({
            "steps_total": self.steps_total,
            "steps_done": self.steps_done,
            "verdict": self.verdict.as_str(),
            "basis": self.basis.as_str(),
            "uncertain_steps": self.uncertain_step_ids,
        })
    }
}

/// Evaluate plan completion after a turn's final Respond.
///
/// `prior_done` carries the per-step flags from an existing carryover (i.e.
/// this turn was a continuation); a step once done stays done. When
/// `prior_done` is present and the eval finds no *new* completed step, the
/// verdict is `Blocked` — a continuation that made no forward progress must
/// not spin the loop again.
pub fn evaluate_plan(
    plan: &ActivePlan,
    prior_done: Option<&[bool]>,
    tool_history: &[(ToolCall, ToolResult)],
) -> PlanEvalOutcome {
    let total = plan.steps.len();
    let plan_declared_done = plan.status == "done";
    let plan_declared_failed = plan.status == "failed";

    // The model "engaged" with status tracking if the plan or any step carries
    // a terminal status. When silent, completion falls to the heuristic.
    let model_engaged = plan_declared_done
        || plan_declared_failed
        || plan
            .steps
            .iter()
            .any(|s| matches!(s.status.as_str(), "done" | "failed"));

    let mut flags = vec![false; total];
    let mut uncertain: Vec<u32> = Vec::new();
    let mut any_step_failed = false;

    for (i, step) in plan.steps.iter().enumerate() {
        match step.status.as_str() {
            "done" => flags[i] = true,
            "failed" => any_step_failed = true,
            _ => {
                if plan_declared_done {
                    // Whole-plan claim covers steps the model forgot to mark.
                    flags[i] = true;
                } else if heuristic_step_done(step, tool_history) {
                    flags[i] = true;
                    uncertain.push(step.id);
                }
            }
        }
        if let Some(prior) = prior_done {
            if prior.get(i).copied().unwrap_or(false) {
                flags[i] = true;
            }
        }
    }

    let done = flags.iter().filter(|f| **f).count();
    let basis = if model_engaged {
        PlanEvalBasis::ModelReported
    } else {
        PlanEvalBasis::Heuristic
    };

    let verdict = if plan_declared_failed {
        PlanEvalVerdict::Blocked
    } else if total > 0 && done == total {
        PlanEvalVerdict::Complete
    } else if plan_declared_done {
        PlanEvalVerdict::Complete
    } else if any_step_failed
        && plan
            .steps
            .iter()
            .enumerate()
            .all(|(i, s)| flags[i] || s.status == "failed")
    {
        // Everything is either done or failed — nothing left to continue with.
        PlanEvalVerdict::Blocked
    } else if let Some(prior) = prior_done {
        let prior_count = prior.iter().filter(|f| **f).count();
        if done <= prior_count {
            // Continuation turn made no forward progress.
            PlanEvalVerdict::Blocked
        } else {
            PlanEvalVerdict::Continue
        }
    } else {
        PlanEvalVerdict::Continue
    };

    PlanEvalOutcome {
        steps_total: total,
        steps_done: done,
        steps_done_flags: flags,
        uncertain_step_ids: uncertain,
        verdict,
        basis,
    }
}

/// Cheap heuristic: does the tool history contain a successful call that
/// plausibly executed this step? Conservative on purpose — under-marking only
/// costs one continuation turn, over-marking silently skips work.
fn heuristic_step_done(step: &PlanStep, history: &[(ToolCall, ToolResult)]) -> bool {
    // Strongest signal: the step is bound to a tool and that tool ran successfully.
    if let Some(tool) = step.tool_name.as_deref() {
        if !tool.is_empty() {
            return history
                .iter()
                .any(|(call, result)| call.tool_name == tool && tool_result_looks_ok(result));
        }
    }

    // Fallback: significant token overlap between the step description and a
    // successful call's tool name + arguments.
    let step_tokens = significant_tokens(&step.description);
    if step_tokens.is_empty() {
        return false;
    }
    history.iter().any(|(call, result)| {
        if !tool_result_looks_ok(result) {
            return false;
        }
        let haystack = format!(
            "{} {}",
            call.tool_name.replace(['.', '_', '-'], " "),
            call.arguments
        )
        .to_lowercase();
        let matched = step_tokens
            .iter()
            .filter(|t| haystack.contains(t.as_str()))
            .count();
        matched >= 2 || (step_tokens.len() == 1 && matched == 1)
    })
}

fn tool_result_looks_ok(result: &ToolResult) -> bool {
    let trimmed = result.content.trim_start().to_lowercase();
    !(trimmed.starts_with("error")
        || trimmed.starts_with("{\"error\"")
        || trimmed.starts_with("tool execution failed"))
}

fn significant_tokens(text: &str) -> Vec<String> {
    const STOPWORDS: &[&str] = &[
        "with", "from", "then", "that", "this", "into", "step", "using", "each", "their", "them",
        "over", "about", "after", "before", "call", "check",
    ];
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 4 && !STOPWORDS.contains(t))
        .map(str::to_string)
        .collect()
}

/// Structured "continue the plan" brief for the synthesized continuation turn.
/// Cites exactly what is done and what remains so the next turn's model sees
/// the remaining work without re-planning.
pub fn plan_continuation_brief(carryover: &CarryoverPlan, budget: u32) -> String {
    let done_list = step_list(&carryover.plan, &carryover.steps_done, true);
    let remaining_list = step_list(&carryover.plan, &carryover.steps_done, false);
    let mut brief = format!(
        "[Plan continuation {}/{}] Continue executing your existing plan. Goal: {}\n",
        carryover.continuations_used + 1,
        budget,
        carryover.plan.goal
    );
    if !done_list.is_empty() {
        brief.push_str(&format!("Completed steps:\n{done_list}\n"));
    }
    brief.push_str(&format!(
        "Remaining steps:\n{remaining_list}\n\
         Pick up at the first remaining step. Do not re-plan or repeat completed work. \
         Update step statuses in active_plan as you execute; when every step is done, \
         deliver a final summary of the whole plan to the user."
    ));
    brief
}

/// One tight operator-facing message for a plan that stopped (blocked or
/// budget exhausted): what's done, what's not, and why the loop stopped.
pub fn plan_stop_notice(carryover: &CarryoverPlan, reason: &str) -> String {
    let done = carryover.steps_done_count();
    let total = carryover.plan.steps.len();
    let remaining_list = step_list(&carryover.plan, &carryover.steps_done, false);
    let remaining = if remaining_list.is_empty() {
        "(none)".to_string()
    } else {
        remaining_list
    };
    format!(
        "*(Plan paused: {reason}. Goal: {}. {done}/{total} steps done. Remaining:\n{remaining}\n\
         Send a message to keep going manually, or /plan drop to discard.)*",
        carryover.plan.goal
    )
}

fn step_list(plan: &ActivePlan, flags: &[bool], done: bool) -> String {
    plan.steps
        .iter()
        .enumerate()
        .filter(|(i, _)| flags.get(*i).copied().unwrap_or(false) == done)
        .map(|(_, s)| format!("- step {}: {}", s.id, s.description))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::r#loop::{ToolCall, ToolResult};
    use crate::session::{ActivePlan, PlanStep};

    fn plan(status: &str, steps: &[(&str, Option<&str>, &str)]) -> ActivePlan {
        ActivePlan {
            goal: "ship the feature".into(),
            steps: steps
                .iter()
                .enumerate()
                .map(|(i, (desc, tool, st))| PlanStep {
                    id: i as u32 + 1,
                    description: (*desc).to_string(),
                    tool_name: tool.map(str::to_string),
                    status: (*st).to_string(),
                })
                .collect(),
            status: status.into(),
            context_1_advisory: None,
        }
    }

    fn history(entries: &[(&str, &str)]) -> Vec<(ToolCall, ToolResult)> {
        entries
            .iter()
            .map(|(tool, content)| {
                (
                    ToolCall {
                        tool_name: (*tool).to_string(),
                        arguments: serde_json::json!({}),
                    },
                    ToolResult {
                        tool_name: (*tool).to_string(),
                        content: (*content).to_string(),
                    },
                )
            })
            .collect()
    }

    #[test]
    fn model_reported_all_done_is_complete() {
        let p = plan(
            "executing",
            &[("read config", None, "done"), ("apply fix", None, "done")],
        );
        let out = evaluate_plan(&p, None, &[]);
        assert_eq!(out.verdict, PlanEvalVerdict::Complete);
        assert_eq!(out.basis, PlanEvalBasis::ModelReported);
        assert_eq!(out.steps_done, 2);
        assert!(out.uncertain_step_ids.is_empty());
    }

    #[test]
    fn plan_status_done_trusts_whole_plan_claim() {
        let p = plan(
            "done",
            &[
                ("read config", None, "pending"),
                ("apply fix", None, "done"),
            ],
        );
        let out = evaluate_plan(&p, None, &[]);
        assert_eq!(out.verdict, PlanEvalVerdict::Complete);
        assert_eq!(out.basis, PlanEvalBasis::ModelReported);
        assert_eq!(out.steps_done, 2);
    }

    #[test]
    fn model_reported_partial_continues() {
        let p = plan(
            "executing",
            &[
                ("read config", None, "done"),
                ("apply fix", None, "pending"),
                ("verify deploy", None, "pending"),
            ],
        );
        let out = evaluate_plan(&p, None, &[]);
        assert_eq!(out.verdict, PlanEvalVerdict::Continue);
        assert_eq!(out.basis, PlanEvalBasis::ModelReported);
        assert_eq!(out.steps_done, 1);
    }

    #[test]
    fn plan_failed_is_blocked() {
        let p = plan("failed", &[("read config", None, "pending")]);
        let out = evaluate_plan(&p, None, &[]);
        assert_eq!(out.verdict, PlanEvalVerdict::Blocked);
        assert_eq!(out.basis, PlanEvalBasis::ModelReported);
    }

    #[test]
    fn all_remaining_steps_failed_is_blocked() {
        let p = plan(
            "executing",
            &[("read config", None, "done"), ("apply fix", None, "failed")],
        );
        let out = evaluate_plan(&p, None, &[]);
        assert_eq!(out.verdict, PlanEvalVerdict::Blocked);
    }

    #[test]
    fn heuristic_bound_tool_success_marks_step_uncertain() {
        // Contract silent: every step "pending", but the bound tool ran fine.
        let p = plan(
            "executing",
            &[
                ("check hotel status", Some("hotel.status"), "pending"),
                ("summarize findings", None, "pending"),
            ],
        );
        let h = history(&[("hotel.status", "hotel green")]);
        let out = evaluate_plan(&p, None, &h);
        assert_eq!(out.basis, PlanEvalBasis::Heuristic);
        assert_eq!(out.verdict, PlanEvalVerdict::Continue);
        assert_eq!(out.steps_done, 1);
        assert_eq!(out.uncertain_step_ids, vec![1]);
    }

    #[test]
    fn heuristic_bound_tool_error_does_not_mark_step() {
        let p = plan(
            "executing",
            &[("check hotel status", Some("hotel.status"), "pending")],
        );
        let h = history(&[("hotel.status", "Error: connection refused")]);
        let out = evaluate_plan(&p, None, &h);
        assert_eq!(out.steps_done, 0);
        assert_eq!(out.verdict, PlanEvalVerdict::Continue);
    }

    #[test]
    fn heuristic_description_token_match() {
        let p = plan(
            "executing",
            &[("recall session memory context", None, "pending")],
        );
        let h = history(&[("memory.recall", "3 memories found for session context")]);
        let out = evaluate_plan(&p, None, &h);
        assert_eq!(out.basis, PlanEvalBasis::Heuristic);
        assert_eq!(out.steps_done, 1);
        assert_eq!(out.uncertain_step_ids, vec![1]);
    }

    #[test]
    fn heuristic_silent_history_continues_with_zero_done() {
        let p = plan(
            "executing",
            &[("write deployment runbook", None, "pending")],
        );
        let out = evaluate_plan(&p, None, &[]);
        assert_eq!(out.basis, PlanEvalBasis::Heuristic);
        assert_eq!(out.verdict, PlanEvalVerdict::Continue);
        assert_eq!(out.steps_done, 0);
    }

    #[test]
    fn prior_done_flags_merge_and_stay_done() {
        let p = plan(
            "executing",
            &[
                ("read config", None, "pending"),
                ("apply fix", None, "done"),
                ("verify deploy", None, "pending"),
            ],
        );
        let out = evaluate_plan(&p, Some(&[true, false, false]), &[]);
        // step 1 stays done from prior, step 2 newly model-reported done.
        assert_eq!(out.steps_done, 2);
        assert!(out.steps_done_flags[0] && out.steps_done_flags[1]);
        assert_eq!(out.verdict, PlanEvalVerdict::Continue);
    }

    #[test]
    fn continuation_without_progress_is_blocked() {
        let p = plan(
            "executing",
            &[
                ("read config", None, "done"),
                ("apply fix", None, "pending"),
            ],
        );
        // Prior already had step 1 done — this continuation added nothing.
        let out = evaluate_plan(&p, Some(&[true, false]), &[]);
        assert_eq!(out.verdict, PlanEvalVerdict::Blocked);
    }

    #[test]
    fn continuation_brief_cites_remaining_steps_only() {
        let carry = CarryoverPlan {
            plan: plan(
                "executing",
                &[
                    ("read config", None, "done"),
                    ("apply fix", None, "pending"),
                ],
            ),
            steps_done: vec![true, false],
            continuations_used: 1,
            created_turn_id: "turn-0".into(),
        };
        let brief = plan_continuation_brief(&carry, 3);
        assert!(brief.contains("[Plan continuation 2/3]"));
        assert!(brief.contains("Completed steps:\n- step 1: read config"));
        assert!(brief.contains("Remaining steps:\n- step 2: apply fix"));
    }

    #[test]
    fn stop_notice_reports_done_undone_and_reason() {
        let carry = CarryoverPlan {
            plan: plan(
                "executing",
                &[
                    ("read config", None, "done"),
                    ("apply fix", None, "pending"),
                ],
            ),
            steps_done: vec![true, false],
            continuations_used: 3,
            created_turn_id: "turn-0".into(),
        };
        let notice = plan_stop_notice(&carry, "continuation budget exhausted");
        assert!(notice.contains("1/2 steps done"));
        assert!(notice.contains("continuation budget exhausted"));
        assert!(notice.contains("- step 2: apply fix"));
        assert!(notice.contains("/plan drop"));
    }
}
