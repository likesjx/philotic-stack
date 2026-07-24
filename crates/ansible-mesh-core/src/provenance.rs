//! `ProvenanceEnvelope` — one provenance contract shared across every memory
//! write plane (Muninn, intel-graph decisions, LifeGraph mutations, autonomy
//! audit records).
//!
//! Memory Transparency proposal (`docs/architecture/MEMORY_TRANSPARENCY_PROPOSAL.md`)
//! Slice M1: philotic has three memory planes with distinct authority and,
//! before this slice, three different partial vocabularies for "why does the
//! system believe this." This module defines the one shared shape. Adoption
//! on each plane's write path is tracked plane-by-plane — see the proposal's
//! M1 disposition for what actually carries the envelope today vs. what is a
//! named, deferred seam.
//!
//! Every field has a `#[serde(default)]` so a message or stored record
//! written before this slice — or by a component that has not adopted the
//! envelope yet — keeps deserializing. Absence of an envelope is never a
//! deserialize failure; it is a gap this proposal's Standing Rule 1 ("no
//! naked writes") makes visible instead.

use serde::{Deserialize, Serialize};

/// How the fact behind a memory write was established.
///
/// Mirrors Muninn's existing trust tiers (the primitive this proposal
/// generalizes fleet-wide — see Core Recommendation). Standing Rule 2:
/// `Observed` requires evidence pointers; `Inferred` and `Told` are never
/// silently upgraded, only an explicit confirmation event changes tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TrustTier {
    /// Directly observed by the writing component (a log line, a graph
    /// mutation the component itself performed, a test result).
    Observed,
    /// Derived by a component's inference — pattern match, model output,
    /// heuristic classification — not a direct observation.
    #[default]
    Inferred,
    /// Reported by an operator or another agent as a stated fact, not
    /// independently verified by the writing component.
    Told,
}

impl TrustTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            TrustTier::Observed => "observed",
            TrustTier::Inferred => "inferred",
            TrustTier::Told => "told",
        }
    }
}

/// One provenance contract for a memory write, adopted fleet-wide.
///
/// ```text
/// ProvenanceEnvelope {
///   source:   turn/event/session id that caused the write,
///   author:   agent + role (or system component name),
///   trust:    Observed | Inferred | Told,
///   evidence: pointer strings — log refs, graph node ids, PR/commit, signal ids,
///   reversal: Option<named undo path — soft-delete id, revert edge, restore point>,
/// }
/// ```
///
/// All fields carry sensible serde defaults so a partially-populated or
/// pre-M1 record still round-trips. `source` and `author` default to the
/// empty string rather than `Option` because every write DOES have a cause
/// and a component of origin — an empty string is a visible gap (grep-able),
/// not a silently-absorbed `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProvenanceEnvelope {
    /// Turn/event/session id that caused the write. Empty when the writing
    /// component has no such id available yet (a gap to close, not to hide).
    #[serde(default)]
    pub source: String,
    /// Agent + role, or system/scanner component name, that performed the
    /// write (e.g. `"philote:bjork/orchestrator"`, `"memory-hygiene-sweep"`,
    /// `"heal-dispatcher"`, `"attention-steward"`).
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub trust: TrustTier,
    /// Pointer strings: log refs, graph node ids, PR/commit shas, signal
    /// ids — whatever backs the write, in whatever form the writing
    /// component already carries as strings.
    #[serde(default)]
    pub evidence: Vec<String>,
    /// Named undo path — soft-delete id, revert edge, restore point — when
    /// one exists. `None` is honest: not every write has a reversal path
    /// yet (Standing Rule 3 names this as work, not silence).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reversal: Option<String>,
}

impl ProvenanceEnvelope {
    /// Build an envelope for a system/scanner component (no per-turn agent
    /// identity — a cron sweep, a dispatcher, a steward).
    pub fn from_component(component: impl Into<String>) -> Self {
        Self {
            source: String::new(),
            author: component.into(),
            trust: TrustTier::Inferred,
            evidence: Vec::new(),
            reversal: None,
        }
    }

    /// Build an envelope for an agent+role author.
    pub fn from_agent(agent_id: impl Into<String>, role: Option<&str>) -> Self {
        let agent_id = agent_id.into();
        let author = match role {
            Some(role) if !role.is_empty() => format!("{agent_id}/{role}"),
            _ => agent_id,
        };
        Self {
            source: String::new(),
            author,
            trust: TrustTier::Inferred,
            evidence: Vec::new(),
            reversal: None,
        }
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    pub fn with_trust(mut self, trust: TrustTier) -> Self {
        self.trust = trust;
        self
    }

    pub fn with_evidence(mut self, evidence: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.evidence = evidence.into_iter().map(Into::into).collect();
        self
    }

    pub fn push_evidence(mut self, pointer: impl Into<String>) -> Self {
        self.evidence.push(pointer.into());
        self
    }

    pub fn with_reversal(mut self, reversal: impl Into<String>) -> Self {
        self.reversal = Some(reversal.into());
        self
    }

    /// `true` when the envelope carries no more information than a fresh
    /// default — a naked write in disguise (Standing Rule 1). Callers filing
    /// `Some(envelope)` should not file this.
    pub fn is_empty_shell(&self) -> bool {
        self.source.is_empty()
            && self.author.is_empty()
            && self.evidence.is_empty()
            && self.reversal.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_serde_json() {
        let envelope = ProvenanceEnvelope::from_agent("agent-bjork-01", Some("orchestrator"))
            .with_source("turn:abc123")
            .with_trust(TrustTier::Observed)
            .with_evidence(["graph:node:doc:FOO", "pr:#229"])
            .with_reversal("soft_delete:engram:xyz");

        let json = serde_json::to_string(&envelope).expect("serialize");
        let back: ProvenanceEnvelope = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(envelope, back);
        assert_eq!(back.author, "agent-bjork-01/orchestrator");
        assert_eq!(back.trust, TrustTier::Observed);
        assert_eq!(back.evidence, vec!["graph:node:doc:FOO", "pr:#229"]);
        assert_eq!(back.reversal.as_deref(), Some("soft_delete:engram:xyz"));
    }

    #[test]
    fn defaults_to_inferred_and_no_reversal() {
        let envelope = ProvenanceEnvelope::from_component("memory-hygiene-sweep");
        assert_eq!(envelope.trust, TrustTier::Inferred);
        assert!(envelope.reversal.is_none());
        assert!(envelope.evidence.is_empty());
    }

    #[test]
    fn absent_envelope_field_does_not_break_deserialization() {
        // A stored/wire record written before this slice — no `provenance`
        // key at all — must still deserialize into a wrapper that carries
        // `Option<ProvenanceEnvelope>` as `None`.
        #[derive(Debug, Deserialize)]
        struct Wrapper {
            #[serde(default)]
            provenance: Option<ProvenanceEnvelope>,
            #[allow(dead_code)]
            other_field: String,
        }
        let legacy_json = r#"{"other_field": "unchanged"}"#;
        let wrapper: Wrapper = serde_json::from_str(legacy_json).expect("deserialize legacy");
        assert!(wrapper.provenance.is_none());
    }

    #[test]
    fn partial_envelope_json_fills_missing_fields_with_defaults() {
        // A caller that only ever populates `author` + `evidence` (e.g. an
        // older adoption of the envelope) still deserializes cleanly with
        // `trust` defaulting to `Inferred` and `reversal` to `None`.
        let partial_json = r#"{"author": "heal-dispatcher", "evidence": ["pattern:x"]}"#;
        let envelope: ProvenanceEnvelope = serde_json::from_str(partial_json).expect("deserialize");
        assert_eq!(envelope.author, "heal-dispatcher");
        assert_eq!(envelope.source, "");
        assert_eq!(envelope.trust, TrustTier::Inferred);
        assert_eq!(envelope.reversal, None);
    }

    #[test]
    fn is_empty_shell_detects_naked_default() {
        assert!(ProvenanceEnvelope::default().is_empty_shell());
        assert!(!ProvenanceEnvelope::from_component("x").is_empty_shell());
    }
}
