//! Autonomy grants — per-lane earned-autonomy postures, budgets, and audit records.
//!
//! Implements Slice A1 (`autonomy-grant-core`) of the Autopoiesis epic
//! (`docs/architecture/AUTOPOIESIS_PROPOSAL.md`). This module is the pure
//! library layer: posture state machine, budget accounting, and kill-switch
//! parsing are all pure functions with injected clocks/env readers so they can
//! be tested without wall-clock time or process environment. Persistence lives
//! in `GraphDomain` (node kinds `autonomy_grant` / `autonomy_audit`).
//!
//! Standing rules (Autonomy Contract):
//! 1. Every autonomous action is auditable, reversible, and budgeted.
//! 2. Postures only promote on operator-confirmed outcomes; any operator
//!    reversal demotes exactly one level. Grants are always **created** at
//!    [`AutonomyPosture::ProposalOnly`] — the safest level.
//! 3. Per-lane env kill switches (`PHILOTIC_AUTONOMY_DISABLE_<LANE>`) override
//!    everything, always.

use serde::{Deserialize, Serialize};

// ── Lane ──────────────────────────────────────────────────────────────────────

/// Known lane: bridge `RELATES_TO` edges from retrieval feedback (Loop 1).
pub const LANE_GRAPH_BRIDGE_EDGES: &str = "graph.bridge_edges";
/// Known lane: recurring heal-queue patterns file intel-graph proposals (Loop 2).
pub const LANE_FLEET_HEAL_SLICES: &str = "fleet.heal_slices";
/// Known lane: observation sweeps author scored proposals (Loop 3).
pub const LANE_WORK_FILE_PROPOSALS: &str = "work.file_proposals";
/// Known lane: attention steward bounded active check-ins (Loop 4).
pub const LANE_STEWARD_ACTIVE_CHECKINS: &str = "steward.active_checkins";
/// Known lane: scheduled sessions execute SVE slices end-to-end (Loop 5).
pub const LANE_WORK_EXECUTE_SLICES: &str = "work.execute_slices";
/// Known lane: nightly Muninn contradiction/staleness sweep files aggregated
/// annotation-only findings (Memory Transparency Slice M4).
pub const LANE_MEMORY_HYGIENE: &str = "memory.hygiene";

/// All lanes named by the Autopoiesis proposal. Lanes are open vocabulary —
/// this list exists for enumeration (dashboards, kill-switch sweeps), not as a
/// closed set.
pub const KNOWN_LANES: [&str; 6] = [
    LANE_GRAPH_BRIDGE_EDGES,
    LANE_FLEET_HEAL_SLICES,
    LANE_WORK_FILE_PROPOSALS,
    LANE_STEWARD_ACTIVE_CHECKINS,
    LANE_WORK_EXECUTE_SLICES,
    LANE_MEMORY_HYGIENE,
];

/// Prefix for the per-lane env kill switch.
pub const AUTONOMY_KILL_SWITCH_ENV_PREFIX: &str = "PHILOTIC_AUTONOMY_DISABLE_";

/// A self-building lane identifier (e.g. `"graph.bridge_edges"`).
///
/// String newtype so lanes serialize as plain strings in grant/audit records.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AutonomyLane(String);

impl AutonomyLane {
    pub fn new(lane: impl Into<String>) -> Self {
        Self(lane.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Env var name for this lane's kill switch:
    /// `PHILOTIC_AUTONOMY_DISABLE_` + lane uppercased with dots mapped to
    /// underscores (e.g. `graph.bridge_edges` →
    /// `PHILOTIC_AUTONOMY_DISABLE_GRAPH_BRIDGE_EDGES`).
    pub fn kill_switch_env_var(&self) -> String {
        format!(
            "{}{}",
            AUTONOMY_KILL_SWITCH_ENV_PREFIX,
            self.0.to_ascii_uppercase().replace('.', "_")
        )
    }
}

impl std::fmt::Display for AutonomyLane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for AutonomyLane {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

/// Kill switch: is autonomous action on `lane` enabled?
///
/// Pure function — `env` injects the environment reader (pass
/// `|k| std::env::var(k).ok()` in production). The lane is **disabled** when
/// the kill-switch var is set to anything other than `""`, `"0"`, or `"false"`
/// (case-insensitive). Unset means enabled.
pub fn lane_enabled(lane: &AutonomyLane, env: impl Fn(&str) -> Option<String>) -> bool {
    match env(&lane.kill_switch_env_var()) {
        None => true,
        Some(value) => {
            let v = value.trim().to_ascii_lowercase();
            v.is_empty() || v == "0" || v == "false"
        }
    }
}

// ── Posture ───────────────────────────────────────────────────────────────────

/// Autonomy posture for a lane, ordered from least to most autonomous.
///
/// `Ord` follows autonomy level: `ProposalOnly < ConfirmFirst < AutoWithAudit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyPosture {
    /// The lane may only file proposals; a human (or higher process) executes.
    ProposalOnly,
    /// The lane may act, but each action requires operator confirmation first.
    ConfirmFirst,
    /// The lane acts autonomously; every action writes an audit record.
    AutoWithAudit,
}

impl AutonomyPosture {
    /// One level more autonomous (saturates at `AutoWithAudit`).
    pub fn promoted(self) -> Self {
        match self {
            Self::ProposalOnly => Self::ConfirmFirst,
            Self::ConfirmFirst | Self::AutoWithAudit => Self::AutoWithAudit,
        }
    }

    /// One level less autonomous (saturates at `ProposalOnly`).
    pub fn demoted(self) -> Self {
        match self {
            Self::AutoWithAudit => Self::ConfirmFirst,
            Self::ConfirmFirst | Self::ProposalOnly => Self::ProposalOnly,
        }
    }
}

// ── Grant record ──────────────────────────────────────────────────────────────

fn default_required_for_promotion() -> u32 {
    5
}

/// Hard ceilings for autonomous action on a lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutonomyBudget {
    /// Maximum autonomous actions per UTC day.
    pub max_actions_per_day: u32,
    /// Consecutive failures that freeze the lane pending operator review.
    pub max_consecutive_failures: u32,
}

impl Default for AutonomyBudget {
    fn default() -> Self {
        Self {
            max_actions_per_day: 10,
            max_consecutive_failures: 3,
        }
    }
}

/// Earned-trust counters for a lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutonomyEarned {
    /// Operator-confirmed good outcomes at the **current** posture.
    /// Reset on promotion and on any operator reversal.
    pub confirmed_good_outcomes: u32,
    /// Confirmed-good outcomes required to promote one posture level.
    #[serde(default = "default_required_for_promotion")]
    pub required_for_promotion: u32,
    /// Current consecutive-failure streak (reset by any confirmed-good outcome
    /// or by operator review clearing a freeze).
    pub consecutive_failures: u32,
}

impl Default for AutonomyEarned {
    fn default() -> Self {
        Self {
            confirmed_good_outcomes: 0,
            required_for_promotion: default_required_for_promotion(),
            consecutive_failures: 0,
        }
    }
}

/// Per-lane autonomy grant — the operator approves postures, not patches.
///
/// Persisted via `GraphDomain` as node kind `autonomy_grant`
/// (key `autonomy_grant:{lane}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutonomyGrant {
    pub lane: AutonomyLane,
    pub posture: AutonomyPosture,
    pub budget: AutonomyBudget,
    pub earned: AutonomyEarned,
    /// When true, the lane's consecutive-failure ceiling was hit: posture is
    /// retained but autonomous action is blocked until an explicit operator
    /// review clears the flag (`GraphDomain::clear_autonomy_freeze`).
    #[serde(default)]
    pub frozen_until_operator_review: bool,
    /// Autonomous actions consumed on `action_day_utc`.
    #[serde(default)]
    pub actions_today: u32,
    /// UTC day (days since the Unix epoch) that `actions_today` counts against.
    #[serde(default)]
    pub action_day_utc: u64,
    /// Unix timestamp (seconds) of the last mutation.
    pub updated_at: u64,
}

impl AutonomyGrant {
    /// Create a grant for `lane` with default budget/earned counters.
    ///
    /// Every grant starts at [`AutonomyPosture::ProposalOnly`] — never higher.
    /// Autonomy Contract rule 2 forbids any lane starting above ConfirmFirst;
    /// creation uses the safest level and trust is earned from there.
    pub fn new(lane: AutonomyLane, now: u64) -> Self {
        Self {
            lane,
            posture: AutonomyPosture::ProposalOnly,
            budget: AutonomyBudget::default(),
            earned: AutonomyEarned::default(),
            frozen_until_operator_review: false,
            actions_today: 0,
            action_day_utc: utc_day(now),
            updated_at: now,
        }
    }
}

/// UTC day number (days since the Unix epoch) for a Unix timestamp in seconds.
pub fn utc_day(now: u64) -> u64 {
    now / 86_400
}

// ── Posture state machine ─────────────────────────────────────────────────────

/// Outcome of an autonomous (or proposed) action, as judged by the operator
/// or by downstream verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Operator confirmed the action was good.
    ConfirmedGood,
    /// Operator reversed/undid the action.
    OperatorReversal,
    /// The action failed (verification, execution, or downstream error).
    Failure,
}

/// Result of applying an [`Outcome`] to a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// Counters may have moved but posture and freeze state did not.
    NoChange,
    /// Earned promotion: `required_for_promotion` confirmed-good outcomes at
    /// the previous posture.
    Promoted {
        from: AutonomyPosture,
        to: AutonomyPosture,
    },
    /// Operator reversal: demoted exactly one level, earned counter reset.
    Demoted {
        from: AutonomyPosture,
        to: AutonomyPosture,
    },
    /// Consecutive-failure ceiling hit: lane frozen pending operator review.
    /// Callers must write an audit record for this transition.
    Frozen,
}

/// Has `grant` already met the bar [`record_outcome`] uses to promote one
/// posture level?
///
/// Pure — mirrors exactly the condition `record_outcome` checks before
/// promoting, so callers (e.g. the A9 trust-ledger status report) never
/// invent a second copy of the promotion rule that can drift from the one
/// that actually fires. True when: not frozen, not already at the ceiling
/// (`AutoWithAudit`), and the confirmed-good streak has already reached
/// `required_for_promotion`. Note this is normally `false` in steady state:
/// `record_outcome` promotes and resets the streak the instant it reaches
/// the threshold, so under normal operation the streak never sits at or
/// above `required_for_promotion` between outcomes. It only happens in
/// practice when a lane was frozen while confirmations kept accumulating
/// (freeze suppresses promotion outright, so the streak can pile up past
/// the threshold); clearing the freeze then reports this lane as eligible.
pub fn promotion_eligible(grant: &AutonomyGrant) -> bool {
    !grant.frozen_until_operator_review
        && grant.posture != AutonomyPosture::AutoWithAudit
        && grant.earned.confirmed_good_outcomes >= grant.earned.required_for_promotion
}

/// Apply `outcome` to `grant`. Pure — the clock is injected via `now`.
///
/// Rules (Autopoiesis proposal, Autonomy Contract):
/// - `ConfirmedGood` clears the failure streak and counts toward promotion.
///   Reaching `required_for_promotion` confirmed-good outcomes at the current
///   posture promotes one level (`ProposalOnly → ConfirmFirst →
///   AutoWithAudit`) and resets the earned counter. No promotion happens while
///   the lane is frozen or once at `AutoWithAudit`.
/// - `OperatorReversal` demotes exactly one level and resets the earned
///   counter. At `ProposalOnly` the posture cannot drop further, but the
///   counter still resets.
/// - `Failure` increments the consecutive-failure streak; reaching
///   `budget.max_consecutive_failures` freezes the lane
///   (`frozen_until_operator_review = true`, posture retained). The caller
///   must record an audit entry when `Transition::Frozen` is returned.
pub fn record_outcome(grant: &mut AutonomyGrant, outcome: Outcome, now: u64) -> Transition {
    grant.updated_at = now;
    match outcome {
        Outcome::ConfirmedGood => {
            grant.earned.consecutive_failures = 0;
            grant.earned.confirmed_good_outcomes =
                grant.earned.confirmed_good_outcomes.saturating_add(1);
            if promotion_eligible(grant) {
                let from = grant.posture;
                grant.posture = from.promoted();
                grant.earned.confirmed_good_outcomes = 0;
                Transition::Promoted {
                    from,
                    to: grant.posture,
                }
            } else {
                Transition::NoChange
            }
        }
        Outcome::OperatorReversal => {
            grant.earned.confirmed_good_outcomes = 0;
            let from = grant.posture;
            grant.posture = from.demoted();
            if grant.posture == from {
                Transition::NoChange
            } else {
                Transition::Demoted {
                    from,
                    to: grant.posture,
                }
            }
        }
        Outcome::Failure => {
            grant.earned.consecutive_failures = grant.earned.consecutive_failures.saturating_add(1);
            let ceiling_hit =
                grant.earned.consecutive_failures >= grant.budget.max_consecutive_failures;
            if ceiling_hit && !grant.frozen_until_operator_review {
                grant.frozen_until_operator_review = true;
                Transition::Frozen
            } else {
                Transition::NoChange
            }
        }
    }
}

/// Consume one unit of the grant's daily action budget.
///
/// Pure — the clock is injected via `now`. Rolls the per-day counter over on
/// UTC-date change. Returns `true` (and increments the counter) when the
/// action is allowed; returns `false` when the lane is frozen pending
/// operator review or the daily budget is exhausted.
pub fn try_consume_daily_action(grant: &mut AutonomyGrant, now: u64) -> bool {
    if grant.frozen_until_operator_review {
        return false;
    }
    let today = utc_day(now);
    if grant.action_day_utc != today {
        grant.action_day_utc = today;
        grant.actions_today = 0;
    }
    if grant.actions_today >= grant.budget.max_actions_per_day {
        return false;
    }
    grant.actions_today += 1;
    grant.updated_at = now;
    true
}

// ── Audit record ──────────────────────────────────────────────────────────────

/// Maximum stored length of an audit record's evidence field, in bytes.
pub const MAX_AUDIT_EVIDENCE_BYTES: usize = 4096;

/// Truncate evidence to [`MAX_AUDIT_EVIDENCE_BYTES`] on a char boundary,
/// appending a truncation marker when clipped.
pub fn bound_evidence(evidence: &str) -> String {
    if evidence.len() <= MAX_AUDIT_EVIDENCE_BYTES {
        return evidence.to_string();
    }
    let mut cut = MAX_AUDIT_EVIDENCE_BYTES;
    while !evidence.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}… [truncated]", &evidence[..cut])
}

/// Review state of an audited autonomous action (Autopoiesis Slice A9 —
/// `trust-ledger`).
///
/// Stamped by [`crate::domain::GraphDomain::set_autonomy_audit_outcome`] via
/// the `record_autonomy_outcome` IPC path. `ConfirmedGood` and `Reversed`
/// come from an explicit operator report and drive the paired grant-level
/// [`Outcome`] (`ConfirmedGood` feeds the earn counter, `Reversed` feeds the
/// demote transition). `Neutral` is a review with no promotion/demotion
/// effect — a wash, not a signal either way — and never mutates the grant.
///
/// TODO(A9): a configurable timeout-to-`Neutral` (an audit left `Pending`
/// past some age auto-resolves to `Neutral` so it stops silently occupying
/// "awaiting review" state forever) is not wired up. Today `Neutral` is only
/// reachable through an explicit stamp; the timeout sweep needs a cron-like
/// caller and is deferred to whichever slice adds scheduled hotel-side
/// sweeps generally, rather than inventing a one-off timer for this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    /// Action taken; awaiting operator confirmation or reversal.
    Pending,
    /// Operator confirmed the action was good. Feeds `Outcome::ConfirmedGood`.
    /// `#[serde(alias)]` accepts the pre-A9 wire value `"confirmed"` so
    /// audit records written before this rename still deserialize.
    #[serde(alias = "confirmed")]
    ConfirmedGood,
    /// Operator reversed the action (see `reversal_hint`). Feeds
    /// `Outcome::OperatorReversal`.
    Reversed,
    /// Reviewed with no promotion/demotion effect — a wash. Never mutates
    /// the grant's earn/demote counters.
    Neutral,
}

/// Decision record for one autonomous action on a lane.
///
/// Persisted via `GraphDomain` as node kind `autonomy_audit`
/// (key `autonomy_audit:{audit_id}`, append-only by distinct `audit_id`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutonomyAuditRecord {
    /// Unique id for this audit entry (node key suffix).
    pub audit_id: String,
    pub lane: AutonomyLane,
    /// What the lane did (one line).
    pub action_summary: String,
    /// Why — bounded evidence blob (see [`bound_evidence`]).
    pub evidence: String,
    /// How to undo the action (soft-retire / revert path).
    pub reversal_hint: String,
    /// Posture the lane held when the action was taken.
    pub posture_at_action: AutonomyPosture,
    pub outcome: AuditOutcome,
    /// Unix timestamp (seconds) when the action was taken.
    pub created_at: u64,
    /// Unix timestamp (seconds) of the last outcome update.
    pub updated_at: u64,
}

impl AutonomyAuditRecord {
    /// Build a `Pending` audit record, bounding `evidence` to
    /// [`MAX_AUDIT_EVIDENCE_BYTES`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        audit_id: impl Into<String>,
        lane: AutonomyLane,
        action_summary: impl Into<String>,
        evidence: &str,
        reversal_hint: impl Into<String>,
        posture_at_action: AutonomyPosture,
        now: u64,
    ) -> Self {
        Self {
            audit_id: audit_id.into(),
            lane,
            action_summary: action_summary.into(),
            evidence: bound_evidence(evidence),
            reversal_hint: reversal_hint.into(),
            posture_at_action,
            outcome: AuditOutcome::Pending,
            created_at: now,
            updated_at: now,
        }
    }
}

// ── Trust-ledger status report ──────────────────────────────────────────────

/// A computed, per-lane snapshot of the trust ledger (Autopoiesis Slice A9 —
/// `trust-ledger`): "arithmetic instead of vibes."
///
/// Every field is read straight off [`AutonomyGrant`] and the promotion rule
/// already enforced by [`record_outcome`] — this type invents no new
/// thresholds. It exists so operator surfaces (`phil autonomy status`) can
/// report the same numbers the state machine itself acts on, instead of a
/// human eyeballing "has it been about two weeks."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneStatusReport {
    pub lane: AutonomyLane,
    pub posture: AutonomyPosture,
    pub frozen_until_operator_review: bool,
    /// Autonomous actions consumed so far today (UTC), reset to 0 in this
    /// report if `now` has rolled past the grant's stored `action_day_utc` —
    /// mirrors the rollover [`try_consume_daily_action`] applies on the next
    /// real consult, so the report never shows a stale yesterday's count.
    pub actions_today: u32,
    pub max_actions_per_day: u32,
    pub consecutive_failures: u32,
    pub max_consecutive_failures: u32,
    /// Confirmed-good outcomes accrued at the *current* posture. A1 has no
    /// time-window ("N over ≥ 14 days") concept — this is the raw earn
    /// counter, exactly what [`record_outcome`] compares against
    /// `required_for_promotion`.
    pub confirmed_good_streak: u32,
    pub required_for_promotion: u32,
    /// See [`promotion_eligible`] — true iff the confirmed-good streak has
    /// already met the promotion threshold (normally `false`: reaching the
    /// threshold promotes and resets it immediately; `true` mainly signals a
    /// frozen lane whose streak piled up past the bar while suppressed).
    pub promotion_eligible: bool,
}

/// Compute [`LaneStatusReport`] for `grant` as of `now`. Pure — the clock is
/// injected so this is testable without wall-clock time.
pub fn lane_status_report(grant: &AutonomyGrant, now: u64) -> LaneStatusReport {
    let actions_today = if grant.action_day_utc == utc_day(now) {
        grant.actions_today
    } else {
        0
    };
    LaneStatusReport {
        lane: grant.lane.clone(),
        posture: grant.posture,
        frozen_until_operator_review: grant.frozen_until_operator_review,
        actions_today,
        max_actions_per_day: grant.budget.max_actions_per_day,
        consecutive_failures: grant.earned.consecutive_failures,
        max_consecutive_failures: grant.budget.max_consecutive_failures,
        confirmed_good_streak: grant.earned.confirmed_good_outcomes,
        required_for_promotion: grant.earned.required_for_promotion,
        promotion_eligible: promotion_eligible(grant),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const T0: u64 = 1_750_000_000; // fake clock origin

    fn grant() -> AutonomyGrant {
        AutonomyGrant::new(AutonomyLane::new(LANE_GRAPH_BRIDGE_EDGES), T0)
    }

    #[test]
    fn new_grant_starts_at_proposal_only_with_defaults() {
        let g = grant();
        assert_eq!(g.posture, AutonomyPosture::ProposalOnly);
        assert_eq!(g.earned.required_for_promotion, 5);
        assert_eq!(g.earned.confirmed_good_outcomes, 0);
        assert_eq!(g.earned.consecutive_failures, 0);
        assert!(!g.frozen_until_operator_review);
        assert_eq!(g.actions_today, 0);
        assert_eq!(g.action_day_utc, utc_day(T0));
        assert_eq!(g.updated_at, T0);
    }

    #[test]
    fn promotion_after_exactly_n_confirmed_outcomes() {
        let mut g = grant();
        // Outcomes 1..N-1: counters move, posture holds.
        for i in 1..5 {
            let t = record_outcome(&mut g, Outcome::ConfirmedGood, T0 + i);
            assert_eq!(t, Transition::NoChange, "outcome {} must not promote", i);
            assert_eq!(g.posture, AutonomyPosture::ProposalOnly);
            assert_eq!(g.earned.confirmed_good_outcomes, i as u32);
        }
        // Outcome N: promotes exactly one level and resets the counter.
        let t = record_outcome(&mut g, Outcome::ConfirmedGood, T0 + 5);
        assert_eq!(
            t,
            Transition::Promoted {
                from: AutonomyPosture::ProposalOnly,
                to: AutonomyPosture::ConfirmFirst,
            }
        );
        assert_eq!(g.posture, AutonomyPosture::ConfirmFirst);
        assert_eq!(g.earned.confirmed_good_outcomes, 0);
        assert_eq!(g.updated_at, T0 + 5);

        // Five more at ConfirmFirst reach the ceiling.
        for i in 6..=10 {
            record_outcome(&mut g, Outcome::ConfirmedGood, T0 + i);
        }
        assert_eq!(g.posture, AutonomyPosture::AutoWithAudit);
        assert_eq!(g.earned.confirmed_good_outcomes, 0);

        // At AutoWithAudit, confirmed outcomes accumulate but never promote.
        for i in 11..=20 {
            let t = record_outcome(&mut g, Outcome::ConfirmedGood, T0 + i);
            assert_eq!(t, Transition::NoChange);
        }
        assert_eq!(g.posture, AutonomyPosture::AutoWithAudit);
    }

    #[test]
    fn confirmed_good_clears_failure_streak() {
        let mut g = grant();
        record_outcome(&mut g, Outcome::Failure, T0 + 1);
        record_outcome(&mut g, Outcome::Failure, T0 + 2);
        assert_eq!(g.earned.consecutive_failures, 2);
        record_outcome(&mut g, Outcome::ConfirmedGood, T0 + 3);
        assert_eq!(g.earned.consecutive_failures, 0);
    }

    #[test]
    fn reversal_demotes_one_level_and_resets_counter() {
        let mut g = grant();
        g.posture = AutonomyPosture::AutoWithAudit;
        g.earned.confirmed_good_outcomes = 3;

        let t = record_outcome(&mut g, Outcome::OperatorReversal, T0 + 1);
        assert_eq!(
            t,
            Transition::Demoted {
                from: AutonomyPosture::AutoWithAudit,
                to: AutonomyPosture::ConfirmFirst,
            }
        );
        assert_eq!(g.posture, AutonomyPosture::ConfirmFirst);
        assert_eq!(g.earned.confirmed_good_outcomes, 0);

        // Reversal demotes exactly one level — not to the floor.
        g.earned.confirmed_good_outcomes = 4;
        let t = record_outcome(&mut g, Outcome::OperatorReversal, T0 + 2);
        assert_eq!(
            t,
            Transition::Demoted {
                from: AutonomyPosture::ConfirmFirst,
                to: AutonomyPosture::ProposalOnly,
            }
        );
        assert_eq!(g.earned.confirmed_good_outcomes, 0);

        // At the floor: posture holds, counter still resets.
        g.earned.confirmed_good_outcomes = 2;
        let t = record_outcome(&mut g, Outcome::OperatorReversal, T0 + 3);
        assert_eq!(t, Transition::NoChange);
        assert_eq!(g.posture, AutonomyPosture::ProposalOnly);
        assert_eq!(g.earned.confirmed_good_outcomes, 0);
    }

    #[test]
    fn freeze_on_consecutive_failures_retains_posture_and_blocks_actions() {
        let mut g = grant();
        g.posture = AutonomyPosture::AutoWithAudit;
        assert_eq!(g.budget.max_consecutive_failures, 3);

        assert_eq!(
            record_outcome(&mut g, Outcome::Failure, T0 + 1),
            Transition::NoChange
        );
        assert_eq!(
            record_outcome(&mut g, Outcome::Failure, T0 + 2),
            Transition::NoChange
        );
        assert_eq!(
            record_outcome(&mut g, Outcome::Failure, T0 + 3),
            Transition::Frozen
        );
        assert!(g.frozen_until_operator_review);
        // Posture stays — freeze blocks action without demoting.
        assert_eq!(g.posture, AutonomyPosture::AutoWithAudit);
        // Frozen lane cannot consume budget.
        assert!(!try_consume_daily_action(&mut g, T0 + 4));
        // Further failures do not re-fire the Frozen transition.
        assert_eq!(
            record_outcome(&mut g, Outcome::Failure, T0 + 5),
            Transition::NoChange
        );
        // No promotion while frozen, even with enough confirmed outcomes.
        for i in 6..=20 {
            let t = record_outcome(&mut g, Outcome::ConfirmedGood, T0 + i);
            assert_eq!(t, Transition::NoChange);
        }
        assert_eq!(g.posture, AutonomyPosture::AutoWithAudit);
    }

    #[test]
    fn daily_budget_enforced_and_rolls_over_on_utc_date() {
        let mut g = grant();
        g.budget.max_actions_per_day = 2;

        assert!(try_consume_daily_action(&mut g, T0));
        assert!(try_consume_daily_action(&mut g, T0 + 60));
        // Budget exhausted for the day.
        assert!(!try_consume_daily_action(&mut g, T0 + 120));
        assert_eq!(g.actions_today, 2);

        // Same UTC day much later — still exhausted.
        let same_day_late = (utc_day(T0) * 86_400) + 86_399;
        assert!(!try_consume_daily_action(&mut g, same_day_late));

        // Next UTC day — counter rolls over.
        let next_day = (utc_day(T0) + 1) * 86_400;
        assert!(try_consume_daily_action(&mut g, next_day));
        assert_eq!(g.action_day_utc, utc_day(T0) + 1);
        assert_eq!(g.actions_today, 1);
    }

    #[test]
    fn kill_switch_env_var_names_for_every_known_lane() {
        let expected = [
            (
                LANE_GRAPH_BRIDGE_EDGES,
                "PHILOTIC_AUTONOMY_DISABLE_GRAPH_BRIDGE_EDGES",
            ),
            (
                LANE_FLEET_HEAL_SLICES,
                "PHILOTIC_AUTONOMY_DISABLE_FLEET_HEAL_SLICES",
            ),
            (
                LANE_WORK_FILE_PROPOSALS,
                "PHILOTIC_AUTONOMY_DISABLE_WORK_FILE_PROPOSALS",
            ),
            (
                LANE_STEWARD_ACTIVE_CHECKINS,
                "PHILOTIC_AUTONOMY_DISABLE_STEWARD_ACTIVE_CHECKINS",
            ),
            (
                LANE_WORK_EXECUTE_SLICES,
                "PHILOTIC_AUTONOMY_DISABLE_WORK_EXECUTE_SLICES",
            ),
            (
                LANE_MEMORY_HYGIENE,
                "PHILOTIC_AUTONOMY_DISABLE_MEMORY_HYGIENE",
            ),
        ];
        assert_eq!(expected.len(), KNOWN_LANES.len());
        for (lane, var) in expected {
            assert!(KNOWN_LANES.contains(&lane));
            assert_eq!(AutonomyLane::new(lane).kill_switch_env_var(), var);
        }
    }

    #[test]
    fn kill_switch_parsing_for_every_known_lane() {
        for lane_name in KNOWN_LANES {
            let lane = AutonomyLane::new(lane_name);
            let var = lane.kill_switch_env_var();

            // Unset → enabled.
            assert!(lane_enabled(&lane, |_| None), "{lane_name}: unset");
            // Explicit off-values → enabled.
            for off in ["", "0", "false", "FALSE", " 0 "] {
                let v = var.clone();
                assert!(
                    lane_enabled(&lane, move |k| (k == v).then(|| off.to_string())),
                    "{lane_name}: value {off:?} must not disable"
                );
            }
            // Any other value → disabled.
            for on in ["1", "true", "TRUE", "yes", "anything"] {
                let v = var.clone();
                assert!(
                    !lane_enabled(&lane, move |k| (k == v).then(|| on.to_string())),
                    "{lane_name}: value {on:?} must disable"
                );
            }
            // A different lane's kill switch must not affect this lane.
            assert!(lane_enabled(&lane, |k| {
                (k == "PHILOTIC_AUTONOMY_DISABLE_SOME_OTHER_LANE").then(|| "1".to_string())
            }));
        }
    }

    #[test]
    fn evidence_is_bounded() {
        let short = "small evidence";
        assert_eq!(bound_evidence(short), short);

        let long = "x".repeat(MAX_AUDIT_EVIDENCE_BYTES + 500);
        let bounded = bound_evidence(&long);
        assert!(bounded.len() < long.len());
        assert!(bounded.ends_with("[truncated]"));

        // Multi-byte chars: never panic, never split a char.
        let unicode = "é".repeat(MAX_AUDIT_EVIDENCE_BYTES);
        let bounded = bound_evidence(&unicode);
        assert!(bounded.ends_with("[truncated]"));

        let record = AutonomyAuditRecord::new(
            "audit-1",
            AutonomyLane::new(LANE_GRAPH_BRIDGE_EDGES),
            "bridged edge",
            &long,
            "detach the edge",
            AutonomyPosture::AutoWithAudit,
            T0,
        );
        assert!(record.evidence.len() <= MAX_AUDIT_EVIDENCE_BYTES + "… [truncated]".len());
        assert_eq!(record.outcome, AuditOutcome::Pending);
    }

    #[test]
    fn grant_serde_round_trip_and_snake_case_postures() {
        let mut g = grant();
        g.posture = AutonomyPosture::ConfirmFirst;
        let json = serde_json::to_value(&g).expect("serialize");
        assert_eq!(json["lane"], LANE_GRAPH_BRIDGE_EDGES);
        assert_eq!(json["posture"], "confirm_first");
        let back: AutonomyGrant = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, g);

        // Older records without the newer fields still deserialize.
        let legacy = serde_json::json!({
            "lane": "fleet.heal_slices",
            "posture": "proposal_only",
            "budget": {"max_actions_per_day": 5, "max_consecutive_failures": 2},
            "earned": {"confirmed_good_outcomes": 1, "consecutive_failures": 0},
            "updated_at": 123,
        });
        let g: AutonomyGrant = serde_json::from_value(legacy).expect("legacy deserialize");
        assert_eq!(g.earned.required_for_promotion, 5);
        assert!(!g.frozen_until_operator_review);
        assert_eq!(g.actions_today, 0);
    }

    #[test]
    fn audit_outcome_confirmed_good_accepts_legacy_confirmed_alias() {
        // Pre-A9 audit records serialized `AuditOutcome::Confirmed` as
        // `"confirmed"`. The rename to `ConfirmedGood` must not strand
        // already-stored records — snake_case now emits "confirmed_good"
        // but "confirmed" still deserializes to the same variant.
        let current: AuditOutcome = serde_json::from_value(serde_json::json!("confirmed_good"))
            .expect("current wire value");
        let legacy: AuditOutcome =
            serde_json::from_value(serde_json::json!("confirmed")).expect("legacy wire value");
        assert_eq!(current, AuditOutcome::ConfirmedGood);
        assert_eq!(legacy, AuditOutcome::ConfirmedGood);
        assert_eq!(
            serde_json::to_value(AuditOutcome::ConfirmedGood).unwrap(),
            serde_json::json!("confirmed_good")
        );
        assert_eq!(
            serde_json::to_value(AuditOutcome::Neutral).unwrap(),
            serde_json::json!("neutral")
        );
    }

    // ── promotion_eligible / lane_status_report (Slice A9) ──────────────────

    #[test]
    fn promotion_eligible_matches_record_outcome_promotion_condition() {
        let mut g = grant();
        assert!(!promotion_eligible(&g), "fresh grant has no streak yet");

        // One short of the threshold: not yet eligible.
        g.earned.confirmed_good_outcomes = g.earned.required_for_promotion - 1;
        assert!(!promotion_eligible(&g));

        // At the threshold: eligible.
        g.earned.confirmed_good_outcomes = g.earned.required_for_promotion;
        assert!(promotion_eligible(&g));

        // Frozen suppresses eligibility even with plenty of streak.
        g.frozen_until_operator_review = true;
        assert!(!promotion_eligible(&g));
        g.frozen_until_operator_review = false;

        // Already at the ceiling: never "eligible" (nowhere higher to go).
        g.posture = AutonomyPosture::AutoWithAudit;
        assert!(!promotion_eligible(&g));
    }

    #[test]
    fn promotion_eligible_reflects_freeze_then_clear_like_record_outcome() {
        // Exercises the real scenario the doc comment describes: a lane
        // frozen mid-streak accumulates confirmed-good outcomes past the
        // threshold without record_outcome ever promoting it (matches
        // `freeze_on_consecutive_failures_retains_posture_and_blocks_actions`).
        // Once an operator clears the freeze, the report must say eligible.
        let mut g = grant();
        g.posture = AutonomyPosture::ConfirmFirst;
        g.frozen_until_operator_review = true;
        for i in 1..=10 {
            let t = record_outcome(&mut g, Outcome::ConfirmedGood, T0 + i);
            assert_eq!(t, Transition::NoChange, "frozen lane must not promote");
        }
        assert!(g.earned.confirmed_good_outcomes >= g.earned.required_for_promotion);
        assert!(!promotion_eligible(&g), "still frozen");

        g.frozen_until_operator_review = false;
        assert!(promotion_eligible(&g), "freeze cleared, streak already met");
    }

    #[test]
    fn lane_status_report_maps_grant_fields_directly() {
        let mut g = grant();
        g.posture = AutonomyPosture::ConfirmFirst;
        g.earned.consecutive_failures = 2;
        g.earned.confirmed_good_outcomes = 3;
        g.actions_today = 4;
        g.action_day_utc = utc_day(T0);

        let report = lane_status_report(&g, T0);
        assert_eq!(report.lane, g.lane);
        assert_eq!(report.posture, AutonomyPosture::ConfirmFirst);
        assert!(!report.frozen_until_operator_review);
        assert_eq!(report.actions_today, 4);
        assert_eq!(report.max_actions_per_day, g.budget.max_actions_per_day);
        assert_eq!(report.consecutive_failures, 2);
        assert_eq!(
            report.max_consecutive_failures,
            g.budget.max_consecutive_failures
        );
        assert_eq!(report.confirmed_good_streak, 3);
        assert_eq!(
            report.required_for_promotion,
            g.earned.required_for_promotion
        );
        assert!(!report.promotion_eligible, "3 < 5 required");
    }

    #[test]
    fn lane_status_report_zeroes_actions_today_on_utc_day_rollover() {
        let mut g = grant();
        g.actions_today = 9;
        g.action_day_utc = utc_day(T0);

        // Same day: stale count reported as-is.
        let report = lane_status_report(&g, T0 + 10);
        assert_eq!(report.actions_today, 9);

        // A day later: the report reflects the rollover try_consume_daily_action
        // would apply, even though the grant itself hasn't been touched.
        let next_day = (utc_day(T0) + 1) * 86_400;
        let report = lane_status_report(&g, next_day);
        assert_eq!(report.actions_today, 0);
        // The stored grant is untouched — this is a read, not a mutation.
        assert_eq!(g.actions_today, 9);
    }
}
