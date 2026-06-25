// Pure Cypher compilation for Life Graph OS observe operations.
// No I/O — all functions are deterministic and fully testable.

use crate::{
    ConflictHandoff, ConflictHandoffStatus, LifeCommitInput, LifeObserveInput,
    LifePatchProposalInput, LifeResolveInput, PatchKind, RetrievalFeedbackInput,
    RetrievalFeedbackRating, SourceKind, ValidationState,
};

const KNOWN_LABELS: &[&str] = &[
    "Person",
    "Role",
    "Goal",
    "System",
    "Habit",
    "Project",
    "Commitment",
    "OpenLoop",
    "NextAction",
    "Routine",
    "Decision",
    "Preference",
    "Value",
    "Concern",
    "Event",
    "Signal",
    "GrowthHypothesis",
    "GrowthExperiment",
    "DriftFinding",
    "CapabilityPatch",
    "SkillPatch",
    "ToolPatch",
    "SchemaPatch",
    "AttentionPatch",
    "SystemPatch",
    "StewardshipInstruction",
];

/// Validated, compiled `life.observe` Cypher ready for execution.
#[derive(Debug)]
pub struct ObserveCypher {
    pub query: String,
    pub node_id: String,
    pub label: String,
    pub confidence: f64,
    pub source_membrane: String,
    pub provenance: String,
    pub validation_state: String,
    pub observed_at: String,
    pub created_at: String,
    pub claim_summary: String,
    pub observation_id: String,
    pub packet_id: String,
}

#[derive(Debug)]
pub struct CommitCypher {
    pub query: String,
    pub node_id: String,
    pub label: String,
    pub packet_id: String,
    pub confidence: f64,
    pub claim_summary: String,
    pub confirmed_at: String,
}

#[derive(Debug)]
pub struct ConflictCypher {
    pub query: String,
    pub handoff_id: String,
    pub conflict_id: String,
    pub status: String,
    pub summary: String,
    pub updated_at: String,
    pub resolution_summary: Option<String>,
    pub handoff_json: String,
}

#[derive(Debug)]
pub struct PatchProposalCypher {
    pub query: String,
    pub patch_id: String,
    pub label: String,
    pub patch_kind: String,
    pub summary: String,
    pub rationale: String,
    pub risk: String,
    pub status: String,
    pub proposed_at: String,
    pub patch_json: String,
}

#[derive(Debug)]
pub struct RecallFeedbackCypher {
    pub query: String,
    pub feedback_id: String,
    pub packet_id: String,
    pub rating: String,
    pub query_summary: String,
    pub note: String,
    pub candidate_count: i64,
    pub connected_candidate_count: i64,
    pub connectivity_ratio: Option<f64>,
    pub feedback_json: String,
    pub evaluation_json: String,
    pub observed_at: String,
}

pub fn compile_observe(input: &LifeObserveInput, now_iso: &str) -> Result<ObserveCypher, String> {
    let label = &input.evidence.claim_ref.label;
    if !is_known_label(label) {
        return Err(format!("unknown Life Graph label: {label}"));
    }

    let source_membrane = input
        .evidence
        .source_refs
        .first()
        .map(|s| s.source_id.clone())
        .or_else(|| {
            input
                .evidence
                .passage_refs
                .first()
                .and_then(|p| p.source_ref_id.clone())
        })
        .unwrap_or_else(|| "agent:memorygraphrag".to_string());

    let provenance = input
        .evidence
        .source_refs
        .first()
        .map(|s| source_kind_to_provenance(&s.source_kind))
        .unwrap_or_else(|| "agent_inferred".to_string());

    let validation_state = validation_state_str(&input.evidence.validation_state);

    let observed_at = input
        .evidence
        .observed_at
        .clone()
        .unwrap_or_else(|| now_iso.to_string());

    // Label is whitelisted above — safe to interpolate. All string values are
    // escaped via escape_cypher_str before embedding in the query.
    let query = format!(
        concat!(
            "MERGE (n:{label} {{id: $id}}) ",
            "ON CREATE SET ",
            "n.created_at = $created_at, ",
            "n.source_membrane = $source_membrane, ",
            "n.provenance = $provenance, ",
            "n.confidence = $confidence, ",
            "n.validation_state = $validation_state, ",
            "n.observed_at = $observed_at, ",
            "n.last_confirmed_at = null, ",
            "n.claim_summary = $claim_summary, ",
            "n.observation_id = $observation_id, ",
            "n.packet_id = $packet_id ",
            "ON MATCH SET ",
            "n.confidence = $confidence, ",
            "n.observation_id = $observation_id, ",
            "n.packet_id = $packet_id ",
            "RETURN n.id AS id, n.validation_state AS validation_state",
        ),
        label = label
    );

    Ok(ObserveCypher {
        query,
        node_id: input.evidence.claim_ref.id.clone(),
        label: label.clone(),
        confidence: input.evidence.confidence as f64,
        source_membrane,
        provenance,
        validation_state: validation_state.to_string(),
        observed_at,
        created_at: now_iso.to_string(),
        claim_summary: input.evidence.claim_summary.clone(),
        observation_id: input.observation_id.clone(),
        packet_id: input.evidence.packet_id.clone(),
    })
}

pub fn compile_commit(input: &LifeCommitInput, now_iso: &str) -> Result<CommitCypher, String> {
    input.evidence.validate().map_err(|e| e.to_string())?;
    let label = &input.evidence.claim_ref.label;
    if !is_known_label(label) {
        return Err(format!("unknown Life Graph label: {label}"));
    }

    let query = format!(
        concat!(
            "MERGE (n:{label} {{id: $id}}) ",
            "SET n.validation_state = 'confirmed', ",
            "n.last_confirmed_at = $confirmed_at, ",
            "n.confidence = $confidence, ",
            "n.claim_summary = $claim_summary, ",
            "n.packet_id = $packet_id ",
            "RETURN n.id AS id, n.validation_state AS validation_state",
        ),
        label = label
    );

    Ok(CommitCypher {
        query,
        node_id: input.evidence.claim_ref.id.clone(),
        label: label.clone(),
        packet_id: input.evidence.packet_id.clone(),
        confidence: input.evidence.confidence as f64,
        claim_summary: input.evidence.claim_summary.clone(),
        confirmed_at: now_iso.to_string(),
    })
}

pub fn compile_conflict_handoff(
    handoff: &ConflictHandoff,
    now_iso: &str,
) -> Result<ConflictCypher, String> {
    handoff.validate().map_err(|e| e.to_string())?;
    let handoff_json = serde_json::to_string(handoff).map_err(|e| e.to_string())?;

    Ok(ConflictCypher {
        query: concat!(
            "MERGE (h:ConflictHandoff {id: $handoff_id}) ",
            "SET h.conflict_id = $conflict_id, ",
            "h.summary = $summary, ",
            "h.status = $status, ",
            "h.updated_at = $updated_at, ",
            "h.handoff_json = $handoff_json ",
            "RETURN h.id AS id, h.status AS status"
        )
        .to_string(),
        handoff_id: handoff.handoff_id.clone(),
        conflict_id: handoff.conflict_id.clone(),
        status: conflict_status_str(&handoff.status).to_string(),
        summary: handoff.summary.clone(),
        updated_at: now_iso.to_string(),
        resolution_summary: None,
        handoff_json,
    })
}

pub fn compile_resolve(input: &LifeResolveInput, now_iso: &str) -> Result<ConflictCypher, String> {
    let mut handoff = input.handoff.clone();
    handoff.status = ConflictHandoffStatus::Resolved;
    handoff.validate().map_err(|e| e.to_string())?;
    let handoff_json = serde_json::to_string(&handoff).map_err(|e| e.to_string())?;

    Ok(ConflictCypher {
        query: concat!(
            "MERGE (h:ConflictHandoff {id: $handoff_id}) ",
            "SET h.conflict_id = $conflict_id, ",
            "h.summary = $summary, ",
            "h.status = 'resolved', ",
            "h.resolution_summary = $resolution_summary, ",
            "h.resolved_at = $updated_at, ",
            "h.updated_at = $updated_at, ",
            "h.handoff_json = $handoff_json ",
            "RETURN h.id AS id, h.status AS status"
        )
        .to_string(),
        handoff_id: handoff.handoff_id.clone(),
        conflict_id: handoff.conflict_id.clone(),
        status: "resolved".to_string(),
        summary: handoff.summary.clone(),
        updated_at: now_iso.to_string(),
        resolution_summary: Some(input.resolution_summary.clone()),
        handoff_json,
    })
}

pub fn compile_patch_proposal(
    input: &LifePatchProposalInput,
    now_iso: &str,
) -> Result<PatchProposalCypher, String> {
    let patch_json = serde_json::to_string(input).map_err(|e| e.to_string())?;
    let label = patch_label(&input.patch_kind);

    let query = format!(
        concat!(
            "MERGE (p:{label} {{id: $patch_id}}) ",
            "SET p.patch_kind = $patch_kind, ",
            "p.summary = $summary, ",
            "p.rationale = $rationale, ",
            "p.risk = $risk, ",
            "p.status = $status, ",
            "p.proposed_at = $proposed_at, ",
            "p.patch_json = $patch_json ",
            "RETURN p.id AS id, p.status AS status"
        ),
        label = label
    );

    Ok(PatchProposalCypher {
        query,
        patch_id: input.patch_id.clone(),
        label: label.to_string(),
        patch_kind: patch_kind_str(&input.patch_kind).to_string(),
        summary: input.summary.clone(),
        rationale: input.rationale.clone(),
        risk: format!("{:?}", input.risk).to_ascii_lowercase(),
        status: "proposed".to_string(),
        proposed_at: now_iso.to_string(),
        patch_json,
    })
}

pub fn compile_recall_feedback(
    input: &RetrievalFeedbackInput,
    growth_evaluation: &serde_json::Value,
    now_iso: &str,
) -> Result<RecallFeedbackCypher, String> {
    input.validate().map_err(|e| e.to_string())?;
    let feedback_json = serde_json::to_string(input).map_err(|e| e.to_string())?;
    let evaluation_json = serde_json::to_string(growth_evaluation).map_err(|e| e.to_string())?;
    let connectivity_ratio = input.connectivity_ratio().map(f64::from);

    Ok(RecallFeedbackCypher {
        query: concat!(
            "MERGE (s:Signal {id: $feedback_id}) ",
            "SET s.signal_type = 'life.recall.feedback', ",
            "s.packet_id = $packet_id, ",
            "s.rating = $rating, ",
            "s.query_summary = $query_summary, ",
            "s.note = $note, ",
            "s.candidate_count = $candidate_count, ",
            "s.connected_candidate_count = $connected_candidate_count, ",
            "s.connectivity_ratio = $connectivity_ratio, ",
            "s.feedback_json = $feedback_json, ",
            "s.growth_evaluation_json = $evaluation_json, ",
            "s.source_membrane = 'agent:memorygraphrag', ",
            "s.provenance = 'runtime_observation', ",
            "s.confidence = 1.0, ",
            "s.validation_state = 'confirmed', ",
            "s.observed_at = $observed_at, ",
            "s.last_confirmed_at = $observed_at ",
            "RETURN s.id AS id, s.rating AS rating"
        )
        .to_string(),
        feedback_id: input.feedback_id.clone(),
        packet_id: input.packet_id.clone(),
        rating: feedback_rating_str(&input.rating).to_string(),
        query_summary: input.query_summary.clone().unwrap_or_default(),
        note: input.note.clone().unwrap_or_default(),
        candidate_count: input.candidate_count as i64,
        connected_candidate_count: input.connected_candidate_count as i64,
        connectivity_ratio,
        feedback_json,
        evaluation_json,
        observed_at: now_iso.to_string(),
    })
}

fn source_kind_to_provenance(kind: &SourceKind) -> String {
    match kind {
        SourceKind::OperatorConfirmation => "operator_confirmed",
        SourceKind::MembraneEvent => "transcript",
        SourceKind::MuninnEngram => "agent_inferred",
        SourceKind::GraphPassage => "agent_inferred",
        SourceKind::ImportedRecord => "calendar",
        SourceKind::AgentInference => "agent_inferred",
        SourceKind::RuntimeObservation => "transcript",
    }
    .to_string()
}

fn validation_state_str(state: &ValidationState) -> &'static str {
    match state {
        ValidationState::Inferred => "inferred",
        ValidationState::Proposed => "proposed",
        ValidationState::Confirmed => "confirmed",
        ValidationState::Retired => "retired",
        ValidationState::Conflicted => "conflicted",
    }
}

pub fn is_known_label(label: &str) -> bool {
    KNOWN_LABELS.contains(&label)
}

fn conflict_status_str(status: &ConflictHandoffStatus) -> &'static str {
    match status {
        ConflictHandoffStatus::Open => "open",
        ConflictHandoffStatus::SentToMuninn => "sent_to_muninn",
        ConflictHandoffStatus::AwaitingOperator => "awaiting_operator",
        ConflictHandoffStatus::Resolved => "resolved",
        ConflictHandoffStatus::ClosedNoAction => "closed_no_action",
    }
}

fn patch_label(kind: &PatchKind) -> &'static str {
    match kind {
        PatchKind::SchemaPatch => "SchemaPatch",
        PatchKind::SkillPatch => "SkillPatch",
        PatchKind::ToolPatch => "ToolPatch",
        PatchKind::AttentionPatch => "AttentionPatch",
        PatchKind::SystemPatch => "SystemPatch",
    }
}

fn patch_kind_str(kind: &PatchKind) -> &'static str {
    match kind {
        PatchKind::SchemaPatch => "schema_patch",
        PatchKind::SkillPatch => "skill_patch",
        PatchKind::ToolPatch => "tool_patch",
        PatchKind::AttentionPatch => "attention_patch",
        PatchKind::SystemPatch => "system_patch",
    }
}

fn feedback_rating_str(rating: &RetrievalFeedbackRating) -> &'static str {
    match rating {
        RetrievalFeedbackRating::Useful => "useful",
        RetrievalFeedbackRating::Stale => "stale",
        RetrievalFeedbackRating::Missing => "missing",
        RetrievalFeedbackRating::Noisy => "noisy",
        RetrievalFeedbackRating::Overconfident => "overconfident",
        RetrievalFeedbackRating::Disconnected => "disconnected",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AdjudicationStatus, EvidencePacket, GraphRecordRef, LifeObserveInput, ReliabilityBasis,
        RetrievalFeedbackInput, RetrievalFeedbackRating, SourceKind, SourceRef, SourceReliability,
        ValidationState,
    };

    fn minimal_observe_input(label: &str) -> LifeObserveInput {
        LifeObserveInput {
            observation_id: "obs-001".to_string(),
            evidence: EvidencePacket {
                packet_id: "pkt-001".to_string(),
                claim_ref: GraphRecordRef {
                    id: "signal-abc".to_string(),
                    label: label.to_string(),
                    datasource: None,
                },
                claim_summary: "test signal".to_string(),
                source_refs: vec![SourceRef {
                    source_id: "membrane:telegram".to_string(),
                    source_kind: SourceKind::MembraneEvent,
                    reliability: SourceReliability {
                        score: 0.9,
                        basis: ReliabilityBasis::DirectObservation,
                    },
                    uri: None,
                    captured_at: None,
                }],
                passage_refs: vec![],
                confidence: 0.8,
                validation_state: ValidationState::Proposed,
                observed_at: Some("2026-06-04T00:00:00Z".to_string()),
                valid_time_range: None,
                source_reliability: 0.9,
                conflict_ids: vec![],
                adjudication_status: AdjudicationStatus::NotNeeded,
                metadata: serde_json::Value::Null,
            },
            proposed_graph_refs: vec![],
        }
    }

    #[test]
    fn compile_observe_signal_label_ok() {
        let input = minimal_observe_input("Signal");
        let compiled = compile_observe(&input, "2026-06-04T12:00:00Z").unwrap();

        assert_eq!(compiled.label, "Signal");
        assert_eq!(compiled.node_id, "signal-abc");
        assert_eq!(compiled.observation_id, "obs-001");
        assert_eq!(compiled.packet_id, "pkt-001");
        assert!((compiled.confidence - 0.8_f64).abs() < 1e-5);
        assert_eq!(compiled.validation_state, "proposed");
        assert_eq!(compiled.source_membrane, "membrane:telegram");
        assert_eq!(compiled.provenance, "transcript");
        assert_eq!(compiled.observed_at, "2026-06-04T00:00:00Z");
        assert!(compiled.query.contains("MERGE (n:Signal {id: $id})"));
        assert!(compiled.query.contains("ON CREATE SET"));
        assert!(compiled.query.contains("ON MATCH SET"));
        assert!(compiled.query.contains("RETURN n.id AS id"));
    }

    #[test]
    fn compile_observe_unknown_label_is_rejected() {
        let mut input = minimal_observe_input("Signal");
        input.evidence.claim_ref.label = "Gadget".to_string();
        let err = compile_observe(&input, "2026-06-04T12:00:00Z").unwrap_err();
        assert!(err.contains("Gadget"));
    }

    #[test]
    fn compile_observe_all_known_labels_accepted() {
        for label in KNOWN_LABELS {
            let mut input = minimal_observe_input(label);
            input.evidence.claim_ref.id = format!("{}-test-id", label.to_lowercase());
            let result = compile_observe(&input, "2026-06-04T12:00:00Z");
            assert!(result.is_ok(), "label {label} should be accepted");
        }
    }

    #[test]
    fn compile_observe_uses_now_when_observed_at_absent() {
        let mut input = minimal_observe_input("Goal");
        input.evidence.claim_ref.id = "goal-xyz".to_string();
        input.evidence.observed_at = None;
        let compiled = compile_observe(&input, "2026-06-04T09:00:00Z").unwrap();
        assert_eq!(compiled.observed_at, "2026-06-04T09:00:00Z");
        assert_eq!(compiled.created_at, "2026-06-04T09:00:00Z");
    }

    #[test]
    fn compile_observe_defaults_source_membrane_when_no_source_refs() {
        let mut input = minimal_observe_input("Event");
        input.evidence.claim_ref.id = "event-xyz".to_string();
        input.evidence.source_refs = vec![];
        input.evidence.passage_refs = vec![crate::PassageRef {
            passage_id: "p-1".to_string(),
            source_ref_id: Some("membrane:calendar".to_string()),
            excerpt_hash: None,
            muninn_engram_id: None,
            graph_node_id: None,
        }];
        let compiled = compile_observe(&input, "2026-06-04T09:00:00Z").unwrap();
        assert_eq!(compiled.source_membrane, "membrane:calendar");
    }

    #[test]
    fn compile_commit_confirms_known_life_graph_label() {
        let mut evidence = minimal_observe_input("OpenLoop").evidence;
        evidence.validation_state = ValidationState::Confirmed;
        let compiled = compile_commit(
            &crate::LifeCommitInput {
                evidence,
                operator_approved: false,
            },
            "2026-06-05T09:00:00Z",
        )
        .unwrap();

        assert_eq!(compiled.label, "OpenLoop");
        assert_eq!(compiled.confirmed_at, "2026-06-05T09:00:00Z");
        assert!(compiled.query.contains("MERGE (n:OpenLoop {id: $id})"));
        assert!(compiled.query.contains("n.validation_state = 'confirmed'"));
    }

    #[test]
    fn compile_conflict_handoff_stores_open_handoff() {
        let handoff = ConflictHandoff {
            handoff_id: "handoff:1".into(),
            conflict_id: "conflict:1".into(),
            finding_type: crate::ConflictFindingType::DirectContradiction,
            summary: "Graph and Muninn disagree.".into(),
            graph_fact_refs: vec![GraphRecordRef {
                id: "life:open_loop:1".into(),
                label: "OpenLoop".into(),
                datasource: None,
            }],
            evidence_packets: vec![minimal_observe_input("OpenLoop").evidence],
            muninn_engram_ids: vec![],
            recommended_owner: crate::HandoffOwner::DataMemoryGraphRag,
            requested_muninn_action: crate::MuninnRequestedAction::None,
            risk: crate::ConflictRisk::Low,
            requires_operator: false,
            status: ConflictHandoffStatus::Open,
            metadata: serde_json::json!({}),
        };

        let compiled = compile_conflict_handoff(&handoff, "2026-06-05T09:00:00Z").unwrap();
        assert_eq!(compiled.status, "open");
        assert_eq!(compiled.conflict_id, "conflict:1");
        assert!(compiled.query.contains("MERGE (h:ConflictHandoff"));
        assert!(compiled.handoff_json.contains("direct_contradiction"));
    }

    #[test]
    fn compile_resolve_marks_handoff_resolved() {
        let handoff = ConflictHandoff {
            handoff_id: "handoff:2".into(),
            conflict_id: "conflict:2".into(),
            finding_type: crate::ConflictFindingType::TemporalConflict,
            summary: "Old and new commitment dates conflict.".into(),
            graph_fact_refs: vec![GraphRecordRef {
                id: "life:commitment:1".into(),
                label: "Commitment".into(),
                datasource: None,
            }],
            evidence_packets: vec![minimal_observe_input("Commitment").evidence],
            muninn_engram_ids: vec![],
            recommended_owner: crate::HandoffOwner::DataMemoryGraphRag,
            requested_muninn_action: crate::MuninnRequestedAction::None,
            risk: crate::ConflictRisk::Low,
            requires_operator: false,
            status: ConflictHandoffStatus::Open,
            metadata: serde_json::json!({}),
        };

        let compiled = compile_resolve(
            &crate::LifeResolveInput {
                handoff,
                resolution_summary: "Use the newer date.".into(),
                operator_approved: false,
            },
            "2026-06-05T09:00:00Z",
        )
        .unwrap();

        assert_eq!(compiled.status, "resolved");
        assert_eq!(
            compiled.resolution_summary.as_deref(),
            Some("Use the newer date.")
        );
        assert!(compiled.query.contains("h.status = 'resolved'"));
    }

    #[test]
    fn compile_patch_proposal_uses_patch_label() {
        let compiled = compile_patch_proposal(
            &crate::LifePatchProposalInput {
                patch_id: "patch:attention:1".into(),
                patch_kind: PatchKind::AttentionPatch,
                summary: "Dampen a noisy nudge.".into(),
                rationale: "Recent friction suggests timing is off.".into(),
                evidence_packets: vec![minimal_observe_input("Signal").evidence],
                risk: crate::PatchRisk::Medium,
                operator_approved: false,
            },
            "2026-06-05T09:00:00Z",
        )
        .unwrap();

        assert_eq!(compiled.label, "AttentionPatch");
        assert_eq!(compiled.patch_kind, "attention_patch");
        assert!(compiled.query.contains("MERGE (p:AttentionPatch"));
        assert!(compiled.patch_json.contains("Dampen"));
    }

    #[test]
    fn compile_recall_feedback_writes_signal_with_growth_evaluation() {
        let feedback = RetrievalFeedbackInput {
            feedback_id: "feedback:recall:1".into(),
            packet_id: "packet:recall:1".into(),
            query_summary: Some("Re-enter LifeGraph work".into()),
            rating: RetrievalFeedbackRating::Disconnected,
            note: Some("Good facts, but they were not connected to the active project.".into()),
            candidate_count: 4,
            connected_candidate_count: 1,
            missing_context_refs: vec![],
            noisy_node_refs: vec![],
            stale_node_refs: vec![],
            evidence_packets: vec![minimal_observe_input("Signal").evidence],
        };
        let growth_evaluation = serde_json::json!({
            "disposition": "apply_with_audit",
            "rationale": ["low connected-candidate ratio"]
        });

        let compiled =
            compile_recall_feedback(&feedback, &growth_evaluation, "2026-06-22T10:00:00Z").unwrap();

        assert_eq!(compiled.feedback_id, "feedback:recall:1");
        assert_eq!(compiled.rating, "disconnected");
        assert_eq!(compiled.connectivity_ratio, Some(0.25));
        assert!(compiled.query.contains("MERGE (s:Signal"));
        assert!(compiled.feedback_json.contains("packet:recall:1"));
        assert!(compiled.evaluation_json.contains("apply_with_audit"));
    }
}
