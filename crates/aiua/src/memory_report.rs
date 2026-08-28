//! S6a — Muninn admin observability (reporting, read-only).
//!
//! The pure, testable heart of the `memory.report` admin surface described in
//! `docs/architecture/MUNINN_MEMORY_CORE_PROPOSAL.md`. This module owns the
//! report *shape* and the *assembly* logic; fetching the live inputs (Muninn
//! status per node, session-event counts, disk headroom) is the caller's job,
//! wired in a follow-up.
//!
//! The one non-negotiable contract: **a field the fleet cannot actually source
//! is reported [`ReportField::Unavailable`] with a reason — never guessed.** An
//! admin philote confidently reporting cluster health it cannot see is worse
//! than no report (see the Beacon-confabulation precedent). Derived fields
//! (divergence, recall hit-rate) propagate `Unavailable` from any missing input
//! rather than fabricating a number.

// The report model + assembler is the tested foundation of S6a; the live
// `memory.report` IPC op + admin-gated philote tool that call it land in the
// S6a follow-up. Until then these items are exercised only by unit tests.
#![allow(dead_code)]

use ansible_mesh_core::domain::GraphDomain;
use serde::{Deserialize, Serialize};

/// Window for the recall-effectiveness event count — bounds the query so it can
/// never become a full-history scan (the DEF-080 meltdown class).
const RECALL_EVENT_WINDOW: usize = 5000;

/// Assemble the admin report from live hotel sources available in S6a today.
///
/// Currently sources **recall effectiveness** from the session-event ledger
/// (`memory_auto_recall_completed` / `_skipped`, windowed) — the metric that
/// answers "are philotes actually using memory?". Every other field is honestly
/// [`ReportField::Unavailable`]: per-node vault counts + divergence need the
/// multi-node Muninn status calls (S6a-wire follow-up), and replication
/// lag/peer/backlog need a muninndb API (S6b). Nothing is guessed.
pub fn assemble_live_memory_report(graph: &GraphDomain) -> MemoryReport {
    let recall = match (
        graph.count_recent_session_events_by_kind(
            "memory_auto_recall_completed",
            RECALL_EVENT_WINDOW,
        ),
        graph
            .count_recent_session_events_by_kind("memory_auto_recall_skipped", RECALL_EVENT_WINDOW),
    ) {
        (Ok(completed), Ok(skipped)) => Some(RecallEffectiveness {
            completed: completed as u64,
            skipped: skipped as u64,
        }),
        _ => None,
    };

    assemble_memory_report(MemoryReportInputs {
        recall,
        // Multi-node status/divergence is the S6a-wire follow-up; until then the
        // report is honest that it compared nothing rather than implying parity.
        cortex_reachable: false,
        ..Default::default()
    })
}

/// A reported value that is honest about whether it could be sourced.
///
/// Serializes as `{"status":"available","value":...,"source":...}` or
/// `{"status":"unavailable","reason":...}` so the model reading the report can
/// never mistake an absent field for a real zero.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ReportField<T> {
    Available {
        value: T,
        /// Where this value came from, e.g. `"muninn_status@cortex"`,
        /// `"session_events"`, `"statvfs"`.
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        as_of: Option<String>,
    },
    Unavailable {
        reason: String,
    },
}

impl<T> ReportField<T> {
    pub fn available(value: T, source: impl Into<String>) -> Self {
        Self::Available {
            value,
            source: source.into(),
            as_of: None,
        }
    }

    pub fn available_as_of(value: T, source: impl Into<String>, as_of: impl Into<String>) -> Self {
        Self::Available {
            value,
            source: source.into(),
            as_of: Some(as_of.into()),
        }
    }

    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable {
            reason: reason.into(),
        }
    }

    pub fn value(&self) -> Option<&T> {
        match self {
            Self::Available { value, .. } => Some(value),
            Self::Unavailable { .. } => None,
        }
    }

    pub fn is_available(&self) -> bool {
        matches!(self, Self::Available { .. })
    }
}

/// Per-node, per-vault status as returned by `muninn_status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeVaultStat {
    /// Logical node label, e.g. `"cortex"` or `"observer:mac-jane"`.
    pub node: String,
    pub vault: String,
    pub total_memories: u64,
    pub health: String,
}

/// Divergence between the Cortex and one observer for a single vault: how many
/// ids each holds that the other does not. Computed only when both nodes'
/// id-sets were actually enumerated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VaultDivergence {
    pub vault: String,
    pub observer_only: u64,
    pub cortex_only: u64,
}

/// Per-turn auto-recall effectiveness, counted from the aiua session-event
/// ledger (`memory_auto_recall_completed` / `_skipped`). These are philote turn
/// events, NOT Muninn tools — the report must source them from the ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEffectiveness {
    pub completed: u64,
    pub skipped: u64,
}

impl RecallEffectiveness {
    /// Fraction of eligible turns where auto-recall actually ran. `None` when no
    /// turns were observed (avoid a fabricated 0/0 = 0 rate).
    pub fn hit_rate(&self) -> Option<f64> {
        let total = self.completed + self.skipped;
        (total > 0).then(|| self.completed as f64 / total as f64)
    }
}

/// Host disk headroom for the node running this hotel's Muninn — the
/// ENOSPC/silent-wedge risk (DEF-078).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiskReport {
    pub used_bytes: u64,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

impl DiskReport {
    pub fn used_fraction(&self) -> Option<f64> {
        (self.total_bytes > 0).then(|| self.used_bytes as f64 / self.total_bytes as f64)
    }
}

/// The inputs a caller has managed to source. Each is `Option`: `None` means the
/// caller could not fetch it, which the assembler renders as `Unavailable` with
/// a specific reason — the honest-sourcing contract.
#[derive(Debug, Clone, Default)]
pub struct MemoryReportInputs {
    /// Per-node vault stats. Empty vec = nothing reachable.
    pub node_vault_stats: Vec<NodeVaultStat>,
    /// Divergence, only if both id-sets were enumerated (the sweep is heavy, so
    /// a caller may legitimately skip it).
    pub divergence: Option<Vec<VaultDivergence>>,
    pub recall: Option<RecallEffectiveness>,
    /// Per-agent forwarded write counts from the session-event ledger.
    pub write_counts: Option<Vec<(String, u64)>>,
    pub contradictions: Option<u64>,
    pub soft_deleted: Option<u64>,
    pub disk: Option<DiskReport>,
    /// Whether the Cortex admin endpoint was reachable at all — distinguishes
    /// "no divergence" from "could not compare".
    pub cortex_reachable: bool,
}

/// The assembled admin report. Every derived field is honest about sourcing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryReport {
    pub nodes: Vec<NodeVaultStat>,
    pub divergence: ReportField<Vec<VaultDivergence>>,
    pub recall_effectiveness: ReportField<RecallEffectiveness>,
    pub recall_hit_rate: ReportField<f64>,
    pub write_counts: ReportField<Vec<(String, u64)>>,
    pub contradictions: ReportField<u64>,
    pub soft_deleted: ReportField<u64>,
    pub disk: ReportField<DiskReport>,
    /// Fields this slice (S6a) cannot source at all — they need a muninndb API
    /// (S6b). Always reported as unavailable so the reader knows they exist and
    /// are pending, not that they are healthy.
    pub replication_lag: ReportField<String>,
    pub peer_state: ReportField<String>,
    pub replication_backlog: ReportField<String>,
}

const S6B_REASON: &str =
    "requires a muninndb replication API not exposed by any current Muninn MCP tool (proposal S6b)";

/// Assemble the admin report from whatever the caller could source, applying
/// the honest-sourcing contract: a missing input becomes `Unavailable(reason)`,
/// and derived fields propagate unavailability rather than inventing a value.
pub fn assemble_memory_report(inputs: MemoryReportInputs) -> MemoryReport {
    let divergence = match inputs.divergence {
        Some(d) => ReportField::available(d, "muninn_session_sweep"),
        None if !inputs.cortex_reachable => {
            ReportField::unavailable("Cortex admin endpoint not reachable — cannot compare nodes")
        }
        None => ReportField::unavailable(
            "divergence sweep not run this report (heavy) — request it explicitly",
        ),
    };

    let recall_hit_rate = match inputs
        .recall
        .as_ref()
        .and_then(RecallEffectiveness::hit_rate)
    {
        Some(rate) => ReportField::available(rate, "session_events"),
        None => ReportField::unavailable(match inputs.recall {
            Some(_) => "no auto-recall turns observed in window",
            None => "session-event ledger not consulted",
        }),
    };

    let recall_effectiveness = match inputs.recall {
        Some(r) => ReportField::available(r, "session_events"),
        None => ReportField::unavailable("session-event ledger not consulted"),
    };

    MemoryReport {
        nodes: inputs.node_vault_stats,
        divergence,
        recall_effectiveness,
        recall_hit_rate,
        write_counts: match inputs.write_counts {
            Some(w) => ReportField::available(w, "session_events"),
            None => ReportField::unavailable("session-event ledger not consulted"),
        },
        contradictions: match inputs.contradictions {
            Some(c) => ReportField::available(c, "muninn_contradictions"),
            None => ReportField::unavailable("muninn_contradictions not queried"),
        },
        soft_deleted: match inputs.soft_deleted {
            Some(s) => ReportField::available(s, "muninn_list_deleted"),
            None => ReportField::unavailable("muninn_list_deleted not queried"),
        },
        disk: match inputs.disk {
            Some(d) => ReportField::available(d, "statvfs"),
            None => ReportField::unavailable("disk stat unavailable on this host"),
        },
        replication_lag: ReportField::unavailable(S6B_REASON),
        peer_state: ReportField::unavailable(S6B_REASON),
        replication_backlog: ReportField::unavailable(S6B_REASON),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cortex(vault: &str, n: u64) -> NodeVaultStat {
        NodeVaultStat {
            node: "cortex".into(),
            vault: vault.into(),
            total_memories: n,
            health: "good".into(),
        }
    }

    #[test]
    fn missing_inputs_are_unavailable_never_zero() {
        // The whole point: an empty report must NOT read as a healthy zero.
        let report = assemble_memory_report(MemoryReportInputs::default());
        assert!(!report.divergence.is_available());
        assert!(!report.recall_effectiveness.is_available());
        assert!(!report.recall_hit_rate.is_available());
        assert!(!report.contradictions.is_available());
        assert!(!report.disk.is_available());
        // S6b fields are always unavailable in this slice, with the API reason.
        match &report.replication_lag {
            ReportField::Unavailable { reason } => assert!(reason.contains("muninndb")),
            _ => panic!("replication_lag must be unavailable in S6a"),
        }
    }

    #[test]
    fn divergence_distinguishes_not_run_from_unreachable() {
        let not_run = assemble_memory_report(MemoryReportInputs {
            cortex_reachable: true,
            ..Default::default()
        });
        match &not_run.divergence {
            ReportField::Unavailable { reason } => assert!(reason.contains("not run")),
            _ => panic!(),
        }
        let unreachable = assemble_memory_report(MemoryReportInputs {
            cortex_reachable: false,
            ..Default::default()
        });
        match &unreachable.divergence {
            ReportField::Unavailable { reason } => assert!(reason.contains("not reachable")),
            _ => panic!(),
        }
    }

    #[test]
    fn recall_hit_rate_is_none_on_zero_turns_not_fabricated() {
        let r = RecallEffectiveness {
            completed: 0,
            skipped: 0,
        };
        assert_eq!(r.hit_rate(), None, "0/0 must not fabricate a 0.0 rate");
        let report = assemble_memory_report(MemoryReportInputs {
            recall: Some(r),
            ..Default::default()
        });
        // recall counts are available (we observed them) but the rate is not.
        assert!(report.recall_effectiveness.is_available());
        assert!(!report.recall_hit_rate.is_available());
    }

    #[test]
    fn recall_hit_rate_computed_when_turns_observed() {
        let report = assemble_memory_report(MemoryReportInputs {
            recall: Some(RecallEffectiveness {
                completed: 3,
                skipped: 1,
            }),
            ..Default::default()
        });
        assert_eq!(report.recall_hit_rate.value().copied(), Some(0.75));
    }

    #[test]
    fn divergence_and_disk_flow_through_when_sourced() {
        let report = assemble_memory_report(MemoryReportInputs {
            node_vault_stats: vec![cortex("default", 822)],
            divergence: Some(vec![VaultDivergence {
                vault: "default".into(),
                observer_only: 179,
                cortex_only: 14,
            }]),
            disk: Some(DiskReport {
                used_bytes: 346,
                total_bytes: 460,
                available_bytes: 78,
            }),
            cortex_reachable: true,
            ..Default::default()
        });
        assert_eq!(report.nodes.len(), 1);
        let div = report.divergence.value().expect("divergence available");
        assert_eq!(div[0].observer_only, 179);
        let disk = report.disk.value().expect("disk available");
        assert!((disk.used_fraction().unwrap() - 346.0 / 460.0).abs() < 1e-9);
    }

    #[test]
    fn report_roundtrips_json_with_honest_tags() {
        let report = assemble_memory_report(MemoryReportInputs {
            recall: Some(RecallEffectiveness {
                completed: 2,
                skipped: 2,
            }),
            ..Default::default()
        });
        let json = serde_json::to_string(&report).expect("serialize");
        assert!(json.contains("\"status\":\"unavailable\""));
        assert!(json.contains("\"status\":\"available\""));
        let back: MemoryReport = serde_json::from_str(&json).expect("roundtrip");
        assert_eq!(back, report);
    }
}
