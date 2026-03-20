pub mod cognitive;
pub mod engine;
pub mod null;
pub mod rest_client;
pub mod types;

// ──── Re-exports ──────────────────────────────────────────────────────────────

pub use cognitive::{
    ActivationPattern, AutonomyTier, CognitiveEngine, ContradictionResolution, ContradictionResult,
    CrossRoleOverlap, DecayAlert, IntrospectionProposal, IntrospectionReport, Pattern, TagCluster,
};
pub use engine::MemoryEngine;
pub use null::NullMemoryEngine;
pub use rest_client::{MuninnConfig, MuninnRestEngine, VaultResolver};
pub use types::{
    ActivationResult, AgentId, AttentionalLens, CognitiveOutcome, Engram, EngramId, EngramRef,
    LinkKind, MemoryScope, SessionId, UserId, VaultId,
};
