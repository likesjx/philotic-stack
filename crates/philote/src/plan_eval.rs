//! Plan → execute → verify each step → evaluate the whole plan.
//!
//! There are two layers here, and the difference between them matters.
//!
//! **Cross-turn carryover (the repeat stage).** After a turn's final Respond,
//! [`evaluate_plan`] derives a verdict for the turn's `ActivePlan` without a
//! second model round-trip. The runtime persists a
//! [`crate::session::CarryoverPlan`], emits `plan_eval` / `plan_continuation`
//! turn events, and synthesizes budgeted continuation turns through
//! `pending_drains`. This is the overflow path for work that genuinely exceeds
//! one turn, and it repeats until the plan is verifiably done, the loop stalls
//! out, or the budget is spent.
//!
//! This layer used to trust `step.status == "done"` — the same self-certification
//! the in-turn layer below was built to eliminate. That left the two halves of
//! the cycle disagreeing: the turn refused to *claim* completion it could not
//! prove, while the eval that decided whether to *repeat* took the model's word
//! for it, so a plan that marked itself done escaped the loop with the work
//! undone. Both layers now share [`verify_plan_steps`] / [`evaluate_whole_plan`],
//! and the carryover carries evidence (`verified_step_ids`) separately from
//! settlement (`steps_done`) so a model claim can never be fed back as proof.
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

/// Ceiling on the outstanding-scaled continuation budget. Bounds the repeat
/// stage so a plan can never loop indefinitely, however many steps it declares.
pub const PLAN_CONTINUATION_BUDGET_CEILING: u32 = 8;

/// Absolute cap on continuation turns over a plan's whole lifetime.
///
/// The per-stretch budget is refunded whenever a continuation settles a new
/// step (progress must never be what exhausts the loop — stalls are what the
/// budget exists to bound, and `MAX_CONSECUTIVE_PLAN_STALLS` already blocks a
/// spin). This cap is the backstop the refund needs: a plan that keeps growing
/// its own step list could otherwise alternate one settled step with fresh
/// work forever. Sized at 3× the per-stretch ceiling — far above any plan the
/// loop legitimately runs, so hitting it is itself a signal worth surfacing.
pub const PLAN_CONTINUATION_LIFETIME_CAP: u32 = 24;

/// Consecutive stalled continuations tolerated before the plan is `Blocked`.
///
/// One stall is not failure. Under grounded evaluation a turn only counts as
/// progress when a step actually settles, and a turn legitimately spent on a
/// failed call, a rate limit, or a read that sets up the next step settles
/// nothing — the previous rule (`Blocked` on the first stall) killed those
/// plans one turn before they would have recovered. Two in a row is a spin.
pub const MAX_CONSECUTIVE_PLAN_STALLS: u32 = 2;

/// Continuation budget for a plan, widened to fit the work actually left.
///
/// A flat budget of 3 is generous for a two-step plan and arbitrary for a
/// twelve-step one; the same ceiling then truncates every large plan at the
/// same place regardless of size. Scale with outstanding steps, bounded by
/// [`PLAN_CONTINUATION_BUDGET_CEILING`], and never shrink a configured budget.
pub fn scaled_continuation_budget(configured: u32, outstanding: usize) -> u32 {
    let scaled = u32::try_from(outstanding)
        .unwrap_or(PLAN_CONTINUATION_BUDGET_CEILING)
        .min(PLAN_CONTINUATION_BUDGET_CEILING);
    configured.max(scaled)
}

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
    /// At least one step is bound to a tool, so completion was checked against
    /// real tool results.
    Grounded,
    /// No step in the plan declares a tool, so there is no artifact to check
    /// and the model's own claims are the only signal available. Surfaced in
    /// the `plan_eval` event because it is also the cheapest way to evade
    /// verification: a plan that binds no tools cannot be contradicted.
    ModelReported,
}

impl PlanEvalBasis {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Grounded => "grounded",
            Self::ModelReported => "model_reported",
        }
    }
}

/// Result of a plan eval over a completed turn.
#[derive(Debug, Clone)]
pub struct PlanEvalOutcome {
    pub steps_total: usize,
    /// Steps that are settled: verified by evidence, or unverifiable and
    /// claimed done by the model.
    pub steps_done: usize,
    /// Steps backed by an actual tool result. Never exceeds `steps_done`.
    pub steps_verified: usize,
    /// Index-aligned settled flags.
    pub steps_done_flags: Vec<bool>,
    /// Ids of evidence-backed steps, to carry into the next continuation.
    pub verified_step_ids: Vec<u32>,
    /// Steps counted as settled on the model's word alone (no bound tool).
    pub uncertain_step_ids: Vec<u32>,
    /// Steps the model marked done that the tool history contradicts.
    pub contradicted_step_ids: Vec<u32>,
    /// Steps bundling several outcomes; never settled until split.
    pub non_atomic_step_ids: Vec<u32>,
    /// Ids of steps still to do.
    pub outstanding_step_ids: Vec<u32>,
    /// Consecutive continuations (including this one) that settled nothing new.
    pub stalled_continuations: u32,
    pub verdict: PlanEvalVerdict,
    pub basis: PlanEvalBasis,
}

impl PlanEvalOutcome {
    /// Compact JSON record for the `plan_eval` turn event.
    pub fn event_json(&self) -> serde_json::Value {
        json!({
            "steps_total": self.steps_total,
            "steps_done": self.steps_done,
            "steps_verified": self.steps_verified,
            "verdict": self.verdict.as_str(),
            "basis": self.basis.as_str(),
            "uncertain_steps": self.uncertain_step_ids,
            "contradicted_steps": self.contradicted_step_ids,
            "non_atomic_steps": self.non_atomic_step_ids,
            "outstanding_steps": self.outstanding_step_ids,
            "stalls": self.stalled_continuations,
        })
    }
}

/// What an earlier turn of the *same* plan established, threaded into this eval.
#[derive(Debug, Clone, Copy, Default)]
pub struct PriorPlanState<'a> {
    /// Evidence-backed step ids from previous turns. Only ever set by
    /// [`verify_plan_steps`] against a real tool result.
    pub verified_step_ids: &'a [u32],
    /// How many steps were settled as of the previous eval, for stall detection.
    pub settled_count: usize,
    /// Consecutive stalls already recorded for this plan.
    pub stalls: u32,
}

/// True when any step declares a tool, i.e. the plan has something checkable.
fn plan_has_checkable_step(plan: &ActivePlan) -> bool {
    plan.steps.iter().any(step_is_tool_bound)
}

/// Evaluate the plan after a turn's final Respond, and decide whether the
/// cycle repeats.
///
/// Completion is grounded: a step settles when [`verify_plan_steps`] attributes
/// a successful tool call to it, or — only when it declares no tool, so there
/// is nothing to check — when the model claims it. `plan.status == "done"` is
/// no longer sufficient on its own; a plan that marks itself finished with
/// unverified steps yields `Continue`, and the loop goes back for the rest.
///
/// `prior` carries the same plan's earlier state. Evidence comes in as step
/// **ids** (never as settled flags — see [`CarryoverPlan::verified_step_ids`]),
/// because a continuation that splits a bundled step renumbers the tail.
pub fn evaluate_plan(
    plan: &ActivePlan,
    prior: Option<PriorPlanState<'_>>,
    tool_history: &[(ToolCall, ToolResult)],
) -> PlanEvalOutcome {
    let total = plan.steps.len();
    let plan_declared_failed = plan.status == "failed";
    let checkable = plan_has_checkable_step(plan);

    // Evidence carried from earlier turns of this plan, re-keyed by step id.
    // Each continuation turn starts with a fresh `working_tool_history`, so
    // without this the proof of an already-finished step disappears and the
    // loop redoes it.
    let prior_verified: Vec<bool> = plan
        .steps
        .iter()
        .map(|s| {
            prior
                .map(|p| p.verified_step_ids.contains(&s.id))
                .unwrap_or(false)
        })
        .collect();

    let verification = verify_plan_steps(plan, tool_history, &prior_verified);
    let completion = evaluate_whole_plan(plan, &verification);

    let mut flags = vec![false; total];
    let mut uncertain: Vec<u32> = Vec::new();
    for (i, step) in plan.steps.iter().enumerate() {
        flags[i] = !completion.outstanding.contains(&i);
        // Settled without evidence: nothing to check it against, so this rests
        // on the model's word. Reported so the gap stays visible.
        if flags[i] && verification.evidence.get(i) != Some(&StepEvidence::Verified) {
            uncertain.push(step.id);
        }
    }

    // A plan that binds no tools anywhere has nothing to verify, so a
    // whole-plan `done` still covers steps the model left unmarked. Without
    // this a purely conversational plan could never settle and would spin the
    // continuation budget on work that has no artifact to produce.
    let mut complete = completion.complete;
    if !checkable && plan.status == "done" && total > 0 {
        flags.iter_mut().for_each(|f| *f = true);
        uncertain = plan.steps.iter().map(|s| s.id).collect();
        complete = true;
    }

    let done = flags.iter().filter(|f| **f).count();
    let verified_step_ids: Vec<u32> = plan
        .steps
        .iter()
        .enumerate()
        .filter(|(i, _)| verification.evidence.get(*i) == Some(&StepEvidence::Verified))
        .map(|(_, s)| s.id)
        .collect();
    let outstanding_step_ids: Vec<u32> = plan
        .steps
        .iter()
        .enumerate()
        .filter(|(i, _)| !flags[*i])
        .map(|(_, s)| s.id)
        .collect();

    // A continuation that settled nothing new is a stall. One is tolerated —
    // see MAX_CONSECUTIVE_PLAN_STALLS — because under grounded evaluation a
    // turn spent on a failed call makes no progress but is not yet a spin.
    let stalls = match prior {
        Some(p) if done <= p.settled_count => p.stalls.saturating_add(1),
        Some(_) => 0,
        None => 0,
    };

    let every_step_settled_or_failed = plan
        .steps
        .iter()
        .enumerate()
        .all(|(i, s)| flags[i] || s.status == "failed");

    let verdict = if plan_declared_failed {
        PlanEvalVerdict::Blocked
    } else if complete {
        PlanEvalVerdict::Complete
    } else if every_step_settled_or_failed {
        // Everything is either settled or explicitly failed — nothing left to
        // continue with.
        PlanEvalVerdict::Blocked
    } else if stalls >= MAX_CONSECUTIVE_PLAN_STALLS {
        PlanEvalVerdict::Blocked
    } else {
        PlanEvalVerdict::Continue
    };

    PlanEvalOutcome {
        steps_total: total,
        steps_done: done,
        steps_verified: verification.verified_count(),
        steps_done_flags: flags,
        verified_step_ids,
        uncertain_step_ids: uncertain,
        contradicted_step_ids: verification.contradicted_step_ids.clone(),
        non_atomic_step_ids: verification.non_atomic_step_ids.clone(),
        outstanding_step_ids,
        stalled_continuations: stalls,
        verdict,
        basis: if checkable {
            PlanEvalBasis::Grounded
        } else {
            PlanEvalBasis::ModelReported
        },
    }
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
///
/// The remaining list is not a bare restatement of the plan. It says *why* each
/// step is still outstanding, because the two ways a step survives a turn need
/// opposite responses: a step the model marked done but no tool call performed
/// must be redone (restating it plainly invites the model to mark it done
/// again), and a step bundling several outcomes must be split before it can
/// ever settle — under grounded evaluation it is unconditionally outstanding,
/// so a continuation that does not split it burns the whole budget and still
/// finishes with the plan unfinished.
pub fn plan_continuation_brief(carryover: &CarryoverPlan, budget: u32) -> String {
    let done_list = step_list(&carryover.plan, &carryover.steps_done, true);
    // No tool history: with the carryover's evidence as the only input, this
    // reports each step's standing as of the last eval.
    let verification = verify_plan_steps(
        &carryover.plan,
        &[],
        &carryover.verified_flags_for(&carryover.plan),
    );
    let completion = evaluate_whole_plan(&carryover.plan, &verification);
    let remaining_list = outstanding_lines(&carryover.plan, &verification, &completion);

    let mut brief = format!(
        "[Plan continuation {}/{}] Continue executing your existing plan. Goal: {}\n",
        carryover.continuations_used + 1,
        budget,
        carryover.plan.goal
    );
    if !done_list.is_empty() {
        brief.push_str(&format!("Completed steps:\n{done_list}\n"));
    }
    brief.push_str(&format!("Remaining steps:\n{remaining_list}\n"));

    if carryover.stalled_continuations > 0 {
        brief.push_str(
            "The previous continuation finished without completing a single outstanding step. \
             Do not repeat the approach that just failed — take a different route to the same \
             outcome, or say specifically what is blocking you.\n",
        );
    }

    // The loop stops after this turn, and it stops as a turn event the operator
    // never sees. If the model does not name the shortfall in its own reply,
    // nobody learns the work was abandoned — which is the same silence as a
    // false success claim, just from the other direction.
    if carryover.continuations_used + 1 >= budget {
        brief.push_str(
            "This is the LAST continuation for this plan — no further automatic turns follow. \
             If you cannot finish everything here, your reply must tell the user plainly which \
             items are done, which are not, and what you need in order to finish. Do not imply \
             that work will continue on its own.\n",
        );
    }

    brief.push_str(
        "Pick up at the first remaining step. Do not re-plan from scratch and do not repeat \
         completed work. Update step statuses in active_plan as you execute. A step counts as \
         done only when a successful tool call in this turn actually did it — marking it done \
         without one leaves it outstanding. When every step is genuinely done, deliver a final \
         summary of the whole plan to the user.",
    );
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
        "*(Plan stopped: {reason}. Goal: {}. {done}/{total} steps done. Remaining:\n{remaining}\n\
         The carryover has been cleared — the remaining steps will not run automatically.)*",
        carryover.plan.goal
    )
}

fn step_list(plan: &ActivePlan, flags: &[bool], done: bool) -> String {
    plan.steps
        .iter()
        .enumerate()
        .filter(|(i, _)| flags.get(*i).copied().unwrap_or(false) == done)
        .map(|(_, s)| format!("- step {}{}: {}", s.id, step_tool_suffix(s), s.description))
        .collect::<Vec<_>>()
        .join("\n")
}

/// ` (tool: X)` for a tool-bound step, empty otherwise.
///
/// Load-bearing in continuation briefs, not decoration: the brief is the
/// continuation turn's only tool-projection relevance text, and the keyword
/// gates in `project_tools_for_turn` know nothing about the plan. A brief that
/// names only step descriptions can have exactly the tools the remaining steps
/// need stripped from the projection — worst case, a goal phrased with a `?`
/// trips the conversational gate and the continuation runs with zero tools,
/// stalls twice, and the plan is blocked. Naming the bound tool makes the
/// explicit tool-name match fire first, which bypasses those gates entirely.
fn step_tool_suffix(step: &PlanStep) -> String {
    match step.tool_name.as_deref() {
        Some(t) if !t.is_empty() => format!(" (tool: {t})"),
        _ => String::new(),
    }
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
/// `RepairStaleSessionTurns { min_age_secs }`). There is no heartbeat that
/// resets the clock, so this is a hard wall-clock ceiling on a single turn —
/// not an iteration ceiling. `iteration_cap` was never the binding constraint.
///
/// Re-exported from [`ansible_mesh_core::turn_budget`] so this file and the
/// hotel-side reaper cannot drift apart again: they were independently
/// hardcoded to 300, which put the backstop at or below every guest-side
/// budget it was supposed to outlast.
pub use ansible_mesh_core::turn_budget::TURN_ZOMBIE_REAP_SECS;

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

/// Quiet period between interim messages inside one turn.
///
/// Sized against the turn's own budget: with [`PLAN_EXECUTION_BUDGET_SECS`] at
/// 210, a 45s floor allows at most a handful of interim notes on the longest
/// possible turn, and none at all on a short one.
pub const INTERIM_REPLY_MIN_GAP_SECS: u64 = 45;

/// Iterations a turn runs before it may speak without finishing.
///
/// The first steps of a plan are usually fast, and narrating them is the
/// per-step receipt behaviour the operator objected to. Silence is correct
/// until a turn is demonstrably long-running.
pub const INTERIM_REPLY_MIN_ITERATION: u32 = 2;

/// Whether the loop will carry an interim message from the model right now.
///
/// The model decides *what* to say — it holds the context. The loop decides
/// *whether it may be said*, because only the loop holds the clock and the
/// mandate not to chatter. An interim message does not end the turn: it edits
/// the same draft the final reply will overwrite, so an admitted note costs
/// nothing in the transcript and a declined one costs nothing at all.
pub fn interim_reply_admissible(
    turn_started_at_unix: Option<u64>,
    last_interim_at_unix: Option<u64>,
    iteration: u32,
    now_unix: u64,
) -> bool {
    if iteration < INTERIM_REPLY_MIN_ITERATION {
        return false;
    }
    // Unknown start time — stay quiet rather than risk narrating a fast turn.
    let Some(started) = turn_started_at_unix else {
        return false;
    };
    if now_unix.saturating_sub(started) < INTERIM_REPLY_MIN_GAP_SECS {
        return false;
    }
    match last_interim_at_unix {
        Some(last) => now_unix.saturating_sub(last) >= INTERIM_REPLY_MIN_GAP_SECS,
        None => true,
    }
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
            Some(format!(
                "- step {}{}: {}{marker}",
                step.id,
                step_tool_suffix(step),
                step.description
            ))
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
        // The interim-note invitation must appear here too, not only on the
        // plan branch below. DEF-077's flush shipped on the ToolCall path for
        // every re-entry, but a plan-less multi-tool turn never saw any prompt
        // to use `partial_replies` — and watched-live (2026-08-06) a 458s,
        // six-tool-call turn stayed silent throughout while the loop's gate
        // was open the whole time. Models do not spontaneously use an
        // unexplained optional contract field.
        return "Review the above tool results. If your task is complete, respond to the user \
                now. Only call another tool if a specific next step is still required. \
                If you are continuing a long stretch of work, you may add one short line to \
                `partial_replies` alongside the tool call to tell the user what you are doing — \
                it reaches them without ending the turn, and the loop drops it if you are \
                speaking too often. Use it when the silence would be long, not after every step."
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
         Keep working in THIS turn — do not hand the plan back to the user between steps. \
         Take exactly one of these three exits:\n\
         1. Execute the next outstanding step now with a tool call. This is the default.\n\
         2. If every step is finished, deliver ONE final reply covering the whole plan.\n\
         3. If a step genuinely cannot proceed without the user — a decision only they can make, \
         a missing credential, an ambiguity you cannot resolve — and no other step can move \
         while you wait, reply now, say specifically what you need, and name the step it blocks. \
         If the other steps CAN still move, do not stop: put what you need in `partial_replies` \
         and carry on with them.\n\
         You may also add one short line to `partial_replies` alongside a tool call to say what \
         you are doing on a long stretch of work. It reaches the user without ending the turn, \
         and the loop drops it if you are speaking too often — so use it when the silence would \
         be long, not after every step.\n\
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

    fn carryover(p: ActivePlan, steps_done: Vec<bool>, used: u32) -> CarryoverPlan {
        CarryoverPlan {
            plan: p,
            steps_done,
            verified_step_ids: Vec::new(),
            stalled_continuations: 0,
            continuations_used: used,
            lifetime_continuations: 0,
            created_turn_id: "turn-0".into(),
        }
    }

    #[test]
    fn unbound_steps_all_claimed_done_is_complete() {
        // Nothing declares a tool, so there is no artifact to check against and
        // the model's claim is the only signal. Complete, but reported as
        // uncertain and `model_reported` so the gap is visible.
        let p = plan(
            "executing",
            &[("read config", None, "done"), ("apply fix", None, "done")],
        );
        let out = evaluate_plan(&p, None, &[]);
        assert_eq!(out.verdict, PlanEvalVerdict::Complete);
        assert_eq!(out.basis, PlanEvalBasis::ModelReported);
        assert_eq!(out.steps_done, 2);
        assert_eq!(out.steps_verified, 0);
        assert_eq!(out.uncertain_step_ids, vec![1, 2]);
    }

    #[test]
    fn whole_plan_done_claim_covers_unmarked_steps_only_when_nothing_is_checkable() {
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

    /// The defect this change exists to close, at the carryover layer.
    ///
    /// The model declares the whole plan done and marks every step done. One
    /// step is bound to a tool that never ran successfully. The old eval
    /// returned `Complete` on the strength of those claims and dropped the
    /// carryover, so the loop exited with the work undone — the same
    /// self-certification the in-turn layer already refused to accept.
    #[test]
    fn plan_declaring_itself_done_still_continues_when_a_step_is_unverified() {
        let p = plan(
            "done",
            &[
                ("add Zerin as a Person node", Some("life.observe"), "done"),
                ("add Daxton as a Person node", Some("life.observe"), "done"),
            ],
        );
        // Only Zerin's call landed.
        let h = history_args(&[(
            "life.observe",
            serde_json::json!({"text": "Zerin is a child"}),
            "created life:person:zerin",
        )]);
        let out = evaluate_plan(&p, None, &h);
        assert_eq!(out.verdict, PlanEvalVerdict::Continue);
        assert_eq!(out.basis, PlanEvalBasis::Grounded);
        assert_eq!(out.steps_verified, 1);
        assert_eq!(out.outstanding_step_ids, vec![2]);
        // The false claim is surfaced, not silently accepted.
        assert_eq!(out.contradicted_step_ids, vec![2]);
    }

    #[test]
    fn plan_failed_is_blocked() {
        let p = plan("failed", &[("read config", None, "pending")]);
        let out = evaluate_plan(&p, None, &[]);
        assert_eq!(out.verdict, PlanEvalVerdict::Blocked);
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
    fn bound_tool_success_verifies_step() {
        let p = plan(
            "executing",
            &[
                ("check hotel status", Some("hotel.status"), "pending"),
                ("summarize findings", None, "pending"),
            ],
        );
        let h = history(&[("hotel.status", "hotel green")]);
        let out = evaluate_plan(&p, None, &h);
        assert_eq!(out.basis, PlanEvalBasis::Grounded);
        assert_eq!(out.verdict, PlanEvalVerdict::Continue);
        assert_eq!(out.steps_verified, 1);
        assert_eq!(out.verified_step_ids, vec![1]);
        // Step 1 has evidence, so it is not uncertain; step 2 is unbound and
        // unclaimed, so it is outstanding rather than settled.
        assert!(out.uncertain_step_ids.is_empty());
        assert_eq!(out.outstanding_step_ids, vec![2]);
    }

    #[test]
    fn bound_tool_error_does_not_verify_step() {
        let p = plan(
            "executing",
            &[("check hotel status", Some("hotel.status"), "pending")],
        );
        let h = history(&[("hotel.status", "Error: connection refused")]);
        let out = evaluate_plan(&p, None, &h);
        assert_eq!(out.steps_done, 0);
        assert_eq!(out.verdict, PlanEvalVerdict::Continue);
    }

    /// Token attribution still settles an unbound step — but one call now
    /// clears exactly one step.
    ///
    /// The old heuristic asked "does *any* successful call look like this
    /// step?" for every step independently, so a single `life.observe` answered
    /// yes for all five children and the plan read as complete. Attribution is
    /// consuming: N sibling steps need N successful calls.
    #[test]
    fn one_call_settles_one_step_not_every_similar_sibling() {
        let p = plan(
            "executing",
            &[
                ("recall Zerin from memory", None, "pending"),
                ("recall Daxton from memory", None, "pending"),
            ],
        );
        let h = history_args(&[(
            "memory.recall",
            serde_json::json!({"query": "Zerin"}),
            "1 memory found",
        )]);
        let out = evaluate_plan(&p, None, &h);
        assert_eq!(out.steps_verified, 1, "one call must clear only one step");
        assert_eq!(out.verified_step_ids, vec![1]);
        assert_eq!(out.outstanding_step_ids, vec![2]);
        assert_eq!(out.verdict, PlanEvalVerdict::Continue);
    }

    #[test]
    fn silent_history_continues_with_zero_done() {
        let p = plan(
            "executing",
            &[("write deployment runbook", None, "pending")],
        );
        let out = evaluate_plan(&p, None, &[]);
        assert_eq!(out.verdict, PlanEvalVerdict::Continue);
        assert_eq!(out.steps_done, 0);
    }

    /// Evidence, not settlement, is what carries across turns: a step proven in
    /// turn 1 stays proven in turn 2 even though the continuation starts with
    /// an empty tool history.
    #[test]
    fn prior_evidence_keeps_a_step_verified_across_turns() {
        let p = plan(
            "executing",
            &[
                ("add Zerin", Some("life.observe"), "done"),
                ("add Daxton", Some("life.observe"), "pending"),
            ],
        );
        let prior = PriorPlanState {
            verified_step_ids: &[1],
            settled_count: 1,
            stalls: 0,
        };
        let out = evaluate_plan(&p, Some(prior), &[]);
        assert!(out.steps_done_flags[0], "turn-1 evidence must survive");
        assert_eq!(out.verified_step_ids, vec![1]);
        assert_eq!(out.outstanding_step_ids, vec![2]);
        assert_eq!(out.verdict, PlanEvalVerdict::Continue);
    }

    /// A carryover must never be able to promote a model claim into evidence.
    /// Only `verified_step_ids` seeds verification, and it is populated solely
    /// from `verify_plan_steps` output.
    #[test]
    fn settled_flags_are_not_accepted_as_evidence() {
        let p = plan("executing", &[("add Daxton", Some("life.observe"), "done")]);
        // Nothing ran, and no prior *evidence* exists — only a claim.
        let prior = PriorPlanState {
            verified_step_ids: &[],
            settled_count: 1,
            stalls: 0,
        };
        let out = evaluate_plan(&p, Some(prior), &[]);
        assert_eq!(out.steps_verified, 0);
        assert_eq!(out.outstanding_step_ids, vec![1]);
        assert_ne!(out.verdict, PlanEvalVerdict::Complete);
    }

    /// One stalled continuation is tolerated; two in a row is a spin.
    ///
    /// Under grounded evaluation a turn spent on a failing tool settles
    /// nothing, and blocking on the first such turn kills plans one turn before
    /// they recover.
    #[test]
    fn first_stall_continues_and_second_blocks() {
        let p = plan(
            "executing",
            &[
                ("read config", Some("hotel.status"), "pending"),
                ("apply fix", Some("life.observe"), "pending"),
            ],
        );
        let first = evaluate_plan(
            &p,
            Some(PriorPlanState {
                verified_step_ids: &[],
                settled_count: 0,
                stalls: 0,
            }),
            &[],
        );
        assert_eq!(first.verdict, PlanEvalVerdict::Continue);
        assert_eq!(first.stalled_continuations, 1);

        let second = evaluate_plan(
            &p,
            Some(PriorPlanState {
                verified_step_ids: &[],
                settled_count: 0,
                stalls: first.stalled_continuations,
            }),
            &[],
        );
        assert_eq!(second.verdict, PlanEvalVerdict::Blocked);
        assert_eq!(second.stalled_continuations, 2);
    }

    #[test]
    fn forward_progress_resets_the_stall_counter() {
        let p = plan(
            "executing",
            &[
                ("check hotel status", Some("hotel.status"), "pending"),
                ("apply fix", Some("life.observe"), "pending"),
            ],
        );
        let h = history(&[("hotel.status", "hotel green")]);
        let out = evaluate_plan(
            &p,
            Some(PriorPlanState {
                verified_step_ids: &[],
                settled_count: 0,
                stalls: 1,
            }),
            &h,
        );
        assert_eq!(out.stalled_continuations, 0);
        assert_eq!(out.verdict, PlanEvalVerdict::Continue);
    }

    // ── Interim replies: speaking without ending the turn ────────────────

    #[test]
    fn interim_reply_is_declined_on_the_first_iterations() {
        // Long-running turn, but only one step in: narrating here is the
        // per-step receipt behaviour, not a progress note.
        assert!(!interim_reply_admissible(Some(1_000), None, 0, 1_500));
        assert!(!interim_reply_admissible(Some(1_000), None, 1, 1_500));
        assert!(interim_reply_admissible(Some(1_000), None, 2, 1_500));
    }

    #[test]
    fn interim_reply_is_declined_until_the_turn_is_actually_long() {
        let started = 1_000;
        // Deep into the tool loop but only a few seconds in — still silent.
        assert!(!interim_reply_admissible(
            Some(started),
            None,
            9,
            started + INTERIM_REPLY_MIN_GAP_SECS - 1
        ));
        assert!(interim_reply_admissible(
            Some(started),
            None,
            9,
            started + INTERIM_REPLY_MIN_GAP_SECS
        ));
    }

    #[test]
    fn interim_reply_enforces_a_quiet_period_between_messages() {
        let started = 1_000;
        let first = started + 60;
        assert!(!interim_reply_admissible(
            Some(started),
            Some(first),
            9,
            first + INTERIM_REPLY_MIN_GAP_SECS - 1
        ));
        assert!(interim_reply_admissible(
            Some(started),
            Some(first),
            9,
            first + INTERIM_REPLY_MIN_GAP_SECS
        ));
    }

    /// Unknown start time means the clock cannot be trusted; stay quiet rather
    /// than risk narrating a fast turn.
    #[test]
    fn interim_reply_is_declined_without_a_known_start_time() {
        assert!(!interim_reply_admissible(None, None, 9, 99_999));
    }

    /// The gate must leave room for several notes across the longest turn the
    /// in-turn budget allows, or a long plan still runs effectively silent.
    #[test]
    fn interim_gap_admits_several_notes_within_the_turn_budget() {
        assert!(PLAN_EXECUTION_BUDGET_SECS / INTERIM_REPLY_MIN_GAP_SECS >= 3);
    }

    /// The re-entry hint has to invite the channel it just enabled, or the
    /// model never populates `partial_replies` and the flush is dead code.
    #[test]
    fn reentry_hint_invites_interim_notes_without_licensing_receipts() {
        let p = plan(
            "executing",
            &[
                ("read config", Some("hotel.status"), "pending"),
                ("apply fix", Some("life.observe"), "pending"),
            ],
        );
        let hint = reentry_hint(&turn_with(Some(p), vec![]));
        assert!(hint.contains("partial_replies"));
        assert!(
            hint.contains("without ending the turn"),
            "the model must know speaking does not stop the plan, got: {hint}"
        );
        // The blanket ban is gone, but the anti-chatter framing must remain.
        assert!(!hint.contains("do not send a progress receipt after each tool call"));
        assert!(hint.contains("not after every step"));
    }

    /// The plan-less branch needs the same invitation. Watched-live
    /// (2026-08-06): a 458s six-tool-call turn with no plan emitted zero
    /// interim notes because this branch never mentioned `partial_replies`
    /// while the plan branch did — the field was offered in the contract but
    /// nothing told the model it existed.
    #[test]
    fn planless_reentry_hint_also_invites_interim_notes() {
        let hint = reentry_hint(&turn_with(None, vec![]));
        assert!(hint.contains("partial_replies"));
        assert!(
            hint.contains("without ending the turn"),
            "the model must know speaking does not stop the work, got: {hint}"
        );
        assert!(hint.contains("not after every step"));
    }

    /// Exit 3 used to end the plan whenever the model needed anything. It
    /// should only do so when nothing else can move.
    #[test]
    fn reentry_hint_blocks_only_when_no_other_step_can_move() {
        let p = plan(
            "executing",
            &[
                ("read config", Some("hotel.status"), "pending"),
                ("apply fix", Some("life.observe"), "pending"),
            ],
        );
        let hint = reentry_hint(&turn_with(Some(p), vec![]));
        assert!(hint.contains("If the other steps CAN still move, do not stop"));
    }

    #[test]
    fn continuation_budget_scales_with_outstanding_work_and_is_bounded() {
        // Never shrinks a configured budget.
        assert_eq!(scaled_continuation_budget(3, 1), 3);
        assert_eq!(scaled_continuation_budget(3, 0), 3);
        // Widens to fit the work that is actually left.
        assert_eq!(scaled_continuation_budget(3, 6), 6);
        // Bounded, so a large plan cannot loop indefinitely.
        assert_eq!(
            scaled_continuation_budget(3, 40),
            PLAN_CONTINUATION_BUDGET_CEILING
        );
        assert_eq!(scaled_continuation_budget(12, 40), 12);
    }

    #[test]
    fn continuation_brief_cites_remaining_steps_only() {
        let carry = carryover(
            plan(
                "executing",
                &[
                    ("read config", None, "done"),
                    ("apply fix", None, "pending"),
                ],
            ),
            vec![true, false],
            1,
        );
        let brief = plan_continuation_brief(&carry, 3);
        assert!(brief.contains("[Plan continuation 2/3]"));
        assert!(brief.contains("Completed steps:\n- step 1: read config"));
        assert!(brief.contains("Remaining steps:\n- step 2: apply fix"));
    }

    /// A step the model marked done that no tool call performed must be told to
    /// redo it. Restating it as an ordinary pending step invites the model to
    /// mark it done a second time.
    #[test]
    fn continuation_brief_tells_the_model_to_redo_an_unbacked_claim() {
        let carry = carryover(
            plan("executing", &[("add Daxton", Some("life.observe"), "done")]),
            vec![false],
            1,
        );
        let brief = plan_continuation_brief(&carry, 3);
        assert!(brief.contains("Redo it."), "brief was: {brief}");
    }

    /// A bundled step is unconditionally outstanding, so a plan containing one
    /// can never reach `Complete` until the model splits it. That is deliberate
    /// — but it must not mean the plan grinds through the entire continuation
    /// budget. A model that ignores the split instruction settles nothing new
    /// each turn, so the stall guard stops it after two, not eight.
    #[test]
    fn a_bundled_step_the_model_never_splits_stops_on_stalls_not_budget() {
        let p = plan(
            "done",
            &[(
                "propose Zerin, Mali and Daxton as Person nodes",
                Some("life.observe"),
                "done",
            )],
        );
        let h = history_args(&[(
            "life.observe",
            serde_json::json!({"text": "Zerin"}),
            "created life:person:zerin",
        )]);

        // Turn 1: evidence exists, but a bundled step is never settled.
        let first = evaluate_plan(&p, None, &h);
        assert_eq!(first.verdict, PlanEvalVerdict::Continue);
        assert_eq!(first.non_atomic_step_ids, vec![1]);
        assert_eq!(first.steps_done, 0);

        // Continuations that re-emit the same bundled step settle nothing.
        let second = evaluate_plan(
            &p,
            Some(PriorPlanState {
                verified_step_ids: &first.verified_step_ids,
                settled_count: first.steps_done,
                stalls: 0,
            }),
            &h,
        );
        assert_eq!(second.verdict, PlanEvalVerdict::Continue);
        let third = evaluate_plan(
            &p,
            Some(PriorPlanState {
                verified_step_ids: &second.verified_step_ids,
                settled_count: second.steps_done,
                stalls: second.stalled_continuations,
            }),
            &h,
        );
        assert_eq!(
            third.verdict,
            PlanEvalVerdict::Blocked,
            "an unsplit bundle must stop on the stall guard"
        );
    }

    /// And when the model *does* split it, the plan settles — the split steps
    /// are individually attributable, and the goal is unchanged so the
    /// carryover keeps its continuity.
    #[test]
    fn splitting_a_bundled_step_lets_the_plan_complete() {
        let bundled = plan(
            "executing",
            &[(
                "propose Zerin, Mali and Daxton as Person nodes",
                Some("life.observe"),
                "done",
            )],
        );
        let first = evaluate_plan(&bundled, None, &[]);
        assert_eq!(first.verdict, PlanEvalVerdict::Continue);

        // The continuation re-emits the plan as one step per artifact.
        let split = plan(
            "executing",
            &[
                (
                    "propose Zerin as a Person node",
                    Some("life.observe"),
                    "done",
                ),
                (
                    "propose Mali as a Person node",
                    Some("life.observe"),
                    "done",
                ),
                (
                    "propose Daxton as a Person node",
                    Some("life.observe"),
                    "done",
                ),
            ],
        );
        let h = history_args(&[
            (
                "life.observe",
                serde_json::json!({"text": "Zerin"}),
                "created life:person:zerin",
            ),
            (
                "life.observe",
                serde_json::json!({"text": "Mali"}),
                "created life:person:mali",
            ),
            (
                "life.observe",
                serde_json::json!({"text": "Daxton"}),
                "created life:person:daxton",
            ),
        ]);
        let out = evaluate_plan(
            &split,
            Some(PriorPlanState {
                verified_step_ids: &first.verified_step_ids,
                settled_count: first.steps_done,
                stalls: first.stalled_continuations,
            }),
            &h,
        );
        assert_eq!(out.verdict, PlanEvalVerdict::Complete);
        assert_eq!(out.steps_verified, 3);
    }

    /// A bundled step is unconditionally outstanding, so a continuation that
    /// does not split it can never settle the plan — it would burn the whole
    /// budget and still finish unfinished.
    #[test]
    fn continuation_brief_demands_a_bundled_step_be_split() {
        let carry = carryover(
            plan(
                "executing",
                &[(
                    "propose Zerin, Mali and Daxton as Person nodes",
                    Some("life.observe"),
                    "done",
                )],
            ),
            vec![false],
            1,
        );
        let brief = plan_continuation_brief(&carry, 3);
        assert!(brief.contains("Split them into one step per outcome"));
    }

    /// The loop's terminal state is a turn event the operator never sees, so
    /// the last continuation has to put the shortfall in the model's own reply.
    #[test]
    fn last_continuation_requires_the_reply_to_name_the_shortfall() {
        let p = plan(
            "executing",
            &[
                ("add Zerin", Some("life.observe"), "pending"),
                ("add Daxton", Some("life.observe"), "pending"),
            ],
        );
        // continuations_used = 2 of a budget of 3: this is the last one.
        let last = carryover(p.clone(), vec![false, false], 2);
        let brief = plan_continuation_brief(&last, 3);
        assert!(brief.contains("LAST continuation"));
        assert!(brief.contains("Do not imply that work will continue on its own."));

        // An earlier continuation must not say so.
        let earlier = carryover(p, vec![false, false], 0);
        assert!(!plan_continuation_brief(&earlier, 3).contains("LAST continuation"));
    }

    #[test]
    fn continuation_brief_after_a_stall_demands_a_different_approach() {
        let mut carry = carryover(
            plan(
                "executing",
                &[("apply fix", Some("life.observe"), "pending")],
            ),
            vec![false],
            1,
        );
        carry.stalled_continuations = 1;
        let brief = plan_continuation_brief(&carry, 3);
        assert!(brief.contains("Do not repeat the approach that just failed"));
    }

    #[test]
    fn stop_notice_reports_done_undone_and_reason() {
        let carry = carryover(
            plan(
                "executing",
                &[
                    ("read config", None, "done"),
                    ("apply fix", None, "pending"),
                ],
            ),
            vec![true, false],
            3,
        );
        let notice = plan_stop_notice(&carry, "continuation budget exhausted");
        assert!(notice.contains("1/2 steps done"));
        assert!(notice.contains("continuation budget exhausted"));
        assert!(notice.contains("- step 2: apply fix"));
        // The notice must tell the truth about what the loop did: the
        // carryover is gone, so it must not advertise resumption paths
        // ("send a message", "/plan drop") that no longer exist.
        assert!(notice.contains("will not run automatically"));
        assert!(!notice.contains("/plan drop"));
    }

    #[test]
    fn step_lists_name_bound_tools_for_projection() {
        // The brief is the continuation turn's only tool-projection relevance
        // text: a bound tool must be named so the explicit tool-name match
        // fires before any keyword gate can strip it (or zero-tool the turn).
        let carry = carryover(
            plan(
                "executing",
                &[
                    ("read config", None, "done"),
                    ("apply fix", Some("bash.exec"), "pending"),
                ],
            ),
            vec![true, false],
            0,
        );
        let brief = plan_continuation_brief(&carry, 3);
        assert!(brief.contains("(tool: bash.exec)"), "{brief}");
        let notice = plan_stop_notice(&carry, "whatever");
        assert!(notice.contains("(tool: bash.exec)"), "{notice}");
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

        // The carryover evaluator agrees. It used to accept the model's own
        // claim here and return `Complete`, which dropped the carryover and
        // ended the loop with Daxton still missing — the in-turn layer refused
        // to *claim* the plan was done while the layer deciding whether to
        // *repeat* took the claim at face value. Both are grounded now.
        let out = evaluate_plan(&p, None, &h);
        assert_eq!(out.verdict, PlanEvalVerdict::Continue);
        assert_eq!(out.outstanding_step_ids, vec![5]);
        assert_eq!(out.contradicted_step_ids, vec![5]);
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
