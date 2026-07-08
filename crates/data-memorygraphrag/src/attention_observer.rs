// Pure mapping from AttentionStewardDecision → LifeObserveInput.
// No I/O — fully deterministic and unit-testable.

use ansible_mesh_core::attention_steward::{
    AttentionStewardDecision, AttentionStewardResponse, AttentionStewardSignal,
    ProposedStewardshipInstruction,
};
use ulid::Ulid;

use crate::{
    AdjudicationStatus, EvidencePacket, GraphRecordRef, LifeObserveInput, ReliabilityBasis,
    SourceKind, SourceRef, SourceReliability, ValidationState,
};

/// Map an evaluated `AttentionStewardDecision` into a `LifeObserveInput` ready for
/// fire-and-forget dispatch to the life-graph-runner.
///
/// Returns `None` for `DeferSignal` and `UpdateSilMetadata` — no node is written.
pub fn decision_to_observe_input(
    decision: &AttentionStewardDecision,
    signal: &AttentionStewardSignal,
    now_iso: &str,
) -> Option<LifeObserveInput> {
    match &decision.response {
        AttentionStewardResponse::RecordObservation => {
            Some(record_observation_input(signal, now_iso))
        }
        AttentionStewardResponse::ProposeSilEntry => decision
            .proposed_sil_entry
            .as_ref()
            .map(|sil| propose_sil_input(sil, signal, now_iso)),
        // Slice A5: an authorized check-in that the steward.active_checkins
        // lane did NOT clear for push delivery (ConfirmFirst / ProposalOnly
        // posture, exhausted budget, kill switch, or no delivery route) is
        // written as a proposed Signal tagged `awaiting_operator_posture`
        // instead of interrupting the operator.
        AttentionStewardResponse::ActiveCheckIn { message, sil_ref } => Some(
            active_checkin_awaiting_posture_input(message, sil_ref.as_deref(), signal, now_iso),
        ),
        AttentionStewardResponse::DeferSignal | AttentionStewardResponse::UpdateSilMetadata => None,
    }
}

fn record_observation_input(signal: &AttentionStewardSignal, _now_iso: &str) -> LifeObserveInput {
    let node_id = format!("signal:paracrine:{}", signal.signal_id);
    let ulid = Ulid::new().to_string().to_lowercase();
    let observation_id = format!("obs:attn:{ulid}");
    let packet_id = format!("pkt:attn:{ulid}");

    LifeObserveInput {
        observation_id,
        evidence: EvidencePacket {
            packet_id,
            claim_ref: GraphRecordRef {
                id: node_id,
                label: "Signal".to_string(),
                datasource: Some("life-graph".to_string()),
            },
            claim_summary: signal.payload_summary.clone(),
            source_refs: vec![SourceRef {
                source_id: format!("hotel:{}", signal.source_hotel),
                source_kind: SourceKind::AgentInference,
                reliability: SourceReliability {
                    score: 0.75,
                    basis: ReliabilityBasis::AgentInferred,
                },
                uri: None,
                captured_at: Some(signal.observed_at.clone()),
            }],
            passage_refs: vec![],
            confidence: 0.75,
            validation_state: ValidationState::Proposed,
            observed_at: Some(signal.observed_at.clone()),
            valid_time_range: None,
            source_reliability: 0.75,
            conflict_ids: vec![],
            adjudication_status: AdjudicationStatus::NotNeeded,
            metadata: serde_json::json!({
                "signal_type": signal.signal_type,
                "scope": signal.scope,
                "cadence": signal.cadence,
                "priority": signal.priority,
                "policy_tags": signal.policy_tags,
                "subject_refs": signal.subject_refs,
            }),
        },
        proposed_graph_refs: vec![],
        observed_by: None,
        observed_role: None,
        edges: vec![],
    }
}

fn propose_sil_input(
    sil: &ProposedStewardshipInstruction,
    signal: &AttentionStewardSignal,
    now_iso: &str,
) -> LifeObserveInput {
    // Stable node ID from signal_id so repeated evaluation of the same signal is idempotent.
    let node_id = format!("sil:proposed:{}", signal.signal_id);
    let ulid = Ulid::new().to_string().to_lowercase();
    let observation_id = format!("obs:sil:{ulid}");
    let packet_id = format!("pkt:sil:{ulid}");

    LifeObserveInput {
        observation_id,
        evidence: EvidencePacket {
            packet_id,
            claim_ref: GraphRecordRef {
                id: node_id,
                label: "StewardshipInstruction".to_string(),
                datasource: Some("life-graph".to_string()),
            },
            claim_summary: sil.situation.clone(),
            source_refs: vec![SourceRef {
                source_id: format!("hotel:{}", signal.source_hotel),
                source_kind: SourceKind::AgentInference,
                reliability: SourceReliability {
                    score: 0.65,
                    basis: ReliabilityBasis::AgentInferred,
                },
                uri: None,
                captured_at: Some(signal.observed_at.clone()),
            }],
            passage_refs: vec![],
            confidence: 0.65,
            validation_state: ValidationState::Proposed,
            observed_at: Some(now_iso.to_string()),
            valid_time_range: None,
            source_reliability: 0.65,
            conflict_ids: vec![],
            adjudication_status: AdjudicationStatus::NotNeeded,
            metadata: serde_json::json!({
                "trigger": sil.trigger,
                "recommended_action": sil.recommended_action,
                "tone": sil.tone,
                "owner": sil.owner,
                "status": sil.status,
                "evidence_refs": sil.evidence_refs,
                "signal_id": signal.signal_id,
            }),
        },
        proposed_graph_refs: vec![],
        observed_by: None,
        observed_role: None,
        edges: vec![],
    }
}

/// Slice A5 degraded path: a gate-open check-in the autonomy lane refused to
/// push is proposed into the LifeGraph as a Signal node awaiting operator
/// posture review, so nothing is lost and nothing interrupts.
fn active_checkin_awaiting_posture_input(
    message: &str,
    sil_ref: Option<&str>,
    signal: &AttentionStewardSignal,
    now_iso: &str,
) -> LifeObserveInput {
    // Stable node ID from signal_id so re-evaluation of the same signal is idempotent.
    let node_id = format!("checkin:proposed:{}", signal.signal_id);
    let ulid = Ulid::new().to_string().to_lowercase();
    let observation_id = format!("obs:checkin:{ulid}");
    let packet_id = format!("pkt:checkin:{ulid}");

    let mut policy_tags = signal.policy_tags.clone();
    if !policy_tags.iter().any(|t| t == "awaiting_operator_posture") {
        policy_tags.push("awaiting_operator_posture".to_string());
    }

    LifeObserveInput {
        observation_id,
        evidence: EvidencePacket {
            packet_id,
            claim_ref: GraphRecordRef {
                id: node_id,
                label: "Signal".to_string(),
                datasource: Some("life-graph".to_string()),
            },
            claim_summary: message.to_string(),
            source_refs: vec![SourceRef {
                source_id: format!("hotel:{}", signal.source_hotel),
                source_kind: SourceKind::AgentInference,
                reliability: SourceReliability {
                    score: 0.75,
                    basis: ReliabilityBasis::AgentInferred,
                },
                uri: None,
                captured_at: Some(signal.observed_at.clone()),
            }],
            passage_refs: vec![],
            confidence: 0.75,
            validation_state: ValidationState::Proposed,
            observed_at: Some(now_iso.to_string()),
            valid_time_range: None,
            source_reliability: 0.75,
            conflict_ids: vec![],
            adjudication_status: AdjudicationStatus::NotNeeded,
            metadata: serde_json::json!({
                "signal_type": signal.signal_type,
                "scope": signal.scope,
                "cadence": signal.cadence,
                "priority": signal.priority,
                "policy_tags": policy_tags,
                "subject_refs": signal.subject_refs,
                "signal_id": signal.signal_id,
                "checkin_kind": "active_checkin",
                "sil_ref": sil_ref,
                "confirmed_sil_entries": signal.confirmed_sil_entries,
            }),
        },
        proposed_graph_refs: vec![],
        observed_by: None,
        observed_role: None,
        edges: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ansible_mesh_core::attention_steward::{
        AttentionStewardPolicy, AttentionStewardResponse, AttentionStewardSignal,
    };
    use serde_json::json;

    fn valid_signal() -> AttentionStewardSignal {
        AttentionStewardSignal::from_value(json!({
            "signal_id": "cron:job-42:1717531200",
            "signal_type": "open_loop_staleness",
            "scope": "personal",
            "source_hotel": "vps-jane-aiua-01",
            "target_role_type": "attention-steward",
            "subject_refs": ["lifegraph:open_loop:rowing"],
            "cadence": "daily",
            "priority": "medium",
            "observed_at": "2026-06-04T20:00:00Z",
            "expires_at": null,
            "payload_summary": "Rowing follow-up open loop has not been updated in 7 days.",
            "policy_tags": ["observe_only", "adhd-support"]
        }))
        .unwrap()
    }

    fn new_pattern_signal() -> AttentionStewardSignal {
        let mut s = valid_signal();
        s.policy_tags.push("new_pattern".into());
        s
    }

    #[test]
    fn record_observation_produces_signal_node() {
        let signal = valid_signal();
        let policy = AttentionStewardPolicy::default();
        let decision = policy.evaluate_at(&signal, "2026-06-04T20:01:00Z".parse().unwrap());

        assert_eq!(
            decision.response,
            AttentionStewardResponse::RecordObservation
        );

        let input = decision_to_observe_input(&decision, &signal, "2026-06-04T20:01:00Z")
            .expect("RecordObservation should produce input");

        assert_eq!(input.evidence.claim_ref.label, "Signal");
        assert_eq!(
            input.evidence.claim_ref.id,
            "signal:paracrine:cron:job-42:1717531200"
        );
        assert_eq!(
            input.evidence.claim_summary,
            "Rowing follow-up open loop has not been updated in 7 days."
        );
        assert_eq!(input.evidence.validation_state, ValidationState::Proposed);
        assert!((input.evidence.confidence - 0.75).abs() < 1e-5);
        assert_eq!(
            input.evidence.observed_at.as_deref(),
            Some("2026-06-04T20:00:00Z")
        );
        assert!(!input.observation_id.is_empty());
        assert!(!input.evidence.packet_id.is_empty());
    }

    #[test]
    fn propose_sil_entry_produces_stewardship_instruction_node() {
        let signal = new_pattern_signal();
        let policy = AttentionStewardPolicy::default();
        let decision = policy.evaluate_at(&signal, "2026-06-04T20:01:00Z".parse().unwrap());

        assert_eq!(decision.response, AttentionStewardResponse::ProposeSilEntry);

        let input = decision_to_observe_input(&decision, &signal, "2026-06-04T20:01:00Z")
            .expect("ProposeSilEntry should produce input");

        assert_eq!(input.evidence.claim_ref.label, "StewardshipInstruction");
        assert_eq!(
            input.evidence.claim_ref.id,
            "sil:proposed:cron:job-42:1717531200"
        );
        assert_eq!(input.evidence.validation_state, ValidationState::Proposed);
        assert!((input.evidence.confidence - 0.65).abs() < 1e-5);
        assert_eq!(
            input.evidence.observed_at.as_deref(),
            Some("2026-06-04T20:01:00Z")
        );
        assert_eq!(
            input.evidence.metadata["recommended_action"].as_str(),
            Some("defer")
        );
        assert_eq!(input.evidence.metadata["tone"].as_str(), Some("quiet"));
    }

    #[test]
    fn defer_signal_produces_no_input() {
        let mut signal = valid_signal();
        signal.target_role_type = "coach".into();
        let policy = AttentionStewardPolicy::default();
        let decision = policy.evaluate_at(&signal, "2026-06-04T20:01:00Z".parse().unwrap());

        assert_eq!(decision.response, AttentionStewardResponse::DeferSignal);
        assert!(decision_to_observe_input(&decision, &signal, "2026-06-04T20:01:00Z").is_none());
    }

    #[test]
    fn record_observation_passes_cypher_compile() {
        use crate::cypher;
        let signal = valid_signal();
        let policy = AttentionStewardPolicy::default();
        let decision = policy.evaluate_at(&signal, "2026-06-04T20:01:00Z".parse().unwrap());
        let input = decision_to_observe_input(&decision, &signal, "2026-06-04T20:01:00Z").unwrap();
        cypher::compile_observe(&input, "2026-06-04T20:01:00Z")
            .expect("Signal observe input should compile to valid Cypher");
    }

    #[test]
    fn active_checkin_awaiting_posture_produces_tagged_signal_node() {
        use ansible_mesh_core::attention_steward::{ACTIVE_CHECKIN_POLICY_TAG, ActivationState};

        let mut signal = valid_signal();
        signal.policy_tags.push(ACTIVE_CHECKIN_POLICY_TAG.into());
        signal.confirmed_sil_entries = 6;
        signal.sil_ref = Some("sil:confirmed:01jz-example".into());

        let decision = AttentionStewardPolicy::default().evaluate_at_with_activation(
            &signal,
            &ActivationState::from_signal(&signal),
            "2026-06-04T20:01:00Z".parse().unwrap(),
        );
        assert!(matches!(
            decision.response,
            AttentionStewardResponse::ActiveCheckIn { .. }
        ));

        let input = decision_to_observe_input(&decision, &signal, "2026-06-04T20:01:00Z")
            .expect("degraded ActiveCheckIn should produce input");

        assert_eq!(input.evidence.claim_ref.label, "Signal");
        assert_eq!(
            input.evidence.claim_ref.id,
            "checkin:proposed:cron:job-42:1717531200"
        );
        assert_eq!(input.evidence.validation_state, ValidationState::Proposed);
        let tags = input.evidence.metadata["policy_tags"]
            .as_array()
            .expect("policy_tags array");
        assert!(tags.iter().any(|t| t == "awaiting_operator_posture"));
        assert_eq!(
            input.evidence.metadata["sil_ref"].as_str(),
            Some("sil:confirmed:01jz-example")
        );
        assert_eq!(
            input.evidence.metadata["checkin_kind"].as_str(),
            Some("active_checkin")
        );

        // And it must compile to valid Cypher like the other inputs.
        use crate::cypher;
        cypher::compile_observe(&input, "2026-06-04T20:01:00Z")
            .expect("awaiting-posture check-in input should compile to valid Cypher");
    }

    #[test]
    fn propose_sil_entry_passes_cypher_compile() {
        use crate::cypher;
        let signal = new_pattern_signal();
        let policy = AttentionStewardPolicy::default();
        let decision = policy.evaluate_at(&signal, "2026-06-04T20:01:00Z".parse().unwrap());
        let input = decision_to_observe_input(&decision, &signal, "2026-06-04T20:01:00Z").unwrap();
        cypher::compile_observe(&input, "2026-06-04T20:01:00Z")
            .expect("StewardshipInstruction observe input should compile to valid Cypher");
    }
}
