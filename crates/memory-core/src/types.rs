use serde::{Deserialize, Serialize};

// ──── Type Aliases ────────────────────────────────────────────────────────────

pub type EngramId = String;
pub type VaultId = String;
pub type AgentId = String;
pub type UserId = String;
pub type SessionId = String;

// ──── Memory Scope ────────────────────────────────────────────────────────────

/// Routes a memory operation to the appropriate MuninnDB vault.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryScope {
    /// Agent's private autobiographical memory — vault `self:{agent_id}`
    SelfOnly,
    /// Shared knowledge about the user — vault `user:{user_id}`
    SharedUser,
    /// Active working context — vault `session:{session_id}`
    Session(SessionId),
    /// Fan-out query across multiple vaults simultaneously
    CrossScope(Vec<MemoryScope>),
}

// ──── Attentional Lens ────────────────────────────────────────────────────────

/// Shapes how a role interacts with the agent's unified self vault.
/// Same underlying memory store; different retrieval and write profile.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AttentionalLens {
    /// Tags that activate engrams (retrieval include filter)
    pub include_tags: Vec<String>,
    /// Tags that suppress engrams from activation
    pub exclude_tags: Vec<String>,
    /// Weight toward recently-accessed engrams (0.0–1.0)
    pub recency_bias: f32,
    /// Weight toward frequently-accessed engrams (0.0–1.0)
    pub frequency_bias: f32,
    /// Maximum results from activate(); None means backend default
    pub max_results: Option<usize>,
    /// Tags automatically applied to engrams written under this lens
    pub auto_tags: Vec<String>,
    /// Default scope for writes when scope is not explicitly provided
    pub default_scope: Option<MemoryScope>,
}

// ──── Engram ──────────────────────────────────────────────────────────────────

/// The atomic unit of memory. Maps directly to a MuninnDB engram.
/// Memory value emerges from relational topology, not from isolated note quality.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Engram {
    pub id: EngramId,
    pub vault_id: VaultId,
    pub concept: String,
    pub content: String,
    pub tags: Vec<String>,
    pub confidence: f32,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Lightweight reference returned after a successful `remember()` call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngramRef {
    pub id: EngramId,
    pub vault_id: VaultId,
}

// ──── Activation ──────────────────────────────────────────────────────────────

/// Result of an `activate()` call — salient engrams for a given context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivationResult {
    pub engrams: Vec<Engram>,
    pub total: usize,
}

// ──── Link ────────────────────────────────────────────────────────────────────

/// Typed association between two engrams. Feeds MuninnDB Hebbian co-activation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    Related,
    Contradicts,
    Supersedes,
    Supports,
    DerivedFrom,
    Custom(String),
}

// ──── Cognitive Outcomes (Attend Step) ───────────────────────────────────────

/// Outcomes extracted from `WorkingTurn` at turn-end by the Attend step.
///
/// The Attend step filters for cognitive outcomes, not cognitive process.
/// The reasoning path stays in `WorkingTurn` and dies with the turn.
/// Only what the agent concluded, resolved, or observed about itself is
/// memory-eligible.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CognitiveOutcome {
    /// A contradiction that arose during the turn and was resolved.
    /// Store the resolution — not the deliberation path.
    ResolvedContradiction {
        description: String,
        resolution: String,
        /// IDs of engrams involved, if already stored.
        #[serde(default)]
        engram_ids: Vec<EngramId>,
    },
    /// A belief that solidified during reasoning (e.g. about user preferences).
    /// L1-eligible at turn-end even if not explicitly stated in the response.
    SolidifiedBelief {
        concept: String,
        content: String,
        confidence: f32,
    },
    /// A self-observation about cognitive patterns.
    /// Stored as a `cognitive:meta` engram.
    MetacognitiveObservation { observation: String },
    /// An approach tried and rejected, with the reason.
    /// Durable value for future turns on similar problems.
    RejectedApproach { description: String, reason: String },
}
