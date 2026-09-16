pub mod cognitive;
pub mod engine;
pub mod null;
pub mod recall;
pub mod rest_client;
pub mod types;

// ──── Re-exports ──────────────────────────────────────────────────────────────

pub use cognitive::{
    ActivationPattern, AutonomyTier, CognitiveEngine, ContradictionResolution, ContradictionResult,
    CrossRoleOverlap, DecayAlert, IntrospectionProposal, IntrospectionReport, Pattern, TagCluster,
};
pub use engine::MemoryEngine;
pub use null::NullMemoryEngine;
pub use recall::{
    RECALL_METADATA_KEY, RecallContext, RecallDecision, RecallMode, RecallTrigger,
    TurnRecallResult, engram_recall_score, engram_relevance_band, evaluate_recall,
    retain_turn_relevant,
};
pub use rest_client::{
    MuninnConfig, MuninnRestEngine, TokenRejected, VaultResolver, is_cortex_routable_vault,
    is_fleet_shared_vault, token_rejected_vault,
};
pub use types::{
    ActivationResult, AgentId, AttentionalLens, CognitiveOutcome, Engram, EngramId, EngramRef,
    LinkKind, MemoryScope, SessionId, UserId, VaultId,
};
