//! S4 — promotion of durable memories into the shared `fleet_knowledge` vault.
//!
//! The judgment-heavy core of proposal S4: **which** memories deserve to become
//! fleet-wide knowledge, and **whether** a given autonomy posture may act on
//! that. Both are pure and unit-tested here; the sweep integration that fetches
//! candidates and performs the gated write (which must be live-validated before
//! it mutates the store) is the remaining wiring, consistent with how S6a's
//! tested assembler preceded its tool wiring.
//!
//! Two safety rails, because promotion writes to a vault every philote recalls
//! (S1) and reads per-agent `self_` memories:
//!
//! 1. **Conservative criterion** — only high-importance, durable-type,
//!    non-sensitive memories from a per-agent/user vault are promotable. This
//!    keeps `fleet_knowledge` high-signal rather than a dump of every memory.
//! 2. **Posture gate** — under the default [`AutonomyPosture::ProposalOnly`]
//!    promotion only *proposes* (files an audit record); it never writes. A
//!    write requires the operator to raise the `memory.hygiene` lane's posture.

#![allow(dead_code)]

use ansible_mesh_core::autonomy::AutonomyPosture;

/// Minimum importance for a memory to be considered fleet-worthy. Deliberately
/// high — fleet knowledge is the shared, always-recalled layer.
pub const PROMOTION_IMPORTANCE_FLOOR: f64 = 0.7;

/// Memory type labels durable enough to promote. Transient kinds (event,
/// observation, task, issue) stay per-agent even when important — they describe
/// a moment, not standing knowledge.
pub const PROMOTABLE_TYPES: &[&str] = &[
    "decision",
    "preference",
    "constraint",
    "identity",
    "procedure",
    "reference",
];

/// The signals used to decide whether one memory should be promoted. Sourced
/// (in the wiring step) from the memory's Muninn record.
#[derive(Debug, Clone, PartialEq)]
pub struct PromotableSignals {
    pub importance: f64,
    pub type_label: String,
    /// The vault the memory currently lives in (e.g. `self_agent-aria`,
    /// `user_likesjx`). Only per-agent/user vaults are promotion sources.
    pub source_vault: String,
    /// Operator/agent-marked sensitive — never promoted regardless of the rest.
    pub sensitive: bool,
    /// Length of the memory content; a near-empty memory is not worth promoting.
    pub content_len: usize,
}

impl PromotableSignals {
    /// Whether this memory is fleet-worthy. Conservative by construction: a
    /// `false` here keeps the memory private, which is always the safe default.
    pub fn is_promotable(&self) -> bool {
        if self.sensitive {
            return false;
        }
        if self.importance < PROMOTION_IMPORTANCE_FLOOR {
            return false;
        }
        if self.content_len < 16 {
            return false;
        }
        if !PROMOTABLE_TYPES
            .iter()
            .any(|t| t.eq_ignore_ascii_case(self.type_label.trim()))
        {
            return false;
        }
        // Source must be a per-agent or per-user vault. Already-fleet, session,
        // and unknown vaults are not promotion sources.
        self.source_vault.starts_with("self_") || self.source_vault.starts_with("user_")
    }
}

/// What the promotion pass may do for a promotable candidate under a given
/// autonomy posture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionAction {
    /// Not promotable, or posture forbids acting — do nothing.
    Skip,
    /// File a proposal (audit record) naming the candidate; do NOT write.
    Propose,
    /// Write the memory into `fleet_knowledge`, with an audit record.
    Execute,
}

/// Map (promotable?, posture) to an action. The default `ProposalOnly` posture
/// never writes; `ConfirmFirst` also only proposes (the operator confirms out
/// of band); only `AutoWithAudit` executes.
pub fn promotion_action(is_promotable: bool, posture: AutonomyPosture) -> PromotionAction {
    if !is_promotable {
        return PromotionAction::Skip;
    }
    match posture {
        AutonomyPosture::ProposalOnly | AutonomyPosture::ConfirmFirst => PromotionAction::Propose,
        AutonomyPosture::AutoWithAudit => PromotionAction::Execute,
    }
}

/// The target vault promoted memories land in — the single fleet-wide,
/// replicated, always-recalled knowledge vault (matches
/// `memory_core::MemoryScope::SharedFleet`).
pub const FLEET_KNOWLEDGE_VAULT: &str = "fleet_knowledge";

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(importance: f64, type_label: &str, vault: &str) -> PromotableSignals {
        PromotableSignals {
            importance,
            type_label: type_label.into(),
            source_vault: vault.into(),
            sensitive: false,
            content_len: 120,
        }
    }

    #[test]
    fn promotes_high_importance_durable_from_agent_or_user_vault() {
        assert!(sig(0.8, "decision", "self_agent-aria").is_promotable());
        assert!(sig(0.7, "preference", "user_likesjx").is_promotable());
        assert!(sig(0.9, "constraint", "user_likesjx").is_promotable());
    }

    #[test]
    fn rejects_transient_low_importance_or_wrong_source() {
        // transient type
        assert!(!sig(0.9, "event", "self_agent-aria").is_promotable());
        assert!(!sig(0.9, "observation", "user_likesjx").is_promotable());
        // below the importance floor
        assert!(!sig(0.5, "decision", "self_agent-aria").is_promotable());
        // already fleet / session / unknown are not promotion sources
        assert!(!sig(0.9, "decision", "fleet_knowledge").is_promotable());
        assert!(!sig(0.9, "decision", "session_abc").is_promotable());
        assert!(!sig(0.9, "decision", "default").is_promotable());
    }

    #[test]
    fn sensitive_and_empty_are_never_promoted() {
        let mut s = sig(0.9, "decision", "user_likesjx");
        s.sensitive = true;
        assert!(!s.is_promotable(), "sensitive memories must never promote");
        let mut e = sig(0.9, "decision", "user_likesjx");
        e.content_len = 4;
        assert!(
            !e.is_promotable(),
            "near-empty memories are not worth promoting"
        );
    }

    #[test]
    fn posture_gate_never_writes_below_auto() {
        // Default posture: propose only, never write.
        assert_eq!(
            promotion_action(true, AutonomyPosture::ProposalOnly),
            PromotionAction::Propose
        );
        assert_eq!(
            promotion_action(true, AutonomyPosture::ConfirmFirst),
            PromotionAction::Propose
        );
        // Only the highest posture executes the write.
        assert_eq!(
            promotion_action(true, AutonomyPosture::AutoWithAudit),
            PromotionAction::Execute
        );
        // Non-promotable is always skipped, whatever the posture.
        assert_eq!(
            promotion_action(false, AutonomyPosture::AutoWithAudit),
            PromotionAction::Skip
        );
    }
}
