//! The Relocation Ceremony record (R6): a recorded, resumable multi-phase
//! move of a role incarnation — and optionally its paired transport — from
//! an origin hotel to a target hotel, initiated by `hotel.relocate`.
//!
//! See `docs/architecture/RELOCATION_CEREMONY_PROPOSAL.md` for the ceremony
//! phases, invariants, and risk tiers this record implements.

use serde::{Deserialize, Serialize};

/// The ceremony's phase. `Intent` through `Close` is the proposal's happy
/// path; `RolledBack` and `Failed` are additional terminal states required
/// by invariant 7 ("a crashed ceremony resumes from its last recorded phase
/// or rolls back to the origin; it never leaves two acting holders or
/// zero").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RelocationCeremonyPhase {
    #[default]
    Intent,
    Feasibility,
    Standby,
    Continuity,
    Switch,
    Reconcile,
    Close,
    /// Declined or interrupted before SWITCH — origin was never touched, so
    /// rollback is free (no undo needed).
    RolledBack,
    /// Interrupted at or after SWITCH — origin state may be inconsistent.
    /// Never auto-resumed; always surfaced via `needs_operator_review`.
    Failed,
}

impl RelocationCeremonyPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Feasibility => "feasibility",
            Self::Standby => "standby",
            Self::Continuity => "continuity",
            Self::Switch => "switch",
            Self::Reconcile => "reconcile",
            Self::Close => "close",
            Self::RolledBack => "rolled_back",
            Self::Failed => "failed",
        }
    }

    /// True for every phase before SWITCH, the first phase that mutates
    /// origin-side `home_node`/transport-home truth. A ceremony interrupted
    /// while still pre-commitment can always roll back for free.
    pub fn is_pre_commitment(&self) -> bool {
        matches!(
            self,
            Self::Intent | Self::Feasibility | Self::Standby | Self::Continuity
        )
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Close | Self::RolledBack | Self::Failed)
    }
}

/// Risk tier of the component class being relocated, per the proposal's
/// "Authority and risk tiers" table. Determines the approval gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RelocationRiskTier {
    /// Role incarnation alone, no external custody. Gate: operational admin
    /// authority (`is_admin || role_name == "orchestrator"`), same as
    /// `role.set_home`/`transport.set_home`/`hotel.materialize_request`.
    #[default]
    Low,
    /// Role incarnation plus its paired transport (external identity
    /// custody, e.g. a Telegram bot token). Gate: full admin authority
    /// (`is_admin` alone) as a v1 stand-in for the proposal's "operator
    /// approval through the existing approval UX." Wiring the full
    /// DEF-103-style unconditional-gate + hazard-scan machinery for this
    /// tier is a deliberate follow-on, not built here — see the R6 scope
    /// note in RELOCATION_CEREMONY_PROPOSAL.md.
    High,
}

impl RelocationRiskTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::High => "high",
        }
    }
}

/// One phase transition, kept for audit and for boot-time resume to explain
/// why an interrupted ceremony ended up where it did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelocationPhaseEvent {
    pub phase: RelocationCeremonyPhase,
    pub at_unix: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// A recorded, resumable relocation ceremony. Node kind:
/// `relocation_ceremony`. Node key: `relocation_ceremony:{ceremony_id}`.
///
/// **Never deleted** — a rolled-back or failed ceremony is its own audit
/// trail (mirrors `ProcedurePatchRecord`'s "never deleted" convention).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelocationCeremonyRecord {
    pub ceremony_id: String,
    pub agent_id: String,
    pub role_name: String,
    pub origin_hotel: String,
    pub target_hotel: String,
    /// If true, the paired transport (see `transport`/`transport_resource_ref`)
    /// moves atomically with the role. Bumps `risk_tier` to `High`.
    #[serde(default)]
    pub include_transport: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_resource_ref: Option<String>,
    pub risk_tier: RelocationRiskTier,
    pub phase: RelocationCeremonyPhase,
    #[serde(default)]
    pub requested_by_role: String,
    #[serde(default)]
    pub reason: String,
    /// The R3/R4 `MaterializeRequest` id this ceremony's STANDBY phase
    /// dispatched, so `hotel.relocate_status` can join against
    /// `materialize_ready:{request_id}` while waiting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materialize_request_id: Option<String>,
    /// The R5 `ContinuityImport` id this ceremony's CONTINUITY phase sent,
    /// joined against `continuity_ack:{request_id}` while waiting. `None`
    /// when the target predates continuity and the move ran degraded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuity_request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decline_reason: Option<String>,
    /// Set when a boot-time scan finds this ceremony interrupted at or
    /// after SWITCH (origin state may be inconsistent). Never cleared
    /// automatically — only an operator/follow-up ceremony clears it.
    #[serde(default)]
    pub needs_operator_review: bool,
    #[serde(default)]
    pub phase_history: Vec<RelocationPhaseEvent>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl RelocationCeremonyRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        ceremony_id: String,
        agent_id: String,
        role_name: String,
        origin_hotel: String,
        target_hotel: String,
        include_transport: bool,
        transport: Option<String>,
        transport_resource_ref: Option<String>,
        requested_by_role: String,
        reason: String,
        now_unix: u64,
    ) -> Self {
        let risk_tier = if include_transport {
            RelocationRiskTier::High
        } else {
            RelocationRiskTier::Low
        };
        let phase = RelocationCeremonyPhase::Intent;
        Self {
            ceremony_id,
            agent_id,
            role_name,
            origin_hotel,
            target_hotel,
            include_transport,
            transport,
            transport_resource_ref,
            risk_tier,
            phase,
            requested_by_role,
            reason,
            materialize_request_id: None,
            continuity_request_id: None,
            decline_reason: None,
            needs_operator_review: false,
            phase_history: vec![RelocationPhaseEvent {
                phase,
                at_unix: now_unix,
                note: String::new(),
            }],
            created_at: now_unix,
            updated_at: now_unix,
        }
    }

    /// Advance to `phase`, recording the transition. Callers own the actual
    /// side effects (STANDBY's mesh call, SWITCH's `home_node` write, ...);
    /// this only updates the ceremony's own bookkeeping.
    pub fn advance(
        &mut self,
        phase: RelocationCeremonyPhase,
        note: impl Into<String>,
        now_unix: u64,
    ) {
        self.phase = phase;
        self.updated_at = now_unix;
        self.phase_history.push(RelocationPhaseEvent {
            phase,
            at_unix: now_unix,
            note: note.into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_ceremony_starts_at_intent_with_low_tier_when_transport_excluded() {
        let rec = RelocationCeremonyRecord::new(
            "cer-1".into(),
            "agent-1".into(),
            "orchestrator".into(),
            "mac-jane-aiua-01".into(),
            "vps-jane-aiua-01".into(),
            false,
            None,
            None,
            "orchestrator".into(),
            "test move".into(),
            1000,
        );
        assert_eq!(rec.phase, RelocationCeremonyPhase::Intent);
        assert_eq!(rec.risk_tier, RelocationRiskTier::Low);
        assert_eq!(rec.phase_history.len(), 1);
    }

    #[test]
    fn including_transport_bumps_risk_tier_to_high() {
        let rec = RelocationCeremonyRecord::new(
            "cer-2".into(),
            "agent-1".into(),
            "orchestrator".into(),
            "mac-jane-aiua-01".into(),
            "vps-jane-aiua-01".into(),
            true,
            Some("telegram".into()),
            Some("bjork-bot".into()),
            "orchestrator".into(),
            "test move with transport".into(),
            1000,
        );
        assert_eq!(rec.risk_tier, RelocationRiskTier::High);
    }

    #[test]
    fn advance_appends_history_and_bumps_updated_at() {
        let mut rec = RelocationCeremonyRecord::new(
            "cer-3".into(),
            "agent-1".into(),
            "orchestrator".into(),
            "mac-jane-aiua-01".into(),
            "vps-jane-aiua-01".into(),
            false,
            None,
            None,
            "orchestrator".into(),
            "test move".into(),
            1000,
        );
        rec.advance(RelocationCeremonyPhase::Feasibility, "offer accepted", 1010);
        assert_eq!(rec.phase, RelocationCeremonyPhase::Feasibility);
        assert_eq!(rec.updated_at, 1010);
        assert_eq!(rec.phase_history.len(), 2);
        assert_eq!(rec.phase_history[1].note, "offer accepted");
    }

    #[test]
    fn pre_commitment_and_terminal_classification() {
        assert!(RelocationCeremonyPhase::Intent.is_pre_commitment());
        assert!(RelocationCeremonyPhase::Continuity.is_pre_commitment());
        assert!(!RelocationCeremonyPhase::Switch.is_pre_commitment());
        assert!(!RelocationCeremonyPhase::Reconcile.is_pre_commitment());
        assert!(RelocationCeremonyPhase::Close.is_terminal());
        assert!(RelocationCeremonyPhase::RolledBack.is_terminal());
        assert!(RelocationCeremonyPhase::Failed.is_terminal());
        assert!(!RelocationCeremonyPhase::Standby.is_terminal());
    }
}
