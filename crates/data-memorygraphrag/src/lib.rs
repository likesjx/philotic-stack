//! Life Graph OS MemoryGraphRAG contract types.
//!
//! This crate owns Life Graph-specific evidence and conflict payloads while
//! keeping `graph-datasource` generic. Runtime adapters can serialize these
//! contracts into graph writes, context packets, or Muninn true-up requests.

use serde::{Deserialize, Serialize};
use std::fmt;

pub type PacketId = String;
pub type ConflictId = String;
pub type GraphRecordId = String;
pub type MuninnEngramId = String;

pub const LIFE_GRAPH_EMBEDDING_DIMS: usize = 1536;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationState {
    Inferred,
    Proposed,
    Confirmed,
    Retired,
    Conflicted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdjudicationStatus {
    NotNeeded,
    Pending,
    MuninnFirst,
    GraphReview,
    OperatorRequired,
    Resolved,
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    OperatorConfirmation,
    MembraneEvent,
    MuninnEngram,
    GraphPassage,
    ImportedRecord,
    AgentInference,
    RuntimeObservation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReliabilityBasis {
    OperatorConfirmed,
    DirectObservation,
    MuninnTrust,
    ImportedAuthority,
    AgentInferred,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceReliability {
    pub score: f32,
    pub basis: ReliabilityBasis,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceRef {
    pub source_id: String,
    pub source_kind: SourceKind,
    pub reliability: SourceReliability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub captured_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PassageRef {
    pub passage_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excerpt_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub muninn_engram_id: Option<MuninnEngramId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_node_id: Option<GraphRecordId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphRecordRef {
    pub id: GraphRecordId,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub datasource: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starts_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidencePacket {
    pub packet_id: PacketId,
    pub claim_ref: GraphRecordRef,
    pub claim_summary: String,
    #[serde(default)]
    pub source_refs: Vec<SourceRef>,
    #[serde(default)]
    pub passage_refs: Vec<PassageRef>,
    pub confidence: f32,
    pub validation_state: ValidationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_time_range: Option<TimeRange>,
    pub source_reliability: f32,
    #[serde(default)]
    pub conflict_ids: Vec<ConflictId>,
    pub adjudication_status: AdjudicationStatus,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl EvidencePacket {
    pub fn validate(&self) -> Result<(), ContractError> {
        let mut violations = Vec::new();

        require_non_empty(&mut violations, "packet_id", &self.packet_id);
        require_non_empty(&mut violations, "claim_ref.id", &self.claim_ref.id);
        require_non_empty(&mut violations, "claim_ref.label", &self.claim_ref.label);
        require_non_empty(&mut violations, "claim_summary", &self.claim_summary);
        require_unit_interval(&mut violations, "confidence", self.confidence);
        require_unit_interval(
            &mut violations,
            "source_reliability",
            self.source_reliability,
        );

        if self.source_refs.is_empty() && self.passage_refs.is_empty() {
            violations.push(
                "evidence packet requires at least one source_ref or passage_ref".to_string(),
            );
        }

        for (idx, source) in self.source_refs.iter().enumerate() {
            require_non_empty(
                &mut violations,
                &format!("source_refs[{idx}].source_id"),
                &source.source_id,
            );
            require_unit_interval(
                &mut violations,
                &format!("source_refs[{idx}].reliability.score"),
                source.reliability.score,
            );
        }

        for (idx, passage) in self.passage_refs.iter().enumerate() {
            require_non_empty(
                &mut violations,
                &format!("passage_refs[{idx}].passage_id"),
                &passage.passage_id,
            );
        }

        if matches!(self.validation_state, ValidationState::Conflicted)
            && self.conflict_ids.is_empty()
        {
            violations.push("conflicted evidence packets require at least one conflict_id".into());
        }

        if let Some(range) = &self.valid_time_range {
            if range.starts_at.is_none() && range.ends_at.is_none() {
                violations
                    .push("valid_time_range requires starts_at, ends_at, or both".to_string());
            }
        }

        finish_validation(violations)
    }

    pub fn requires_muninn_handoff(&self) -> bool {
        matches!(
            self.validation_state,
            ValidationState::Conflicted | ValidationState::Retired
        ) || matches!(
            self.adjudication_status,
            AdjudicationStatus::MuninnFirst | AdjudicationStatus::OperatorRequired
        ) || !self.conflict_ids.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictFindingType {
    DirectContradiction,
    TemporalConflict,
    GranularityConflict,
    IdentityAmbiguity,
    Staleness,
    PolicyRisk,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandoffOwner {
    Muninn,
    DataMemoryGraphRag,
    SharedGate,
    Operator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MuninnRequestedAction {
    None,
    TrueUp,
    ContradictionReview,
    TrustUpdate,
    Cultivate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictRisk {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictHandoffStatus {
    Open,
    SentToMuninn,
    AwaitingOperator,
    Resolved,
    ClosedNoAction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConflictHandoff {
    pub handoff_id: String,
    pub conflict_id: ConflictId,
    pub finding_type: ConflictFindingType,
    pub summary: String,
    #[serde(default)]
    pub graph_fact_refs: Vec<GraphRecordRef>,
    #[serde(default)]
    pub evidence_packets: Vec<EvidencePacket>,
    #[serde(default)]
    pub muninn_engram_ids: Vec<MuninnEngramId>,
    pub recommended_owner: HandoffOwner,
    pub requested_muninn_action: MuninnRequestedAction,
    pub risk: ConflictRisk,
    pub requires_operator: bool,
    pub status: ConflictHandoffStatus,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl ConflictHandoff {
    pub fn validate(&self) -> Result<(), ContractError> {
        let mut violations = Vec::new();

        require_non_empty(&mut violations, "handoff_id", &self.handoff_id);
        require_non_empty(&mut violations, "conflict_id", &self.conflict_id);
        require_non_empty(&mut violations, "summary", &self.summary);

        if self.graph_fact_refs.is_empty()
            && self.evidence_packets.is_empty()
            && self.muninn_engram_ids.is_empty()
        {
            violations.push(
                "conflict handoff requires graph_fact_refs, evidence_packets, or muninn_engram_ids"
                    .to_string(),
            );
        }

        for (idx, fact_ref) in self.graph_fact_refs.iter().enumerate() {
            require_non_empty(
                &mut violations,
                &format!("graph_fact_refs[{idx}].id"),
                &fact_ref.id,
            );
            require_non_empty(
                &mut violations,
                &format!("graph_fact_refs[{idx}].label"),
                &fact_ref.label,
            );
        }

        for (idx, packet) in self.evidence_packets.iter().enumerate() {
            if let Err(err) = packet.validate() {
                for violation in err.violations {
                    violations.push(format!("evidence_packets[{idx}].{violation}"));
                }
            }
        }

        if matches!(
            self.recommended_owner,
            HandoffOwner::Muninn | HandoffOwner::SharedGate
        ) && matches!(self.requested_muninn_action, MuninnRequestedAction::None)
        {
            violations
                .push("Muninn or shared-gate handoffs require a requested_muninn_action".into());
        }

        if matches!(self.risk, ConflictRisk::High) && !self.requires_operator {
            violations.push("high-risk conflict handoffs require operator review".into());
        }

        finish_validation(violations)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticSpace {
    LifeEventSemantic,
    GoalSystemSemantic,
    SkillToolSemantic,
    RolePersonSemantic,
    MemoryBridgeSemantic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalStrategy {
    SemanticPivot,
    VectorThenExpand,
    MemoryAwareGraphRank,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyFilter {
    ExcludeRetired,
    ExcludeConflictedUnlessRequested,
    RequireEvidence,
    RoleAppropriate,
    LowAgencyOnly,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SemanticPivot {
    pub space: SemanticSpace,
    pub embedding_model: String,
    pub embedding_dims: usize,
    pub query_text_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpansionPolicy {
    pub max_hops: u8,
    pub max_nodes: usize,
    #[serde(default)]
    pub allowed_edge_types: Vec<String>,
}

impl Default for ExpansionPolicy {
    fn default() -> Self {
        Self {
            max_hops: 2,
            max_nodes: 32,
            allowed_edge_types: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankingWeights {
    pub semantic_similarity: f32,
    pub graph_specificity: f32,
    pub recency: f32,
    pub confirmation: f32,
    pub active_commitment: f32,
}

impl Default for RankingWeights {
    fn default() -> Self {
        Self {
            semantic_similarity: 0.45,
            graph_specificity: 0.2,
            recency: 0.1,
            confirmation: 0.15,
            active_commitment: 0.1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalQuery {
    pub query_id: String,
    pub query_text: String,
    pub strategy: RetrievalStrategy,
    #[serde(default)]
    pub semantic_pivots: Vec<SemanticPivot>,
    pub expansion_policy: ExpansionPolicy,
    #[serde(default)]
    pub policy_filters: Vec<PolicyFilter>,
    pub ranking_weights: RankingWeights,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_intent: Option<String>,
    pub max_context_packets: usize,
}

impl RetrievalQuery {
    pub fn validate(&self) -> Result<(), ContractError> {
        let mut violations = Vec::new();

        require_non_empty(&mut violations, "query_id", &self.query_id);
        require_non_empty(&mut violations, "query_text", &self.query_text);

        if self.semantic_pivots.is_empty() {
            violations.push("retrieval query requires at least one semantic_pivot".into());
        }

        for (idx, pivot) in self.semantic_pivots.iter().enumerate() {
            require_non_empty(
                &mut violations,
                &format!("semantic_pivots[{idx}].embedding_model"),
                &pivot.embedding_model,
            );
            require_non_empty(
                &mut violations,
                &format!("semantic_pivots[{idx}].query_text_hash"),
                &pivot.query_text_hash,
            );
            if pivot.embedding_dims != LIFE_GRAPH_EMBEDDING_DIMS {
                violations.push(format!(
                    "semantic_pivots[{idx}].embedding_dims must be {LIFE_GRAPH_EMBEDDING_DIMS}"
                ));
            }
        }

        if self.expansion_policy.max_hops > 4 {
            violations.push("expansion_policy.max_hops must be <= 4".into());
        }
        if self.expansion_policy.max_nodes == 0 {
            violations.push("expansion_policy.max_nodes must be greater than 0".into());
        }
        if self.max_context_packets == 0 {
            violations.push("max_context_packets must be greater than 0".into());
        }

        require_unit_interval(
            &mut violations,
            "ranking_weights.semantic_similarity",
            self.ranking_weights.semantic_similarity,
        );
        require_unit_interval(
            &mut violations,
            "ranking_weights.graph_specificity",
            self.ranking_weights.graph_specificity,
        );
        require_unit_interval(
            &mut violations,
            "ranking_weights.recency",
            self.ranking_weights.recency,
        );
        require_unit_interval(
            &mut violations,
            "ranking_weights.confirmation",
            self.ranking_weights.confirmation,
        );
        require_unit_interval(
            &mut violations,
            "ranking_weights.active_commitment",
            self.ranking_weights.active_commitment,
        );

        finish_validation(violations)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankedEvidencePacket {
    pub packet: EvidencePacket,
    pub score: f32,
    #[serde(default)]
    pub matched_policy_filters: Vec<PolicyFilter>,
    #[serde(default)]
    pub evidence_path: Vec<GraphRecordRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalContextPacket {
    pub context_id: String,
    pub query_id: String,
    pub strategy: RetrievalStrategy,
    #[serde(default)]
    pub ranked_packets: Vec<RankedEvidencePacket>,
    #[serde(default)]
    pub omitted_conflict_ids: Vec<ConflictId>,
    pub token_budget: usize,
    pub generated_at: String,
}

impl RetrievalContextPacket {
    pub fn validate(&self) -> Result<(), ContractError> {
        let mut violations = Vec::new();

        require_non_empty(&mut violations, "context_id", &self.context_id);
        require_non_empty(&mut violations, "query_id", &self.query_id);
        require_non_empty(&mut violations, "generated_at", &self.generated_at);

        if self.ranked_packets.is_empty() {
            violations.push("retrieval context packet requires at least one ranked_packet".into());
        }
        if self.token_budget == 0 {
            violations.push("token_budget must be greater than 0".into());
        }

        for (idx, ranked) in self.ranked_packets.iter().enumerate() {
            require_unit_interval(
                &mut violations,
                &format!("ranked_packets[{idx}].score"),
                ranked.score,
            );
            if let Err(err) = ranked.packet.validate() {
                for violation in err.violations {
                    violations.push(format!("ranked_packets[{idx}].packet.{violation}"));
                }
            }
        }

        finish_validation(violations)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractError {
    pub violations: Vec<String>,
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "contract validation failed: {}",
            self.violations.join("; ")
        )
    }
}

impl std::error::Error for ContractError {}

fn require_non_empty(violations: &mut Vec<String>, field: &str, value: &str) {
    if value.trim().is_empty() {
        violations.push(format!("{field} must not be empty"));
    }
}

fn require_unit_interval(violations: &mut Vec<String>, field: &str, value: f32) {
    if !(0.0..=1.0).contains(&value) {
        violations.push(format!("{field} must be between 0.0 and 1.0"));
    }
}

fn finish_validation(violations: Vec<String>) -> Result<(), ContractError> {
    if violations.is_empty() {
        Ok(())
    } else {
        Err(ContractError { violations })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph_ref(id: &str) -> GraphRecordRef {
        GraphRecordRef {
            id: id.into(),
            label: "OpenLoop".into(),
            datasource: Some("life-graph".into()),
        }
    }

    fn source_ref() -> SourceRef {
        SourceRef {
            source_id: "muninn:01KTA0Z5K942VAP9NPAAABH20T".into(),
            source_kind: SourceKind::MuninnEngram,
            reliability: SourceReliability {
                score: 0.82,
                basis: ReliabilityBasis::MuninnTrust,
            },
            uri: None,
            captured_at: Some("2026-06-04T19:15:10Z".into()),
        }
    }

    fn evidence_packet() -> EvidencePacket {
        EvidencePacket {
            packet_id: "evidence:open-loop-rowing:20260604".into(),
            claim_ref: graph_ref("life:open_loop:rowing-follow-up"),
            claim_summary: "Operator has an unresolved rowing follow-up open loop.".into(),
            source_refs: vec![source_ref()],
            passage_refs: vec![PassageRef {
                passage_id: "passage:telegram:abc123".into(),
                source_ref_id: Some("muninn:01KTA0Z5K942VAP9NPAAABH20T".into()),
                excerpt_hash: Some("sha256:abc123".into()),
                muninn_engram_id: Some("01KTA0Z5K942VAP9NPAAABH20T".into()),
                graph_node_id: None,
            }],
            confidence: 0.74,
            validation_state: ValidationState::Proposed,
            observed_at: Some("2026-06-04T19:15:10Z".into()),
            valid_time_range: Some(TimeRange {
                starts_at: Some("2026-06-04T00:00:00Z".into()),
                ends_at: None,
            }),
            source_reliability: 0.82,
            conflict_ids: vec![],
            adjudication_status: AdjudicationStatus::Pending,
            metadata: serde_json::json!({"role": "beacon"}),
        }
    }

    #[test]
    fn evidence_packet_serializes_snake_case_contract() {
        let packet = evidence_packet();
        packet.validate().expect("packet should be valid");

        let json = serde_json::to_value(&packet).expect("serialize packet");
        assert_eq!(json["validation_state"], "proposed");
        assert_eq!(json["adjudication_status"], "pending");
        assert_eq!(json["source_refs"][0]["source_kind"], "muninn_engram");
        assert_eq!(
            json["source_refs"][0]["reliability"]["basis"],
            "muninn_trust"
        );
    }

    #[test]
    fn evidence_packet_requires_grounding_refs() {
        let mut packet = evidence_packet();
        packet.source_refs.clear();
        packet.passage_refs.clear();

        let err = packet
            .validate()
            .expect_err("packet should reject no evidence");
        assert!(
            err.violations
                .iter()
                .any(|v| v.contains("at least one source_ref or passage_ref"))
        );
    }

    #[test]
    fn conflicted_packet_requires_conflict_id_and_muninn_handoff() {
        let mut packet = evidence_packet();
        packet.validation_state = ValidationState::Conflicted;
        packet.adjudication_status = AdjudicationStatus::MuninnFirst;

        let err = packet
            .validate()
            .expect_err("conflicted packet should require ids");
        assert!(
            err.violations
                .iter()
                .any(|v| v.contains("require at least one conflict_id"))
        );

        packet
            .conflict_ids
            .push("conflict:open-loop:resolved-vs-active".into());
        packet.validate().expect("conflict id completes packet");
        assert!(packet.requires_muninn_handoff());
    }

    #[test]
    fn conflict_handoff_requires_muninn_action_for_muninn_owner() {
        let handoff = ConflictHandoff {
            handoff_id: "handoff:conflict:1".into(),
            conflict_id: "conflict:open-loop:resolved-vs-active".into(),
            finding_type: ConflictFindingType::DirectContradiction,
            summary: "Graph says open loop is active while Muninn recall says it was resolved."
                .into(),
            graph_fact_refs: vec![graph_ref("life:open_loop:rowing-follow-up")],
            evidence_packets: vec![evidence_packet()],
            muninn_engram_ids: vec!["01KTA0Z5K942VAP9NPAAABH20T".into()],
            recommended_owner: HandoffOwner::Muninn,
            requested_muninn_action: MuninnRequestedAction::None,
            risk: ConflictRisk::Medium,
            requires_operator: false,
            status: ConflictHandoffStatus::Open,
            metadata: serde_json::json!({}),
        };

        let err = handoff
            .validate()
            .expect_err("Muninn handoff should request an action");
        assert!(
            err.violations
                .iter()
                .any(|v| v.contains("requested_muninn_action"))
        );
    }

    #[test]
    fn high_risk_conflict_requires_operator_review() {
        let mut handoff = ConflictHandoff {
            handoff_id: "handoff:conflict:2".into(),
            conflict_id: "conflict:identity:ambiguous-person".into(),
            finding_type: ConflictFindingType::IdentityAmbiguity,
            summary: "Identity bridge would merge two people with only nickname evidence.".into(),
            graph_fact_refs: vec![graph_ref("life:person:ambiguous")],
            evidence_packets: vec![evidence_packet()],
            muninn_engram_ids: vec![],
            recommended_owner: HandoffOwner::Operator,
            requested_muninn_action: MuninnRequestedAction::None,
            risk: ConflictRisk::High,
            requires_operator: false,
            status: ConflictHandoffStatus::Open,
            metadata: serde_json::json!({}),
        };

        let err = handoff.validate().expect_err("high risk requires operator");
        assert!(err.violations.iter().any(|v| v.contains("operator review")));

        handoff.requires_operator = true;
        handoff.validate().expect("operator gate completes handoff");
    }

    fn semantic_pivot() -> SemanticPivot {
        SemanticPivot {
            space: SemanticSpace::GoalSystemSemantic,
            embedding_model: "text-embedding-3-small".into(),
            embedding_dims: LIFE_GRAPH_EMBEDDING_DIMS,
            query_text_hash: "sha256:query123".into(),
            vector_ref: Some("vector:query123".into()),
        }
    }

    fn retrieval_query() -> RetrievalQuery {
        RetrievalQuery {
            query_id: "retrieval:open-loops:20260604".into(),
            query_text: "What follow-up loops matter for Beacon today?".into(),
            strategy: RetrievalStrategy::MemoryAwareGraphRank,
            semantic_pivots: vec![semantic_pivot()],
            expansion_policy: ExpansionPolicy {
                max_hops: 2,
                max_nodes: 24,
                allowed_edge_types: vec!["supports".into(), "blocks".into(), "belongs_to".into()],
            },
            policy_filters: vec![
                PolicyFilter::ExcludeRetired,
                PolicyFilter::ExcludeConflictedUnlessRequested,
                PolicyFilter::RequireEvidence,
                PolicyFilter::RoleAppropriate,
            ],
            ranking_weights: RankingWeights::default(),
            active_role: Some("beacon".into()),
            operator_intent: Some("attention planning".into()),
            max_context_packets: 6,
        }
    }

    #[test]
    fn retrieval_query_requires_life_graph_embedding_dims() {
        let mut query = retrieval_query();
        query.validate().expect("query should be valid");

        query.semantic_pivots[0].embedding_dims = 768;
        let err = query
            .validate()
            .expect_err("query should reject wrong embedding dims");
        assert!(
            err.violations
                .iter()
                .any(|v| v.contains("embedding_dims must be 1536"))
        );
    }

    #[test]
    fn retrieval_query_bounds_graph_expansion() {
        let mut query = retrieval_query();
        query.expansion_policy.max_hops = 5;

        let err = query
            .validate()
            .expect_err("query should reject unbounded expansion");
        assert!(
            err.violations
                .iter()
                .any(|v| v.contains("max_hops must be <= 4"))
        );
    }

    #[test]
    fn retrieval_context_packet_validates_ranked_evidence() {
        let packet = RetrievalContextPacket {
            context_id: "context:beacon:open-loops".into(),
            query_id: "retrieval:open-loops:20260604".into(),
            strategy: RetrievalStrategy::MemoryAwareGraphRank,
            ranked_packets: vec![RankedEvidencePacket {
                packet: evidence_packet(),
                score: 0.91,
                matched_policy_filters: vec![PolicyFilter::RequireEvidence],
                evidence_path: vec![
                    graph_ref("life:goal:health"),
                    graph_ref("life:open_loop:rowing-follow-up"),
                ],
            }],
            omitted_conflict_ids: vec![],
            token_budget: 2_000,
            generated_at: "2026-06-04T19:36:00Z".into(),
        };

        packet.validate().expect("context packet should be valid");

        let json = serde_json::to_value(packet).expect("serialize context packet");
        assert_eq!(json["strategy"], "memory_aware_graph_rank");
        assert_eq!(
            json["ranked_packets"][0]["matched_policy_filters"][0],
            "require_evidence"
        );
    }
}
