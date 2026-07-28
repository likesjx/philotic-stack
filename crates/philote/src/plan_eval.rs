//! Plan → execute → verify each step → evaluate the whole plan.
//!
//! There are two layers here, and the difference between them matters.
//!
//! **Cross-turn carryover (original).** After a turn's final Respond,
//! [`evaluate_plan`] derives a completion verdict for the turn's `ActivePlan`
//! without a second model round-trip: it trusts the model's own per-step status
//! claims when the response contract carried them (`basis = model_reported`),
//! and otherwise falls back to a cheap heuristic against the turn's tool
//! history (`basis = heuristic`). The runtime persists a
//! [`crate::session::CarryoverPlan`], emits `plan_eval` / `plan_continuation`
//! turn events, and synthesizes budgeted continuation turns through
//! `pending_drains`. This remains the overflow path for work that genuinely
//! exceeds one turn.
//!
//! **In-turn execution and grounded verification (this layer).** Trusting
//! `step.status == "done"` is self-certification, not evaluation. A model that
//! marks a step done without doing it yields a `Complete` verdict and then
//! reports success to the operator — which is exactly what happened live: an
//! agent was asked to add five family members to the LifeGraph, marked every
//! step done, told the operator "Yes, I now have all five", and one of them had
//! never been created.
//!
//! So completion is now grounded in the turn's actual tool results:
//!
//! - [`should_plan`] decides whether a turn plans at all — by default it does,
//!   skipping only trivially conversational messages.
//! - [`plan_directive`] requires atomic, tool-bound steps, because a step that
//!   bundles several artifacts is cleared by one successful call while the rest
//!   silently never happen.
//! - [`verify_plan_steps`] attributes successful tool calls to steps one-to-one,
//!   using the tokens that distinguish each step from its siblings.
//! - [`evaluate_whole_plan`] and [`plan_integrity_note`] make it impossible for
//!   a turn to report a plan as finished while a step is unverified.
//! - [`reentry_hint`] keeps the turn working through its steps instead of
//!   returning to the user after each one, bounded by
//!   [`PLAN_EXECUTION_BUDGET_SECS`] so the hotel's 300s zombie watchdog never
//!   reaps a turn mid-plan.

use crate::r#loop::{ToolCall, ToolResult};
use crate::session::{ActivePlan, CarryoverPlan, PlanStep, WorkingTurn};
use serde_json::json;
use std::collections::BTreeMap;

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

// ── Grounded step verification ──────────────────────────────────────────────
//
// `evaluate_plan` above trusts `step.status == "done"` when the model engaged
// with status tracking. That is self-certification, not evaluation: a model
// that marks a step done without doing it produces a Complete verdict and then
// reports success to the operator. This section grounds completion in the
// turn's actual tool history instead.

/// The hotel's zombie-turn watchdog fails any turn still `running` this many
/// seconds after its `started_at` (`heal-dispatcher` issues
/// `RepairStaleSessionTurns { min_age_secs: 300 }`). There is no heartbeat that
/// resets the clock, so this is a hard wall-clock ceiling on a single turn —
/// not an iteration ceiling. `iteration_cap` was never the binding constraint.
pub const TURN_ZOMBIE_REAP_SECS: u64 = 300;

/// Wall-clock budget for executing plan steps inside one turn, measured from
/// the turn's `started_at` (which includes queue time and the planning call).
/// The remainder of [`TURN_ZOMBIE_REAP_SECS`] is reserved for composing and
/// delivering the final reply — a turn that gets reaped mid-plan loses the
/// reply entirely, which is strictly worse than stopping early with an honest
/// summary. Observed Beacon turns run 2–14s, so this allows roughly 25–70
/// iterations before the budget binds.
pub const PLAN_EXECUTION_BUDGET_SECS: u64 = 210;

/// Current unix seconds — the clock the turn budget and the hotel's zombie
/// watchdog both read.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// True when in-turn plan execution must stop and hand off to the final reply.
pub fn plan_execution_budget_exhausted(started_at_unix: Option<u64>, now_unix: u64) -> bool {
    let Some(started) = started_at_unix else {
        // Unknown start time — fail open rather than truncating every turn.
        return false;
    };
    now_unix.saturating_sub(started) >= PLAN_EXECUTION_BUDGET_SECS
}

/// What the turn's tool history says about one plan step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepEvidence {
    /// A successful tool call in this turn is attributable to this step.
    Verified,
    /// The step declares a tool, that tool's successful calls are all already
    /// attributed to other steps (or never happened), so nothing backs it.
    /// A model claim of `done` does not clear this.
    Missing,
    /// The step declares no tool, so there is no artifact to check against.
    /// The model's own claim is the only available signal.
    NotCheckable,
}

impl StepEvidence {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::Missing => "missing",
            Self::NotCheckable => "not_checkable",
        }
    }
}

/// Grounded verification of every step in a plan.
#[derive(Debug, Clone)]
pub struct PlanVerification {
    /// Index-aligned with `plan.steps`.
    pub evidence: Vec<StepEvidence>,
    /// Steps the model marked `done` that the tool history contradicts. These
    /// are the ones that silently become false claims to the operator.
    pub contradicted_step_ids: Vec<u32>,
    /// Steps that bundle several artifacts into one description and therefore
    /// cannot be verified one-to-one. See [`atomicity_violations`].
    pub non_atomic_step_ids: Vec<u32>,
}

impl PlanVerification {
    pub fn verified_count(&self) -> usize {
        self.evidence
            .iter()
            .filter(|e| **e == StepEvidence::Verified)
            .count()
    }

    /// Steps with no supporting evidence, as `plan.steps` indices.
    pub fn missing_indices(&self) -> Vec<usize> {
        self.evidence
            .iter()
            .enumerate()
            .filter(|(_, e)| **e == StepEvidence::Missing)
            .map(|(i, _)| i)
            .collect()
    }

    /// Sticky flags to carry into the next iteration: evidence can scroll out
    /// of the working tool history, and a step once verified stays verified.
    pub fn verified_flags(&self) -> Vec<bool> {
        self.evidence
            .iter()
            .map(|e| *e == StepEvidence::Verified)
            .collect()
    }

    pub fn event_json(&self) -> serde_json::Value {
        json!({
            "verified": self.verified_count(),
            "total": self.evidence.len(),
            "evidence": self.evidence.iter().map(|e| e.as_str()).collect::<Vec<_>>(),
            "contradicted_steps": self.contradicted_step_ids,
            "non_atomic_steps": self.non_atomic_step_ids,
        })
    }
}

/// Tokens that identify *which* artifact a step is about: the significant
/// tokens unique to that step among its siblings. "Propose Daxton Thomas
/// Wagner as a Person node" and "Propose Zerin Maluy as a Person node" share
/// every word but the names, so the names are what make each step's evidence
/// individually attributable. Without this, one successful `life.observe`
/// clears all five children.
fn distinctive_tokens(plan: &ActivePlan) -> Vec<Vec<String>> {
    let per_step: Vec<Vec<String>> = plan
        .steps
        .iter()
        .map(|s| significant_tokens(&s.description))
        .collect();

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for tokens in &per_step {
        let mut seen: Vec<&String> = Vec::new();
        for t in tokens {
            if !seen.contains(&t) {
                seen.push(t);
                *counts.entry(t.clone()).or_default() += 1;
            }
        }
    }

    per_step
        .iter()
        .map(|tokens| {
            let mut uniq: Vec<String> = Vec::new();
            for t in tokens {
                if counts.get(t).copied() == Some(1) && !uniq.contains(t) {
                    uniq.push(t.clone());
                }
            }
            uniq
        })
        .collect()
}

/// A step bound to a tool only accepts evidence from that tool. An unbound
/// step accepts any tool — the distinctive-token match is what attributes it.
fn step_tool_compatible(step: &PlanStep, call: &ToolCall) -> bool {
    match step.tool_name.as_deref() {
        Some(t) if !t.is_empty() => call.tool_name == t,
        _ => true,
    }
}

fn step_is_tool_bound(step: &PlanStep) -> bool {
    step.tool_name
        .as_deref()
        .map(|t| !t.is_empty())
        .unwrap_or(false)
}

/// Verify each step against the turn's tool history, one call to at most one
/// step. `prior_verified` carries flags from earlier iterations of the same
/// turn; a step once verified stays verified even after its evidence scrolls
/// out of the working history.
///
/// Two passes, both consuming: strong matches (distinctive token present in
/// the call's arguments) are assigned first so that a generic tool-name match
/// cannot steal the evidence a named step needs.
pub fn verify_plan_steps(
    plan: &ActivePlan,
    tool_history: &[(ToolCall, ToolResult)],
    prior_verified: &[bool],
) -> PlanVerification {
    let distinctive = distinctive_tokens(plan);
    let mut consumed = vec![false; tool_history.len()];
    let mut evidence = vec![StepEvidence::NotCheckable; plan.steps.len()];

    for (i, ev) in evidence.iter_mut().enumerate() {
        if prior_verified.get(i).copied().unwrap_or(false) {
            *ev = StepEvidence::Verified;
        }
    }

    let haystacks: Vec<String> = tool_history
        .iter()
        .map(|(call, _)| {
            format!(
                "{} {}",
                call.tool_name.replace(['.', '_', '-'], " "),
                call.arguments
            )
            .to_lowercase()
        })
        .collect();

    // Pass A — strong: a distinctive token of this step appears in the call's
    // arguments (and the tool is compatible).
    for (i, step) in plan.steps.iter().enumerate() {
        if evidence[i] == StepEvidence::Verified || distinctive[i].is_empty() {
            continue;
        }
        for (j, (call, result)) in tool_history.iter().enumerate() {
            if consumed[j] || !tool_result_looks_ok(result) || !step_tool_compatible(step, call) {
                continue;
            }
            if distinctive[i].iter().any(|t| haystacks[j].contains(t)) {
                evidence[i] = StepEvidence::Verified;
                consumed[j] = true;
                break;
            }
        }
    }

    // Pass B — weak: nothing distinguishes this step textually, so a
    // successful call on its bound tool is the best evidence available. Still
    // one-to-one, so N identical steps require N successful calls.
    for (i, step) in plan.steps.iter().enumerate() {
        if evidence[i] == StepEvidence::Verified
            || !distinctive[i].is_empty()
            || !step_is_tool_bound(step)
        {
            continue;
        }
        for (j, (call, result)) in tool_history.iter().enumerate() {
            if consumed[j] || !tool_result_looks_ok(result) {
                continue;
            }
            if step.tool_name.as_deref() == Some(call.tool_name.as_str()) {
                evidence[i] = StepEvidence::Verified;
                consumed[j] = true;
                break;
            }
        }
    }

    // Whatever is left: a tool-bound step is checkable and came up empty.
    // A tool-free step has no artifact to check, so the model's claim stands.
    let mut contradicted = Vec::new();
    for (i, step) in plan.steps.iter().enumerate() {
        if evidence[i] == StepEvidence::Verified || !step_is_tool_bound(step) {
            continue;
        }
        evidence[i] = StepEvidence::Missing;
        if step.status == "done" {
            contradicted.push(step.id);
        }
    }

    PlanVerification {
        evidence,
        contradicted_step_ids: contradicted,
        non_atomic_step_ids: atomicity_violations(plan),
    }
}

/// Steps that bundle several artifacts into one description.
///
/// Such a step cannot be verified one-to-one: the first successful call clears
/// it while the remaining artifacts silently never happen. This is the exact
/// shape of the live Beacon failure — a single step reading "propose Zerin,
/// Mali and Daxton" was marked done after only Zerin landed, and the agent
/// then told the operator all five children were in place when Daxton did not
/// exist. One step must mean one verifiable outcome.
pub fn atomicity_violations(plan: &ActivePlan) -> Vec<u32> {
    plan.steps
        .iter()
        .filter(|s| description_enumerates_artifacts(&s.description))
        .map(|s| s.id)
        .collect()
}

fn description_enumerates_artifacts(description: &str) -> bool {
    let lower = description.to_lowercase();
    let separators = lower.matches(", ").count()
        + lower.matches("; ").count()
        + lower.matches(" and ").count()
        + lower.matches(" & ").count()
        + lower.matches('\n').count();
    separators >= 2
}

/// Whole-plan verdict for the turn, grounded in [`verify_plan_steps`].
#[derive(Debug, Clone)]
pub struct PlanCompletion {
    pub total: usize,
    pub verified: usize,
    /// Indices of steps still outstanding — not verified, and not merely
    /// unverifiable reasoning steps the model has already claimed.
    pub outstanding: Vec<usize>,
    /// True when nothing is outstanding: safe to compose a final reply that
    /// claims the plan is done.
    pub complete: bool,
}

/// Evaluate the plan as a whole. A step counts as settled when it is either
/// verified by tool evidence, or not checkable *and* claimed done by the
/// model. Anything else is outstanding and must not be reported as finished.
///
/// A non-atomic step is never settled, whatever its evidence says. This is the
/// load-bearing case: "propose Zerin, Mali and Daxton" is *verified* by the one
/// `life.observe` that ran for Zerin, so evidence alone reports it complete and
/// the other two artifacts vanish silently — which is the original incident. A
/// step that bundles outcomes cannot be checked one-to-one, so it stays
/// outstanding until it is split.
pub fn evaluate_whole_plan(plan: &ActivePlan, verification: &PlanVerification) -> PlanCompletion {
    let mut outstanding = Vec::new();
    for (i, step) in plan.steps.iter().enumerate() {
        if verification.non_atomic_step_ids.contains(&step.id) {
            outstanding.push(i);
            continue;
        }
        let settled = match verification.evidence.get(i) {
            Some(StepEvidence::Verified) => true,
            Some(StepEvidence::NotCheckable) => step.status == "done",
            _ => false,
        };
        if !settled {
            outstanding.push(i);
        }
    }
    PlanCompletion {
        total: plan.steps.len(),
        verified: verification.verified_count(),
        complete: outstanding.is_empty() && !plan.steps.is_empty(),
        outstanding,
    }
}

/// The guard that stops a turn from claiming work it did not do.
///
/// Injected into the projection before the final reply. Returns `None` when
/// every step is settled — in that case the model is free to report success.
pub fn plan_integrity_note(
    plan: &ActivePlan,
    verification: &PlanVerification,
    completion: &PlanCompletion,
) -> Option<String> {
    if completion.complete {
        return None;
    }

    let mut note = String::from(
        "[Plan integrity check] Do NOT tell the user the plan is finished. \
         Verification against this turn's tool results found work that did not land:\n",
    );

    for i in &completion.outstanding {
        let Some(step) = plan.steps.get(*i) else {
            continue;
        };
        let why = if verification.non_atomic_step_ids.contains(&step.id) {
            "this step bundles several outcomes, so one tool call cannot have completed all of \
             them. Split it into one step per outcome and do the parts that are still missing"
        } else {
            match verification.evidence.get(*i) {
                Some(StepEvidence::Missing) if step.status == "done" => {
                    "you marked this done, but no successful tool call in this turn did it"
                }
                Some(StepEvidence::Missing) => "no successful tool call in this turn did it",
                _ => "not completed",
            }
        };
        note.push_str(&format!(
            "- step {}: {} — {why}\n",
            step.id, step.description
        ));
    }

    if !verification.non_atomic_step_ids.is_empty() {
        note.push_str(&format!(
            "Steps {} each bundle several outcomes, so they cannot be checked individually. \
             Split them into one step per outcome and execute the parts that are still missing.\n",
            verification
                .non_atomic_step_ids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }

    note.push_str(
        "State plainly which items are done and which are not. Never claim an artifact exists \
         unless a tool call in this turn created it.",
    );
    Some(note)
}

/// Whether this turn should declare a plan before acting.
///
/// Planning used to be opt-in behind a keyword allowlist ("plan", "roadmap",
/// "design", ...), so ordinary multi-part requests executed one tool at a time
/// with no structure to verify against. The polarity is now inverted: plan by
/// default, and skip only for turns that are trivially conversational.
///
/// The skip branch is not a nicety. A four-step plan attached to "Going for a
/// run tonight." is a worse experience than the bug being fixed here.
pub fn should_plan(user_content: &str) -> bool {
    let trimmed = user_content.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_lowercase();

    // Anything that asks for something, or asks a question, gets a plan —
    // even a one-step one, so its completion is checkable.
    //
    // Matched on word boundaries, not as raw substrings: "over budget" ends in
    // "get", "forget it" contains "get", and "planning to relax" contains
    // "plan". A false positive only costs a one-step plan, but it costs it on
    // exactly the chit-chat the skip branch exists to protect.
    const REQUEST_MARKERS: &[&str] = &[
        "can you",
        "could you",
        "would you",
        "please",
        "i need",
        "i want",
        "lets",
        "let s",
        "add",
        "create",
        "set",
        "update",
        "change",
        "get",
        "find",
        "make",
        "show",
        "list",
        "record",
        "track",
        "plan",
        "fix",
        "check",
        "remove",
        "delete",
        "schedule",
        "remind",
        "write",
        "build",
        "send",
        "look up",
        "figure out",
        "help me",
        "map",
        "propose",
    ];
    let words_padded = format!(
        " {} ",
        lower
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { ' ' })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    );
    if trimmed.contains('?')
        || REQUEST_MARKERS
            .iter()
            .any(|m| words_padded.contains(&format!(" {m} ")))
    {
        return true;
    }

    // A statement carrying several distinct facts is work even without an
    // explicit ask — each fact is something to capture, and handling only the
    // first is exactly the failure this replaces.
    let words = trimmed.split_whitespace().count();
    let sentences = trimmed
        .split(|c| c == '.' || c == '!' || c == '\n')
        .filter(|s| s.split_whitespace().count() >= 3)
        .count();
    if sentences >= 2 || words >= 25 {
        return true;
    }

    false
}

/// Instruction injected when a plan-worthy turn has not declared a plan yet.
///
/// The atomicity requirement is load-bearing, not style. A step that reads
/// "propose Zerin, Mali and Daxton" is cleared by one successful call while
/// the other two silently never happen — which is precisely how an operator
/// was told all five of their children had been added when one had not.
/// One step must mean one verifiable outcome.
pub fn plan_directive() -> &'static str {
    "[Plan first] Before acting, declare an `active_plan` in your response with a `goal` and \
     ordered `steps`. Rules:\n\
     - One step = one verifiable outcome = one tool call. Never bundle several artifacts into \
     one step; write a separate step per artifact.\n\
     - Set each step's `tool_name` to the tool that will carry it out, so completion can be \
     checked against real results. Leave it unset only for pure reasoning steps.\n\
     - Then execute the steps yourself in THIS turn, updating each step's `status` as you go. \
     Do not send the user a message between steps.\n\
     - A step counts as done only when a successful tool call actually did it."
}

fn outstanding_lines(
    plan: &ActivePlan,
    verification: &PlanVerification,
    completion: &PlanCompletion,
) -> String {
    let mut lines: Vec<String> = completion
        .outstanding
        .iter()
        .filter_map(|i| {
            let step = plan.steps.get(*i)?;
            let marker = match verification.evidence.get(*i) {
                Some(StepEvidence::Missing) if step.status == "done" => {
                    " — you marked this done, but no successful tool call in this turn did it. Redo it."
                }
                _ => "",
            };
            Some(format!("- step {}: {}{marker}", step.id, step.description))
        })
        .collect();

    if !verification.non_atomic_step_ids.is_empty() {
        lines.push(format!(
            "Steps {} each bundle several outcomes into one step, so none of them can be \
             checked individually. Split them into one step per outcome.",
            verification
                .non_atomic_step_ids
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    lines.join("\n")
}

/// The re-entry instruction appended to the projection after tool results.
///
/// Three exits, deliberately. The previous wording offered two — "continue with
/// the next pending step, **or respond to the user if all necessary work is
/// complete**" — and that second clause is the escape hatch that produced
/// one-step turns: the model took it after a single `life.observe` and
/// delivered a receipt instead of finishing the plan, so a five-item request
/// became five round-trips. But deleting the escape entirely is also wrong: a
/// plan can legitimately need the user before it can proceed, and with no
/// stated exit for that the turn burns iterations to the cap and falls back to
/// a canned stop message. So: continue (default), finish, or block — and say
/// which.
pub fn reentry_hint(turn: &WorkingTurn) -> String {
    let Some(plan) = turn.active_plan.as_ref() else {
        return "Review the above tool results. If your task is complete, respond to the user \
                now. Only call another tool if a specific next step is still required."
            .to_string();
    };

    let verification =
        verify_plan_steps(plan, &turn.working_tool_history, &turn.plan_steps_verified);
    let completion = evaluate_whole_plan(plan, &verification);

    if completion.complete {
        return format!(
            "All {} plan steps are complete and verified against this turn's tool results. \
             Deliver your final response to the user now. Do not call any more tools.",
            completion.total
        );
    }

    let outstanding = outstanding_lines(plan, &verification, &completion);

    if plan_execution_budget_exhausted(turn.started_at_unix, unix_now()) {
        return format!(
            "This turn has reached its wall-clock budget with {}/{} steps verified. Stop calling \
             tools and reply now.\nStill outstanding:\n{outstanding}\n\
             State plainly which items are done and which are not, and offer to continue. \
             Never claim an artifact exists unless a tool call in this turn created it.",
            completion.verified, completion.total,
        );
    }

    format!(
        "{}/{} plan steps verified against actual tool results.\nStill outstanding:\n{outstanding}\n\
         Keep working in THIS turn — do not hand the plan back to the user between steps, and do \
         not send a progress receipt after each tool call. Take exactly one of these three exits:\n\
         1. Execute the next outstanding step now with a tool call. This is the default.\n\
         2. If every step is finished, deliver ONE final reply covering the whole plan.\n\
         3. If a step genuinely cannot proceed without the user — a decision only they can make, \
         a missing credential, an ambiguity you cannot resolve — reply now, say specifically what \
         you need, and name the step it blocks.\n\
         Never report a step as done unless a successful tool call in this turn did it.",
        completion.verified, completion.total,
    )
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

    // ── Grounded verification ───────────────────────────────────────────

    /// Tool history with real arguments, so distinctive-token attribution has
    /// something to match against.
    fn history_args(entries: &[(&str, serde_json::Value, &str)]) -> Vec<(ToolCall, ToolResult)> {
        entries
            .iter()
            .map(|(tool, args, content)| {
                (
                    ToolCall {
                        tool_name: (*tool).to_string(),
                        arguments: args.clone(),
                    },
                    ToolResult {
                        tool_name: (*tool).to_string(),
                        content: (*content).to_string(),
                    },
                )
            })
            .collect()
    }

    fn observe(name: &str) -> serde_json::Value {
        serde_json::json!({ "claim": format!("Propose {name} as a Person node") })
    }

    /// The live Beacon failure: five children, one step each, all marked
    /// `done` by the model, but no `life.observe` ever ran for Daxton. The
    /// LifeGraph confirmed Daxton was missing while Beacon reported all five
    /// were in place.
    #[test]
    fn missing_artifact_is_caught_despite_model_claiming_done() {
        let p = plan(
            "done",
            &[
                ("Propose Taysha Telenar", Some("life.observe"), "done"),
                (
                    "Propose Xanthos Gabriel Wagner",
                    Some("life.observe"),
                    "done",
                ),
                ("Propose Zerin Maluy", Some("life.observe"), "done"),
                (
                    "Propose Mali-KJerstine Althoff",
                    Some("life.observe"),
                    "done",
                ),
                ("Propose Daxton Thomas Wagner", Some("life.observe"), "done"),
            ],
        );
        // Four of five actually ran. Daxton never did.
        let h = history_args(&[
            (
                "life.observe",
                observe("Taysha Telenar"),
                "{\"node_id\":\"1\"}",
            ),
            (
                "life.observe",
                observe("Xanthos Gabriel Wagner"),
                "{\"node_id\":\"2\"}",
            ),
            (
                "life.observe",
                observe("Zerin Maluy"),
                "{\"node_id\":\"3\"}",
            ),
            (
                "life.observe",
                observe("Mali-KJerstine Althoff"),
                "{\"node_id\":\"4\"}",
            ),
        ]);

        let v = verify_plan_steps(&p, &h, &[]);
        assert_eq!(v.verified_count(), 4);
        assert_eq!(
            v.evidence[4],
            StepEvidence::Missing,
            "Daxton must not verify"
        );
        assert_eq!(v.contradicted_step_ids, vec![5]);

        let c = evaluate_whole_plan(&p, &v);
        assert!(!c.complete, "plan must not read as complete");
        assert_eq!(c.outstanding, vec![4]);

        // And the final reply is explicitly barred from claiming success.
        let note = plan_integrity_note(&p, &v, &c).expect("integrity note");
        assert!(note.contains("Daxton"));
        assert!(note.contains("you marked this done"));

        // Contrast: the legacy evaluator accepts the model's own claim.
        assert_eq!(
            evaluate_plan(&p, None, &h).verdict,
            PlanEvalVerdict::Complete
        );
    }

    /// One successful call must not clear several same-tool steps.
    #[test]
    fn evidence_is_consumed_one_call_per_step() {
        let p = plan(
            "executing",
            &[
                ("Propose Zerin Maluy", Some("life.observe"), "done"),
                ("Propose Daxton Thomas Wagner", Some("life.observe"), "done"),
            ],
        );
        let h = history_args(&[(
            "life.observe",
            observe("Zerin Maluy"),
            "{\"node_id\":\"3\"}",
        )]);
        let v = verify_plan_steps(&p, &h, &[]);
        assert_eq!(v.verified_count(), 1);
        assert_eq!(v.evidence[0], StepEvidence::Verified);
        assert_eq!(v.evidence[1], StepEvidence::Missing);
    }

    /// Steps with no distinguishing text still consume one call each, so two
    /// identical steps need two successful calls.
    #[test]
    fn indistinguishable_steps_each_need_their_own_call() {
        let p = plan(
            "executing",
            &[
                ("run the sweep", Some("hotel.sweep"), "done"),
                ("run the sweep", Some("hotel.sweep"), "done"),
            ],
        );
        let one = history_args(&[("hotel.sweep", serde_json::json!({}), "ok")]);
        let v = verify_plan_steps(&p, &one, &[]);
        assert_eq!(v.verified_count(), 1);

        let two = history_args(&[
            ("hotel.sweep", serde_json::json!({}), "ok"),
            ("hotel.sweep", serde_json::json!({}), "ok"),
        ]);
        assert_eq!(verify_plan_steps(&p, &two, &[]).verified_count(), 2);
    }

    /// A reasoning step declares no tool, so there is no artifact to check and
    /// the model's own claim stands. Verification must not invent failures.
    #[test]
    fn tool_free_step_is_not_checkable_and_settles_on_model_claim() {
        let p = plan(
            "executing",
            &[
                ("Propose Zerin Maluy", Some("life.observe"), "done"),
                ("Summarize the family map for the user", None, "done"),
            ],
        );
        let h = history_args(&[("life.observe", observe("Zerin Maluy"), "ok")]);
        let v = verify_plan_steps(&p, &h, &[]);
        assert_eq!(v.evidence[1], StepEvidence::NotCheckable);
        assert!(v.contradicted_step_ids.is_empty());

        let c = evaluate_whole_plan(&p, &v);
        assert!(c.complete);
        assert!(plan_integrity_note(&p, &v, &c).is_none());
    }

    /// A pending tool-free step is still outstanding — "not checkable" is not
    /// a free pass.
    #[test]
    fn pending_tool_free_step_stays_outstanding() {
        let p = plan(
            "executing",
            &[("Summarize the family map", None, "pending")],
        );
        let v = verify_plan_steps(&p, &[], &[]);
        let c = evaluate_whole_plan(&p, &v);
        assert!(!c.complete);
        assert_eq!(c.outstanding, vec![0]);
    }

    /// Evidence scrolls out of the working history as a turn grows; a step
    /// verified on an earlier iteration must stay verified.
    #[test]
    fn prior_verified_flags_are_sticky() {
        let p = plan(
            "executing",
            &[("Propose Zerin Maluy", Some("life.observe"), "done")],
        );
        let v = verify_plan_steps(&p, &[], &[true]);
        assert_eq!(v.evidence[0], StepEvidence::Verified);
        assert!(v.contradicted_step_ids.is_empty());
    }

    #[test]
    fn failed_tool_result_is_not_evidence() {
        let p = plan(
            "executing",
            &[("Propose Zerin Maluy", Some("life.observe"), "done")],
        );
        let h = history_args(&[(
            "life.observe",
            observe("Zerin Maluy"),
            "Error: runner unavailable",
        )]);
        let v = verify_plan_steps(&p, &h, &[]);
        assert_eq!(v.evidence[0], StepEvidence::Missing);
        assert_eq!(v.contradicted_step_ids, vec![1]);
    }

    /// The bundled step that started all of this.
    #[test]
    fn bundled_step_is_flagged_non_atomic() {
        let p = plan(
            "executing",
            &[(
                "Propose Zerin Maluy, Mali-KJerstine Althoff, and Daxton Thomas Wagner as Person nodes",
                Some("life.observe"),
                "done",
            )],
        );
        assert_eq!(atomicity_violations(&p), vec![1]);

        // Only Zerin's call ran. Evidence alone would mark the bundled step
        // Verified and let the turn report all three as done — that is the
        // original incident. Bundling must override the evidence.
        let h = history_args(&[("life.observe", observe("Zerin Maluy"), "ok")]);
        let v = verify_plan_steps(&p, &h, &[]);
        assert_eq!(v.evidence[0], StepEvidence::Verified, "one call did match");

        let c = evaluate_whole_plan(&p, &v);
        assert!(
            !c.complete,
            "a bundled step must never settle the plan, however it verified"
        );
        assert_eq!(c.outstanding, vec![0]);

        let note = plan_integrity_note(&p, &v, &c).expect("integrity note must fire");
        assert!(note.contains("bundles several outcomes"));
        assert!(note.contains("Split it into one step per outcome"));
    }

    #[test]
    fn atomic_steps_are_not_flagged() {
        let p = plan(
            "executing",
            &[
                (
                    "Propose Daxton Thomas Wagner as a Person node",
                    None,
                    "pending",
                ),
                ("Read the config and apply the fix", None, "pending"),
            ],
        );
        assert!(atomicity_violations(&p).is_empty());
    }

    // ── Plan-by-default gate ────────────────────────────────────────────

    #[test]
    fn trivial_chat_does_not_get_a_plan() {
        // Real messages from the session that motivated this change.
        for msg in [
            "Going for a run tonight.",
            "Kelley and I are watching The Bear - fourth season 😉",
            "ok",
            "Chef! 🫡",
            // Substring collisions: these contain "get"/"plan"/"set" inside
            // longer words and must not trip the request markers.
            "forget it",
            "we came in over budget",
            "planning to relax tonight",
            "the sunset was unreal",
        ] {
            assert!(!should_plan(msg), "should not plan: {msg}");
        }
    }

    #[test]
    fn requests_and_multi_fact_statements_get_a_plan() {
        for msg in [
            "let's get all my children added 🙂",
            "do we have my children in my lifegraph?",
            "So I do need to make sure that I remember my vacuuming chore every night. \
             I am working on Beethoven's Moonlight Sonata (specifically the 3rd movement). \
             I went into the office today but i feel bad for doing it",
            "can you record this for me",
        ] {
            assert!(should_plan(msg), "should plan: {msg}");
        }
    }

    // ── Re-entry hint ───────────────────────────────────────────────────

    fn turn_with(plan: Option<ActivePlan>, history: Vec<(ToolCall, ToolResult)>) -> WorkingTurn {
        let mut turn = WorkingTurn::for_plan_tests();
        turn.plan_steps_verified = vec![false; plan.as_ref().map(|p| p.steps.len()).unwrap_or(0)];
        turn.active_plan = plan;
        turn.working_tool_history = history;
        turn.started_at_unix = Some(unix_now());
        turn
    }

    /// The regression that matters: with a step still unverified, the hint must
    /// not license bailing out to the user with a receipt.
    #[test]
    fn hint_pushes_the_turn_to_keep_going_when_work_remains() {
        let p = plan(
            "executing",
            &[
                ("Propose Zerin Maluy", Some("life.observe"), "done"),
                ("Propose Daxton Thomas Wagner", Some("life.observe"), "done"),
            ],
        );
        let h = history_args(&[("life.observe", observe("Zerin Maluy"), "ok")]);
        let hint = reentry_hint(&turn_with(Some(p), h));

        assert!(hint.contains("1/2 plan steps verified"));
        assert!(hint.contains("Daxton"));
        assert!(hint.contains("Execute the next outstanding step now"));
        assert!(hint.contains("do not hand the plan back to the user between steps"));
        // The old escape hatch must be gone.
        assert!(!hint.contains("or respond to the user if"));
    }

    /// But a genuine blocker still has somewhere to go — otherwise the turn
    /// just burns iterations into the cap.
    #[test]
    fn hint_keeps_an_exit_for_work_that_needs_the_user() {
        let p = plan(
            "executing",
            &[(
                "Propose Daxton Thomas Wagner",
                Some("life.observe"),
                "pending",
            )],
        );
        let hint = reentry_hint(&turn_with(Some(p), vec![]));
        assert!(hint.contains("cannot proceed without the user"));
        assert!(hint.contains("name the step it blocks"));
    }

    #[test]
    fn hint_releases_the_turn_once_everything_is_verified() {
        let p = plan(
            "executing",
            &[("Propose Zerin Maluy", Some("life.observe"), "done")],
        );
        let h = history_args(&[("life.observe", observe("Zerin Maluy"), "ok")]);
        let hint = reentry_hint(&turn_with(Some(p), h));
        assert!(hint.contains("Deliver your final response to the user now"));
        assert!(hint.contains("Do not call any more tools"));
    }

    #[test]
    fn hint_stops_the_turn_when_the_wall_clock_budget_is_spent() {
        let p = plan(
            "executing",
            &[(
                "Propose Daxton Thomas Wagner",
                Some("life.observe"),
                "pending",
            )],
        );
        let mut turn = turn_with(Some(p), vec![]);
        turn.started_at_unix = Some(unix_now() - PLAN_EXECUTION_BUDGET_SECS - 1);
        let hint = reentry_hint(&turn);
        assert!(hint.contains("wall-clock budget"));
        assert!(hint.contains("Stop calling tools and reply now"));
        assert!(hint.contains("Never claim an artifact exists"));
    }

    #[test]
    fn execution_budget_binds_only_after_the_window() {
        assert!(!plan_execution_budget_exhausted(Some(1_000), 1_000));
        assert!(!plan_execution_budget_exhausted(
            Some(1_000),
            1_000 + PLAN_EXECUTION_BUDGET_SECS - 1
        ));
        assert!(plan_execution_budget_exhausted(
            Some(1_000),
            1_000 + PLAN_EXECUTION_BUDGET_SECS
        ));
        // Budget must leave room to compose a reply before the reaper fires.
        assert!(PLAN_EXECUTION_BUDGET_SECS + 60 <= TURN_ZOMBIE_REAP_SECS);
        // Unknown start time fails open rather than truncating every turn.
        assert!(!plan_execution_budget_exhausted(None, u64::MAX));
    }
}
