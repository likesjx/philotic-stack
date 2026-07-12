//! Memory Transparency Slice M2 (`docs/architecture/MEMORY_TRANSPARENCY_PROPOSAL.md`):
//! shared taxonomy + merge logic for the `memory.explain` query — "why do you
//! believe X?" fanned out across the three memory planes (Muninn, intel-graph,
//! LifeGraph) and merged into one provenance-aware report.
//!
//! This module is intentionally I/O-free: plane clients (in `philote` and
//! `philotic-web`) fetch raw plane data and map it into [`ExplainEvidenceItem`]
//! / [`ExplainPlaneOutcome`]; everything here is pure grouping and rendering so
//! it can be unit-tested without a network or a running MuninnDB/graph server.
//!
//! Taxonomy (the `lifegraph-truth-summarizer` skill's vocabulary, promoted to
//! a runtime type): an item's [`TruthBand`] is read straight off its
//! [`ProvenanceEnvelope`]'s trust tier when one is present. Standing Rule 2
//! ("trust tiers are honest") means we never infer a band when the envelope
//! is simply absent — that is its own band, [`TruthBand::NoProvenance`],
//! rendered as "pre-provenance record" rather than folded into `told` or
//! `inferred`.

use crate::provenance::{ProvenanceEnvelope, TrustTier};
use serde::{Deserialize, Serialize};

/// The three memory planes a `memory.explain` query fans out across.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExplainPlane {
    Muninn,
    IntelGraph,
    LifeGraph,
}

impl ExplainPlane {
    pub fn label(&self) -> &'static str {
        match self {
            ExplainPlane::Muninn => "Muninn",
            ExplainPlane::IntelGraph => "Intel Graph",
            ExplainPlane::LifeGraph => "LifeGraph",
        }
    }
}

/// One piece of evidence returned by a plane for a claim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainEvidenceItem {
    pub plane: ExplainPlane,
    /// Short label — engram concept, decision action, recall strategy, etc.
    pub label: String,
    /// The evidence body — engram content, decision reason, recalled content.
    pub detail: String,
    /// Plane-native identifier (engram id, mutation id, node id) when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    /// Unix-seconds timestamp of the underlying record, when known — used
    /// only for recency ordering within a band, not cross-plane ranking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<i64>,
    /// The Memory Transparency envelope for this item, when the writing
    /// component adopted M1. `None` is honest: it means "pre-provenance
    /// record", not "no trust".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope: Option<ProvenanceEnvelope>,
}

/// A plane's result for one `memory.explain` query: either evidence (possibly
/// empty — the plane answered, it just found nothing) or an honest
/// unavailability reason. Never both, never neither.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainPlaneOutcome {
    pub plane: ExplainPlane,
    #[serde(default)]
    pub items: Vec<ExplainEvidenceItem>,
    /// `Some` means the plane could not be queried at all (transport error,
    /// timeout, or — for surfaces with no transport to a plane at all — a
    /// named structural gap). Degradation must always be labeled, never a
    /// silent empty `items` list standing in for "unreachable".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

impl ExplainPlaneOutcome {
    pub fn ok(plane: ExplainPlane, items: Vec<ExplainEvidenceItem>) -> Self {
        Self {
            plane,
            items,
            unavailable_reason: None,
        }
    }

    pub fn unavailable(plane: ExplainPlane, reason: impl Into<String>) -> Self {
        Self {
            plane,
            items: Vec::new(),
            unavailable_reason: Some(reason.into()),
        }
    }

    pub fn is_available(&self) -> bool {
        self.unavailable_reason.is_none()
    }
}

/// The raw, unmerged fan-out result: one outcome per plane queried.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainReport {
    pub claim: String,
    pub planes: Vec<ExplainPlaneOutcome>,
}

/// The four truth bands a merged item is grouped into. Kept as four distinct
/// variants — never collapsed — per AGENTS.md 2.4 ("proven, inferred, and
/// intended are different") and Standing Rule 2 of the Memory Transparency
/// proposal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruthBand {
    /// `TrustTier::Observed` — evidence pointers back the claim directly.
    Confirmed,
    /// `TrustTier::Inferred` — pattern/heuristic match, not a direct observation.
    Inferred,
    /// `TrustTier::Told` — reported/seeded, not independently verified.
    ToldOrSeeded,
    /// No `ProvenanceEnvelope` at all — a pre-M1 record. Distinct from
    /// `ToldOrSeeded` on purpose: absence of data is not the same claim as
    /// "someone told us this".
    NoProvenance,
}

impl TruthBand {
    pub fn heading(&self) -> &'static str {
        match self {
            TruthBand::Confirmed => "Confirmed",
            TruthBand::Inferred => "Inferred",
            TruthBand::ToldOrSeeded => "Told / seeded",
            TruthBand::NoProvenance => "Pre-provenance record (no envelope)",
        }
    }
}

/// Reads an item's truth band straight off its envelope's trust tier.
pub fn truth_band(item: &ExplainEvidenceItem) -> TruthBand {
    match &item.envelope {
        None => TruthBand::NoProvenance,
        Some(env) => match env.trust {
            TrustTier::Observed => TruthBand::Confirmed,
            TrustTier::Inferred => TruthBand::Inferred,
            TrustTier::Told => TruthBand::ToldOrSeeded,
        },
    }
}

/// The merged, banded view of an [`ExplainReport`] — what the two surfaces
/// (the `memory.explain` philote tool and `phil memory explain`) actually
/// render. Grouping is intentionally simple: recency within a band (by
/// `recorded_at`, most recent first, ties/unknowns keep source order) — no
/// cross-plane ranking sophistication per the M2 slice contract.
#[derive(Debug, Clone)]
pub struct BandedExplainReport {
    pub claim: String,
    pub confirmed: Vec<ExplainEvidenceItem>,
    pub inferred: Vec<ExplainEvidenceItem>,
    pub told_or_seeded: Vec<ExplainEvidenceItem>,
    pub no_provenance: Vec<ExplainEvidenceItem>,
    /// Planes that could not be queried, with the reason — always surfaced,
    /// never dropped silently (the transparency surface must itself be
    /// transparent about coverage).
    pub unavailable_planes: Vec<(ExplainPlane, String)>,
    /// Planes that WERE queried successfully but contributed zero evidence
    /// for this claim. Tracked separately from `unavailable_planes` so a
    /// quiet plane ("I looked, found nothing") is never confused with an
    /// unreachable one — and separately from the bands so a plane's silence
    /// is never just an absence with no explanation.
    pub queried_no_evidence: Vec<ExplainPlane>,
}

impl BandedExplainReport {
    pub fn total_items(&self) -> usize {
        self.confirmed.len() + self.inferred.len() + self.told_or_seeded.len() + self.no_provenance.len()
    }
}

/// Normalizes a MuninnDB `/api/activate` timestamp (observed live to be
/// **nanosecond**-epoch, unlike `/api/engrams`'s second-epoch — a real wire
/// inconsistency between the two endpoints, not a typo) to Unix seconds, so
/// `recorded_at` is comparable across planes. `band_report`'s recency sort
/// merges items from all three planes into one `Vec` per band — silently
/// mixing units there would put every Muninn item first regardless of
/// actual time, since a nanosecond epoch is ~1e9x a second epoch.
pub fn muninn_activate_timestamp_to_unix_seconds(raw: i64) -> i64 {
    raw / 1_000_000_000
}

fn sort_by_recency(items: &mut [ExplainEvidenceItem]) {
    items.sort_by(|a, b| b.recorded_at.unwrap_or(i64::MIN).cmp(&a.recorded_at.unwrap_or(i64::MIN)));
}

/// Merge a raw [`ExplainReport`] into truth bands, honestly separating
/// unavailable planes from planes that answered with zero evidence.
pub fn band_report(report: &ExplainReport) -> BandedExplainReport {
    let mut confirmed = Vec::new();
    let mut inferred = Vec::new();
    let mut told_or_seeded = Vec::new();
    let mut no_provenance = Vec::new();
    let mut unavailable_planes = Vec::new();
    let mut queried_no_evidence = Vec::new();

    for outcome in &report.planes {
        if let Some(reason) = &outcome.unavailable_reason {
            unavailable_planes.push((outcome.plane, reason.clone()));
            continue;
        }
        if outcome.items.is_empty() {
            queried_no_evidence.push(outcome.plane);
            continue;
        }
        for item in &outcome.items {
            match truth_band(item) {
                TruthBand::Confirmed => confirmed.push(item.clone()),
                TruthBand::Inferred => inferred.push(item.clone()),
                TruthBand::ToldOrSeeded => told_or_seeded.push(item.clone()),
                TruthBand::NoProvenance => no_provenance.push(item.clone()),
            }
        }
    }

    sort_by_recency(&mut confirmed);
    sort_by_recency(&mut inferred);
    sort_by_recency(&mut told_or_seeded);
    sort_by_recency(&mut no_provenance);

    BandedExplainReport {
        claim: report.claim.clone(),
        confirmed,
        inferred,
        told_or_seeded,
        no_provenance,
        unavailable_planes,
        queried_no_evidence,
    }
}

fn render_items(out: &mut String, heading: &str, items: &[ExplainEvidenceItem]) {
    if items.is_empty() {
        return;
    }
    out.push_str(heading);
    out.push_str(":\n");
    for item in items {
        let plane = item.plane.label();
        let source = item
            .source_ref
            .as_deref()
            .map(|s| format!(" [{s}]"))
            .unwrap_or_default();
        out.push_str(&format!("- ({plane}) {}: {}{}\n", item.label, item.detail, source));
        if let Some(env) = &item.envelope {
            if !env.evidence.is_empty() {
                out.push_str(&format!("    evidence: {}\n", env.evidence.join(", ")));
            }
            if let Some(reversal) = &env.reversal {
                out.push_str(&format!("    reversal: {reversal}\n"));
            }
            if !env.author.is_empty() {
                out.push_str(&format!("    author: {}\n", env.author));
            }
        }
    }
}

/// Render a banded report as the plain-text body shared by both surfaces
/// (the philote tool result and the `phil memory explain` CLI output).
pub fn render_text(banded: &BandedExplainReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("Why do I believe: \"{}\"\n\n", banded.claim));

    render_items(&mut out, "Confirmed", &banded.confirmed);
    render_items(&mut out, "Inferred", &banded.inferred);
    render_items(&mut out, "Told / seeded", &banded.told_or_seeded);
    render_items(
        &mut out,
        "Pre-provenance records (no envelope — display honest, not fabricated trust)",
        &banded.no_provenance,
    );

    if banded.total_items() == 0 {
        out.push_str("No evidence found on any queried plane for this claim.\n");
    }

    if !banded.queried_no_evidence.is_empty() {
        out.push('\n');
        out.push_str("Plane(s) queried, no matching evidence found:\n");
        for plane in &banded.queried_no_evidence {
            out.push_str(&format!("- {}\n", plane.label()));
        }
    }

    if !banded.unavailable_planes.is_empty() {
        out.push_str("\nPlane(s) unavailable (coverage gap, not silence):\n");
        for (plane, reason) in &banded.unavailable_planes {
            out.push_str(&format!("- {}: {}\n", plane.label(), reason));
        }
    }

    out
}

/// Best-effort parse of a `ProvenanceEnvelope` out of a decision record's
/// loosely-typed `details` JSON blob (intel-graph's `Mutation.details` /
/// `DecideBody` fields — see M1's adoption note: `evidence: Vec<String>`,
/// `reversal: Option<String>`, `trust: Option<String>`, all optional and
/// sent as plain JSON, not the Rust type, to avoid a new crate dependency
/// on the graph-intelligence side). Defensive: `evidence` has been observed
/// in the wild as a bare string (from an older, unrelated producer) as well
/// as an array — both are accepted. Returns `None` (not a naked default)
/// when none of the three fields carry anything, so a decision predating
/// M1 renders as "pre-provenance record" rather than a fabricated envelope.
pub fn envelope_from_decision_details(
    details: &serde_json::Value,
    author: impl Into<String>,
) -> Option<ProvenanceEnvelope> {
    let trust = details
        .get("trust")
        .and_then(|v| v.as_str())
        .and_then(parse_trust_tier);

    let evidence: Vec<String> = match details.get("evidence") {
        Some(serde_json::Value::Array(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        Some(serde_json::Value::String(s)) if !s.is_empty() => vec![s.clone()],
        _ => Vec::new(),
    };

    let reversal = details
        .get("reversal")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    if trust.is_none() && evidence.is_empty() && reversal.is_none() {
        return None;
    }

    Some(ProvenanceEnvelope {
        source: String::new(),
        author: author.into(),
        trust: trust.unwrap_or_default(),
        evidence,
        reversal,
    })
}

fn parse_trust_tier(s: &str) -> Option<TrustTier> {
    match s.to_ascii_lowercase().as_str() {
        "observed" => Some(TrustTier::Observed),
        "inferred" => Some(TrustTier::Inferred),
        "told" => Some(TrustTier::Told),
        _ => None,
    }
}

/// Best-effort parse of a `ProvenanceEnvelope` out of a Muninn engram's
/// `metadata` JSON — `philote`'s `memory.remember` tool stores it under the
/// `"provenance"` key (see `merge_provenance_into_metadata`). Absent key or
/// malformed value both map to `None` — honest "pre-provenance record"
/// display, never a fabricated envelope.
pub fn envelope_from_engram_metadata(metadata: &serde_json::Value) -> Option<ProvenanceEnvelope> {
    metadata
        .get("provenance")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(trust: TrustTier) -> ProvenanceEnvelope {
        ProvenanceEnvelope::from_agent("agent-x", Some("orchestrator"))
            .with_trust(trust)
            .with_evidence(["pointer:1"])
    }

    fn item(plane: ExplainPlane, label: &str, envelope: Option<ProvenanceEnvelope>) -> ExplainEvidenceItem {
        ExplainEvidenceItem {
            plane,
            label: label.into(),
            detail: format!("{label} detail"),
            source_ref: None,
            recorded_at: None,
            envelope,
        }
    }

    #[test]
    fn truth_band_maps_trust_tiers_and_absence_correctly() {
        assert_eq!(
            truth_band(&item(ExplainPlane::Muninn, "a", Some(envelope(TrustTier::Observed)))),
            TruthBand::Confirmed
        );
        assert_eq!(
            truth_band(&item(ExplainPlane::Muninn, "b", Some(envelope(TrustTier::Inferred)))),
            TruthBand::Inferred
        );
        assert_eq!(
            truth_band(&item(ExplainPlane::Muninn, "c", Some(envelope(TrustTier::Told)))),
            TruthBand::ToldOrSeeded
        );
        assert_eq!(
            truth_band(&item(ExplainPlane::Muninn, "d", None)),
            TruthBand::NoProvenance
        );
    }

    #[test]
    fn band_report_groups_items_and_never_collapses_no_provenance_into_told() {
        let report = ExplainReport {
            claim: "the sky is blue".into(),
            planes: vec![
                ExplainPlaneOutcome::ok(
                    ExplainPlane::Muninn,
                    vec![
                        item(ExplainPlane::Muninn, "observed-fact", Some(envelope(TrustTier::Observed))),
                        item(ExplainPlane::Muninn, "old-engram", None),
                    ],
                ),
                ExplainPlaneOutcome::ok(
                    ExplainPlane::IntelGraph,
                    vec![item(ExplainPlane::IntelGraph, "decision", Some(envelope(TrustTier::Told)))],
                ),
            ],
        };

        let banded = band_report(&report);
        assert_eq!(banded.confirmed.len(), 1);
        assert_eq!(banded.told_or_seeded.len(), 1);
        assert_eq!(banded.no_provenance.len(), 1);
        assert!(banded.inferred.is_empty());
        assert!(banded.unavailable_planes.is_empty());
        assert_eq!(banded.total_items(), 3);
    }

    #[test]
    fn band_report_surfaces_unavailable_planes_without_dropping_them() {
        let report = ExplainReport {
            claim: "claim".into(),
            planes: vec![
                ExplainPlaneOutcome::ok(ExplainPlane::Muninn, vec![]),
                ExplainPlaneOutcome::unavailable(ExplainPlane::LifeGraph, "no transport from this surface"),
            ],
        };
        let banded = band_report(&report);
        assert_eq!(banded.total_items(), 0);
        assert_eq!(banded.unavailable_planes.len(), 1);
        assert_eq!(banded.unavailable_planes[0].0, ExplainPlane::LifeGraph);
        assert_eq!(banded.unavailable_planes[0].1, "no transport from this surface");
        // The Muninn plane answered (no unavailable_reason) with zero items —
        // that must be tracked as "queried, found nothing", not silently
        // indistinguishable from a plane that was never asked at all.
        assert_eq!(banded.queried_no_evidence, vec![ExplainPlane::Muninn]);
    }

    #[test]
    fn render_text_distinguishes_queried_empty_from_unavailable() {
        let report = ExplainReport {
            claim: "claim".into(),
            planes: vec![
                ExplainPlaneOutcome::ok(ExplainPlane::IntelGraph, vec![]),
                ExplainPlaneOutcome::unavailable(ExplainPlane::LifeGraph, "no transport"),
            ],
        };
        let text = render_text(&band_report(&report));
        assert!(text.contains("Plane(s) queried, no matching evidence found"));
        assert!(text.contains("Intel Graph"));
        assert!(text.contains("Plane(s) unavailable"));
        assert!(text.contains("LifeGraph: no transport"));
    }

    #[test]
    fn render_text_includes_unavailable_section_and_empty_notice() {
        let report = ExplainReport {
            claim: "claim with no evidence".into(),
            planes: vec![ExplainPlaneOutcome::unavailable(ExplainPlane::IntelGraph, "connection refused")],
        };
        let text = render_text(&band_report(&report));
        assert!(text.contains("No evidence found on any queried plane"));
        assert!(text.contains("Plane(s) unavailable"));
        assert!(text.contains("Intel Graph: connection refused"));
    }

    #[test]
    fn render_text_never_silently_omits_a_populated_band() {
        let report = ExplainReport {
            claim: "claim".into(),
            planes: vec![ExplainPlaneOutcome::ok(
                ExplainPlane::LifeGraph,
                vec![item(ExplainPlane::LifeGraph, "recalled", Some(envelope(TrustTier::Inferred)))],
            )],
        };
        let text = render_text(&band_report(&report));
        assert!(text.contains("Inferred:"));
        assert!(text.contains("recalled"));
        assert!(!text.contains("No evidence found"));
    }

    #[test]
    fn recency_sort_orders_most_recent_first_within_a_band() {
        let mut older = item(ExplainPlane::Muninn, "older", Some(envelope(TrustTier::Observed)));
        older.recorded_at = Some(100);
        let mut newer = item(ExplainPlane::Muninn, "newer", Some(envelope(TrustTier::Observed)));
        newer.recorded_at = Some(200);

        let report = ExplainReport {
            claim: "claim".into(),
            planes: vec![ExplainPlaneOutcome::ok(ExplainPlane::Muninn, vec![older, newer])],
        };
        let banded = band_report(&report);
        assert_eq!(banded.confirmed[0].label, "newer");
        assert_eq!(banded.confirmed[1].label, "older");
    }

    #[test]
    fn recency_sort_is_stable_across_muninn_and_intel_graph_units() {
        // Regression: a raw Muninn `/api/activate` nanosecond timestamp and
        // a raw intel-graph second timestamp must not be compared without
        // normalizing units first, or the (numerically enormous) raw
        // nanosecond value always sorts first regardless of real time.
        let muninn_raw_ns: i64 = 1_782_527_813_435_365_000; // observed live
        let mut muninn_item = item(ExplainPlane::Muninn, "muninn-2026", Some(envelope(TrustTier::Observed)));
        muninn_item.recorded_at = Some(muninn_activate_timestamp_to_unix_seconds(muninn_raw_ns));

        let mut intel_graph_item = item(
            ExplainPlane::IntelGraph,
            "intel-graph-later",
            Some(envelope(TrustTier::Observed)),
        );
        // A later Unix-seconds timestamp than the normalized Muninn one above.
        intel_graph_item.recorded_at = Some(muninn_activate_timestamp_to_unix_seconds(muninn_raw_ns) + 1000);

        let report = ExplainReport {
            claim: "claim".into(),
            planes: vec![ExplainPlaneOutcome::ok(
                ExplainPlane::Muninn,
                vec![muninn_item, intel_graph_item],
            )],
        };
        let banded = band_report(&report);
        assert_eq!(banded.confirmed[0].label, "intel-graph-later");
        assert_eq!(banded.confirmed[1].label, "muninn-2026");
    }

    #[test]
    fn muninn_activate_timestamp_normalizes_nanoseconds_to_seconds() {
        assert_eq!(
            muninn_activate_timestamp_to_unix_seconds(1_782_527_813_435_365_000),
            1_782_527_813
        );
    }

    #[test]
    fn envelope_from_decision_details_parses_array_evidence() {
        let details = serde_json::json!({
            "trust": "observed",
            "evidence": ["pr:#253", "graph:node:x"],
            "reversal": "revert_edge:y",
        });
        let env = envelope_from_decision_details(&details, "claude-local").expect("envelope");
        assert_eq!(env.trust, TrustTier::Observed);
        assert_eq!(env.evidence, vec!["pr:#253", "graph:node:x"]);
        assert_eq!(env.reversal.as_deref(), Some("revert_edge:y"));
        assert_eq!(env.author, "claude-local");
    }

    #[test]
    fn envelope_from_decision_details_accepts_legacy_string_evidence() {
        // Observed in the wild from an older, unrelated producer
        // (`verification_advance`) that predates the M1 `Vec<String>` shape.
        let details = serde_json::json!({ "evidence": "PR #101 merge db164fdc2; 231/231" });
        let env = envelope_from_decision_details(&details, "x").expect("envelope");
        assert_eq!(env.evidence, vec!["PR #101 merge db164fdc2; 231/231"]);
    }

    #[test]
    fn envelope_from_decision_details_returns_none_for_empty_details() {
        assert!(envelope_from_decision_details(&serde_json::json!({}), "x").is_none());
    }

    #[test]
    fn envelope_from_engram_metadata_round_trips_and_handles_absence() {
        let env = envelope(TrustTier::Told);
        let metadata = serde_json::json!({ "provenance": serde_json::to_value(&env).unwrap() });
        let parsed = envelope_from_engram_metadata(&metadata).expect("envelope");
        assert_eq!(parsed, env);

        assert!(envelope_from_engram_metadata(&serde_json::json!({})).is_none());
        assert!(envelope_from_engram_metadata(&serde_json::json!({"provenance": "not-an-envelope"})).is_none());
    }
}
