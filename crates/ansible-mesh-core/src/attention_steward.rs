//! Observe-only Attention Steward policy for cron-backed paracrine signals.
//!
//! This module is deliberately graph-runner agnostic. It decides what an
//! Attention Steward subscriber may do with a signal, but it does not write to
//! Memgraph or notify the operator.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ATTENTION_STEWARD_ROLE_TYPE: &str = "attention-steward";

/// Confirmed SIL entries required before the steward may issue active
/// check-ins (Slice A5 `steward-activation-gate`).
///
/// From `LIFE_GRAPH_OS_PROPOSAL.md` ("5 confirmed SIL entries unlock active
/// interruptions") and the Autopoiesis proposal, Loop 4: "The '5 confirmed SIL
/// entries' gate implemented in code: confirmed entries unlock bounded active
/// check-ins (max/day budget)". Below this threshold every check-in request
/// degrades to the observe-only behavior that exists today.
pub const ACTIVE_CHECKIN_CONFIRMED_SIL_THRESHOLD: u32 = 5;

/// Policy tag that marks a signal as explicitly proposing an active check-in.
pub const ACTIVE_CHECKIN_POLICY_TAG: &str = "active_checkin";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionStewardSignal {
    pub signal_id: String,
    pub signal_type: String,
    pub scope: String,
    pub source_hotel: String,
    pub target_role_type: String,
    #[serde(default)]
    pub subject_refs: Vec<String>,
    pub cadence: String,
    pub priority: String,
    pub observed_at: String,
    #[serde(default)]
    pub expires_at: Option<String>,
    pub payload_summary: String,
    #[serde(default)]
    pub policy_tags: Vec<String>,
    /// Optional urgency marker (`"high"` / `"urgent"`). Either this or the
    /// `active_checkin` policy tag marks the signal as explicitly proposing an
    /// active check-in. Additive (serde default) — old signals parse unchanged.
    #[serde(default)]
    pub urgency: Option<String>,
    /// Confirmed SIL entry count as reported by the signal emitter.
    ///
    /// **v0 sourcing decision (Slice A5):** the activation count travels *in
    /// the signal payload itself* — the steward cron that emits signals is
    /// expected to include it. Counting confirmed SIL nodes via the life-graph
    /// runner is async/remote (a `life.recall`-style round trip), so v0 keeps
    /// activation hotel-local and synchronous. Defaults to 0, which keeps the
    /// gate closed for any emitter that does not know the count.
    #[serde(default)]
    pub confirmed_sil_entries: u32,
    /// Reference to the confirmed SIL entry backing a proposed active
    /// check-in (e.g. `"sil:confirmed:01jz…"`). Additive (serde default).
    #[serde(default)]
    pub sil_ref: Option<String>,
}

/// Activation inputs for the active check-in gate (Slice A5).
///
/// `confirmed_sil_entries` opens the gate at
/// [`ACTIVE_CHECKIN_CONFIRMED_SIL_THRESHOLD`]. `checkins_sent_today` is
/// advisory context for reasons/audit — the *enforced* per-day cap is the
/// `steward.active_checkins` autonomy grant's `max_actions_per_day` budget,
/// consumed hotel-side when the check-in is actually delivered.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivationState {
    #[serde(default)]
    pub confirmed_sil_entries: u32,
    #[serde(default)]
    pub checkins_sent_today: u32,
}

impl ActivationState {
    /// v0 sourcing: read the confirmed count carried in the signal payload.
    pub fn from_signal(signal: &AttentionStewardSignal) -> Self {
        Self {
            confirmed_sil_entries: signal.confirmed_sil_entries,
            checkins_sent_today: 0,
        }
    }

    pub fn gate_open(&self) -> bool {
        self.confirmed_sil_entries >= ACTIVE_CHECKIN_CONFIRMED_SIL_THRESHOLD
    }
}

impl AttentionStewardSignal {
    pub fn from_value(value: Value) -> Result<Self> {
        serde_json::from_value(value).context("parse AttentionStewardSignal")
    }

    fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        self.expires_at
            .as_deref()
            .and_then(|expires_at| DateTime::parse_from_rfc3339(expires_at).ok())
            .map(|expires_at| expires_at.with_timezone(&Utc) <= now)
            .unwrap_or(false)
    }

    fn summary_has_shame_language(&self) -> bool {
        let summary = self.payload_summary.to_ascii_lowercase();
        ["failed", "falling behind", "broken streak"]
            .iter()
            .any(|phrase| summary.contains(phrase))
    }

    /// Does this signal explicitly propose an active check-in?
    /// Either the `active_checkin` policy tag or an urgency marker qualifies.
    pub fn proposes_active_checkin(&self) -> bool {
        self.policy_tags
            .iter()
            .any(|tag| tag == ACTIVE_CHECKIN_POLICY_TAG)
            || self
                .urgency
                .as_deref()
                .map(|u| {
                    let u = u.trim().to_ascii_lowercase();
                    u == "high" || u == "urgent"
                })
                .unwrap_or(false)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttentionStewardResponse {
    RecordObservation,
    ProposeSilEntry,
    UpdateSilMetadata,
    DeferSignal,
    /// Earned active check-in (Slice A5). Produced only when the signal
    /// explicitly proposes a check-in AND the activation gate is open
    /// (`confirmed_sil_entries >= ACTIVE_CHECKIN_CONFIRMED_SIL_THRESHOLD`).
    ///
    /// The policy only *authorizes* the check-in content; delivery is decided
    /// downstream by the `steward.active_checkins` autonomy lane (posture +
    /// daily budget), enforced hotel-side.
    ActiveCheckIn {
        /// Operator-facing check-in text (the emitter's `payload_summary`).
        message: String,
        /// Confirmed SIL entry this check-in acts on, when known.
        sil_ref: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProposedStewardshipInstruction {
    pub situation: String,
    pub trigger: String,
    pub recommended_action: String,
    pub tone: String,
    pub evidence_refs: Vec<String>,
    pub owner: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AttentionStewardDecision {
    pub response: AttentionStewardResponse,
    pub signal_id: String,
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_sil_entry: Option<ProposedStewardshipInstruction>,
}

pub struct AttentionStewardPolicy {
    pub owner: String,
}

impl Default for AttentionStewardPolicy {
    fn default() -> Self {
        Self {
            owner: "agent:beacon".into(),
        }
    }
}

impl AttentionStewardPolicy {
    pub fn evaluate_now(&self, signal: &AttentionStewardSignal) -> AttentionStewardDecision {
        self.evaluate_at(signal, Utc::now())
    }

    /// Evaluate with the activation gate closed (no [`ActivationState`]) —
    /// active check-in requests degrade to today's observe-only behavior.
    pub fn evaluate_at(
        &self,
        signal: &AttentionStewardSignal,
        now: DateTime<Utc>,
    ) -> AttentionStewardDecision {
        self.evaluate_at_with_activation(signal, &ActivationState::default(), now)
    }

    pub fn evaluate_now_with_activation(
        &self,
        signal: &AttentionStewardSignal,
        activation: &ActivationState,
    ) -> AttentionStewardDecision {
        self.evaluate_at_with_activation(signal, activation, Utc::now())
    }

    pub fn evaluate_at_with_activation(
        &self,
        signal: &AttentionStewardSignal,
        activation: &ActivationState,
        now: DateTime<Utc>,
    ) -> AttentionStewardDecision {
        if signal.target_role_type != ATTENTION_STEWARD_ROLE_TYPE {
            return AttentionStewardDecision::defer(
                signal,
                "signal target_role_type is not attention-steward",
            );
        }

        if signal.is_expired_at(now) {
            return AttentionStewardDecision::defer(signal, "signal expired before evaluation");
        }

        if signal.summary_has_shame_language()
            || signal
                .policy_tags
                .iter()
                .any(|tag| tag == "shame_language" || tag == "operator_review_required")
        {
            return AttentionStewardDecision::defer(
                signal,
                "anti-policy held signal for Beacon review",
            );
        }

        // Active check-in gate (Slice A5). Runs AFTER the expiry and shame /
        // operator-review guards above, so those anti-policies stay in force
        // for ActiveCheckIn exactly as for every other outcome. When the gate
        // is closed the request falls through and degrades to today's
        // observe-only behavior (ProposeSilEntry / RecordObservation).
        let mut checkin_gate_closed_reason = None;
        if signal.proposes_active_checkin() {
            if activation.gate_open() {
                return AttentionStewardDecision {
                    response: AttentionStewardResponse::ActiveCheckIn {
                        message: signal.payload_summary.clone(),
                        sil_ref: signal.sil_ref.clone(),
                    },
                    signal_id: signal.signal_id.clone(),
                    reason: format!(
                        "active check-in authorized: {}/{} confirmed SIL entries \
                         ({} check-ins already sent today); delivery gated by the \
                         steward.active_checkins autonomy lane",
                        activation.confirmed_sil_entries,
                        ACTIVE_CHECKIN_CONFIRMED_SIL_THRESHOLD,
                        activation.checkins_sent_today,
                    ),
                    proposed_sil_entry: None,
                };
            }
            checkin_gate_closed_reason = Some(format!(
                "active check-in gate closed: {}/{} confirmed SIL entries; \
                 degraded to observe-only",
                activation.confirmed_sil_entries, ACTIVE_CHECKIN_CONFIRMED_SIL_THRESHOLD,
            ));
        }

        if signal
            .policy_tags
            .iter()
            .any(|tag| tag == "propose_sil" || tag == "new_pattern")
        {
            return AttentionStewardDecision {
                response: AttentionStewardResponse::ProposeSilEntry,
                signal_id: signal.signal_id.clone(),
                reason: "new pattern signal may become a proposed SIL entry".into(),
                proposed_sil_entry: Some(ProposedStewardshipInstruction {
                    situation: signal.payload_summary.clone(),
                    trigger: signal.signal_type.clone(),
                    recommended_action: "defer".into(),
                    tone: "quiet".into(),
                    evidence_refs: vec![signal.signal_id.clone()],
                    owner: self.owner.clone(),
                    status: "proposed".into(),
                }),
            };
        }

        AttentionStewardDecision {
            response: AttentionStewardResponse::RecordObservation,
            signal_id: signal.signal_id.clone(),
            reason: checkin_gate_closed_reason
                .unwrap_or_else(|| "valid observe-only attention signal".into()),
            proposed_sil_entry: None,
        }
    }
}

impl AttentionStewardDecision {
    fn defer(signal: &AttentionStewardSignal, reason: impl Into<String>) -> Self {
        Self {
            response: AttentionStewardResponse::DeferSignal,
            signal_id: signal.signal_id.clone(),
            reason: reason.into(),
            proposed_sil_entry: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn signal() -> AttentionStewardSignal {
        AttentionStewardSignal::from_value(json!({
            "signal_id": "cron:job-1:1000",
            "signal_type": "open_loop_staleness",
            "scope": "personal",
            "source_hotel": "vps-jane-aiua-01",
            "target_role_type": "attention-steward",
            "subject_refs": ["lifegraph:open_loop"],
            "cadence": "daily",
            "priority": "medium",
            "observed_at": "2026-06-04T20:00:00Z",
            "expires_at": null,
            "payload_summary": "Scan stale open loops and defer unless policy says to record.",
            "policy_tags": ["observe_only", "adhd-support"]
        }))
        .unwrap()
    }

    #[test]
    fn valid_signal_records_observation() {
        let policy = AttentionStewardPolicy::default();
        let decision = policy.evaluate_at(&signal(), "2026-06-04T20:01:00Z".parse().unwrap());

        assert_eq!(
            decision.response,
            AttentionStewardResponse::RecordObservation
        );
        assert_eq!(decision.signal_id, "cron:job-1:1000");
        assert!(decision.proposed_sil_entry.is_none());
    }

    #[test]
    fn new_pattern_signal_proposes_sil_entry_in_observe_only_mode() {
        let mut signal = signal();
        signal.policy_tags.push("new_pattern".into());

        let decision = AttentionStewardPolicy::default()
            .evaluate_at(&signal, "2026-06-04T20:01:00Z".parse().unwrap());

        assert_eq!(decision.response, AttentionStewardResponse::ProposeSilEntry);
        let proposed = decision.proposed_sil_entry.unwrap();
        assert_eq!(proposed.status, "proposed");
        assert_eq!(proposed.recommended_action, "defer");
        assert_eq!(proposed.owner, "agent:beacon");
    }

    #[test]
    fn non_attention_target_is_deferred() {
        let mut signal = signal();
        signal.target_role_type = "coach".into();

        let decision = AttentionStewardPolicy::default()
            .evaluate_at(&signal, "2026-06-04T20:01:00Z".parse().unwrap());

        assert_eq!(decision.response, AttentionStewardResponse::DeferSignal);
        assert!(decision.reason.contains("target_role_type"));
    }

    #[test]
    fn expired_signal_is_deferred() {
        let mut signal = signal();
        signal.expires_at = Some("2026-06-04T19:00:00Z".into());

        let decision = AttentionStewardPolicy::default()
            .evaluate_at(&signal, "2026-06-04T20:01:00Z".parse().unwrap());

        assert_eq!(decision.response, AttentionStewardResponse::DeferSignal);
        assert!(decision.reason.contains("expired"));
    }

    #[test]
    fn shame_language_is_deferred_for_beacon_review() {
        let mut signal = signal();
        signal.payload_summary = "You failed to keep the habit streak.".into();

        let decision = AttentionStewardPolicy::default()
            .evaluate_at(&signal, "2026-06-04T20:01:00Z".parse().unwrap());

        assert_eq!(decision.response, AttentionStewardResponse::DeferSignal);
        assert!(decision.reason.contains("anti-policy"));
    }

    // ── Slice A5: active check-in gate ────────────────────────────────────────

    fn checkin_signal() -> AttentionStewardSignal {
        let mut s = signal();
        s.policy_tags.push(ACTIVE_CHECKIN_POLICY_TAG.into());
        s.payload_summary = "Gentle nudge: the dentist form is still open.".into();
        s.sil_ref = Some("sil:confirmed:01jz-example".into());
        s
    }

    const NOW: &str = "2026-06-04T20:01:00Z";

    fn eval(signal: &AttentionStewardSignal, confirmed: u32) -> AttentionStewardDecision {
        AttentionStewardPolicy::default().evaluate_at_with_activation(
            signal,
            &ActivationState {
                confirmed_sil_entries: confirmed,
                checkins_sent_today: 0,
            },
            NOW.parse().unwrap(),
        )
    }

    #[test]
    fn gate_closed_below_threshold_degrades_to_record_observation() {
        for confirmed in [0, 1, 4] {
            let decision = eval(&checkin_signal(), confirmed);
            assert_eq!(
                decision.response,
                AttentionStewardResponse::RecordObservation,
                "confirmed={confirmed} must degrade"
            );
            assert!(
                decision.reason.contains("gate closed"),
                "reason must explain the closed gate: {}",
                decision.reason
            );
        }
    }

    #[test]
    fn gate_open_at_threshold_produces_active_checkin() {
        for confirmed in [5, 6, 100] {
            let decision = eval(&checkin_signal(), confirmed);
            match decision.response {
                AttentionStewardResponse::ActiveCheckIn { message, sil_ref } => {
                    assert_eq!(message, "Gentle nudge: the dentist form is still open.");
                    assert_eq!(sil_ref.as_deref(), Some("sil:confirmed:01jz-example"));
                }
                other => panic!("confirmed={confirmed}: expected ActiveCheckIn, got {other:?}"),
            }
            assert!(decision.reason.contains("steward.active_checkins"));
        }
    }

    #[test]
    fn urgency_marker_also_proposes_active_checkin() {
        let mut signal = signal();
        signal.urgency = Some("urgent".into());
        assert!(signal.proposes_active_checkin());
        let decision = eval(&signal, 5);
        assert!(matches!(
            decision.response,
            AttentionStewardResponse::ActiveCheckIn { .. }
        ));

        // Non-urgent markers do not.
        signal.urgency = Some("low".into());
        assert!(!signal.proposes_active_checkin());
        let decision = eval(&signal, 5);
        assert_eq!(
            decision.response,
            AttentionStewardResponse::RecordObservation
        );
    }

    #[test]
    fn legacy_evaluate_at_never_produces_active_checkin() {
        // Callers on the pre-A5 entry point get a permanently closed gate.
        let decision =
            AttentionStewardPolicy::default().evaluate_at(&checkin_signal(), NOW.parse().unwrap());
        assert_eq!(
            decision.response,
            AttentionStewardResponse::RecordObservation
        );
    }

    #[test]
    fn shame_guard_still_applies_to_active_checkin() {
        let mut signal = checkin_signal();
        signal.payload_summary = "You failed to keep the habit streak.".into();
        let decision = eval(&signal, 10);
        assert_eq!(decision.response, AttentionStewardResponse::DeferSignal);
        assert!(decision.reason.contains("anti-policy"));

        // The operator_review_required tag also holds check-ins.
        let mut signal = checkin_signal();
        signal.policy_tags.push("operator_review_required".into());
        let decision = eval(&signal, 10);
        assert_eq!(decision.response, AttentionStewardResponse::DeferSignal);
    }

    #[test]
    fn expiry_guard_still_applies_to_active_checkin() {
        let mut signal = checkin_signal();
        signal.expires_at = Some("2026-06-04T19:00:00Z".into());
        let decision = eval(&signal, 10);
        assert_eq!(decision.response, AttentionStewardResponse::DeferSignal);
        assert!(decision.reason.contains("expired"));
    }

    #[test]
    fn signal_contract_serde_back_compat() {
        // The pre-A5 wire shape (no urgency / confirmed_sil_entries / sil_ref)
        // must still parse, with closed-gate defaults.
        let legacy = signal();
        assert_eq!(legacy.confirmed_sil_entries, 0);
        assert_eq!(legacy.urgency, None);
        assert_eq!(legacy.sil_ref, None);
        assert!(!legacy.proposes_active_checkin());

        // Round trip with the new fields set.
        let extended = checkin_signal();
        let json = serde_json::to_value(&extended).unwrap();
        let back = AttentionStewardSignal::from_value(json).unwrap();
        assert_eq!(back, extended);

        // ActivationState sourced from the signal payload (v0 decision).
        let mut with_count = checkin_signal();
        with_count.confirmed_sil_entries = 7;
        let activation = ActivationState::from_signal(&with_count);
        assert_eq!(activation.confirmed_sil_entries, 7);
        assert!(activation.gate_open());
        assert!(!ActivationState::from_signal(&legacy).gate_open());
    }

    #[test]
    fn active_checkin_decision_serde_round_trip() {
        let decision = eval(&checkin_signal(), 5);
        let json = serde_json::to_value(&decision).unwrap();
        let back: AttentionStewardDecision = serde_json::from_value(json).unwrap();
        assert_eq!(back, decision);
    }
}
