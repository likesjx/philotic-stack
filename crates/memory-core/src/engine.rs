use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::recall::{RecallContext, RecallDecision, TurnRecallResult, evaluate_recall};
use crate::types::{
    ActivationResult, AttentionalLens, Engram, EngramId, EngramRef, LinkKind, MemoryScope,
};

// ──── MemoryEngine ────────────────────────────────────────────────────────────

/// Storage, retrieval, and association — the hippocampus of the system.
///
/// Any implementation that persists and recalls structured memories satisfies
/// this contract. `CognitiveEngine` extends it with reasoning *about* memory.
///
/// All operations are async — every implementation makes network calls to
/// MuninnDB (REST initially, MBP when validated).
#[async_trait]
pub trait MemoryEngine: Send + Sync {
    // ── Write ─────────────────────────────────────────────────────────────────

    /// Store an engram in a scoped vault. Lens auto-tags are applied.
    /// Returns a lightweight reference to the stored engram.
    async fn remember(
        &self,
        scope: MemoryScope,
        concept: &str,
        content: &str,
        tags: Vec<String>,
    ) -> anyhow::Result<EngramRef>;

    /// Store multiple engrams in a single round-trip.
    async fn remember_batch(
        &self,
        scope: MemoryScope,
        entries: Vec<(String, String, Vec<String>)>, // (concept, content, tags)
    ) -> anyhow::Result<Vec<EngramRef>>;

    /// Update an existing engram's content or tags.
    /// The engram's core identity (id, vault) is immutable.
    async fn evolve(
        &self,
        id: &EngramId,
        content: &str,
        tags: Option<Vec<String>>,
    ) -> anyhow::Result<EngramRef>;

    /// Mark an engram for removal or accelerated decay.
    /// Soft delete — consolidation may finalize.
    async fn forget(&self, id: &EngramId) -> anyhow::Result<()>;

    /// Immediately queue an engram for enrichment (entity extraction, summary,
    /// relationship inference). Bypasses the background batch schedule.
    /// Call after writing high-value memories (decisions, preferences, identity)
    /// in the Attend phase so enrichment happens promptly rather than on the
    /// next background sweep.
    ///
    /// Implementations that do not support enrichment may no-op safely.
    async fn retry_enrich(&self, _id: &EngramId) -> anyhow::Result<()> {
        Ok(())
    }

    // ── Read ──────────────────────────────────────────────────────────────────

    /// Retrieve the most salient engrams for a context string and scope.
    /// Lens filters and biases apply. Uses MuninnDB's ACTIVATE pipeline:
    /// full-text + vector fusion, Hebbian co-activation, predictive activation,
    /// graph traversal, and ACT-R decay scoring.
    async fn activate(
        &self,
        context: &str,
        scope: MemoryScope,
        max_results: Option<usize>,
    ) -> anyhow::Result<ActivationResult>;

    /// Direct retrieval by identifier. Bypasses activation scoring.
    /// Use when the engram ID is already known.
    async fn read(&self, id: &EngramId) -> anyhow::Result<Option<Engram>>;

    // ── Association ───────────────────────────────────────────────────────────

    /// Create a typed association between two engrams.
    /// Feeds MuninnDB's Hebbian co-activation — internal linking is handled
    /// by the backend; this surfaces it through the Philotic type system.
    async fn link(
        &self,
        from_id: &EngramId,
        to_id: &EngramId,
        kind: LinkKind,
    ) -> anyhow::Result<()>;

    /// Walk the association graph from a starting engram.
    /// Supports spreading activation patterns — retrieving one memory
    /// primes related memories.
    async fn traverse(
        &self,
        from_id: &EngramId,
        max_depth: Option<usize>,
    ) -> anyhow::Result<Vec<Engram>>;

    // ── Lens ──────────────────────────────────────────────────────────────────

    /// Apply an attentional lens — shapes retrieval bias and write conventions.
    /// Called when an agent incarnates into a role.
    async fn set_lens(&self, lens: AttentionalLens) -> anyhow::Result<()>;

    /// Inspect the currently active attentional lens.
    async fn current_lens(&self) -> anyhow::Result<Option<AttentionalLens>>;

    // ── Turn Recall Policy ───────────────────────────────────────────────────

    /// Deterministically decide whether long-term recall should run for this
    /// runtime context. This is distinct from explicit `memory.recall` tool use:
    /// it powers bounded turn-start augmentation and recovery-time continuity.
    async fn evaluate_recall(&self, context: &RecallContext) -> anyhow::Result<RecallDecision> {
        let mut merged = context.clone();
        if merged.lens.is_none() {
            merged.lens = self.current_lens().await.unwrap_or(None);
        }
        Ok(evaluate_recall(&merged))
    }

    /// Run the bounded recall flow for a runtime context. Uses the deterministic
    /// policy above to decide whether to call `activate()`, then returns both the
    /// decision metadata and any recalled engrams.
    async fn recall_for_turn(&self, context: &RecallContext) -> anyhow::Result<TurnRecallResult> {
        let decision = self.evaluate_recall(context).await?;
        if !decision.should_recall() {
            return Ok(TurnRecallResult {
                decision,
                engrams: Vec::new(),
                total: 0,
            });
        }

        let query = decision
            .query
            .clone()
            .ok_or_else(|| anyhow::anyhow!("recall decision missing query"))?;
        let activation = self
            .activate(&query, context.scope.clone(), decision.limit)
            .await?;

        Ok(TurnRecallResult {
            decision,
            engrams: activation.engrams,
            total: activation.total,
        })
    }

    // ── Subscription (Phase 5) ────────────────────────────────────────────────

    /// Register for real-time activation pushes matching a context.
    /// Maps to MuninnDB's ActivationPush subscription.
    ///
    /// Phase 5 — not available until MBP transport is validated.
    /// Implementations should return `Err` with a clear message until then.
    async fn subscribe(
        &self,
        context: &str,
        scope: MemoryScope,
    ) -> anyhow::Result<mpsc::Receiver<Engram>>;
}
