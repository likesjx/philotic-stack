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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttentionStewardResponse {
    RecordObservation,
    ProposeSilEntry,
    UpdateSilMetadata,
    DeferSignal,
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

    pub fn evaluate_at(
        &self,
        signal: &AttentionStewardSignal,
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
            reason: "valid observe-only attention signal".into(),
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
}
