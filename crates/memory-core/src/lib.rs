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
    RecallContext, RecallDecision, RecallMode, RecallTrigger, TurnRecallResult, evaluate_recall,
};
pub use rest_client::{MuninnConfig, MuninnRestEngine, VaultResolver, is_fleet_shared_vault};
pub use types::{
    ActivationResult, AgentId, AttentionalLens, CognitiveOutcome, Engram, EngramId, EngramRef,
    LinkKind, MemoryScope, SessionId, UserId, VaultId,
};
