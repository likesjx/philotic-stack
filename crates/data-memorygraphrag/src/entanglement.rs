//! Cross-domain entanglement: dual-similarity intersection + living-cycle
//! bridge discovery for the `cross_domain_entanglement` recall strategy.
//!
//! The operator's question is "does X affect Y?". A node is *entangled* when:
//!
//! - **semantic_both** — it scores above threshold against BOTH domain
//!   embeddings (true semantic entanglement), ranked by `min(score_a, score_b)`
//!   so a hit must be genuinely close to both sides to rank high; or
//! - **bridge** — it is reachable over one living-cycle hop
//!   (`OWNS|SHAPES|SETS|SPAWNS|RELATES_TO`) from a strong domain-A hit AND a
//!   strong domain-B hit, scored `min(best_a_anchor, best_b_anchor) * decay`.
//!
//! A few strong single-domain hits are kept for context, clearly labeled
//! `domain_a_only` / `domain_b_only` and damped so they rank below real
//! entanglement. Every hit carries a human-readable `reason` explaining WHY
//! it is entangled (which anchors/loops it touches on each side).

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use crate::RetrievalContextPacket;
use crate::SemanticSpace;
use crate::cypher::LIVING_CYCLE_REL_TYPES;
use crate::projection::VectorHit;

/// Per-side similarity threshold: a hit is "strong" on a domain side (and an
/// anchor for bridge discovery) only at or above this. Matches the 0.4 pivot
/// threshold in SEMANTIC_RETRIEVAL.md §5.
pub const CROSS_DOMAIN_MIN_SIMILARITY: f32 = 0.4;

/// Vector-search floor for the dual sweep. Deliberately below
/// [`CROSS_DOMAIN_MIN_SIMILARITY`] so near-threshold second-side scores are
/// still observed and can be reported on single-domain context hits.
pub const CROSS_DOMAIN_SEARCH_FLOOR: f32 = 0.3;

/// Bridge hits are one hop away from the semantic evidence, so they inherit
/// `min(anchor_a, anchor_b)` decayed by this factor (mirrors read-side
/// expansion decay).
pub const BRIDGE_SCORE_DECAY: f32 = 0.6;

/// Damping applied to single-domain context hits so they sort below
/// semantic_both and bridge hits of comparable similarity.
pub const SINGLE_DOMAIN_CONTEXT_DAMP: f32 = 0.5;

/// How many single-domain context hits to keep per side.
pub const MAX_SINGLE_DOMAIN_CONTEXT_HITS: usize = 3;

/// Candidate (space, label) pairs scored against BOTH domain embeddings.
/// Life-event side + goal side of the graph; must stay within the V001 index
/// vocabulary (see `projection::index_name`).
pub fn candidate_spaces() -> [(SemanticSpace, &'static str); 4] {
    [
        (SemanticSpace::LifeEventSemantic, "Signal"),
        (SemanticSpace::LifeEventSemantic, "Event"),
        (SemanticSpace::LifeEventSemantic, "OpenLoop"),
        (SemanticSpace::GoalSystemSemantic, "Goal"),
    ]
}

/// Why a hit is in the entanglement packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntanglementKind {
    /// Above threshold against both domain embeddings.
    SemanticBoth,
    /// One living-cycle hop from a strong domain-A hit AND a strong
    /// domain-B hit.
    Bridge,
    /// Strong on domain A only — context, clearly labeled.
    DomainAOnly,
    /// Strong on domain B only — context, clearly labeled.
    DomainBOnly,
}

impl EntanglementKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SemanticBoth => "semantic_both",
            Self::Bridge => "bridge",
            Self::DomainAOnly => "domain_a_only",
            Self::DomainBOnly => "domain_b_only",
        }
    }
}

/// A strong single-domain hit used as a bridge anchor.
#[derive(Debug, Clone)]
pub struct AnchorRef {
    pub id: String,
    pub label: String,
    pub title: String,
    pub similarity: f32,
}

/// One entangled candidate, ready for packet projection.
#[derive(Debug, Clone)]
pub struct EntangledHit {
    pub hit: VectorHit,
    pub kind: EntanglementKind,
    /// Ranking score (min-based for semantic_both, decayed for bridges,
    /// damped for single-domain context).
    pub score: f32,
    /// Similarity vs the domain-A embedding, when observed in the sweep.
    pub domain_a_similarity: Option<f32>,
    /// Similarity vs the domain-B embedding, when observed in the sweep.
    pub domain_b_similarity: Option<f32>,
    /// For bridges: the strong domain-A hits this node touches.
    pub domain_a_anchors: Vec<AnchorRef>,
    /// For bridges: the strong domain-B hits this node touches.
    pub domain_b_anchors: Vec<AnchorRef>,
    /// For bridges: living-cycle rel types on the domain-A side.
    pub bridge_a_rel_types: Vec<String>,
    /// For bridges: living-cycle rel types on the domain-B side.
    pub bridge_b_rel_types: Vec<String>,
}

impl EntangledHit {
    fn plain(hit: VectorHit, kind: EntanglementKind, score: f32) -> Self {
        Self {
            hit,
            kind,
            score,
            domain_a_similarity: None,
            domain_b_similarity: None,
            domain_a_anchors: Vec::new(),
            domain_b_anchors: Vec::new(),
            bridge_a_rel_types: Vec::new(),
            bridge_b_rel_types: Vec::new(),
        }
    }

    /// Human-readable answer to "why is this in the packet?" — which sides it
    /// scores on, and for bridges which loops/goals it touches on each side.
    pub fn reason(&self) -> String {
        match self.kind {
            EntanglementKind::SemanticBoth => format!(
                "semantically entangled with both domains (domain A similarity {:.2}, domain B similarity {:.2})",
                self.domain_a_similarity.unwrap_or(0.0),
                self.domain_b_similarity.unwrap_or(0.0),
            ),
            EntanglementKind::Bridge => {
                let a = self
                    .domain_a_anchors
                    .first()
                    .map(anchor_desc)
                    .unwrap_or_else(|| "a domain-A hit".to_string());
                let b = self
                    .domain_b_anchors
                    .first()
                    .map(anchor_desc)
                    .unwrap_or_else(|| "a domain-B hit".to_string());
                format!(
                    "bridge node: living-cycle {} link to {} on the domain-A side and {} link to {} on the domain-B side",
                    join_or(&self.bridge_a_rel_types, "edge"),
                    a,
                    join_or(&self.bridge_b_rel_types, "edge"),
                    b,
                )
            }
            EntanglementKind::DomainAOnly => format!(
                "domain A context only (similarity {:.2}; domain B {})",
                self.domain_a_similarity.unwrap_or(0.0),
                below_threshold_desc(self.domain_b_similarity),
            ),
            EntanglementKind::DomainBOnly => format!(
                "domain B context only (similarity {:.2}; domain A {})",
                self.domain_b_similarity.unwrap_or(0.0),
                below_threshold_desc(self.domain_a_similarity),
            ),
        }
    }

    /// Metadata blob merged into the projected evidence packet so the
    /// response envelope says WHY each hit is entangled.
    pub fn metadata(&self) -> Value {
        let anchors_json = |anchors: &[AnchorRef]| {
            anchors
                .iter()
                .map(|a| {
                    json!({
                        "id": a.id,
                        "label": a.label,
                        "title": a.title,
                        "similarity": a.similarity,
                    })
                })
                .collect::<Vec<_>>()
        };
        let mut meta = json!({
            "entanglement_kind": self.kind.as_str(),
            "entanglement_reason": self.reason(),
        });
        if let Some(obj) = meta.as_object_mut() {
            if let Some(a) = self.domain_a_similarity {
                obj.insert("domain_a_similarity".into(), json!(a));
            }
            if let Some(b) = self.domain_b_similarity {
                obj.insert("domain_b_similarity".into(), json!(b));
            }
            if self.kind == EntanglementKind::Bridge {
                obj.insert(
                    "entangled_via".into(),
                    json!({
                        "domain_a": anchors_json(&self.domain_a_anchors),
                        "domain_b": anchors_json(&self.domain_b_anchors),
                        "domain_a_rel_types": self.bridge_a_rel_types,
                        "domain_b_rel_types": self.bridge_b_rel_types,
                    }),
                );
            }
        }
        meta
    }
}

fn anchor_desc(anchor: &AnchorRef) -> String {
    if anchor.title.is_empty() {
        format!("{} ({})", anchor.id, anchor.label)
    } else {
        format!("'{}' ({})", anchor.title, anchor.label)
    }
}

fn join_or(rel_types: &[String], fallback: &str) -> String {
    if rel_types.is_empty() {
        fallback.to_string()
    } else {
        rel_types.join("/")
    }
}

fn below_threshold_desc(similarity: Option<f32>) -> String {
    match similarity {
        Some(sim) => format!("below threshold at {sim:.2}"),
        None => "no match observed".to_string(),
    }
}

// ── Dual-similarity intersection ──────────────────────────────────────────────

/// Result of intersecting the two per-domain sweeps.
#[derive(Debug, Default)]
pub struct DomainIntersection {
    /// Above threshold on BOTH sides, sorted by `min(a, b)` descending.
    pub entangled: Vec<EntangledHit>,
    /// Strong on domain A only, damped, sorted descending.
    pub a_only: Vec<EntangledHit>,
    /// Strong on domain B only, damped, sorted descending.
    pub b_only: Vec<EntangledHit>,
}

impl DomainIntersection {
    /// Strong domain-A hits (semantic_both + a_only) as bridge anchors.
    pub fn domain_a_anchors(&self) -> HashMap<String, AnchorRef> {
        anchor_map(self.entangled.iter().chain(self.a_only.iter()), |hit| {
            hit.domain_a_similarity
        })
    }

    /// Strong domain-B hits (semantic_both + b_only) as bridge anchors.
    pub fn domain_b_anchors(&self) -> HashMap<String, AnchorRef> {
        anchor_map(self.entangled.iter().chain(self.b_only.iter()), |hit| {
            hit.domain_b_similarity
        })
    }
}

fn anchor_map<'a>(
    hits: impl Iterator<Item = &'a EntangledHit>,
    side_similarity: impl Fn(&EntangledHit) -> Option<f32>,
) -> HashMap<String, AnchorRef> {
    hits.filter_map(|entangled| {
        let similarity = side_similarity(entangled)?;
        let id = entangled.hit.node_id();
        if id.is_empty() {
            return None;
        }
        Some((
            id.to_string(),
            AnchorRef {
                id: id.to_string(),
                label: entangled.hit.label.clone(),
                title: entangled.hit.title().to_string(),
                similarity,
            },
        ))
    })
    .collect()
}

/// Intersect the domain-A and domain-B sweeps by node id.
///
/// Duplicate node ids within a sweep keep their max similarity. Nodes below
/// `threshold` on both sides are dropped entirely; nodes above threshold on
/// exactly one side become damped single-domain context hits (the other
/// side's sub-threshold similarity is preserved when observed).
pub fn intersect_domain_hits(
    hits_a: Vec<VectorHit>,
    hits_b: Vec<VectorHit>,
    threshold: f32,
) -> DomainIntersection {
    struct Candidate {
        hit: VectorHit,
        sim_a: Option<f32>,
        sim_b: Option<f32>,
    }

    let mut by_id: HashMap<String, Candidate> = HashMap::new();
    for hit in hits_a {
        let id = hit.node_id().to_string();
        if id.is_empty() {
            continue;
        }
        match by_id.get_mut(&id) {
            Some(existing) => {
                let sim = existing
                    .sim_a
                    .map_or(hit.similarity, |s| s.max(hit.similarity));
                existing.sim_a = Some(sim);
            }
            None => {
                let sim = hit.similarity;
                by_id.insert(
                    id,
                    Candidate {
                        hit,
                        sim_a: Some(sim),
                        sim_b: None,
                    },
                );
            }
        }
    }
    for hit in hits_b {
        let id = hit.node_id().to_string();
        if id.is_empty() {
            continue;
        }
        match by_id.get_mut(&id) {
            Some(existing) => {
                let sim = existing
                    .sim_b
                    .map_or(hit.similarity, |s| s.max(hit.similarity));
                existing.sim_b = Some(sim);
            }
            None => {
                let sim = hit.similarity;
                by_id.insert(
                    id,
                    Candidate {
                        hit,
                        sim_a: None,
                        sim_b: Some(sim),
                    },
                );
            }
        }
    }

    let mut result = DomainIntersection::default();
    for (_, candidate) in by_id {
        let a = candidate.sim_a;
        let b = candidate.sim_b;
        let strong_a = a.is_some_and(|s| s >= threshold);
        let strong_b = b.is_some_and(|s| s >= threshold);
        match (strong_a, strong_b) {
            (true, true) => {
                let (a, b) = (a.unwrap_or(0.0), b.unwrap_or(0.0));
                let mut entangled = EntangledHit::plain(
                    candidate.hit,
                    EntanglementKind::SemanticBoth,
                    a.min(b).clamp(0.0, 1.0),
                );
                entangled.domain_a_similarity = Some(a);
                entangled.domain_b_similarity = Some(b);
                result.entangled.push(entangled);
            }
            (true, false) => {
                let a_sim = a.unwrap_or(0.0);
                let mut entangled = EntangledHit::plain(
                    candidate.hit,
                    EntanglementKind::DomainAOnly,
                    (a_sim * SINGLE_DOMAIN_CONTEXT_DAMP).clamp(0.0, 1.0),
                );
                entangled.domain_a_similarity = Some(a_sim);
                entangled.domain_b_similarity = b;
                result.a_only.push(entangled);
            }
            (false, true) => {
                let b_sim = b.unwrap_or(0.0);
                let mut entangled = EntangledHit::plain(
                    candidate.hit,
                    EntanglementKind::DomainBOnly,
                    (b_sim * SINGLE_DOMAIN_CONTEXT_DAMP).clamp(0.0, 1.0),
                );
                entangled.domain_a_similarity = a;
                entangled.domain_b_similarity = Some(b_sim);
                result.b_only.push(entangled);
            }
            (false, false) => {}
        }
    }

    let by_score_desc = |a: &EntangledHit, b: &EntangledHit| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    };
    result.entangled.sort_by(by_score_desc);
    result.a_only.sort_by(by_score_desc);
    result.b_only.sort_by(by_score_desc);
    result
}

// ── Bridge discovery ──────────────────────────────────────────────────────────

fn escape_single_quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

fn quoted_id_list(ids: &[&str]) -> String {
    ids.iter()
        .map(|id| format!("'{}'", escape_single_quoted(id)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Cypher for bridge discovery: non-retired nodes one living-cycle hop
/// (either direction) from a strong domain-A anchor AND a strong domain-B
/// anchor. Anchors themselves are excluded — a bridge is a NEW node the
/// vector sweep did not already surface as strong on either side.
pub fn bridge_discovery_cypher(a_ids: &[&str], b_ids: &[&str], limit: usize) -> String {
    let a_list = quoted_id_list(a_ids);
    let b_list = quoted_id_list(b_ids);
    let rel_types = LIVING_CYCLE_REL_TYPES
        .iter()
        .map(|rel| format!("'{rel}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "MATCH (a)-[ra]-(bridge)-[rb]-(b) \
         WHERE a.id IN [{a_list}] AND b.id IN [{b_list}] \
         AND type(ra) IN [{rel_types}] AND type(rb) IN [{rel_types}] \
         AND bridge.id IS NOT NULL \
         AND NOT bridge.id IN [{a_list}] AND NOT bridge.id IN [{b_list}] \
         AND coalesce(bridge.validation_state, 'inferred') <> 'retired' \
         RETURN bridge AS node, \
                collect(DISTINCT a.id) AS a_anchor_ids, \
                collect(DISTINCT b.id) AS b_anchor_ids, \
                collect(DISTINCT type(ra)) AS a_rel_types, \
                collect(DISTINCT type(rb)) AS b_rel_types \
         LIMIT {limit}",
    )
}

/// One parsed bridge row: the bridge node plus which anchors reached it on
/// each side and over which living-cycle rel types.
#[derive(Debug, Clone)]
pub struct BridgeRow {
    pub hit: VectorHit,
    pub a_anchor_ids: Vec<String>,
    pub b_anchor_ids: Vec<String>,
    pub a_rel_types: Vec<String>,
    pub b_rel_types: Vec<String>,
}

fn string_array(row: &Value, key: &str) -> Vec<String> {
    row.get(key)
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Parse [`bridge_discovery_cypher`] output rows.
pub fn parse_bridge_rows(result: &Value) -> Vec<BridgeRow> {
    let rows = match result.get("rows").and_then(Value::as_array) {
        Some(r) => r,
        None => return Vec::new(),
    };
    let mut parsed = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(node) = row.get("node") else {
            continue;
        };
        parsed.push(BridgeRow {
            hit: crate::projection::hit_from_bolt_node(node, 0.0),
            a_anchor_ids: string_array(row, "a_anchor_ids"),
            b_anchor_ids: string_array(row, "b_anchor_ids"),
            a_rel_types: string_array(row, "a_rel_types"),
            b_rel_types: string_array(row, "b_rel_types"),
        });
    }
    parsed
}

/// Fold parsed bridge rows into [`EntangledHit`]s.
///
/// A row survives only when at least one anchor on EACH side resolves against
/// the strong-anchor maps. Score is `min(best_a, best_b) * decay` — the bridge
/// is only as entangled as its weaker side, decayed for being a hop away.
pub fn fold_bridge_hits(
    rows: Vec<BridgeRow>,
    anchors_a: &HashMap<String, AnchorRef>,
    anchors_b: &HashMap<String, AnchorRef>,
    decay: f32,
) -> Vec<EntangledHit> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut bridges = Vec::new();
    for row in rows {
        let node_id = row.hit.node_id().to_string();
        if node_id.is_empty() || !seen.insert(node_id) {
            continue;
        }
        let resolve = |ids: &[String], map: &HashMap<String, AnchorRef>| {
            let mut anchors: Vec<AnchorRef> =
                ids.iter().filter_map(|id| map.get(id).cloned()).collect();
            anchors.sort_by(|x, y| {
                y.similarity
                    .partial_cmp(&x.similarity)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            anchors
        };
        let a_anchors = resolve(&row.a_anchor_ids, anchors_a);
        let b_anchors = resolve(&row.b_anchor_ids, anchors_b);
        let (Some(best_a), Some(best_b)) = (a_anchors.first(), b_anchors.first()) else {
            continue;
        };
        let score = (best_a.similarity.min(best_b.similarity) * decay).clamp(0.0, 1.0);
        let mut entangled = EntangledHit::plain(row.hit, EntanglementKind::Bridge, score);
        entangled.domain_a_anchors = a_anchors;
        entangled.domain_b_anchors = b_anchors;
        entangled.bridge_a_rel_types = row.a_rel_types;
        entangled.bridge_b_rel_types = row.b_rel_types;
        bridges.push(entangled);
    }
    bridges
}

// ── Candidate assembly ────────────────────────────────────────────────────────

/// Merge intersection + bridge results into one ranked candidate list.
///
/// Priority on id collision: semantic_both > bridge > single-domain (bridges
/// duplicating an already-surfaced node are dropped; the Cypher excludes
/// anchors in-query, this is defense in depth). Single-domain context is
/// capped at `max_single_per_side` per side. Output is sorted by score
/// descending.
pub fn assemble_entangled_candidates(
    intersection: DomainIntersection,
    bridges: Vec<EntangledHit>,
    max_single_per_side: usize,
) -> Vec<EntangledHit> {
    let mut candidates = intersection.entangled;
    let mut a_only = intersection.a_only;
    let mut b_only = intersection.b_only;
    a_only.truncate(max_single_per_side);
    b_only.truncate(max_single_per_side);

    let mut seen: HashSet<String> = candidates
        .iter()
        .chain(a_only.iter())
        .chain(b_only.iter())
        .map(|c| c.hit.node_id().to_string())
        .collect();
    for bridge in bridges {
        let id = bridge.hit.node_id().to_string();
        if seen.insert(id) {
            candidates.push(bridge);
        }
    }
    candidates.extend(a_only);
    candidates.extend(b_only);
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    candidates
}

/// Merge per-hit entanglement metadata into an already-projected context
/// packet, keyed by claim id.
pub fn annotate_packet(
    packet: &mut RetrievalContextPacket,
    metadata_by_id: &HashMap<String, Value>,
) {
    for ranked in &mut packet.ranked_packets {
        let Some(extra) = metadata_by_id.get(&ranked.packet.claim_ref.id) else {
            continue;
        };
        let (Some(meta), Some(extra)) = (ranked.packet.metadata.as_object_mut(), extra.as_object())
        else {
            continue;
        };
        for (key, value) in extra {
            meta.insert(key.clone(), value.clone());
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hit(id: &str, label: &str, title: &str, similarity: f32) -> VectorHit {
        VectorHit {
            bolt_id: 1,
            label: label.to_string(),
            properties: json!({
                "id": id,
                "title": title,
                "confidence": 0.7,
                "validation_state": "proposed",
                "observed_at": "2026-07-01T10:00:00Z"
            }),
            similarity,
        }
    }

    #[test]
    fn intersection_keeps_dual_threshold_nodes_ranked_by_min_score() {
        // (0.9, 0.45) has min 0.45; (0.6, 0.58) has min 0.58 → ranks first.
        let hits_a = vec![
            hit("l:signal:sleep", "Signal", "Sleep drops", 0.9),
            hit("l:signal:focus", "Signal", "Focus dips", 0.6),
        ];
        let hits_b = vec![
            hit("l:signal:sleep", "Signal", "Sleep drops", 0.45),
            hit("l:signal:focus", "Signal", "Focus dips", 0.58),
        ];

        let result = intersect_domain_hits(hits_a, hits_b, CROSS_DOMAIN_MIN_SIMILARITY);

        assert_eq!(result.entangled.len(), 2);
        assert!(result.a_only.is_empty());
        assert!(result.b_only.is_empty());
        assert_eq!(result.entangled[0].hit.node_id(), "l:signal:focus");
        assert!((result.entangled[0].score - 0.58).abs() < 0.001);
        assert_eq!(result.entangled[1].hit.node_id(), "l:signal:sleep");
        assert!((result.entangled[1].score - 0.45).abs() < 0.001);
        assert_eq!(result.entangled[0].kind, EntanglementKind::SemanticBoth);
        assert_eq!(
            result.entangled[0].domain_a_similarity,
            Some(0.6),
            "both raw side scores must be preserved"
        );
        assert_eq!(result.entangled[0].domain_b_similarity, Some(0.58));
    }

    #[test]
    fn intersection_threshold_behavior_splits_single_domain_and_drops_weak() {
        let hits_a = vec![
            // Strong A, sub-threshold B → domain_a_only with B sim recorded.
            hit("l:ol:code", "OpenLoop", "Refactor sprint", 0.45),
            // Weak on both sides → dropped entirely.
            hit("l:ol:noise", "OpenLoop", "Noise", 0.39),
        ];
        let hits_b = vec![
            hit("l:ol:code", "OpenLoop", "Refactor sprint", 0.39),
            hit("l:ol:noise", "OpenLoop", "Noise", 0.38),
            // Strong B, never seen in A → domain_b_only, no A sim.
            hit("l:goal:health", "Goal", "Sleep 8h", 0.7),
        ];

        let result = intersect_domain_hits(hits_a, hits_b, 0.4);

        assert!(result.entangled.is_empty());
        assert_eq!(result.a_only.len(), 1);
        assert_eq!(result.a_only[0].hit.node_id(), "l:ol:code");
        assert_eq!(result.a_only[0].kind, EntanglementKind::DomainAOnly);
        assert_eq!(result.a_only[0].domain_b_similarity, Some(0.39));
        assert!((result.a_only[0].score - 0.45 * SINGLE_DOMAIN_CONTEXT_DAMP).abs() < 0.001);
        assert_eq!(result.b_only.len(), 1);
        assert_eq!(result.b_only[0].hit.node_id(), "l:goal:health");
        assert_eq!(result.b_only[0].kind, EntanglementKind::DomainBOnly);
        assert_eq!(result.b_only[0].domain_a_similarity, None);
    }

    #[test]
    fn intersection_dedupes_within_a_sweep_keeping_max_similarity() {
        // Same node from two label indexes in the A sweep: keep max.
        let hits_a = vec![
            hit("l:sig:x", "Signal", "X", 0.42),
            hit("l:sig:x", "Signal", "X", 0.55),
        ];
        let hits_b = vec![hit("l:sig:x", "Signal", "X", 0.5)];

        let result = intersect_domain_hits(hits_a, hits_b, 0.4);
        assert_eq!(result.entangled.len(), 1);
        assert_eq!(result.entangled[0].domain_a_similarity, Some(0.55));
        assert!((result.entangled[0].score - 0.5).abs() < 0.001);
    }

    #[test]
    fn anchor_maps_include_semantic_both_and_own_side_singles() {
        let hits_a = vec![
            hit("l:both", "Signal", "Both", 0.6),
            hit("l:a", "Signal", "A only", 0.5),
        ];
        let hits_b = vec![
            hit("l:both", "Signal", "Both", 0.55),
            hit("l:b", "Goal", "B only", 0.65),
        ];
        let result = intersect_domain_hits(hits_a, hits_b, 0.4);

        let anchors_a = result.domain_a_anchors();
        assert_eq!(anchors_a.len(), 2);
        assert!(anchors_a.contains_key("l:both"));
        assert!(anchors_a.contains_key("l:a"));
        assert!((anchors_a["l:a"].similarity - 0.5).abs() < 0.001);

        let anchors_b = result.domain_b_anchors();
        assert_eq!(anchors_b.len(), 2);
        assert!(anchors_b.contains_key("l:both"));
        assert!(anchors_b.contains_key("l:b"));
    }

    #[test]
    fn bridge_cypher_shape_uses_living_cycle_rels_and_excludes_anchors() {
        let cypher = bridge_discovery_cypher(&["l:a:1", "l:a'2"], &["l:b:1"], 16);

        assert!(cypher.contains("MATCH (a)-[ra]-(bridge)-[rb]-(b)"));
        assert!(cypher.contains("a.id IN ['l:a:1', 'l:a\\'2']"));
        assert!(cypher.contains("b.id IN ['l:b:1']"));
        // SCOPED_TO joined LIVING_CYCLE_REL_TYPES as the structural
        // node->Role anchor rel type (LifeGraph auto-anchor Slice 1); bridge
        // discovery shares that same vocabulary constant.
        assert!(cypher.contains(
            "type(ra) IN ['OWNS', 'SHAPES', 'SETS', 'SPAWNS', 'RELATES_TO', 'SCOPED_TO']"
        ));
        assert!(cypher.contains(
            "type(rb) IN ['OWNS', 'SHAPES', 'SETS', 'SPAWNS', 'RELATES_TO', 'SCOPED_TO']"
        ));
        assert!(cypher.contains("NOT bridge.id IN ['l:a:1', 'l:a\\'2']"));
        assert!(cypher.contains("NOT bridge.id IN ['l:b:1']"));
        assert!(cypher.contains("coalesce(bridge.validation_state, 'inferred') <> 'retired'"));
        assert!(cypher.contains("RETURN bridge AS node"));
        assert!(cypher.contains("collect(DISTINCT a.id) AS a_anchor_ids"));
        assert!(cypher.contains("collect(DISTINCT b.id) AS b_anchor_ids"));
        assert!(cypher.contains("collect(DISTINCT type(ra)) AS a_rel_types"));
        assert!(cypher.contains("collect(DISTINCT type(rb)) AS b_rel_types"));
        assert!(cypher.contains("LIMIT 16"));
    }

    #[test]
    fn parse_bridge_rows_reads_node_and_anchor_arrays() {
        let result = json!({
            "rows": [{
                "node": {
                    "kind": "node",
                    "id": 7,
                    "labels": ["Habit"],
                    "properties": { "id": "l:habit:early-bed", "title": "Early bedtime" }
                },
                "a_anchor_ids": ["l:sig:sleep"],
                "b_anchor_ids": ["l:goal:ship"],
                "a_rel_types": ["SHAPES"],
                "b_rel_types": ["RELATES_TO"]
            }]
        });

        let rows = parse_bridge_rows(&result);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hit.node_id(), "l:habit:early-bed");
        assert_eq!(rows[0].hit.label, "Habit");
        assert_eq!(rows[0].a_anchor_ids, vec!["l:sig:sleep"]);
        assert_eq!(rows[0].b_anchor_ids, vec!["l:goal:ship"]);
        assert_eq!(rows[0].a_rel_types, vec!["SHAPES"]);
        assert_eq!(rows[0].b_rel_types, vec!["RELATES_TO"]);
        assert!(parse_bridge_rows(&json!({})).is_empty());
    }

    fn anchor(id: &str, similarity: f32) -> (String, AnchorRef) {
        (
            id.to_string(),
            AnchorRef {
                id: id.to_string(),
                label: "Signal".to_string(),
                title: format!("title {id}"),
                similarity,
            },
        )
    }

    #[test]
    fn fold_bridge_hits_scores_min_of_best_anchors_decayed() {
        let anchors_a: HashMap<_, _> = [anchor("l:a:1", 0.8), anchor("l:a:2", 0.5)].into();
        let anchors_b: HashMap<_, _> = [anchor("l:b:1", 0.6)].into();
        let rows = vec![BridgeRow {
            hit: hit("l:habit:bridge", "Habit", "Bridge habit", 0.0),
            a_anchor_ids: vec!["l:a:2".into(), "l:a:1".into()],
            b_anchor_ids: vec!["l:b:1".into()],
            a_rel_types: vec!["SHAPES".into()],
            b_rel_types: vec!["OWNS".into()],
        }];

        let bridges = fold_bridge_hits(rows, &anchors_a, &anchors_b, BRIDGE_SCORE_DECAY);
        assert_eq!(bridges.len(), 1);
        let bridge = &bridges[0];
        assert_eq!(bridge.kind, EntanglementKind::Bridge);
        // best_a = 0.8, best_b = 0.6 → min 0.6 * 0.6 decay = 0.36
        assert!((bridge.score - 0.6 * BRIDGE_SCORE_DECAY).abs() < 0.001);
        // Anchors sorted by similarity desc.
        assert_eq!(bridge.domain_a_anchors[0].id, "l:a:1");
        assert_eq!(bridge.bridge_a_rel_types, vec!["SHAPES"]);
    }

    #[test]
    fn fold_bridge_hits_requires_resolved_anchors_on_both_sides() {
        let anchors_a: HashMap<_, _> = [anchor("l:a:1", 0.8)].into();
        let anchors_b: HashMap<String, AnchorRef> = HashMap::new();
        let rows = vec![BridgeRow {
            hit: hit("l:habit:orphan", "Habit", "Orphan", 0.0),
            a_anchor_ids: vec!["l:a:1".into()],
            b_anchor_ids: vec!["l:b:unknown".into()],
            a_rel_types: vec![],
            b_rel_types: vec![],
        }];
        assert!(fold_bridge_hits(rows, &anchors_a, &anchors_b, BRIDGE_SCORE_DECAY).is_empty());
    }

    #[test]
    fn assemble_labels_kinds_caps_singles_and_sorts_by_score() {
        let mk = |id: &str, kind: EntanglementKind, score: f32| {
            EntangledHit::plain(hit(id, "Signal", id, score), kind, score)
        };
        let intersection = DomainIntersection {
            entangled: vec![mk("l:both", EntanglementKind::SemanticBoth, 0.55)],
            a_only: vec![
                mk("l:a:1", EntanglementKind::DomainAOnly, 0.30),
                mk("l:a:2", EntanglementKind::DomainAOnly, 0.28),
                mk("l:a:3", EntanglementKind::DomainAOnly, 0.26),
                mk("l:a:4", EntanglementKind::DomainAOnly, 0.24),
            ],
            b_only: vec![mk("l:b:1", EntanglementKind::DomainBOnly, 0.29)],
        };
        let bridges = vec![
            mk("l:bridge", EntanglementKind::Bridge, 0.40),
            // Duplicate of an already-surfaced node → dropped.
            mk("l:both", EntanglementKind::Bridge, 0.99),
        ];

        let candidates =
            assemble_entangled_candidates(intersection, bridges, MAX_SINGLE_DOMAIN_CONTEXT_HITS);

        let ids: Vec<&str> = candidates.iter().map(|c| c.hit.node_id()).collect();
        assert_eq!(
            ids,
            vec!["l:both", "l:bridge", "l:a:1", "l:b:1", "l:a:2", "l:a:3"],
            "sorted by score, singles capped at {MAX_SINGLE_DOMAIN_CONTEXT_HITS} per side, \
             duplicate bridge dropped"
        );
        assert_eq!(candidates[0].kind, EntanglementKind::SemanticBoth);
        assert_eq!(candidates[1].kind, EntanglementKind::Bridge);
    }

    #[test]
    fn reason_and_metadata_explain_why_entangled() {
        let mut both = EntangledHit::plain(
            hit("l:both", "Signal", "Sleep vs shipping", 0.6),
            EntanglementKind::SemanticBoth,
            0.52,
        );
        both.domain_a_similarity = Some(0.6);
        both.domain_b_similarity = Some(0.52);
        let reason = both.reason();
        assert!(reason.contains("both domains"));
        assert!(reason.contains("0.60"));
        assert!(reason.contains("0.52"));
        let meta = both.metadata();
        assert_eq!(meta["entanglement_kind"], "semantic_both");
        assert!((meta["domain_a_similarity"].as_f64().unwrap() - 0.6).abs() < 0.001);

        let mut bridge = EntangledHit::plain(
            hit("l:bridge", "Habit", "Early bedtime", 0.0),
            EntanglementKind::Bridge,
            0.36,
        );
        bridge.domain_a_anchors = vec![AnchorRef {
            id: "l:sig:sleep".into(),
            label: "Signal".into(),
            title: "Sleep drops".into(),
            similarity: 0.8,
        }];
        bridge.domain_b_anchors = vec![AnchorRef {
            id: "l:goal:ship".into(),
            label: "Goal".into(),
            title: "Ship the port".into(),
            similarity: 0.6,
        }];
        bridge.bridge_a_rel_types = vec!["SHAPES".into()];
        bridge.bridge_b_rel_types = vec!["RELATES_TO".into()];
        let reason = bridge.reason();
        assert!(reason.contains("bridge node"));
        assert!(reason.contains("'Sleep drops' (Signal)"));
        assert!(reason.contains("'Ship the port' (Goal)"));
        assert!(reason.contains("SHAPES"));
        assert!(reason.contains("RELATES_TO"));
        let meta = bridge.metadata();
        assert_eq!(meta["entanglement_kind"], "bridge");
        assert_eq!(meta["entangled_via"]["domain_a"][0]["id"], "l:sig:sleep");
        assert_eq!(
            meta["entangled_via"]["domain_b"][0]["title"],
            "Ship the port"
        );

        let mut a_only = EntangledHit::plain(
            hit("l:a", "OpenLoop", "Refactor", 0.45),
            EntanglementKind::DomainAOnly,
            0.225,
        );
        a_only.domain_a_similarity = Some(0.45);
        let reason = a_only.reason();
        assert!(reason.contains("domain A context only"));
        assert!(reason.contains("no match observed"));
    }

    #[test]
    fn annotate_packet_merges_entanglement_metadata_by_claim_id() {
        use crate::RetrievalStrategy;
        use crate::projection::{ScoredHit, project_context_packet};

        let scored = vec![
            ScoredHit {
                hit: hit("l:both", "Signal", "Sleep vs shipping", 0.6),
                score: 0.52,
                matched_policy_filters: Vec::new(),
                expansion_origin: None,
                fallback_origin: false,
            },
            ScoredHit {
                hit: hit("l:plain", "Signal", "Unrelated", 0.5),
                score: 0.30,
                matched_policy_filters: Vec::new(),
                expansion_origin: None,
                fallback_origin: false,
            },
        ];
        let mut packet = project_context_packet(
            "ctx:q1",
            "q1",
            RetrievalStrategy::MemoryAwareGraphRank,
            scored,
            Vec::new(),
            10_000,
            "2026-07-06T00:00:00Z",
        );

        let mut both = EntangledHit::plain(
            hit("l:both", "Signal", "Sleep vs shipping", 0.6),
            EntanglementKind::SemanticBoth,
            0.52,
        );
        both.domain_a_similarity = Some(0.6);
        both.domain_b_similarity = Some(0.52);
        let metadata_by_id: HashMap<String, Value> =
            [("l:both".to_string(), both.metadata())].into();

        annotate_packet(&mut packet, &metadata_by_id);

        let annotated = &packet.ranked_packets[0].packet;
        assert_eq!(annotated.claim_ref.id, "l:both");
        assert_eq!(annotated.metadata["entanglement_kind"], "semantic_both");
        assert!(
            annotated.metadata["entanglement_reason"]
                .as_str()
                .unwrap()
                .contains("both domains")
        );
        // Pre-existing projection metadata survives the merge.
        assert_eq!(annotated.metadata["from_vector_search"], true);
        // Unannotated packets are untouched.
        let plain = &packet.ranked_packets[1].packet;
        assert!(plain.metadata.get("entanglement_kind").is_none());
    }
}
