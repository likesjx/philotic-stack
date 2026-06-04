//! Life Graph OS MemoryGraphRAG contract types.
//!
//! This crate owns Life Graph-specific evidence and conflict payloads while
//! keeping `graph-datasource` generic. Runtime adapters can serialize these
//! contracts into graph writes, context packets, or Muninn true-up requests.

pub mod cypher;

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifeGraphToolName {
    LifeObserve,
    LifeRecall,
    LifeCommit,
    LifeResolve,
    LifePatchPropose,
}

impl LifeGraphToolName {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LifeObserve => "life.observe",
            Self::LifeRecall => "life.recall",
            Self::LifeCommit => "life.commit",
            Self::LifeResolve => "life.resolve",
            Self::LifePatchPropose => "life.patch.propose",
        }
    }

    pub fn mutates_graph(&self) -> bool {
        matches!(
            self,
            Self::LifeObserve | Self::LifeCommit | Self::LifeResolve | Self::LifePatchPropose
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LifeObserveInput {
    pub observation_id: String,
    pub evidence: EvidencePacket,
    #[serde(default)]
    pub proposed_graph_refs: Vec<GraphRecordRef>,
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
    pub operator_approved: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "tool", content = "input", rename_all = "snake_case")]
pub enum LifeGraphToolRequest {
    LifeObserve(LifeObserveInput),
    LifeRecall(RetrievalQuery),
    LifeCommit(LifeCommitInput),
    LifeResolve(LifeResolveInput),
    LifePatchPropose(LifePatchProposalInput),
}

impl LifeGraphToolRequest {
    pub fn tool_name(&self) -> LifeGraphToolName {
        match self {
            Self::LifeObserve(_) => LifeGraphToolName::LifeObserve,
            Self::LifeRecall(_) => LifeGraphToolName::LifeRecall,
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
                MuninnRequestedAction::ContradictionReview => "memory.contradiction_review",
                MuninnRequestedAction::TrustUpdate => "memory.trust_update",
                MuninnRequestedAction::Cultivate => "memory.cultivate",
                MuninnRequestedAction::None => "memory.none",
            };
            steps.push(RunnerPlanStep {
                target: RunnerPlanTarget::Muninn,
                action: muninn_action.into(),
                payload: serde_json::json!({
                    "conflict_id": input.handoff.conflict_id,
                    "muninn_engram_ids": input.handoff.muninn_engram_ids,
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
}
