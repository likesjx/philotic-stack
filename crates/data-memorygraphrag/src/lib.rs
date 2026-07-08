//! Life Graph OS MemoryGraphRAG contract types.
//!
//! This crate owns Life Graph-specific evidence and conflict payloads while
//! keeping `graph-datasource` generic. Runtime adapters can serialize these
//! contracts into graph writes, context packets, or Muninn true-up requests.

pub mod attention_observer;
pub mod cypher;
pub mod entanglement;
pub mod projection;
pub mod zoning;

use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fmt};

pub type PacketId = String;
pub type ConflictId = String;
pub type GraphRecordId = String;
pub type MuninnEngramId = String;

/// Canonical embedding dimension for the Life Graph.
/// Canonical model: Xenova/all-mpnet-base-v2 (768d, sentence-transformers).
/// Fine-tune on HuggingFace; bump embedding_model_gen to trigger reindex.
pub const LIFE_GRAPH_EMBEDDING_DIMS: usize = 768;

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

/// Composite ranking weights for LifeGraph retrieval.
///
/// The five base weights (`semantic_similarity`, `graph_specificity`,
/// `recency`, `confirmation`, `active_commitment`) sum to 1.0 by default.
/// `role_relevance` is an *additive soft-zoning bonus* on top of that base:
/// it is only earned by hits tied to the caller's `active_role` domain (via a
/// living-cycle edge to the V005 domain Role node, or `observed_by`
/// provenance mapping to the domain's steward agent). It biases ranking
/// toward the caller's domain WITHOUT ever filtering cross-domain hits — the
/// final score is clamped to `[0.0, 1.0]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankingWeights {
    pub semantic_similarity: f32,
    pub graph_specificity: f32,
    pub recency: f32,
    pub confirmation: f32,
    pub active_commitment: f32,
    /// Soft domain-affinity bonus; defaults to 0.15. Kept `serde(default)` so
    /// pre-existing five-field wire payloads still deserialize.
    #[serde(default = "default_role_relevance_weight")]
    pub role_relevance: f32,
}

fn default_role_relevance_weight() -> f32 {
    0.15
}

impl Default for RankingWeights {
    fn default() -> Self {
        Self {
            semantic_similarity: 0.45,
            graph_specificity: 0.2,
            recency: 0.1,
            confirmation: 0.15,
            active_commitment: 0.1,
            role_relevance: default_role_relevance_weight(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalQuery {
    pub query_id: String,
    pub query_text: String,
    #[serde(default = "default_retrieval_strategy")]
    pub strategy: RetrievalStrategy,
    #[serde(default)]
    pub semantic_pivots: Vec<SemanticPivot>,
    #[serde(default)]
    pub expansion_policy: ExpansionPolicy,
    #[serde(default)]
    pub policy_filters: Vec<PolicyFilter>,
    #[serde(default)]
    pub ranking_weights: RankingWeights,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_intent: Option<String>,
    #[serde(default = "default_max_context_packets")]
    pub max_context_packets: usize,
}

fn default_retrieval_strategy() -> RetrievalStrategy {
    RetrievalStrategy::MemoryAwareGraphRank
}

fn default_max_context_packets() -> usize {
    6
}

impl RetrievalQuery {
    pub fn validate(&self) -> Result<(), ContractError> {
        let mut violations = Vec::new();

        require_non_empty(&mut violations, "query_id", &self.query_id);
        require_non_empty(&mut violations, "query_text", &self.query_text);

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
        require_unit_interval(
            &mut violations,
            "ranking_weights.role_relevance",
            self.ranking_weights.role_relevance,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextAuthority {
    MuninnContinuity,
    LifeGraphTruth,
    LifeGraphEvidence,
    IntelGraphProjectTruth,
    RuntimeObservation,
    AgentInference,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextRefKind {
    MuninnEngram,
    LifeGraphNode,
    LifeGraphEvidencePacket,
    LifeGraphRetrievalPacket,
    IntelGraphNode,
    RepoDoc,
    RuntimeObservation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextRef {
    pub ref_id: String,
    pub kind: ContextRefKind,
    pub authority: ContextAuthority,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_state: Option<ValidationState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl ContextRef {
    pub fn validate(&self) -> Result<(), ContractError> {
        let mut violations = Vec::new();
        require_non_empty(&mut violations, "ref_id", &self.ref_id);

        match (&self.kind, &self.authority) {
            (ContextRefKind::MuninnEngram, ContextAuthority::MuninnContinuity)
            | (ContextRefKind::LifeGraphNode, ContextAuthority::LifeGraphTruth)
            | (ContextRefKind::LifeGraphNode, ContextAuthority::LifeGraphEvidence)
            | (ContextRefKind::LifeGraphEvidencePacket, ContextAuthority::LifeGraphEvidence)
            | (ContextRefKind::LifeGraphRetrievalPacket, ContextAuthority::LifeGraphEvidence)
            | (ContextRefKind::IntelGraphNode, ContextAuthority::IntelGraphProjectTruth)
            | (ContextRefKind::RepoDoc, ContextAuthority::IntelGraphProjectTruth)
            | (ContextRefKind::RuntimeObservation, ContextAuthority::RuntimeObservation)
            | (_, ContextAuthority::AgentInference) => {}
            _ => violations.push(format!(
                "{:?} cannot claim {:?} authority",
                self.kind, self.authority
            )),
        }

        if matches!(self.authority, ContextAuthority::LifeGraphTruth)
            && matches!(
                self.validation_state,
                Some(ValidationState::Inferred | ValidationState::Proposed)
            )
        {
            violations.push("LifeGraphTruth refs cannot be inferred or proposed".into());
        }

        finish_validation(violations)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextPacketSection {
    pub title: String,
    pub authority: ContextAuthority,
    #[serde(default)]
    pub ref_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl ContextPacketSection {
    pub fn validate(&self) -> Result<(), ContractError> {
        let mut violations = Vec::new();
        require_non_empty(&mut violations, "title", &self.title);
        if self.ref_ids.is_empty() && self.text.as_deref().unwrap_or("").trim().is_empty() {
            violations.push("section requires ref_ids or text".into());
        }
        finish_validation(violations)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MuninnRecallMemory {
    pub id: MuninnEngramId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concept: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Cross-agent context envelope that can carry Muninn, LifeGraph, Intel Graph,
/// repo, and runtime references without erasing their authority boundaries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextPacket {
    pub packet_id: String,
    pub generated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience_role: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub refs: Vec<ContextRef>,
    #[serde(default)]
    pub sections: Vec<ContextPacketSection>,
    #[serde(default)]
    pub policy_notes: Vec<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl ContextPacket {
    pub fn validate(&self) -> Result<(), ContractError> {
        let mut violations = Vec::new();
        require_non_empty(&mut violations, "packet_id", &self.packet_id);
        require_non_empty(&mut violations, "generated_at", &self.generated_at);
        require_non_empty(&mut violations, "summary", &self.summary);

        if self.refs.is_empty() && self.sections.is_empty() {
            violations.push("context packet requires at least one ref or section".into());
        }

        let known_ref_ids: BTreeSet<_> = self.refs.iter().map(|r| r.ref_id.as_str()).collect();

        for (idx, context_ref) in self.refs.iter().enumerate() {
            if let Err(err) = context_ref.validate() {
                for violation in err.violations {
                    violations.push(format!("refs[{idx}].{violation}"));
                }
            }
        }

        for (idx, section) in self.sections.iter().enumerate() {
            if let Err(err) = section.validate() {
                for violation in err.violations {
                    violations.push(format!("sections[{idx}].{violation}"));
                }
            }
            for ref_id in &section.ref_ids {
                if !known_ref_ids.contains(ref_id.as_str()) {
                    violations.push(format!(
                        "sections[{idx}].ref_ids contains unknown ref {ref_id}"
                    ));
                }
            }
        }

        finish_validation(violations)
    }

    pub fn from_lifegraph_retrieval(
        packet: &RetrievalContextPacket,
        summary: impl Into<String>,
        audience_role: Option<String>,
    ) -> Self {
        let mut refs = vec![ContextRef {
            ref_id: packet.context_id.clone(),
            kind: ContextRefKind::LifeGraphRetrievalPacket,
            authority: ContextAuthority::LifeGraphEvidence,
            summary: Some("LifeGraph retrieval context packet".into()),
            validation_state: None,
            uri: None,
            metadata: serde_json::json!({
                "query_id": packet.query_id,
                "strategy": packet.strategy,
            }),
        }];

        let mut seen_muninn = BTreeSet::new();
        let mut seen_lifegraph = BTreeSet::new();

        for ranked in &packet.ranked_packets {
            let evidence = &ranked.packet;
            if seen_lifegraph.insert(evidence.claim_ref.id.clone()) {
                refs.push(ContextRef {
                    ref_id: evidence.claim_ref.id.clone(),
                    kind: ContextRefKind::LifeGraphNode,
                    authority: match evidence.validation_state {
                        ValidationState::Confirmed => ContextAuthority::LifeGraphTruth,
                        _ => ContextAuthority::LifeGraphEvidence,
                    },
                    summary: Some(evidence.claim_summary.clone()),
                    validation_state: Some(evidence.validation_state.clone()),
                    uri: None,
                    metadata: serde_json::json!({
                        "label": evidence.claim_ref.label,
                        "datasource": evidence.claim_ref.datasource,
                        "score": ranked.score,
                    }),
                });
            }

            refs.push(ContextRef {
                ref_id: evidence.packet_id.clone(),
                kind: ContextRefKind::LifeGraphEvidencePacket,
                authority: ContextAuthority::LifeGraphEvidence,
                summary: Some(evidence.claim_summary.clone()),
                validation_state: Some(evidence.validation_state.clone()),
                uri: None,
                metadata: serde_json::json!({
                    "claim_ref": evidence.claim_ref.id,
                    "confidence": evidence.confidence,
                    "source_reliability": evidence.source_reliability,
                }),
            });

            for source in &evidence.source_refs {
                if matches!(source.source_kind, SourceKind::MuninnEngram)
                    && seen_muninn.insert(source.source_id.clone())
                {
                    refs.push(ContextRef {
                        ref_id: source.source_id.clone(),
                        kind: ContextRefKind::MuninnEngram,
                        authority: ContextAuthority::MuninnContinuity,
                        summary: Some("Muninn continuity source for LifeGraph evidence".into()),
                        validation_state: None,
                        uri: source.uri.clone(),
                        metadata: serde_json::json!({
                            "captured_at": source.captured_at,
                            "reliability": source.reliability,
                        }),
                    });
                }
            }

            for passage in &evidence.passage_refs {
                if let Some(muninn_id) = &passage.muninn_engram_id {
                    if seen_muninn.insert(muninn_id.clone()) {
                        refs.push(ContextRef {
                            ref_id: muninn_id.clone(),
                            kind: ContextRefKind::MuninnEngram,
                            authority: ContextAuthority::MuninnContinuity,
                            summary: Some("Muninn passage source for LifeGraph evidence".into()),
                            validation_state: None,
                            uri: None,
                            metadata: serde_json::json!({
                                "passage_id": passage.passage_id,
                                "excerpt_hash": passage.excerpt_hash,
                            }),
                        });
                    }
                }
            }
        }

        let ref_ids = refs.iter().map(|r| r.ref_id.clone()).collect();
        Self {
            packet_id: format!("context:{}", packet.context_id),
            generated_at: packet.generated_at.clone(),
            query_id: Some(packet.query_id.clone()),
            audience_role,
            summary: summary.into(),
            refs,
            sections: vec![ContextPacketSection {
                title: "LifeGraph recall".into(),
                authority: ContextAuthority::LifeGraphEvidence,
                ref_ids,
                text: None,
            }],
            policy_notes: vec![
                "Muninn refs are continuity handles, not confirmed LifeGraph truth.".into(),
                "LifeGraph proposed evidence requires governance before promotion.".into(),
            ],
            metadata: serde_json::json!({
                "source_context_packet": packet.context_id,
                "omitted_conflict_ids": packet.omitted_conflict_ids,
            }),
        }
    }

    pub fn from_muninn_recall(
        packet_id: impl Into<String>,
        generated_at: impl Into<String>,
        query_id: Option<String>,
        summary: impl Into<String>,
        memories: &[MuninnRecallMemory],
    ) -> Self {
        let refs: Vec<_> = memories
            .iter()
            .map(|memory| ContextRef {
                ref_id: memory.id.clone(),
                kind: ContextRefKind::MuninnEngram,
                authority: ContextAuthority::MuninnContinuity,
                summary: memory
                    .summary
                    .clone()
                    .or_else(|| memory.concept.clone())
                    .or_else(|| memory.content.clone()),
                validation_state: None,
                uri: None,
                metadata: serde_json::json!({
                    "concept": memory.concept,
                    "score": memory.score,
                    "trust": memory.trust,
                    "source": "muninn_recall",
                    "extra": memory.metadata,
                }),
            })
            .collect();
        let ref_ids = refs.iter().map(|r| r.ref_id.clone()).collect();

        Self {
            packet_id: packet_id.into(),
            generated_at: generated_at.into(),
            query_id,
            audience_role: None,
            summary: summary.into(),
            refs,
            sections: vec![ContextPacketSection {
                title: "Muninn recall".into(),
                authority: ContextAuthority::MuninnContinuity,
                ref_ids,
                text: None,
            }],
            policy_notes: vec![
                "Muninn refs are continuity memory, not confirmed LifeGraph truth.".into(),
                "Promote life-relevant claims through LifeGraph evidence/governance before treating them as structured life truth.".into(),
            ],
            metadata: serde_json::json!({
                "source": "muninn_recall",
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifeGraphToolName {
    LifeObserve,
    LifeRecall,
    LifeRecallFeedback,
    LifeCommit,
    LifeResolve,
    LifePatchPropose,
}

impl LifeGraphToolName {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LifeObserve => "life.observe",
            Self::LifeRecall => "life.recall",
            Self::LifeRecallFeedback => "life.recall.feedback",
            Self::LifeCommit => "life.commit",
            Self::LifeResolve => "life.resolve",
            Self::LifePatchPropose => "life.patch.propose",
        }
    }

    pub fn mutates_graph(&self) -> bool {
        matches!(
            self,
            Self::LifeObserve
                | Self::LifeRecallFeedback
                | Self::LifeCommit
                | Self::LifeResolve
                | Self::LifePatchPropose
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifeGraphToolSpec {
    pub name: LifeGraphToolName,
    pub tool_name: String,
    pub description: String,
    pub mutates_graph: bool,
    pub requires_operator_by_default: bool,
}

impl LifeGraphToolSpec {
    fn new(
        name: LifeGraphToolName,
        description: impl Into<String>,
        requires_operator_by_default: bool,
    ) -> Self {
        let tool_name = name.as_str().to_string();
        let mutates_graph = name.mutates_graph();
        Self {
            name,
            tool_name,
            description: description.into(),
            mutates_graph,
            requires_operator_by_default,
        }
    }
}

pub fn life_graph_tool_catalog() -> Vec<LifeGraphToolSpec> {
    vec![
        LifeGraphToolSpec::new(
            LifeGraphToolName::LifeObserve,
            "Capture a grounded observation as proposed Life Graph evidence.",
            false,
        ),
        LifeGraphToolSpec::new(
            LifeGraphToolName::LifeRecall,
            "Build an evidence-backed Life Graph retrieval context packet.",
            false,
        ),
        LifeGraphToolSpec::new(
            LifeGraphToolName::LifeRecallFeedback,
            "Record retrieval quality feedback and emit governed graph-improvement signals.",
            false,
        ),
        LifeGraphToolSpec::new(
            LifeGraphToolName::LifeCommit,
            "Promote validated evidence into durable Life Graph truth.",
            true,
        ),
        LifeGraphToolSpec::new(
            LifeGraphToolName::LifeResolve,
            "Resolve a Life Graph conflict handoff with Muninn/operator policy gates.",
            true,
        ),
        LifeGraphToolSpec::new(
            LifeGraphToolName::LifePatchPropose,
            "Propose a governed Life Graph schema, skill, tool, or policy patch.",
            true,
        ),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchKind {
    SchemaPatch,
    SkillPatch,
    ToolPatch,
    AttentionPatch,
    SystemPatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchRisk {
    Low,
    Medium,
    High,
}

impl PatchRisk {
    pub fn gate(&self) -> PatchGate {
        match self {
            Self::Low => PatchGate::SafeAutoUpdate,
            Self::Medium => PatchGate::ConfirmFirst,
            Self::High => PatchGate::ProposalOnly,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchGate {
    SafeAutoUpdate,
    ConfirmFirst,
    ProposalOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrowthSignalKind {
    ObservedNeed,
    DriftFinding,
    CapabilityGap,
    GrowthExperiment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DriftCategory {
    NaggingTiming,
    StaleFact,
    InferredGoalAsCommitment,
    ProductivityBias,
    GraphClutter,
    Overgeneralization,
    AgentConvenience,
    ToolAgencyExpansion,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrowthSignal {
    pub signal_id: String,
    pub kind: GrowthSignalKind,
    pub summary: String,
    #[serde(default)]
    pub evidence_packets: Vec<EvidencePacket>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drift_category: Option<DriftCategory>,
}

impl GrowthSignal {
    pub fn validate(&self) -> Result<(), ContractError> {
        let mut violations = Vec::new();
        require_non_empty(&mut violations, "signal_id", &self.signal_id);
        require_non_empty(&mut violations, "summary", &self.summary);
        if matches!(self.kind, GrowthSignalKind::DriftFinding) && self.drift_category.is_none() {
            violations.push("drift findings require a drift_category".into());
        }
        for (idx, packet) in self.evidence_packets.iter().enumerate() {
            if let Err(err) = packet.validate() {
                for violation in err.violations {
                    violations.push(format!("evidence_packets[{idx}].{violation}"));
                }
            }
        }
        finish_validation(violations)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrowthLoopDisposition {
    ApplyWithAudit,
    AwaitOperatorConfirmation,
    StoreProposalOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalFeedbackRating {
    Useful,
    Stale,
    Missing,
    Noisy,
    Overconfident,
    Disconnected,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalFeedbackInput {
    pub feedback_id: String,
    pub packet_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_summary: Option<String>,
    pub rating: RetrievalFeedbackRating,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default)]
    pub candidate_count: usize,
    #[serde(default)]
    pub connected_candidate_count: usize,
    #[serde(default)]
    pub missing_context_refs: Vec<String>,
    #[serde(default)]
    pub noisy_node_refs: Vec<GraphRecordRef>,
    #[serde(default)]
    pub stale_node_refs: Vec<GraphRecordRef>,
    #[serde(default)]
    pub evidence_packets: Vec<EvidencePacket>,
    /// Node id of the query-context anchor this recall was grounded in
    /// (e.g. the OpenLoop/Goal node the caller was working from). When set
    /// alongside candidate refs, `disconnected`/`missing` feedback carries an
    /// unambiguous structural remedy: bridge anchor → candidate (Slice A2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_context_ref: Option<String>,
    /// For `disconnected` feedback: node ids of the candidates that WERE
    /// relevant but not connected enough to the anchor to rank. Together with
    /// `query_context_ref` these define ready-to-apply bridge edges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connected_candidate_refs: Vec<String>,
}

impl RetrievalFeedbackInput {
    pub fn validate(&self) -> Result<(), ContractError> {
        let mut violations = Vec::new();
        require_non_empty(&mut violations, "feedback_id", &self.feedback_id);
        require_non_empty(&mut violations, "packet_id", &self.packet_id);

        if self.connected_candidate_count > self.candidate_count {
            violations
                .push("connected_candidate_count must not exceed candidate_count".to_string());
        }

        if matches!(self.rating, RetrievalFeedbackRating::Missing)
            && self.missing_context_refs.is_empty()
        {
            violations.push("missing feedback requires at least one missing_context_ref".into());
        }

        if matches!(self.rating, RetrievalFeedbackRating::Noisy) && self.noisy_node_refs.is_empty()
        {
            violations.push("noisy feedback requires at least one noisy_node_ref".into());
        }

        if matches!(self.rating, RetrievalFeedbackRating::Stale) && self.stale_node_refs.is_empty()
        {
            violations.push("stale feedback requires at least one stale_node_ref".into());
        }

        for (idx, packet) in self.evidence_packets.iter().enumerate() {
            if let Err(err) = packet.validate() {
                for violation in err.violations {
                    violations.push(format!("evidence_packets[{idx}].{violation}"));
                }
            }
        }

        finish_validation(violations)
    }

    pub fn connectivity_ratio(&self) -> Option<f32> {
        if self.candidate_count == 0 {
            None
        } else {
            Some(self.connected_candidate_count as f32 / self.candidate_count as f32)
        }
    }
}

/// `created_by` stamp carried by every bridge edge written (or proposed) by
/// the feedback-to-action loop (Autopoiesis Slice A2).
pub const FEEDBACK_EDGE_CREATED_BY: &str = "feedback-to-action";

/// A ready-to-apply living-cycle bridge edge derived from actionable
/// retrieval feedback (Autopoiesis Slice A2, lane `graph.bridge_edges`).
///
/// Always `RELATES_TO` — the neutral living-cycle vocabulary edge. The spec
/// is embedded verbatim in the patch node's `patch_json` so a ConfirmFirst
/// patch stays executable later without re-deriving anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedbackEdgeSpec {
    /// Anchor node id (the query context the recall was grounded in).
    pub from_id: String,
    /// Candidate / missing-context node id to bridge to.
    pub to_id: String,
    /// Living-cycle relationship type; always `"RELATES_TO"` for Slice A2.
    pub rel_type: String,
    /// Provenance stamp — [`FEEDBACK_EDGE_CREATED_BY`].
    pub created_by: String,
    /// The `Signal` node id of the feedback that motivated this edge.
    pub feedback_signal_id: String,
    /// ISO timestamp captured when the spec was derived.
    pub created_at: String,
}

/// Derive ready-to-apply bridge edge specs from retrieval feedback.
///
/// Only `disconnected` and `missing` feedback with BOTH a query-context
/// anchor (`query_context_ref`) and candidate node ids
/// (`connected_candidate_refs` / `missing_context_refs`) yield specs — those
/// are the cases whose structural remedy is unambiguous. Everything else
/// returns an empty vec and stays prose-only. Blank ids, self-edges, and
/// duplicate targets are dropped.
pub fn feedback_edge_specs(input: &RetrievalFeedbackInput, now_iso: &str) -> Vec<FeedbackEdgeSpec> {
    let Some(anchor) = input
        .query_context_ref
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    else {
        return Vec::new();
    };

    let targets: &[String] = match input.rating {
        RetrievalFeedbackRating::Disconnected => &input.connected_candidate_refs,
        RetrievalFeedbackRating::Missing => &input.missing_context_refs,
        _ => return Vec::new(),
    };

    let mut seen = std::collections::HashSet::new();
    targets
        .iter()
        .map(|t| t.trim())
        .filter(|t| !t.is_empty() && *t != anchor)
        .filter(|t| seen.insert(t.to_string()))
        .map(|t| FeedbackEdgeSpec {
            from_id: anchor.to_string(),
            to_id: t.to_string(),
            rel_type: "RELATES_TO".to_string(),
            created_by: FEEDBACK_EDGE_CREATED_BY.to_string(),
            feedback_signal_id: input.feedback_id.clone(),
            created_at: now_iso.to_string(),
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrowthLoopEvaluation {
    pub patch_id: String,
    pub gate: PatchGate,
    pub disposition: GrowthLoopDisposition,
    pub requires_operator: bool,
    #[serde(default)]
    pub drift_checks: Vec<DriftCategory>,
    #[serde(default)]
    pub rationale: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrowthLoopPolicy {
    #[serde(default)]
    pub always_check_drift: Vec<DriftCategory>,
}

impl Default for GrowthLoopPolicy {
    fn default() -> Self {
        Self {
            always_check_drift: vec![
                DriftCategory::NaggingTiming,
                DriftCategory::StaleFact,
                DriftCategory::InferredGoalAsCommitment,
                DriftCategory::ProductivityBias,
                DriftCategory::ToolAgencyExpansion,
            ],
        }
    }
}

impl GrowthLoopPolicy {
    pub fn evaluate_patch(
        &self,
        patch: &LifePatchProposalInput,
    ) -> Result<GrowthLoopEvaluation, ContractError> {
        validate_patch_proposal(patch)?;

        let gate = patch.risk.gate();
        let (disposition, requires_operator) = match gate {
            PatchGate::SafeAutoUpdate => (GrowthLoopDisposition::ApplyWithAudit, false),
            PatchGate::ConfirmFirst => (
                GrowthLoopDisposition::AwaitOperatorConfirmation,
                !patch.operator_approved,
            ),
            PatchGate::ProposalOnly => (GrowthLoopDisposition::StoreProposalOnly, true),
        };

        let mut rationale = vec![format!("{:?} maps to {:?}", patch.risk, gate)];
        if matches!(gate, PatchGate::ProposalOnly) {
            rationale.push("proposal-only patches are retained for explicit review".into());
        }

        Ok(GrowthLoopEvaluation {
            patch_id: patch.patch_id.clone(),
            gate,
            disposition,
            requires_operator,
            drift_checks: self.always_check_drift.clone(),
            rationale,
        })
    }

    pub fn evaluate_retrieval_feedback(
        &self,
        feedback: &RetrievalFeedbackInput,
    ) -> Result<GrowthLoopEvaluation, ContractError> {
        feedback.validate()?;

        let mut rationale = vec![format!(
            "{:?} retrieval feedback captured for {}",
            feedback.rating, feedback.packet_id
        )];
        let mut drift_checks = self.always_check_drift.clone();
        let mut requires_operator = false;
        let mut disposition = GrowthLoopDisposition::ApplyWithAudit;

        match feedback.rating {
            RetrievalFeedbackRating::Useful => {
                rationale
                    .push("positive signal reinforces current retrieval and bridge policy".into());
            }
            RetrievalFeedbackRating::Disconnected => {
                rationale.push(
                    "disconnected candidates indicate bridge/ranking improvement pressure".into(),
                );
                drift_checks.push(DriftCategory::GraphClutter);
            }
            RetrievalFeedbackRating::Missing => {
                rationale
                    .push("missing context should propose bridge or capture improvements".into());
            }
            RetrievalFeedbackRating::Noisy => {
                rationale.push(
                    "noisy context should dampen low-value hubs or over-broad bridges".into(),
                );
                drift_checks.push(DriftCategory::GraphClutter);
            }
            RetrievalFeedbackRating::Stale => {
                rationale.push("stale context should mark facts for review before reuse".into());
                drift_checks.push(DriftCategory::StaleFact);
            }
            RetrievalFeedbackRating::Overconfident => {
                rationale
                    .push("overconfident context requires confirmation before promotion".into());
                drift_checks.push(DriftCategory::InferredGoalAsCommitment);
                requires_operator = true;
                disposition = GrowthLoopDisposition::AwaitOperatorConfirmation;
            }
        }

        if let Some(ratio) = feedback.connectivity_ratio() {
            rationale.push(format!("connectivity_ratio={ratio:.2}"));
            if ratio < 0.5 {
                rationale.push(
                    "low connected-candidate ratio should propose bridge-building work".into(),
                );
                drift_checks.push(DriftCategory::GraphClutter);
            }
        }

        drift_checks.sort_by_key(|category| format!("{category:?}"));
        drift_checks.dedup();

        Ok(GrowthLoopEvaluation {
            patch_id: format!("feedback:{}", feedback.feedback_id),
            gate: if requires_operator {
                PatchGate::ConfirmFirst
            } else {
                PatchGate::SafeAutoUpdate
            },
            disposition,
            requires_operator,
            drift_checks,
            rationale,
        })
    }
}

/// A typed living-cycle edge proposed alongside a `life.observe` node write.
///
/// `rel_type` must be one of [`cypher::LIVING_CYCLE_REL_TYPES`]
/// (OWNS / SHAPES / SETS / SPAWNS / RELATES_TO). Unknown rel_types are
/// rejected before the node write. A `target_id` that matches no existing
/// node creates nothing — the miss is reported in the response envelope
/// without failing the node write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObserveEdge {
    pub rel_type: String,
    pub target_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LifeObserveInput {
    pub observation_id: String,
    pub evidence: EvidencePacket,
    #[serde(default)]
    pub proposed_graph_refs: Vec<GraphRecordRef>,
    /// Canonical agent identity that made this observation (e.g. "agent-astrid-01").
    /// Distinct from `source_membrane`, which records the transport the evidence
    /// arrived over. Defaults to None for old callers (persisted as "agent:unknown").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_by: Option<String>,
    /// Active role name of the observing agent, if any (e.g. "orchestrator").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_role: Option<String>,
    /// Optional living-cycle edges to MERGE idempotently with the node write.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<ObserveEdge>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LifeCommitInput {
    pub evidence: EvidencePacket,
    pub operator_approved: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LifeResolveInput {
    pub handoff: ConflictHandoff,
    pub resolution_summary: String,
    pub operator_approved: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LifePatchProposalInput {
    pub patch_id: String,
    pub patch_kind: PatchKind,
    pub summary: String,
    pub rationale: String,
    #[serde(default)]
    pub evidence_packets: Vec<EvidencePacket>,
    pub risk: PatchRisk,
    #[serde(default)]
    pub operator_approved: bool,
    /// Ready-to-apply bridge edge specs (Autopoiesis Slice A2). Empty for
    /// prose-only patches. When the patch is `awaiting_confirmation`, these
    /// are exactly what `life.patch.apply` executes on operator confirm.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edge_specs: Vec<FeedbackEdgeSpec>,
    /// Hotel-side `autonomy_audit` record id for the `graph.bridge_edges`
    /// lane action behind this patch. `life.patch.apply` reports the
    /// confirm/reverse outcome against this id so the lane earns (or loses)
    /// trust.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autonomy_audit_id: Option<String>,
}

/// Operator decision on an `awaiting_confirmation` patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PatchApplyDecision {
    /// Apply the embedded edge specs and report `confirmed_good`.
    Confirm,
    /// Do not apply; mark the patch rejected and report `reversed`.
    Reject,
}

/// Input for `life.patch.apply` — the confirmation actuator for
/// `awaiting_confirmation` patches (Autopoiesis Slice A2).
///
/// The operator/steward confirming the patch (a) applies the embedded edge
/// specs and (b) reports the outcome to the hotel so the
/// `graph.bridge_edges` lane earns (confirm) or demotes (reject).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifePatchApplyInput {
    pub patch_id: String,
    pub decision: PatchApplyDecision,
    /// Must be true — this tool IS the operator confirmation step.
    #[serde(default)]
    pub operator_approved: bool,
}

impl LifePatchApplyInput {
    pub fn validate(&self) -> Result<(), ContractError> {
        let mut violations = Vec::new();
        require_non_empty(&mut violations, "patch_id", &self.patch_id);
        if !self.operator_approved {
            violations.push("life.patch.apply requires operator_approved=true".into());
        }
        finish_validation(violations)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "tool", content = "input", rename_all = "snake_case")]
pub enum LifeGraphToolRequest {
    LifeObserve(LifeObserveInput),
    LifeRecall(RetrievalQuery),
    LifeRecallFeedback(RetrievalFeedbackInput),
    LifeCommit(LifeCommitInput),
    LifeResolve(LifeResolveInput),
    LifePatchPropose(LifePatchProposalInput),
}

impl LifeGraphToolRequest {
    pub fn tool_name(&self) -> LifeGraphToolName {
        match self {
            Self::LifeObserve(_) => LifeGraphToolName::LifeObserve,
            Self::LifeRecall(_) => LifeGraphToolName::LifeRecall,
            Self::LifeRecallFeedback(_) => LifeGraphToolName::LifeRecallFeedback,
            Self::LifeCommit(_) => LifeGraphToolName::LifeCommit,
            Self::LifeResolve(_) => LifeGraphToolName::LifeResolve,
            Self::LifePatchPropose(_) => LifeGraphToolName::LifePatchPropose,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerPlanTarget {
    GraphDatasource,
    Muninn,
    Operator,
    DataMemoryGraphRag,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunnerPlanStep {
    pub target: RunnerPlanTarget,
    pub action: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunnerPlan {
    pub tool_name: LifeGraphToolName,
    #[serde(default)]
    pub steps: Vec<RunnerPlanStep>,
    pub requires_operator: bool,
    #[serde(default)]
    pub blocked_reasons: Vec<String>,
}

impl RunnerPlan {
    pub fn allowed(&self) -> bool {
        !self.requires_operator && self.blocked_reasons.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerConfig {
    pub datasource_id: String,
    pub default_embedding_model: String,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            datasource_id: "life-graph".into(),
            default_embedding_model: "text-embedding-3-small".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryGraphRagRunner {
    pub config: RunnerConfig,
}

impl Default for MemoryGraphRagRunner {
    fn default() -> Self {
        Self {
            config: RunnerConfig::default(),
        }
    }
}

impl MemoryGraphRagRunner {
    pub fn new(config: RunnerConfig) -> Self {
        Self { config }
    }

    pub fn tool_catalog(&self) -> Vec<LifeGraphToolSpec> {
        life_graph_tool_catalog()
    }

    pub fn plan(&self, request: LifeGraphToolRequest) -> Result<RunnerPlan, ContractError> {
        match request {
            LifeGraphToolRequest::LifeObserve(input) => self.plan_observe(input),
            LifeGraphToolRequest::LifeRecall(query) => self.plan_recall(query),
            LifeGraphToolRequest::LifeRecallFeedback(feedback) => {
                self.plan_recall_feedback(feedback)
            }
            LifeGraphToolRequest::LifeCommit(input) => self.plan_commit(input),
            LifeGraphToolRequest::LifeResolve(input) => self.plan_resolve(input),
            LifeGraphToolRequest::LifePatchPropose(input) => self.plan_patch_propose(input),
        }
    }

    fn plan_observe(&self, input: LifeObserveInput) -> Result<RunnerPlan, ContractError> {
        let mut violations = Vec::new();
        require_non_empty(&mut violations, "observation_id", &input.observation_id);
        if let Err(err) = input.evidence.validate() {
            violations.extend(err.violations);
        }
        for (idx, edge) in input.edges.iter().enumerate() {
            if !cypher::is_living_cycle_rel_type(&edge.rel_type) {
                violations.push(format!(
                    "edges[{idx}].rel_type '{}' is not a living-cycle relation (expected one of {})",
                    edge.rel_type,
                    cypher::LIVING_CYCLE_REL_TYPES.join(", ")
                ));
            }
            require_non_empty(
                &mut violations,
                &format!("edges[{idx}].target_id"),
                &edge.target_id,
            );
        }
        finish_validation(violations)?;

        Ok(RunnerPlan {
            tool_name: LifeGraphToolName::LifeObserve,
            steps: vec![RunnerPlanStep {
                target: RunnerPlanTarget::GraphDatasource,
                action: "life.evidence.propose".into(),
                payload: serde_json::json!({
                    "datasource_id": self.config.datasource_id,
                    "observation_id": input.observation_id,
                    "evidence": input.evidence,
                    "proposed_graph_refs": input.proposed_graph_refs,
                    "observed_by": input.observed_by,
                    "observed_role": input.observed_role,
                    "edges": input.edges,
                }),
            }],
            requires_operator: false,
            blocked_reasons: Vec::new(),
        })
    }

    fn plan_recall(&self, query: RetrievalQuery) -> Result<RunnerPlan, ContractError> {
        query.validate()?;
        let max_context_packets = query.max_context_packets;

        Ok(RunnerPlan {
            tool_name: LifeGraphToolName::LifeRecall,
            steps: vec![
                RunnerPlanStep {
                    target: RunnerPlanTarget::GraphDatasource,
                    action: "life.retrieve.semantic_expand".into(),
                    payload: serde_json::json!({
                        "datasource_id": self.config.datasource_id,
                        "query": query,
                    }),
                },
                RunnerPlanStep {
                    target: RunnerPlanTarget::DataMemoryGraphRag,
                    action: "life.context.project_evidence_packet".into(),
                    payload: serde_json::json!({
                        "max_context_packets": max_context_packets,
                    }),
                },
            ],
            requires_operator: false,
            blocked_reasons: Vec::new(),
        })
    }

    fn plan_commit(&self, input: LifeCommitInput) -> Result<RunnerPlan, ContractError> {
        input.evidence.validate()?;
        let confirmed = matches!(input.evidence.validation_state, ValidationState::Confirmed);
        let mut blocked_reasons = Vec::new();
        if !input.operator_approved && !confirmed {
            blocked_reasons.push(
                "life.commit requires confirmed evidence or explicit operator approval".into(),
            );
        }
        if input.evidence.requires_muninn_handoff() {
            blocked_reasons.push("life.commit blocked while Muninn handoff is required".into());
        }

        Ok(RunnerPlan {
            tool_name: LifeGraphToolName::LifeCommit,
            steps: vec![RunnerPlanStep {
                target: RunnerPlanTarget::GraphDatasource,
                action: "life.fact.commit".into(),
                payload: serde_json::json!({
                    "datasource_id": self.config.datasource_id,
                    "evidence": input.evidence,
                }),
            }],
            requires_operator: !input.operator_approved && !confirmed,
            blocked_reasons,
        })
    }

    fn plan_recall_feedback(
        &self,
        feedback: RetrievalFeedbackInput,
    ) -> Result<RunnerPlan, ContractError> {
        let evaluation = GrowthLoopPolicy::default().evaluate_retrieval_feedback(&feedback)?;
        let mut steps = vec![RunnerPlanStep {
            target: RunnerPlanTarget::GraphDatasource,
            action: "life.recall.feedback".into(),
            payload: serde_json::json!({
                "datasource_id": self.config.datasource_id,
                "feedback": feedback,
                "growth_evaluation": evaluation,
            }),
        }];

        if matches!(
            evaluation.disposition,
            GrowthLoopDisposition::ApplyWithAudit
                | GrowthLoopDisposition::AwaitOperatorConfirmation
        ) {
            steps.push(RunnerPlanStep {
                target: RunnerPlanTarget::DataMemoryGraphRag,
                action: "life.graph.improvement_candidates".into(),
                payload: serde_json::json!({
                    "feedback_id": steps[0].payload["feedback"]["feedback_id"].clone(),
                    "growth_evaluation": evaluation,
                }),
            });
        }

        Ok(RunnerPlan {
            tool_name: LifeGraphToolName::LifeRecallFeedback,
            steps,
            requires_operator: evaluation.requires_operator,
            blocked_reasons: Vec::new(),
        })
    }

    fn plan_resolve(&self, input: LifeResolveInput) -> Result<RunnerPlan, ContractError> {
        let mut violations = Vec::new();
        require_non_empty(
            &mut violations,
            "resolution_summary",
            &input.resolution_summary,
        );
        if let Err(err) = input.handoff.validate() {
            violations.extend(err.violations);
        }
        finish_validation(violations)?;

        let mut steps = vec![RunnerPlanStep {
            target: RunnerPlanTarget::GraphDatasource,
            action: "life.conflict.resolve".into(),
            payload: serde_json::json!({
                "datasource_id": self.config.datasource_id,
                "handoff": input.handoff,
                "resolution_summary": input.resolution_summary,
            }),
        }];

        if matches!(
            input.handoff.requested_muninn_action,
            MuninnRequestedAction::TrueUp
                | MuninnRequestedAction::ContradictionReview
                | MuninnRequestedAction::TrustUpdate
                | MuninnRequestedAction::Cultivate
        ) {
            let muninn_action = match input.handoff.requested_muninn_action {
                MuninnRequestedAction::TrueUp => "memory.true_up",
                // Philote currently exposes true-up as the implemented review
                // surface. Keep the requested action in payload metadata instead
                // of routing to phantom tools.
                MuninnRequestedAction::ContradictionReview => "memory.true_up",
                MuninnRequestedAction::TrustUpdate => "memory.true_up",
                MuninnRequestedAction::Cultivate => "memory.cultivate",
                MuninnRequestedAction::None => "memory.none",
            };
            steps.push(RunnerPlanStep {
                target: RunnerPlanTarget::Muninn,
                action: muninn_action.into(),
                payload: serde_json::json!({
                    "conflict_id": input.handoff.conflict_id,
                    "muninn_engram_ids": input.handoff.muninn_engram_ids,
                    "requested_muninn_action": input.handoff.requested_muninn_action,
                    "resolution_summary": input.resolution_summary,
                }),
            });
        }

        Ok(RunnerPlan {
            tool_name: LifeGraphToolName::LifeResolve,
            steps,
            requires_operator: input.handoff.requires_operator && !input.operator_approved,
            blocked_reasons: if input.handoff.requires_operator && !input.operator_approved {
                vec!["life.resolve requires operator approval for this handoff".into()]
            } else {
                Vec::new()
            },
        })
    }

    fn plan_patch_propose(
        &self,
        input: LifePatchProposalInput,
    ) -> Result<RunnerPlan, ContractError> {
        let evaluation = GrowthLoopPolicy::default().evaluate_patch(&input)?;

        let requires_operator = evaluation.requires_operator;

        Ok(RunnerPlan {
            tool_name: LifeGraphToolName::LifePatchPropose,
            steps: vec![RunnerPlanStep {
                target: RunnerPlanTarget::GraphDatasource,
                action: "life.patch.propose".into(),
                payload: serde_json::json!({
                    "datasource_id": self.config.datasource_id,
                    "growth_evaluation": evaluation,
                    "patch": input,
                }),
            }],
            requires_operator,
            blocked_reasons: if requires_operator {
                vec!["Life Graph patch proposal requires operator review for its risk tier".into()]
            } else {
                Vec::new()
            },
        })
    }
}

fn validate_patch_proposal(input: &LifePatchProposalInput) -> Result<(), ContractError> {
    let mut violations = Vec::new();
    require_non_empty(&mut violations, "patch_id", &input.patch_id);
    require_non_empty(&mut violations, "summary", &input.summary);
    require_non_empty(&mut violations, "rationale", &input.rationale);
    if input.evidence_packets.is_empty() {
        violations.push("life.patch.propose requires at least one evidence packet".into());
    }
    for (idx, packet) in input.evidence_packets.iter().enumerate() {
        if let Err(err) = packet.validate() {
            for violation in err.violations {
                violations.push(format!("evidence_packets[{idx}].{violation}"));
            }
        }
    }
    finish_validation(violations)
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

        query.semantic_pivots[0].embedding_dims = 384;
        let err = query
            .validate()
            .expect_err("query should reject wrong embedding dims");
        assert!(
            err.violations
                .iter()
                .any(|v| v.contains("embedding_dims must be 768"))
        );
    }

    #[test]
    fn retrieval_query_accepts_text_only_auto_embed_shape() {
        let query: RetrievalQuery = serde_json::from_value(serde_json::json!({
            "query_id": "retrieval:text-only:20260703",
            "query_text": "What open loops need attention today?"
        }))
        .expect("text-only recall query should deserialize with defaults");

        assert_eq!(query.strategy, RetrievalStrategy::MemoryAwareGraphRank);
        assert!(query.semantic_pivots.is_empty());
        assert_eq!(query.expansion_policy, ExpansionPolicy::default());
        assert_eq!(query.ranking_weights, RankingWeights::default());
        assert_eq!(query.max_context_packets, 6);

        let plan = MemoryGraphRagRunner::default()
            .plan(LifeGraphToolRequest::LifeRecall(query))
            .expect("text-only recall query should plan for provider auto-embedding");

        assert!(plan.allowed());
        assert_eq!(plan.tool_name, LifeGraphToolName::LifeRecall);
    }

    #[test]
    fn ranking_weights_deserialize_legacy_five_field_payload() {
        // Pre-role_relevance wire payloads must keep deserializing; the new
        // bonus weight defaults in.
        let weights: RankingWeights = serde_json::from_value(serde_json::json!({
            "semantic_similarity": 0.45,
            "graph_specificity": 0.2,
            "recency": 0.1,
            "confirmation": 0.15,
            "active_commitment": 0.1
        }))
        .expect("legacy five-field ranking_weights should deserialize");
        assert!((weights.role_relevance - 0.15).abs() < f32::EPSILON);
    }

    #[test]
    fn retrieval_query_rejects_out_of_range_role_relevance() {
        let mut query = retrieval_query();
        query.ranking_weights.role_relevance = 1.5;
        let err = query
            .validate()
            .expect_err("role_relevance above 1.0 should be rejected");
        assert!(
            err.violations
                .iter()
                .any(|v| v.contains("ranking_weights.role_relevance"))
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

    #[test]
    fn cross_agent_context_packet_preserves_authority_boundaries() {
        let mut evidence = evidence_packet();
        evidence.validation_state = ValidationState::Confirmed;
        evidence.adjudication_status = AdjudicationStatus::NotNeeded;

        let retrieval_packet = RetrievalContextPacket {
            context_id: "context:beacon:open-loops".into(),
            query_id: "retrieval:open-loops:20260604".into(),
            strategy: RetrievalStrategy::MemoryAwareGraphRank,
            ranked_packets: vec![RankedEvidencePacket {
                packet: evidence,
                score: 0.91,
                matched_policy_filters: vec![PolicyFilter::RequireEvidence],
                evidence_path: vec![graph_ref("life:open_loop:rowing-follow-up")],
            }],
            omitted_conflict_ids: vec![],
            token_budget: 2_000,
            generated_at: "2026-06-04T19:36:00Z".into(),
        };

        let packet = ContextPacket::from_lifegraph_retrieval(
            &retrieval_packet,
            "Beacon open-loop context",
            Some("beacon".into()),
        );

        packet.validate().expect("context packet should be valid");

        let muninn_ref = packet
            .refs
            .iter()
            .find(|r| matches!(r.kind, ContextRefKind::MuninnEngram))
            .expect("Muninn engram ref should be projected");
        assert_eq!(muninn_ref.authority, ContextAuthority::MuninnContinuity);

        let life_ref = packet
            .refs
            .iter()
            .find(|r| {
                matches!(r.kind, ContextRefKind::LifeGraphNode)
                    && r.ref_id == "life:open_loop:rowing-follow-up"
            })
            .expect("LifeGraph node ref should be projected");
        assert_eq!(life_ref.authority, ContextAuthority::LifeGraphTruth);

        let json = serde_json::to_value(packet).expect("serialize context packet");
        assert_eq!(json["refs"][0]["kind"], "life_graph_retrieval_packet");
        assert!(
            json["policy_notes"][0]
                .as_str()
                .unwrap()
                .contains("continuity handles")
        );
    }

    #[test]
    fn context_ref_rejects_muninn_as_lifegraph_truth() {
        let context_ref = ContextRef {
            ref_id: "01KTA0Z5K942VAP9NPAAABH20T".into(),
            kind: ContextRefKind::MuninnEngram,
            authority: ContextAuthority::LifeGraphTruth,
            summary: Some("This should remain a continuity handle.".into()),
            validation_state: Some(ValidationState::Confirmed),
            uri: None,
            metadata: serde_json::json!({}),
        };

        let err = context_ref
            .validate()
            .expect_err("Muninn refs must not claim LifeGraph truth authority");
        assert!(err.violations.iter().any(|v| v.contains("cannot claim")));
    }

    #[test]
    fn muninn_recall_context_packet_uses_continuity_authority() {
        let memories = vec![MuninnRecallMemory {
            id: "01KW5TQST4EMBXCMAA0XNDWHDZ".into(),
            concept: Some("cross-agent-knowledge-architecture".into()),
            summary: Some("Keep native Muninn MCP private.".into()),
            content: Some("Decision: native Muninn MCP stays loopback/private.".into()),
            score: Some(0.91),
            trust: Some("inferred".into()),
            metadata: serde_json::json!({"state": "active"}),
        }];

        let packet = ContextPacket::from_muninn_recall(
            "context:muninn:test",
            "2026-06-30T12:00:00Z",
            Some("muninn:recall:test".into()),
            "Muninn recall for credential UAT",
            &memories,
        );

        packet
            .validate()
            .expect("Muninn context packet should validate");
        assert_eq!(packet.refs[0].kind, ContextRefKind::MuninnEngram);
        assert_eq!(packet.refs[0].authority, ContextAuthority::MuninnContinuity);
        assert_eq!(
            packet.sections[0].authority,
            ContextAuthority::MuninnContinuity
        );

        let json = serde_json::to_value(packet).expect("serialize context packet");
        assert_eq!(json["refs"][0]["authority"], "muninn_continuity");
        assert!(
            json["policy_notes"][0]
                .as_str()
                .unwrap()
                .contains("continuity memory")
        );
    }

    #[test]
    fn runner_catalog_exposes_first_life_graph_tool_surface() {
        let runner = MemoryGraphRagRunner::default();
        let catalog = runner.tool_catalog();
        let tool_names: Vec<_> = catalog.iter().map(|tool| tool.tool_name.as_str()).collect();

        assert_eq!(
            tool_names,
            vec![
                "life.observe",
                "life.recall",
                "life.recall.feedback",
                "life.commit",
                "life.resolve",
                "life.patch.propose"
            ]
        );
        assert!(
            !catalog
                .iter()
                .find(|tool| tool.tool_name == "life.recall")
                .expect("recall spec")
                .mutates_graph
        );
        assert!(
            catalog
                .iter()
                .find(|tool| tool.tool_name == "life.recall.feedback")
                .expect("recall feedback spec")
                .mutates_graph
        );
    }

    #[test]
    fn observe_input_roundtrips_provenance_and_edges() {
        let input = LifeObserveInput {
            observation_id: "obs:prov:1".into(),
            evidence: evidence_packet(),
            proposed_graph_refs: vec![],
            observed_by: Some("agent-beacon-01".into()),
            observed_role: Some("chief_of_staff".into()),
            edges: vec![ObserveEdge {
                rel_type: "OWNS".into(),
                target_id: "life:role:chief-of-staff".into(),
            }],
        };

        let json = serde_json::to_value(&input).expect("serialize observe input");
        assert_eq!(json["observed_by"], "agent-beacon-01");
        assert_eq!(json["observed_role"], "chief_of_staff");
        assert_eq!(json["edges"][0]["rel_type"], "OWNS");
        assert_eq!(json["edges"][0]["target_id"], "life:role:chief-of-staff");

        let back: LifeObserveInput = serde_json::from_value(json).expect("roundtrip");
        assert_eq!(back, input);
    }

    #[test]
    fn observe_input_is_backward_compatible_with_old_callers() {
        // Payload shaped exactly like a pre-provenance caller.
        let legacy = serde_json::json!({
            "observation_id": "obs:legacy:1",
            "evidence": serde_json::to_value(evidence_packet()).unwrap(),
            "proposed_graph_refs": [],
        });

        let parsed: LifeObserveInput =
            serde_json::from_value(legacy).expect("legacy payload must still parse");
        assert_eq!(parsed.observed_by, None);
        assert_eq!(parsed.observed_role, None);
        assert!(parsed.edges.is_empty());
    }

    #[test]
    fn runner_observe_plan_carries_provenance_and_edges() {
        let runner = MemoryGraphRagRunner::default();
        let plan = runner
            .plan(LifeGraphToolRequest::LifeObserve(LifeObserveInput {
                observation_id: "obs:prov:2".into(),
                evidence: evidence_packet(),
                proposed_graph_refs: vec![],
                observed_by: Some("agent-astrid-01".into()),
                observed_role: None,
                edges: vec![ObserveEdge {
                    rel_type: "RELATES_TO".into(),
                    target_id: "life:role:librarian".into(),
                }],
            }))
            .expect("observe should plan");

        assert!(plan.allowed());
        assert_eq!(plan.steps[0].payload["observed_by"], "agent-astrid-01");
        assert_eq!(plan.steps[0].payload["edges"][0]["rel_type"], "RELATES_TO");
    }

    #[test]
    fn runner_observe_plan_rejects_unknown_rel_type() {
        let runner = MemoryGraphRagRunner::default();
        let err = runner
            .plan(LifeGraphToolRequest::LifeObserve(LifeObserveInput {
                observation_id: "obs:prov:3".into(),
                evidence: evidence_packet(),
                proposed_graph_refs: vec![],
                observed_by: Some("agent-astrid-01".into()),
                observed_role: None,
                edges: vec![ObserveEdge {
                    rel_type: "DESTROYS".into(),
                    target_id: "life:role:librarian".into(),
                }],
            }))
            .expect_err("unknown rel_type must be rejected");

        assert!(err.to_string().contains("DESTROYS"));
    }

    #[test]
    fn runner_recall_builds_graph_then_context_projection_plan() {
        let runner = MemoryGraphRagRunner::default();
        let plan = runner
            .plan(LifeGraphToolRequest::LifeRecall(retrieval_query()))
            .expect("recall should plan");

        assert_eq!(plan.tool_name, LifeGraphToolName::LifeRecall);
        assert!(plan.allowed());
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].target, RunnerPlanTarget::GraphDatasource);
        assert_eq!(plan.steps[0].action, "life.retrieve.semantic_expand");
        assert_eq!(plan.steps[1].target, RunnerPlanTarget::DataMemoryGraphRag);
        assert_eq!(plan.steps[1].payload["max_context_packets"], 6);
    }

    #[test]
    fn runner_recall_feedback_records_reward_signal_and_improvement_candidates() {
        let runner = MemoryGraphRagRunner::default();
        let feedback = RetrievalFeedbackInput {
            feedback_id: "feedback:lifegraph:1".into(),
            packet_id: "packet:lifegraph:1".into(),
            query_summary: Some("LifeGraph self-improvement".into()),
            rating: RetrievalFeedbackRating::Disconnected,
            note: Some("Returned facts were relevant but not connected to current goals.".into()),
            candidate_count: 6,
            connected_candidate_count: 2,
            missing_context_refs: vec![],
            noisy_node_refs: vec![],
            stale_node_refs: vec![],
            evidence_packets: vec![evidence_packet()],
            query_context_ref: None,
            connected_candidate_refs: vec![],
        };

        let plan = runner
            .plan(LifeGraphToolRequest::LifeRecallFeedback(feedback))
            .expect("feedback should plan");

        assert_eq!(plan.tool_name, LifeGraphToolName::LifeRecallFeedback);
        assert!(plan.allowed());
        assert_eq!(plan.steps[0].action, "life.recall.feedback");
        assert_eq!(
            plan.steps[0].payload["growth_evaluation"]["disposition"],
            "apply_with_audit"
        );
        assert_eq!(
            plan.steps[0].payload["growth_evaluation"]["gate"],
            "safe_auto_update"
        );
        assert_eq!(plan.steps[1].action, "life.graph.improvement_candidates");
        assert!(
            plan.steps[0].payload["growth_evaluation"]["rationale"]
                .as_array()
                .expect("rationale array")
                .iter()
                .any(|entry| entry
                    .as_str()
                    .unwrap_or_default()
                    .contains("connectivity_ratio=0.33"))
        );
    }

    #[test]
    fn runner_recall_feedback_requires_confirmation_when_overconfident() {
        let runner = MemoryGraphRagRunner::default();
        let feedback = RetrievalFeedbackInput {
            feedback_id: "feedback:lifegraph:overconfident".into(),
            packet_id: "packet:lifegraph:2".into(),
            query_summary: Some("Infer commitments".into()),
            rating: RetrievalFeedbackRating::Overconfident,
            note: Some("The packet presented an inferred goal as if it were confirmed.".into()),
            candidate_count: 3,
            connected_candidate_count: 3,
            missing_context_refs: vec![],
            noisy_node_refs: vec![],
            stale_node_refs: vec![],
            evidence_packets: vec![evidence_packet()],
            query_context_ref: None,
            connected_candidate_refs: vec![],
        };

        let plan = runner
            .plan(LifeGraphToolRequest::LifeRecallFeedback(feedback))
            .expect("feedback should plan");

        assert!(!plan.allowed());
        assert!(plan.requires_operator);
        assert_eq!(
            plan.steps[0].payload["growth_evaluation"]["disposition"],
            "await_operator_confirmation"
        );
    }

    #[test]
    fn runner_commit_blocks_unconfirmed_evidence_without_operator() {
        let runner = MemoryGraphRagRunner::default();
        let plan = runner
            .plan(LifeGraphToolRequest::LifeCommit(LifeCommitInput {
                evidence: evidence_packet(),
                operator_approved: false,
            }))
            .expect("commit should produce blocked plan");

        assert_eq!(plan.tool_name, LifeGraphToolName::LifeCommit);
        assert!(!plan.allowed());
        assert!(
            plan.blocked_reasons
                .iter()
                .any(|reason| reason.contains("confirmed evidence"))
        );
    }

    #[test]
    fn runner_commit_allows_confirmed_evidence() {
        let runner = MemoryGraphRagRunner::default();
        let mut evidence = evidence_packet();
        evidence.validation_state = ValidationState::Confirmed;
        evidence.adjudication_status = AdjudicationStatus::NotNeeded;

        let plan = runner
            .plan(LifeGraphToolRequest::LifeCommit(LifeCommitInput {
                evidence,
                operator_approved: false,
            }))
            .expect("confirmed evidence should plan");

        assert!(plan.allowed());
        assert_eq!(plan.steps[0].action, "life.fact.commit");
    }

    #[test]
    fn runner_resolve_adds_muninn_true_up_step() {
        let runner = MemoryGraphRagRunner::default();
        let handoff = ConflictHandoff {
            handoff_id: "handoff:conflict:3".into(),
            conflict_id: "conflict:open-loop:resolved-vs-active".into(),
            finding_type: ConflictFindingType::DirectContradiction,
            summary: "Graph and Muninn disagree about open loop state.".into(),
            graph_fact_refs: vec![graph_ref("life:open_loop:rowing-follow-up")],
            evidence_packets: vec![evidence_packet()],
            muninn_engram_ids: vec!["01KTA0Z5K942VAP9NPAAABH20T".into()],
            recommended_owner: HandoffOwner::SharedGate,
            requested_muninn_action: MuninnRequestedAction::TrueUp,
            risk: ConflictRisk::Medium,
            requires_operator: false,
            status: ConflictHandoffStatus::Open,
            metadata: serde_json::json!({}),
        };

        let plan = runner
            .plan(LifeGraphToolRequest::LifeResolve(LifeResolveInput {
                handoff,
                resolution_summary: "Keep graph fact proposed until Muninn true-up completes."
                    .into(),
                operator_approved: false,
            }))
            .expect("resolve should plan");

        assert!(plan.allowed());
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[1].target, RunnerPlanTarget::Muninn);
        assert_eq!(plan.steps[1].action, "memory.true_up");
    }

    #[test]
    fn runner_resolve_routes_contradiction_review_to_true_up_surface() {
        let runner = MemoryGraphRagRunner::default();
        let handoff = ConflictHandoff {
            handoff_id: "handoff:conflict:4".into(),
            conflict_id: "conflict:preference:stale".into(),
            finding_type: ConflictFindingType::DirectContradiction,
            summary: "Muninn and LifeGraph disagree about the active preference.".into(),
            graph_fact_refs: vec![graph_ref("life:preference:focus-mode")],
            evidence_packets: vec![evidence_packet()],
            muninn_engram_ids: vec!["01KW5TZQ0PBBHRFS23JQDZEDFV".into()],
            recommended_owner: HandoffOwner::SharedGate,
            requested_muninn_action: MuninnRequestedAction::ContradictionReview,
            risk: ConflictRisk::Medium,
            requires_operator: false,
            status: ConflictHandoffStatus::Open,
            metadata: serde_json::json!({}),
        };

        let plan = runner
            .plan(LifeGraphToolRequest::LifeResolve(LifeResolveInput {
                handoff,
                resolution_summary: "Run true-up before any promotion.".into(),
                operator_approved: false,
            }))
            .expect("resolve should plan");

        assert!(plan.allowed());
        assert_eq!(plan.steps[1].target, RunnerPlanTarget::Muninn);
        assert_eq!(plan.steps[1].action, "memory.true_up");
        assert_eq!(
            plan.steps[1].payload["requested_muninn_action"],
            "contradiction_review"
        );
    }

    #[test]
    fn runner_patch_propose_gates_high_risk_patch() {
        let runner = MemoryGraphRagRunner::default();
        let plan = runner
            .plan(LifeGraphToolRequest::LifePatchPropose(
                LifePatchProposalInput {
                    patch_id: "patch:identity-merge-policy".into(),
                    patch_kind: PatchKind::SchemaPatch,
                    summary: "Tighten identity merge policy.".into(),
                    rationale: "Nickname-only merges are too risky.".into(),
                    evidence_packets: vec![evidence_packet()],
                    risk: PatchRisk::High,
                    operator_approved: false,
                    edge_specs: vec![],
                    autonomy_audit_id: None,
                },
            ))
            .expect("patch proposal should plan");

        assert!(!plan.allowed());
        assert!(plan.requires_operator);
        assert!(
            plan.blocked_reasons
                .iter()
                .any(|reason| reason.contains("operator review"))
        );
        assert_eq!(
            plan.steps[0].payload["growth_evaluation"]["gate"],
            "proposal_only"
        );
    }

    #[test]
    fn patch_proposal_defaults_missing_operator_approval_to_false() {
        let input: LifePatchProposalInput = serde_json::from_value(serde_json::json!({
            "patch_id": "patch:ranking-bridge",
            "patch_kind": "attention_patch",
            "summary": "Tune ranking bridge policy.",
            "rationale": "Recent feedback says important commitments are disconnected.",
            "evidence_packets": [evidence_packet()],
            "risk": "medium"
        }))
        .expect("operator_approved should default to false");

        assert!(!input.operator_approved);

        let plan = MemoryGraphRagRunner::default()
            .plan(LifeGraphToolRequest::LifePatchPropose(input))
            .expect("medium-risk patch should plan");

        assert!(!plan.allowed());
        assert!(plan.requires_operator);
    }

    #[test]
    fn growth_policy_allows_low_risk_patch_with_audit() {
        let policy = GrowthLoopPolicy::default();
        let evaluation = policy
            .evaluate_patch(&LifePatchProposalInput {
                patch_id: "patch:stale-marker".into(),
                patch_kind: PatchKind::SystemPatch,
                summary: "Attach stale marker to old open loop.".into(),
                rationale: "Evidence indicates the loop has not been touched recently.".into(),
                evidence_packets: vec![evidence_packet()],
                risk: PatchRisk::Low,
                operator_approved: false,
                edge_specs: vec![],
                autonomy_audit_id: None,
            })
            .expect("low-risk patch should evaluate");

        assert_eq!(evaluation.gate, PatchGate::SafeAutoUpdate);
        assert_eq!(
            evaluation.disposition,
            GrowthLoopDisposition::ApplyWithAudit
        );
        assert!(!evaluation.requires_operator);
        assert!(evaluation.drift_checks.contains(&DriftCategory::StaleFact));
    }

    #[test]
    fn growth_policy_requires_confirmation_for_medium_risk_patch() {
        let policy = GrowthLoopPolicy::default();
        let evaluation = policy
            .evaluate_patch(&LifePatchProposalInput {
                patch_id: "patch:habit-cadence".into(),
                patch_kind: PatchKind::AttentionPatch,
                summary: "Infer a possible habit cadence.".into(),
                rationale: "Repeated evidence suggests a recurring weekly follow-up pattern."
                    .into(),
                evidence_packets: vec![evidence_packet()],
                risk: PatchRisk::Medium,
                operator_approved: false,
                edge_specs: vec![],
                autonomy_audit_id: None,
            })
            .expect("medium-risk patch should evaluate");

        assert_eq!(evaluation.gate, PatchGate::ConfirmFirst);
        assert_eq!(
            evaluation.disposition,
            GrowthLoopDisposition::AwaitOperatorConfirmation
        );
        assert!(evaluation.requires_operator);
    }

    #[test]
    fn drift_findings_require_drift_category() {
        let signal = GrowthSignal {
            signal_id: "growth:drift:1".into(),
            kind: GrowthSignalKind::DriftFinding,
            summary: "A reminder pattern may be getting intrusive.".into(),
            evidence_packets: vec![evidence_packet()],
            drift_category: None,
        };

        let err = signal
            .validate()
            .expect_err("drift finding requires category");
        assert!(
            err.violations
                .iter()
                .any(|violation| violation.contains("drift_category"))
        );
    }

    // ── Feedback-to-action edge specs (Autopoiesis Slice A2) ─────────────────

    fn bridge_feedback(rating: RetrievalFeedbackRating) -> RetrievalFeedbackInput {
        RetrievalFeedbackInput {
            feedback_id: "feedback:recall:a2".into(),
            packet_id: "packet:recall:a2".into(),
            query_summary: Some("open loops for the graph project".into()),
            rating,
            note: None,
            candidate_count: 4,
            connected_candidate_count: 1,
            missing_context_refs: vec!["life:goal:graph".into(), "life:goal:graph".into()],
            noisy_node_refs: vec![],
            stale_node_refs: vec![],
            evidence_packets: vec![],
            query_context_ref: Some("life:open_loop:anchor".into()),
            connected_candidate_refs: vec![
                "life:project:phi".into(),
                " ".into(),
                "life:open_loop:anchor".into(),
                "life:project:phi".into(),
                "life:decision:d1".into(),
            ],
        }
    }

    #[test]
    fn feedback_edge_specs_derived_for_disconnected_and_missing_only() {
        let now = "2026-07-07T00:00:00Z";

        // Disconnected: anchor → connected_candidate_refs, with blank ids,
        // self-edges, and duplicates dropped.
        let specs =
            feedback_edge_specs(&bridge_feedback(RetrievalFeedbackRating::Disconnected), now);
        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].from_id, "life:open_loop:anchor");
        assert_eq!(specs[0].to_id, "life:project:phi");
        assert_eq!(specs[1].to_id, "life:decision:d1");
        for spec in &specs {
            assert_eq!(spec.rel_type, "RELATES_TO");
            assert_eq!(spec.created_by, FEEDBACK_EDGE_CREATED_BY);
            assert_eq!(spec.feedback_signal_id, "feedback:recall:a2");
            assert_eq!(spec.created_at, now);
        }

        // Missing: anchor → missing_context_refs (deduped).
        let specs = feedback_edge_specs(&bridge_feedback(RetrievalFeedbackRating::Missing), now);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].to_id, "life:goal:graph");

        // Every other rating stays prose-only regardless of refs.
        for rating in [
            RetrievalFeedbackRating::Useful,
            RetrievalFeedbackRating::Noisy,
            RetrievalFeedbackRating::Stale,
            RetrievalFeedbackRating::Overconfident,
        ] {
            let specs = feedback_edge_specs(&bridge_feedback(rating.clone()), now);
            assert!(specs.is_empty(), "{rating:?} must not derive edge specs");
        }

        // No anchor → no specs, even for disconnected feedback.
        let mut anchorless = bridge_feedback(RetrievalFeedbackRating::Disconnected);
        anchorless.query_context_ref = None;
        assert!(feedback_edge_specs(&anchorless, now).is_empty());
        anchorless.query_context_ref = Some("   ".into());
        assert!(feedback_edge_specs(&anchorless, now).is_empty());

        // Anchor but no candidates → no specs.
        let mut no_candidates = bridge_feedback(RetrievalFeedbackRating::Disconnected);
        no_candidates.connected_candidate_refs = vec![];
        assert!(feedback_edge_specs(&no_candidates, now).is_empty());
    }

    #[test]
    fn retrieval_feedback_input_serde_back_compat() {
        // A sender built before Slice A2 (no query_context_ref /
        // connected_candidate_refs) must still deserialize.
        let input: RetrievalFeedbackInput = serde_json::from_value(serde_json::json!({
            "feedback_id": "feedback:old:1",
            "packet_id": "packet:old:1",
            "rating": "disconnected",
        }))
        .expect("old feedback shape must parse");
        assert_eq!(input.query_context_ref, None);
        assert!(input.connected_candidate_refs.is_empty());
        assert!(feedback_edge_specs(&input, "2026-07-07T00:00:00Z").is_empty());

        // New fields are omitted from the wire when unset (old readers are
        // untouched) and round-trip when set.
        let json = serde_json::to_value(&input).expect("serialize");
        assert!(json.get("query_context_ref").is_none());
        assert!(json.get("connected_candidate_refs").is_none());

        let full = bridge_feedback(RetrievalFeedbackRating::Disconnected);
        let json = serde_json::to_value(&full).expect("serialize");
        let back: RetrievalFeedbackInput = serde_json::from_value(json).expect("round trip");
        assert_eq!(back, full);
    }

    #[test]
    fn patch_proposal_serde_back_compat_with_edge_specs() {
        // Old patch JSON (no edge_specs / autonomy_audit_id) still parses.
        let patch: LifePatchProposalInput = serde_json::from_value(serde_json::json!({
            "patch_id": "patch:old",
            "patch_kind": "system_patch",
            "summary": "s",
            "rationale": "r",
            "risk": "low",
        }))
        .expect("old patch shape must parse");
        assert!(patch.edge_specs.is_empty());
        assert_eq!(patch.autonomy_audit_id, None);
        let json = serde_json::to_value(&patch).expect("serialize");
        assert!(json.get("edge_specs").is_none());
        assert!(json.get("autonomy_audit_id").is_none());

        // A patch carrying specs round-trips them — this is what makes an
        // awaiting_confirmation patch executable later.
        let mut patch = patch;
        patch.edge_specs = feedback_edge_specs(
            &bridge_feedback(RetrievalFeedbackRating::Disconnected),
            "2026-07-07T00:00:00Z",
        );
        patch.autonomy_audit_id = Some("autonomy:graph.bridge_edges:abc".into());
        let json = serde_json::to_string(&patch).expect("serialize");
        let back: LifePatchProposalInput = serde_json::from_str(&json).expect("round trip");
        assert_eq!(back.edge_specs, patch.edge_specs);
        assert_eq!(
            back.autonomy_audit_id.as_deref(),
            Some("autonomy:graph.bridge_edges:abc")
        );
    }

    #[test]
    fn life_patch_apply_input_requires_operator_approval() {
        let input: LifePatchApplyInput = serde_json::from_value(serde_json::json!({
            "patch_id": "patch:recall-feedback:f1",
            "decision": "confirm",
        }))
        .expect("apply input parses");
        assert_eq!(input.decision, PatchApplyDecision::Confirm);
        let err = input.validate().expect_err("operator approval required");
        assert!(
            err.violations
                .iter()
                .any(|v| v.contains("operator_approved"))
        );

        let ok = LifePatchApplyInput {
            patch_id: "patch:recall-feedback:f1".into(),
            decision: PatchApplyDecision::Reject,
            operator_approved: true,
        };
        ok.validate().expect("approved input validates");
        assert_eq!(
            serde_json::to_value(&ok).expect("serialize")["decision"],
            "reject"
        );

        let blank = LifePatchApplyInput {
            patch_id: "  ".into(),
            decision: PatchApplyDecision::Confirm,
            operator_approved: true,
        };
        assert!(blank.validate().is_err());
    }
}
