// Pure Cypher compilation for Life Graph OS observe operations.
// No I/O — all functions are deterministic and fully testable.

use crate::{LifeObserveInput, SourceKind, ValidationState};

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

pub fn compile_observe(
    input: &LifeObserveInput,
    now_iso: &str,
) -> Result<ObserveCypher, String> {
    let label = &input.evidence.claim_ref.label;
    if !KNOWN_LABELS.contains(&label.as_str()) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AdjudicationStatus, EvidencePacket, GraphRecordRef, LifeObserveInput, ReliabilityBasis,
        SourceKind, SourceReliability, SourceRef, ValidationState,
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
}
