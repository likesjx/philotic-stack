use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::engine::MemoryEngine;
use crate::types::{Engram, EngramId, EngramRef};

// ──── Contradiction ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContradictionResult {
    pub in_tension:  bool,
    pub resolution:  ContradictionResolution,
    pub explanation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContradictionResolution {
    Supersede { winner: EngramId },
    CoexistWithContext { context: String },
    FlagForReview,
}

// ──── Pattern ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pattern {
    pub description: String,
    pub engram_ids:  Vec<EngramId>,
    pub tags:        Vec<String>,
    pub confidence:  f32,
}

// ──── Introspection Report ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagCluster {
    pub tags:                Vec<String>,
    pub engram_count:        usize,
    pub co_occurrence_score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivationPattern {
    pub description: String,
    pub engram_ids:  Vec<EngramId>,
    pub frequency:   f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecayAlert {
    pub engram_id:       EngramId,
    pub concept:         String,
    pub activation_level: f32,
    pub last_accessed_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossRoleOverlap {
    pub role_a:           String,
    pub role_b:           String,
    pub shared_engram_ids: Vec<EngramId>,
    pub overlap_score:    f32,
}

/// A self-optimization proposal produced by the introspection skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntrospectionProposal {
    pub signal:       String,
    pub evidence:     String,
    pub proposal:     String,
    pub autonomy_tier: AutonomyTier,
}

/// Tiered autonomy — mirrors autonomic vs. deliberate cognitive processing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AutonomyTier {
    /// Reflexive, self-regulating. Self-applied and logged as `cognitive:meta`.
    Auto,
    /// Conscious but self-directed. Self-applied, user notified.
    Notify,
    /// Requires external validation. Presented to user for explicit approval.
    Propose,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntrospectionReport {
    pub tag_clusters:       Vec<TagCluster>,
    pub activation_patterns: Vec<ActivationPattern>,
    pub decay_alerts:       Vec<DecayAlert>,
    pub cross_role_overlap: Vec<CrossRoleOverlap>,
    pub proposals:          Vec<IntrospectionProposal>,
}

// ──── CognitiveEngine ─────────────────────────────────────────────────────────

/// Reasoning about memory — the prefrontal cortex to MemoryEngine's hippocampus.
///
/// Interpretation, evaluation, and regulation of memory contents: beliefs with
/// confidence, rules with conditions, contradiction detection, pattern analysis,
/// and self-observation.
///
/// Phase 4 — CognitiveEngine implementations are not required for Phases 1–3.
/// The MemoryEngine contract is sufficient for foundation, attention, and
/// consolidation work.
#[async_trait]
pub trait CognitiveEngine: MemoryEngine {
    /// Record a decision as a first-class engram with rationale, alternatives
    /// considered, and confidence. Decisions are cognitive acts, not just storage.
    async fn decide(
        &self,
        concept:      &str,
        rationale:    &str,
        alternatives: Vec<String>,
        confidence:   f32,
    ) -> anyhow::Result<EngramRef>;

    /// Retrieve currently salient cognitive rules given a context.
    /// Rules are engrams tagged `cognitive:rule` — behavioral patterns
    /// the agent has extracted from experience.
    async fn active_rules(&self, context: &str) -> anyhow::Result<Vec<Engram>>;

    /// Retrieve current beliefs with confidence levels.
    /// Beliefs are engrams tagged `cognitive:belief`.
    /// Confidence is updated via Bayesian inference through MuninnDB.
    async fn active_beliefs(&self, context: &str) -> anyhow::Result<Vec<Engram>>;

    /// Persist a new behavioral rule extracted from experience.
    async fn store_rule(&self, concept: &str, content: &str) -> anyhow::Result<EngramRef>;

    /// Persist a belief with explicit confidence tracking.
    async fn store_belief(
        &self,
        concept:    &str,
        content:    &str,
        confidence: f32,
    ) -> anyhow::Result<EngramRef>;

    /// Evaluate whether two engrams are in tension and produce a resolution.
    /// Leverages MuninnDB contradiction detection.
    async fn contradict(
        &self,
        engram_a: &EngramId,
        engram_b: &EngramId,
    ) -> anyhow::Result<ContradictionResult>;

    /// Analyze a set of engrams for recurring structures — co-occurring tags,
    /// temporal sequences, causal chains.
    /// Powers quiet-hours consolidation and the introspection skill.
    async fn extract_patterns(
        &self,
        engram_ids: &[EngramId],
    ) -> anyhow::Result<Vec<Pattern>>;

    /// Analyze memory patterns — tag co-occurrence, activation clustering,
    /// decay curves, cross-role overlap — and produce a structured report.
    /// The metacognitive operation. Powers the introspection skill on the
    /// orchestrator role.
    async fn introspect(&self) -> anyhow::Result<IntrospectionReport>;
}
