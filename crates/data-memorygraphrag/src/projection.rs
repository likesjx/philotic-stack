//! Projection primitives for Life Graph semantic retrieval.
//!
//! Responsibilities:
//!   - generate `CALL vector_search.search(...)` Cypher strings from a query
//!   - parse `MemgraphCypherProvider::execute_cypher` output into `VectorHit`s
//!   - apply policy filters and compute composite ranking scores
//!   - project ranked hits into `RetrievalContextPacket`
//!
//! Synchronous and dependency-free. The async wiring that actually sends Cypher
//! to `graph-datasource` and drives a complete `RunnerPlan` is a separate slice
//! (left open for the hotel runtime layer).

use crate::{
    AdjudicationStatus, EvidencePacket, GraphRecordRef, PolicyFilter, RankedEvidencePacket,
    RankingWeights, ReliabilityBasis, RetrievalContextPacket, RetrievalStrategy, SemanticSpace,
    SourceKind, SourceRef, SourceReliability, ValidationState,
};
use serde_json::Value;

// ── Cypher generation ─────────────────────────────────────────────────────────

/// Canonical Memgraph vector index name for a given semantic space and node label.
///
/// Must stay in sync with the index names created by V001 migration.
pub fn index_name(space: &SemanticSpace, label: &str) -> String {
    let prefix = match space {
        SemanticSpace::LifeEventSemantic => "life_event_semantic",
        SemanticSpace::GoalSystemSemantic => "goal_system_semantic",
        SemanticSpace::SkillToolSemantic => "skill_tool_semantic",
        SemanticSpace::RolePersonSemantic => "role_person_semantic",
        SemanticSpace::MemoryBridgeSemantic => "memory_bridge_semantic",
    };
    format!("{}__{}", prefix, label)
}

/// All node labels that participate in a given semantic space.
///
/// Callers iterate these and query each per-label index. Must stay in sync with
/// V001 migration index names.
pub fn labels_for_space(space: &SemanticSpace) -> &'static [&'static str] {
    match space {
        SemanticSpace::LifeEventSemantic => &["Event", "Signal", "OpenLoop"],
        SemanticSpace::GoalSystemSemantic => &[
            "Goal",
            "System",
            "Habit",
            "Project",
            "Routine",
            "NextAction",
        ],
        SemanticSpace::SkillToolSemantic => &[
            "GrowthHypothesis",
            "GrowthExperiment",
            "DriftFinding",
            "CapabilityPatch",
            "SkillPatch",
            "ToolPatch",
            "SchemaPatch",
            "AttentionPatch",
            "SystemPatch",
        ],
        SemanticSpace::RolePersonSemantic => &[
            "Role",
            "Aspiration",
            "Person",
            "Value",
            "Preference",
            "Concern",
        ],
        SemanticSpace::MemoryBridgeSemantic => &["Commitment", "Decision"],
    }
}

/// Generates a `CALL vector_search.search(...)` Cypher string.
///
/// Returns the canonical `embedding_space` name for a given Life Graph label,
/// or `None` if the label has no vector index.
pub fn embedding_space_for_label(label: &str) -> Option<&'static str> {
    match label {
        "Event" | "Signal" | "OpenLoop" => Some("life_event_semantic"),
        "Goal" | "System" | "Habit" | "Project" | "Routine" | "NextAction" => {
            Some("goal_system_semantic")
        }
        "GrowthHypothesis" | "GrowthExperiment" | "DriftFinding" | "CapabilityPatch"
        | "SkillPatch" | "ToolPatch" | "SchemaPatch" | "AttentionPatch" | "SystemPatch" => {
            Some("skill_tool_semantic")
        }
        "Role" | "Aspiration" | "Person" | "Value" | "Preference" | "Concern" => {
            Some("role_person_semantic")
        }
        "Commitment" | "Decision" => Some("memory_bridge_semantic"),
        _ => None,
    }
}

/// YIELD names are `node` and `similarity` (no alias) so the parser reads
/// `result["rows"][i]["node"]` and `result["rows"][i]["similarity"]`.
///
/// The query embedding rides as the `$vec` Bolt parameter, NOT inlined into
/// the query text: 768 formatted floats per search made every recall query a
/// unique multi-KB string, defeating Memgraph's query-plan cache on the
/// hottest read path (~8 searches per turn). Callers must bind `vec`.
///
/// Verified against Memgraph 3.10.1 on vps-jane: correct procedure name,
/// arg arity, and index name format. Empty DB returns 0 rows (not an error).
pub fn semantic_expand_cypher(index: &str, top_k: usize, min_similarity: f32) -> String {
    format!(
        "CALL vector_search.search(\"{index}\", {top_k}, $vec) \
         YIELD node, similarity \
         WHERE similarity >= {min_sim:.4} \
         RETURN node, similarity \
         ORDER BY similarity DESC",
        min_sim = min_similarity,
    )
}

// ── VectorHit ─────────────────────────────────────────────────────────────────

/// One parsed result from `CALL vector_search.search(...) YIELD node, similarity`.
///
/// Shape derived from `memgraph_provider.rs` `row_to_json` + `bolt_node_to_json`:
/// ```json
/// {
///   "rows": [{
///     "node": { "kind": "node", "id": 42, "labels": ["OpenLoop"], "properties": {...} },
///     "similarity": 0.91
///   }]
/// }
/// ```
#[derive(Debug, Clone)]
pub struct VectorHit {
    /// Memgraph internal integer node ID (from `bolt_node_to_json`).
    pub bolt_id: i64,
    /// First element of the `labels` array.
    pub label: String,
    /// Raw node properties from Memgraph.
    pub properties: Value,
    /// Cosine similarity score.
    pub similarity: f32,
}

impl VectorHit {
    pub fn prop_str(&self, key: &str) -> Option<&str> {
        self.properties.get(key).and_then(Value::as_str)
    }

    pub fn prop_f64(&self, key: &str) -> Option<f64> {
        self.properties.get(key).and_then(Value::as_f64)
    }

    /// The `id` property (Life Graph canonical node ID, not the Bolt integer ID).
    pub fn node_id(&self) -> &str {
        self.prop_str("id").unwrap_or("")
    }

    pub fn title(&self) -> &str {
        self.prop_str("title")
            .or_else(|| self.prop_str("name"))
            .or_else(|| self.prop_str("description"))
            .or_else(|| self.prop_str("claim_summary"))
            .or_else(|| self.prop_str("summary"))
            .unwrap_or("")
    }

    pub fn confidence(&self) -> f32 {
        self.prop_f64("confidence").unwrap_or(0.5) as f32
    }

    pub fn validation_state(&self) -> ValidationState {
        match self.prop_str("validation_state").unwrap_or("inferred") {
            "confirmed" => ValidationState::Confirmed,
            "proposed" => ValidationState::Proposed,
            "retired" => ValidationState::Retired,
            "conflicted" => ValidationState::Conflicted,
            _ => ValidationState::Inferred,
        }
    }

    pub fn is_retired(&self) -> bool {
        matches!(self.validation_state(), ValidationState::Retired)
            || matches!(
                self.prop_str("status").unwrap_or(""),
                "retired" | "done" | "fulfilled" | "abandoned"
            )
    }
}

/// Build a `VectorHit` from one `bolt_node_to_json`-shaped value.
pub(crate) fn hit_from_bolt_node(node: &Value, similarity: f32) -> VectorHit {
    let bolt_id = node.get("id").and_then(Value::as_i64).unwrap_or(-1);
    let label = node
        .get("labels")
        .and_then(Value::as_array)
        .and_then(|ls| ls.first())
        .and_then(Value::as_str)
        .unwrap_or("Unknown")
        .to_string();
    let properties = node
        .get("properties")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    VectorHit {
        bolt_id,
        label,
        properties,
        similarity,
    }
}

/// Parse the `rows` array from `MemgraphCypherProvider::execute_cypher` output.
pub fn parse_vector_search_rows(result: &Value) -> Vec<VectorHit> {
    let rows = match result.get("rows").and_then(Value::as_array) {
        Some(r) => r,
        None => return Vec::new(),
    };

    let mut hits = Vec::with_capacity(rows.len());
    for row in rows {
        // Key is "node" (the YIELD name, no alias).
        let node = match row.get("node") {
            Some(n) => n,
            None => continue,
        };
        let similarity = row.get("similarity").and_then(Value::as_f64).unwrap_or(0.0) as f32;
        hits.push(hit_from_bolt_node(node, similarity));
    }
    hits
}

// ── Graph expansion (read side) ───────────────────────────────────────────────

/// Score multiplier applied to expansion-discovered hits: they inherit their
/// parent's score decayed by this factor, so a hop away always ranks below
/// the hit that surfaced it.
pub const EXPANSION_SCORE_DECAY: f32 = 0.6;

/// Effective relationship types for read-side expansion: the intersection of
/// the caller's `ExpansionPolicy.allowed_edge_types` with the writable
/// vocabulary (living-cycle + agenda relations). An empty allowlist means
/// all writable types. Unknown caller-supplied types are ignored (never
/// interpolated into Cypher).
pub fn expansion_rel_types(allowed_edge_types: &[String]) -> Vec<&'static str> {
    let writable = crate::cypher::LIVING_CYCLE_REL_TYPES
        .iter()
        .copied()
        .chain(crate::cypher::AGENDA_EDGE_RULES.iter().map(|r| r.rel_type));
    if allowed_edge_types.is_empty() {
        return writable.collect();
    }
    writable
        .filter(|rel| allowed_edge_types.iter().any(|allowed| allowed == rel))
        .collect()
}

fn escape_single_quoted(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Single-round-trip, one-hop expansion over living-cycle edges for a batch
/// of seed node ids (no per-hit N+1 against Memgraph).
///
/// `rel_types` must come from [`expansion_rel_types`] — only whitelisted
/// living-cycle types are ever interpolated. Retired neighbours are excluded
/// in-query; the caller applies the remaining `PolicyFilter`s via
/// [`fold_expansion_hits`]. `max_rows` bounds the round trip
/// (`ExpansionPolicy.max_nodes`).
pub fn expansion_cypher(seed_ids: &[&str], rel_types: &[&str], max_rows: usize) -> String {
    let ids = seed_ids
        .iter()
        .map(|id| format!("'{}'", escape_single_quoted(id)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "MATCH (n)-[r:{rels}]-(related) \
         WHERE n.id IN [{ids}] \
         AND related.id IS NOT NULL \
         AND coalesce(related.validation_state, 'inferred') <> 'retired' \
         RETURN n.id AS origin_id, type(r) AS rel_type, related AS node \
         LIMIT {max_rows}",
        rels = rel_types.join("|"),
        ids = ids,
        max_rows = max_rows,
    )
}

/// One neighbour discovered by [`expansion_cypher`]: the seed node it was
/// reached from, the living-cycle relationship, and the neighbour itself.
#[derive(Debug, Clone)]
pub struct ExpansionHit {
    pub origin_id: String,
    pub rel_type: String,
    pub hit: VectorHit,
}

/// Parse `RETURN n.id AS origin_id, type(r) AS rel_type, related AS node`
/// rows from `execute_cypher` output.
pub fn parse_expansion_rows(result: &Value) -> Vec<ExpansionHit> {
    let rows = match result.get("rows").and_then(Value::as_array) {
        Some(r) => r,
        None => return Vec::new(),
    };
    let mut hits = Vec::with_capacity(rows.len());
    for row in rows {
        let (Some(origin_id), Some(rel_type), Some(node)) = (
            row.get("origin_id").and_then(Value::as_str),
            row.get("rel_type").and_then(Value::as_str),
            row.get("node"),
        ) else {
            continue;
        };
        hits.push(ExpansionHit {
            origin_id: origin_id.to_string(),
            rel_type: rel_type.to_string(),
            hit: hit_from_bolt_node(node, 0.0),
        });
    }
    hits
}

/// Fold expansion-discovered neighbours into scored candidates.
///
/// Each surviving neighbour becomes a [`ScoredHit`] carrying
/// `expansion_origin` provenance, scored `parent_score * decay`. Neighbours
/// are dropped when they duplicate a parent (or an earlier expansion), fail
/// the caller's `PolicyFilter`s, reference an unknown origin, or would exceed
/// `max_nodes` (the `ExpansionPolicy.max_nodes` cap).
pub fn fold_expansion_hits(
    parents: &[ScoredHit],
    expansion: Vec<ExpansionHit>,
    filters: &[PolicyFilter],
    decay: f32,
    max_nodes: usize,
) -> Vec<ScoredHit> {
    let mut seen: std::collections::HashSet<String> = parents
        .iter()
        .map(|p| p.hit.node_id().to_string())
        .filter(|id| !id.is_empty())
        .collect();
    let mut folded = Vec::new();
    for exp in expansion {
        if folded.len() >= max_nodes {
            break;
        }
        let Some(parent) = parents.iter().find(|p| p.hit.node_id() == exp.origin_id) else {
            continue;
        };
        let node_id = exp.hit.node_id().to_string();
        if node_id.is_empty() || seen.contains(&node_id) {
            continue;
        }
        // PolicyFilters apply to expanded nodes exactly as to vector hits.
        let (surviving, _drop_log) = apply_policy_filters(vec![exp.hit], filters);
        let Some(hit) = surviving.into_iter().next() else {
            continue;
        };
        seen.insert(node_id);
        folded.push(ScoredHit {
            score: (parent.score * decay).clamp(0.0, 1.0),
            matched_policy_filters: Vec::new(),
            fallback_origin: false,
            expansion_origin: Some(ExpansionOrigin {
                origin: GraphRecordRef {
                    id: parent.hit.node_id().to_string(),
                    label: parent.hit.label.clone(),
                    datasource: Some("life-graph".into()),
                },
                rel_type: exp.rel_type,
            }),
            hit,
        });
    }
    folded
}

// ── Policy filtering ──────────────────────────────────────────────────────────

/// Apply policy filters. Returns (surviving hits, drop log).
///
/// `RoleAppropriate` and `LowAgencyOnly` require runtime context and are
/// treated as pass-through here — the caller must enforce them.
pub fn apply_policy_filters(
    hits: Vec<VectorHit>,
    filters: &[PolicyFilter],
) -> (Vec<VectorHit>, Vec<String>) {
    let mut surviving = Vec::with_capacity(hits.len());
    let mut drop_log = Vec::new();

    for hit in hits {
        let mut reason: Option<String> = None;

        for filter in filters {
            match filter {
                PolicyFilter::ExcludeRetired => {
                    if hit.is_retired() {
                        reason = Some(format!("ExcludeRetired: {} [{}]", hit.node_id(), hit.label));
                        break;
                    }
                }
                PolicyFilter::ExcludeConflictedUnlessRequested => {
                    if matches!(hit.validation_state(), ValidationState::Conflicted) {
                        reason = Some(format!(
                            "ExcludeConflicted: {} [{}]",
                            hit.node_id(),
                            hit.label
                        ));
                        break;
                    }
                }
                PolicyFilter::RequireEvidence => {
                    if hit.confidence() < 0.2 {
                        reason = Some(format!(
                            "RequireEvidence: {} [{}] confidence {:.2}",
                            hit.node_id(),
                            hit.label,
                            hit.confidence()
                        ));
                        break;
                    }
                }
                // Runtime-only filters: pass through.
                PolicyFilter::RoleAppropriate | PolicyFilter::LowAgencyOnly => {}
            }
        }

        match reason {
            Some(r) => drop_log.push(r),
            None => surviving.push(hit),
        }
    }

    (surviving, drop_log)
}

// ── Ranking ───────────────────────────────────────────────────────────────────

/// Hub labels that appear in many contexts — penalised in graph specificity.
const HUB_LABELS: &[&str] = &["Person", "Event", "Project"];

/// Cheap, property-only check: does this hit belong to the given V005 domain?
///
/// True when either:
///   - the node's `observed_by` provenance maps to the domain's steward agent
///     (segment-matched, e.g. `agent-beacon` → `chief_of_staff`), or
///   - the node itself carries the domain's `domain_slug` property (the V005
///     Role nodes do).
///
/// The living-cycle edge check (edge to the domain's Role node) requires a
/// graph round-trip and is layered on by the provider — see
/// `LifeGraphProvider::domain_edge_node_ids`. This is a *bias signal*, never
/// a filter: soft boundaries.
pub fn hit_matches_domain(hit: &VectorHit, domain_slug: &str) -> bool {
    if hit.prop_str("domain_slug") == Some(domain_slug) {
        return true;
    }
    hit.prop_str("observed_by")
        .and_then(crate::zoning::domain_slug_for_agent)
        == Some(domain_slug)
}

/// Minimum `origin_trust` (Muninn source reliability recorded at write time)
/// for a node to earn the Muninn-origin confirmation lift.
pub const MUNINN_ORIGIN_TRUST_THRESHOLD: f64 = 0.7;

/// Confirmation-term lift applied to unconfirmed nodes whose Muninn origin
/// trust meets [`MUNINN_ORIGIN_TRUST_THRESHOLD`]. Added to the node's stored
/// confidence and capped at 1.0, so a Muninn-trusted node can approach — but
/// never outrank — an operator-confirmed node on the confirmation axis.
pub const MUNINN_ORIGIN_TRUST_BONUS: f32 = 0.15;

/// Composite ranking score. Returns a value in `[0.0, 1.0]`.
///
/// `age_secs`: seconds elapsed since `observed_at`, pre-computed by the caller
/// to keep this crate dependency-free (no chrono, no time parsing).
///
/// `role_matched`: whether the hit is tied to the caller's `active_role`
/// domain (living-cycle edge to the domain Role node, or provenance/zoning
/// property match — see [`hit_matches_domain`]). When true the
/// `role_relevance` weight is added as a soft bonus; when the caller has no
/// active role, pass `false` and ranking is exactly the role-agnostic base.
pub fn ranking_score(
    hit: &VectorHit,
    weights: &RankingWeights,
    age_secs: u64,
    role_matched: bool,
) -> f32 {
    let sim = hit.similarity.clamp(0.0, 1.0);

    // Exponential decay: ~14-day half-life.
    let age_days = age_secs as f32 / 86_400.0;
    let recency = (-age_days / 20.0_f32).exp().clamp(0.0, 1.0);

    // Confirmed nodes score 1.0; others use their stored confidence, plus a
    // small confirmation-style lift for high-trust Muninn-origin nodes (the
    // first fusion term of the lifegraph-muninn-promotion seam). Nodes
    // without an `origin_trust` property — everything written before Muninn
    // provenance preservation — rank exactly as before.
    let confirmation = match hit.validation_state() {
        ValidationState::Confirmed => 1.0_f32,
        _ => {
            let base = hit.confidence();
            let muninn_trusted = hit
                .prop_f64("origin_trust")
                .is_some_and(|t| t >= MUNINN_ORIGIN_TRUST_THRESHOLD);
            if muninn_trusted {
                (base + MUNINN_ORIGIN_TRUST_BONUS).min(1.0)
            } else {
                base
            }
        }
    };

    // Active Commitment bonus.
    let active_commitment =
        if hit.label == "Commitment" && hit.prop_str("status").unwrap_or("") == "open" {
            1.0_f32
        } else {
            0.0_f32
        };

    // Specificity: penalise hubs.
    let specificity = if HUB_LABELS.contains(&hit.label.as_str()) {
        0.3_f32
    } else {
        1.0_f32
    };

    // Soft-zoning bonus: earned only by domain-tied hits, never subtracted.
    let role_relevance = if role_matched { 1.0_f32 } else { 0.0_f32 };

    // Feedback-informed utility: a bounded EWMA in [-1, 0] accumulated from
    // life.recall.feedback noisy/stale flags. Absent property (all nodes
    // written before the feedback loop) ranks exactly as before.
    let recall_utility = hit
        .prop_f64("recall_utility")
        .map(|u| (u as f32).clamp(-1.0, 0.0))
        .unwrap_or(0.0);

    (weights.semantic_similarity * sim
        + weights.recency * recency
        + weights.confirmation * confirmation
        + weights.active_commitment * active_commitment
        + weights.graph_specificity * specificity
        + weights.role_relevance * role_relevance
        + weights.recall_utility * recall_utility)
        .clamp(0.0, 1.0)
}

// ── Context packet projection ─────────────────────────────────────────────────

/// Token cost heuristic: title chars / 4 + 20 base overhead.
fn token_estimate(hit: &VectorHit) -> usize {
    hit.title().len() / 4 + 20
}

/// Project a `VectorHit` into a minimal `EvidencePacket`.
///
/// These are projection-synthesised: `metadata["from_vector_search"] = true`
/// marks them so Codex's evidence-conflict layer can distinguish them from
/// fully-adjudicated packets.
pub fn project_hit_to_evidence_packet(hit: &VectorHit, generated_at: &str) -> EvidencePacket {
    let node_id = hit.node_id().to_string();
    let source_membrane = hit
        .prop_str("source_membrane")
        .unwrap_or("unknown")
        .to_string();

    let basis = match hit.prop_str("provenance").unwrap_or("agent_inferred") {
        "operator_confirmed" | "user_input" => ReliabilityBasis::OperatorConfirmed,
        "transcript" | "calendar" => ReliabilityBasis::DirectObservation,
        "muninn_engram" => ReliabilityBasis::MuninnTrust,
        _ => ReliabilityBasis::AgentInferred,
    };

    EvidencePacket {
        packet_id: format!("proj:{}:{}", node_id, generated_at),
        claim_ref: GraphRecordRef {
            id: node_id,
            label: hit.label.clone(),
            datasource: Some("life-graph".into()),
        },
        claim_summary: hit.title().to_string(),
        source_refs: vec![SourceRef {
            source_id: source_membrane,
            source_kind: SourceKind::RuntimeObservation,
            reliability: SourceReliability {
                score: hit.confidence(),
                basis,
            },
            uri: None,
            captured_at: hit.prop_str("observed_at").map(str::to_string),
        }],
        passage_refs: vec![],
        confidence: hit.confidence(),
        validation_state: hit.validation_state(),
        observed_at: hit.prop_str("observed_at").map(str::to_string),
        valid_time_range: None,
        source_reliability: hit.confidence(),
        conflict_ids: Vec::new(),
        adjudication_status: AdjudicationStatus::NotNeeded,
        metadata: serde_json::json!({
            "from_vector_search": true,
            "similarity": hit.similarity,
            "bolt_id": hit.bolt_id,
        }),
    }
}

/// Provenance of an expansion-discovered hit: the ranked parent it was
/// reached from and the living-cycle relationship that connects them.
#[derive(Debug, Clone)]
pub struct ExpansionOrigin {
    pub origin: GraphRecordRef,
    pub rel_type: String,
}

/// A hit ready for context-packet projection: score, matched filters, and
/// (for expansion-discovered hits) the origin provenance that turns
/// `evidence_path` into a real multi-node path.
#[derive(Debug, Clone)]
pub struct ScoredHit {
    pub hit: VectorHit,
    pub score: f32,
    pub matched_policy_filters: Vec<PolicyFilter>,
    pub expansion_origin: Option<ExpansionOrigin>,
    /// True when the hit came from the raw recency-scan fallback rather
    /// than vector search — surfaced as `metadata["fallback_origin"]` so
    /// consumers can weight recency-only rows below semantic matches.
    pub fallback_origin: bool,
}

impl From<(VectorHit, f32, Vec<PolicyFilter>)> for ScoredHit {
    fn from((hit, score, matched_policy_filters): (VectorHit, f32, Vec<PolicyFilter>)) -> Self {
        Self {
            hit,
            score,
            matched_policy_filters,
            expansion_origin: None,
            fallback_origin: false,
        }
    }
}

/// Assemble a `RetrievalContextPacket` from scored, filtered hits.
///
/// `hits`: [`ScoredHit`]s sorted descending by score.
/// Drops from the tail when `token_budget` would be exceeded.
/// Expansion-discovered hits get a two-node `evidence_path`
/// (`origin -> hit`) and `expansion_origin` / `expansion_rel_type` metadata.
pub fn project_context_packet(
    context_id: &str,
    query_id: &str,
    strategy: RetrievalStrategy,
    hits: Vec<ScoredHit>,
    omitted_conflict_ids: Vec<String>,
    token_budget: usize,
    generated_at: &str,
) -> RetrievalContextPacket {
    let mut ranked_packets = Vec::new();
    let mut tokens_used = 0usize;

    for scored in hits {
        let ScoredHit {
            hit,
            score,
            matched_policy_filters: matched,
            expansion_origin,
            fallback_origin,
        } = scored;
        let cost = token_estimate(&hit);
        if tokens_used + cost > token_budget && !ranked_packets.is_empty() {
            break;
        }
        let claim_ref = GraphRecordRef {
            id: hit.node_id().to_string(),
            label: hit.label.clone(),
            datasource: Some("life-graph".into()),
        };
        let mut packet = project_hit_to_evidence_packet(&hit, generated_at);
        if fallback_origin && let Some(meta) = packet.metadata.as_object_mut() {
            meta.insert("fallback_origin".into(), true.into());
        }
        let evidence_path = match &expansion_origin {
            Some(exp) => {
                if let Some(meta) = packet.metadata.as_object_mut() {
                    meta.insert("expansion_origin".into(), exp.origin.id.clone().into());
                    meta.insert("expansion_rel_type".into(), exp.rel_type.clone().into());
                    meta.insert("from_expansion".into(), true.into());
                }
                vec![exp.origin.clone(), claim_ref]
            }
            None => vec![claim_ref],
        };
        ranked_packets.push(RankedEvidencePacket {
            packet,
            score,
            matched_policy_filters: matched,
            evidence_path,
        });
        tokens_used += cost;
    }

    RetrievalContextPacket {
        context_id: context_id.to_string(),
        query_id: query_id.to_string(),
        strategy,
        ranked_packets,
        omitted_conflict_ids,
        token_budget,
        generated_at: generated_at.to_string(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RankingWeights, SemanticSpace};
    use serde_json::json;

    fn bolt_node_row(bolt_id: i64, label: &str, props: Value, similarity: f64) -> Value {
        // Fixture derived from bolt_node_to_json in memgraph_provider.rs:
        //   {"kind":"node","id":<bolt_id>,"labels":[<label>],"properties":{...}}
        // Wrapped in row_to_json as {"node": <bolt_node>, "similarity": <f64>}.
        json!({
            "node": {
                "kind": "node",
                "id": bolt_id,
                "labels": [label],
                "properties": props
            },
            "similarity": similarity
        })
    }

    fn open_loop_props(id: &str, title: &str, confidence: f64, validation_state: &str) -> Value {
        json!({
            "id": id,
            "title": title,
            "confidence": confidence,
            "validation_state": validation_state,
            "source_membrane": "membrane:telegram",
            "provenance": "transcript",
            "observed_at": "2026-06-04T10:00:00Z",
            "status": "open"
        })
    }

    #[test]
    fn aspiration_is_a_known_civic_label_in_role_person_space() {
        // Beacon's civic core writes Aspiration via life.observe; it must be whitelisted
        // and mapped to the identity (role_person) space, matching the V004 vector index.
        assert!(crate::cypher::is_known_label("Aspiration"));
        assert_eq!(
            embedding_space_for_label("Aspiration"),
            Some("role_person_semantic")
        );
        assert!(labels_for_space(&SemanticSpace::RolePersonSemantic).contains(&"Aspiration"));
        assert_eq!(
            index_name(&SemanticSpace::RolePersonSemantic, "Aspiration"),
            "role_person_semantic__Aspiration"
        );
    }

    #[test]
    fn index_name_matches_v001_migration_names() {
        assert_eq!(
            index_name(&SemanticSpace::LifeEventSemantic, "OpenLoop"),
            "life_event_semantic__OpenLoop"
        );
        assert_eq!(
            index_name(&SemanticSpace::GoalSystemSemantic, "Goal"),
            "goal_system_semantic__Goal"
        );
        assert_eq!(
            index_name(&SemanticSpace::MemoryBridgeSemantic, "Commitment"),
            "memory_bridge_semantic__Commitment"
        );
    }

    #[test]
    fn semantic_expand_cypher_parameterizes_vector() {
        let cypher = semantic_expand_cypher("life_event_semantic__OpenLoop", 5, 0.4);
        assert!(cypher.contains("CALL vector_search.search("));
        assert!(cypher.contains("\"life_event_semantic__OpenLoop\""));
        assert!(cypher.contains(", 5, $vec)"));
        assert!(cypher.contains("YIELD node, similarity"));
        assert!(cypher.contains("WHERE similarity >= 0.4000"));
        assert!(cypher.contains("ORDER BY similarity DESC"));
        // The embedding must ride as a Bolt param, never inline floats — a
        // regression here re-defeats Memgraph's query-plan cache.
        assert!(!cypher.contains("0.100000"));
        // No alias — key in parser must be "node", not "pivot".
        assert!(!cypher.contains("AS pivot"));
    }

    #[test]
    fn parse_vector_search_rows_extracts_hits_from_bolt_node_shape() {
        // Fixture matches bolt_node_to_json output wrapped by row_to_json.
        let result = json!({
            "rows": [
                bolt_node_row(42, "OpenLoop",
                    open_loop_props("life:open_loop:rowing", "Rowing follow-up", 0.74, "proposed"),
                    0.91),
                bolt_node_row(43, "OpenLoop",
                    open_loop_props("life:open_loop:taxes", "File taxes", 0.60, "inferred"),
                    0.78),
            ]
        });

        let hits = parse_vector_search_rows(&result);
        assert_eq!(hits.len(), 2);

        assert_eq!(hits[0].bolt_id, 42);
        assert_eq!(hits[0].label, "OpenLoop");
        assert_eq!(hits[0].node_id(), "life:open_loop:rowing");
        assert_eq!(hits[0].title(), "Rowing follow-up");
        assert!((hits[0].similarity - 0.91).abs() < 0.001);
        assert!((hits[0].confidence() - 0.74).abs() < 0.01);
        assert!(matches!(
            hits[0].validation_state(),
            ValidationState::Proposed
        ));

        assert_eq!(hits[1].bolt_id, 43);
        assert!((hits[1].similarity - 0.78).abs() < 0.001);
    }

    #[test]
    fn parse_vector_search_rows_returns_empty_for_missing_rows_key() {
        assert!(parse_vector_search_rows(&json!({})).is_empty());
        assert!(parse_vector_search_rows(&json!({"rows": []})).is_empty());
    }

    #[test]
    fn policy_filter_excludes_retired_nodes() {
        let result = json!({
            "rows": [
                bolt_node_row(1, "OpenLoop",
                    json!({ "id": "l:ol:open", "title": "Active loop",
                             "confidence": 0.8, "validation_state": "confirmed",
                             "status": "open" }),
                    0.9),
                bolt_node_row(2, "OpenLoop",
                    json!({ "id": "l:ol:done", "title": "Done loop",
                             "confidence": 0.8, "validation_state": "retired",
                             "status": "done" }),
                    0.85),
            ]
        });

        let hits = parse_vector_search_rows(&result);
        let (surviving, log) = apply_policy_filters(hits, &[PolicyFilter::ExcludeRetired]);

        assert_eq!(surviving.len(), 1);
        assert_eq!(surviving[0].node_id(), "l:ol:open");
        assert_eq!(log.len(), 1);
        assert!(log[0].contains("ExcludeRetired"));
    }

    #[test]
    fn policy_filter_drops_low_confidence_on_require_evidence() {
        let result = json!({
            "rows": [
                bolt_node_row(1, "Goal",
                    json!({ "id": "g:strong", "title": "Strong goal", "confidence": 0.7,
                             "validation_state": "proposed", "status": "active" }),
                    0.9),
                bolt_node_row(2, "Goal",
                    json!({ "id": "g:weak", "title": "Weak goal", "confidence": 0.1,
                             "validation_state": "inferred", "status": "active" }),
                    0.8),
            ]
        });

        let hits = parse_vector_search_rows(&result);
        let (surviving, log) = apply_policy_filters(hits, &[PolicyFilter::RequireEvidence]);

        assert_eq!(surviving.len(), 1);
        assert_eq!(surviving[0].node_id(), "g:strong");
        assert!(log[0].contains("RequireEvidence"));
    }

    #[test]
    fn ranking_score_weights_similarity_and_recency() {
        let result = json!({
            "rows": [
                bolt_node_row(1, "OpenLoop",
                    open_loop_props("l:ol:recent", "Recent loop", 0.8, "confirmed"),
                    0.85)
            ]
        });
        let hit = parse_vector_search_rows(&result).pop().unwrap();
        let weights = RankingWeights::default();

        // age = 0: full recency score
        let score_fresh = ranking_score(&hit, &weights, 0, false);
        // age = 60 days in seconds
        let score_stale = ranking_score(&hit, &weights, 60 * 86_400, false);

        assert!(score_fresh > score_stale, "fresh hit should rank higher");
        assert!(score_fresh > 0.5, "fresh confirmed hit should score well");
        assert!((0.0..=1.0).contains(&score_stale));
    }

    #[test]
    fn ranking_score_penalises_feedback_flagged_nodes() {
        // Two identical hits except one carries the feedback-informed
        // recall_utility penalty — it must rank strictly lower, and an
        // absent property must rank exactly like utility 0 (pre-loop nodes).
        let make_hit = |utility: Option<f64>| {
            let mut props = open_loop_props("l:ol:x", "Loop", 0.8, "proposed");
            if let Some(u) = utility {
                props["recall_utility"] = json!(u);
            }
            VectorHit {
                bolt_id: 1,
                label: "OpenLoop".to_string(),
                properties: props,
                similarity: 0.8,
            }
        };
        let weights = RankingWeights::default();
        let clean = ranking_score(&make_hit(None), &weights, 0, false);
        let zeroed = ranking_score(&make_hit(Some(0.0)), &weights, 0, false);
        let flagged = ranking_score(&make_hit(Some(-1.0)), &weights, 0, false);
        assert_eq!(clean, zeroed, "absent property must equal utility 0");
        assert!(
            flagged < clean,
            "flagged node must rank lower: {flagged} vs {clean}"
        );
        assert!(
            (clean - flagged - weights.recall_utility).abs() < 1e-6,
            "penalty magnitude must equal the utility weight"
        );
        // A (nonsensical) positive utility must clamp to 0, never boost.
        let boosted = ranking_score(&make_hit(Some(0.9)), &weights, 0, false);
        assert_eq!(boosted, clean, "positive utility must clamp to 0");
    }

    #[test]
    fn ranking_score_penalises_hub_labels() {
        let make_hit = |label: &str, similarity: f32| VectorHit {
            bolt_id: 1,
            label: label.to_string(),
            properties: json!({
                "id": "test:node",
                "confidence": 0.9,
                "validation_state": "confirmed",
                "status": "active"
            }),
            similarity,
        };

        let weights = RankingWeights::default();
        let specific = ranking_score(&make_hit("OpenLoop", 0.8), &weights, 0, false);
        let hub = ranking_score(&make_hit("Person", 0.8), &weights, 0, false);

        assert!(specific > hub, "specific label should rank higher than hub");
    }

    #[test]
    fn ranking_score_muninn_origin_trust_bonus_on_and_off() {
        let make_hit = |extra: Value| {
            let mut props = open_loop_props("l:ol:muninn", "Muninn-origin loop", 0.5, "proposed");
            if let (Some(base), Some(add)) = (props.as_object_mut(), extra.as_object()) {
                for (k, v) in add {
                    base.insert(k.clone(), v.clone());
                }
            }
            VectorHit {
                bolt_id: 1,
                label: "OpenLoop".to_string(),
                properties: props,
                similarity: 0.6,
            }
        };
        let weights = RankingWeights::default();

        // Old node with no origin_trust property: exactly the base score.
        let plain = ranking_score(&make_hit(json!({})), &weights, 0, false);
        // High-trust Muninn origin: earns the confirmation-term lift.
        let trusted = ranking_score(
            &make_hit(json!({ "origin_trust": 0.9 })),
            &weights,
            0,
            false,
        );
        // Below-threshold trust: no lift.
        let untrusted = ranking_score(
            &make_hit(json!({ "origin_trust": 0.4 })),
            &weights,
            0,
            false,
        );

        assert!(
            trusted > plain,
            "high-trust Muninn origin must outrank the identical plain hit"
        );
        assert!(
            (trusted - plain - weights.confirmation * MUNINN_ORIGIN_TRUST_BONUS).abs() < 0.001,
            "lift should equal confirmation weight x bonus (unclamped range)"
        );
        assert!(
            (untrusted - plain).abs() < f32::EPSILON,
            "below-threshold origin_trust must not change ranking"
        );

        // Confirmed nodes are already at the confirmation ceiling: no change.
        let confirmed = |extra: Value| {
            let mut hit = make_hit(extra);
            hit.properties["validation_state"] = json!("confirmed");
            hit
        };
        assert!(
            (ranking_score(
                &confirmed(json!({ "origin_trust": 0.9 })),
                &weights,
                0,
                false
            ) - ranking_score(&confirmed(json!({})), &weights, 0, false))
            .abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn project_hit_maps_muninn_engram_provenance_to_muninn_trust_basis() {
        let mut props = open_loop_props("l:ol:muninn-proj", "Muninn loop", 0.7, "proposed");
        props["provenance"] = json!("muninn_engram");
        props["origin_trust"] = json!(0.85);
        let hit = VectorHit {
            bolt_id: 7,
            label: "OpenLoop".to_string(),
            properties: props,
            similarity: 0.8,
        };

        let packet = project_hit_to_evidence_packet(&hit, "2026-07-07T00:00:00Z");
        assert_eq!(
            packet.source_refs[0].reliability.basis,
            crate::ReliabilityBasis::MuninnTrust
        );
    }

    #[test]
    fn ranking_score_applies_role_relevance_bonus_when_matched() {
        let result = json!({
            "rows": [
                bolt_node_row(1, "OpenLoop",
                    open_loop_props("l:ol:domain", "Domain-tied loop", 0.5, "proposed"),
                    0.6)
            ]
        });
        let hit = parse_vector_search_rows(&result).pop().unwrap();
        let weights = RankingWeights::default();

        let unmatched = ranking_score(&hit, &weights, 60 * 86_400, false);
        let matched = ranking_score(&hit, &weights, 60 * 86_400, true);

        assert!(
            matched > unmatched,
            "role-matched hit must rank above the identical unmatched hit"
        );
        assert!(
            (matched - unmatched - weights.role_relevance).abs() < 0.001,
            "bonus delta should equal the role_relevance weight (unclamped range)"
        );

        // Zero weight disables the bonus entirely.
        let weights_off = RankingWeights {
            role_relevance: 0.0,
            ..RankingWeights::default()
        };
        assert!(
            (ranking_score(&hit, &weights_off, 0, true)
                - ranking_score(&hit, &weights_off, 0, false))
            .abs()
                < f32::EPSILON
        );
    }

    #[test]
    fn hit_matches_domain_via_observed_by_provenance() {
        let make_hit = |props: Value| VectorHit {
            bolt_id: 1,
            label: "OpenLoop".to_string(),
            properties: props,
            similarity: 0.8,
        };

        // observed_by steward maps to the domain.
        let beacon_hit = make_hit(json!({ "id": "l:ol:x", "observed_by": "agent-beacon" }));
        assert!(hit_matches_domain(&beacon_hit, "chief_of_staff"));
        assert!(!hit_matches_domain(&beacon_hit, "librarian"));

        // Segment matching: ariel is communications, not architect (aria).
        let ariel_hit = make_hit(json!({ "id": "l:ol:y", "observed_by": "agent-ariel-01" }));
        assert!(hit_matches_domain(&ariel_hit, "communications"));
        assert!(!hit_matches_domain(&ariel_hit, "architect"));

        // V005 Role nodes carry domain_slug directly.
        let role_hit = VectorHit {
            bolt_id: 2,
            label: "Role".to_string(),
            properties: json!({ "id": "life:role:chief-of-staff",
                                 "domain_slug": "chief_of_staff" }),
            similarity: 0.8,
        };
        assert!(hit_matches_domain(&role_hit, "chief_of_staff"));

        // No provenance at all: no match, but (soft boundary) never an error.
        let bare_hit = make_hit(json!({ "id": "l:ol:z" }));
        assert!(!hit_matches_domain(&bare_hit, "chief_of_staff"));
    }

    #[test]
    fn project_context_packet_respects_token_budget() {
        // Create 10 hits — each title is 40 chars → ~10 tokens + 20 base = 30 tokens each.
        // Budget of 100 should fit ~3.
        let rows: Vec<Value> = (0..10)
            .map(|i| {
                bolt_node_row(
                    i,
                    "OpenLoop",
                    json!({
                        "id": format!("l:ol:{}", i),
                        "title": format!("Open loop number {:0>28}", i),
                        "confidence": 0.7,
                        "validation_state": "proposed",
                        "status": "open"
                    }),
                    0.9 - (i as f64 * 0.05),
                )
            })
            .collect();

        let result = json!({ "rows": rows });
        let hits = parse_vector_search_rows(&result);
        let weights = RankingWeights::default();

        let scored: Vec<ScoredHit> = hits
            .into_iter()
            .map(|h| {
                let s = ranking_score(&h, &weights, 0, false);
                ScoredHit::from((h, s, vec![]))
            })
            .collect();

        let packet = project_context_packet(
            "ctx:test",
            "q:test",
            RetrievalStrategy::MemoryAwareGraphRank,
            scored,
            vec![],
            100,
            "2026-06-04T20:00:00Z",
        );

        assert!(
            packet.ranked_packets.len() < 10,
            "budget should cap candidates"
        );
        assert!(packet.token_budget == 100);
    }

    // ── Graph expansion (read side) ───────────────────────────────────────

    fn expansion_row(origin_id: &str, rel_type: &str, bolt_id: i64, props: Value) -> Value {
        json!({
            "origin_id": origin_id,
            "rel_type": rel_type,
            "node": {
                "kind": "node",
                "id": bolt_id,
                "labels": ["Goal"],
                "properties": props
            }
        })
    }

    fn parent_hit(id: &str, score: f32) -> ScoredHit {
        ScoredHit::from((
            VectorHit {
                bolt_id: 1,
                label: "OpenLoop".to_string(),
                properties: open_loop_props(id, "Parent loop", 0.8, "confirmed"),
                similarity: 0.9,
            },
            score,
            vec![],
        ))
    }

    #[test]
    fn expansion_cypher_matches_living_cycle_edges_in_one_round_trip() {
        let rel_types = expansion_rel_types(&[]);
        let cypher = expansion_cypher(&["l:ol:a", "l:ol:b'quote"], &rel_types, 32);

        // Empty allowlist -> full writable vocabulary: living-cycle (incl.
        // SCOPED_TO, the server-injected node->Role anchor) plus the agenda
        // relations (LIFE_GRAPH_ACTIVE S2) so recall expansion traverses
        // goal/commitment topology too.
        assert!(cypher.contains(
            "MATCH (n)-[r:OWNS|SHAPES|SETS|SPAWNS|RELATES_TO|SCOPED_TO|ADVANCES|BLOCKED_BY|NEEDS_FOLLOWUP|PROMISED_TO|CONTAINS|SUPPORTS]-(related)"
        ));
        assert!(cypher.contains("n.id IN ['l:ol:a', 'l:ol:b\\'quote']"));
        assert!(cypher.contains("coalesce(related.validation_state, 'inferred') <> 'retired'"));
        assert!(cypher.contains("RETURN n.id AS origin_id, type(r) AS rel_type, related AS node"));
        assert!(cypher.contains("LIMIT 32"));
    }

    #[test]
    fn expansion_rel_types_intersects_allowlist_with_writable_vocabulary() {
        assert_eq!(
            expansion_rel_types(&[]),
            vec![
                "OWNS",
                "SHAPES",
                "SETS",
                "SPAWNS",
                "RELATES_TO",
                "SCOPED_TO",
                "ADVANCES",
                "BLOCKED_BY",
                "NEEDS_FOLLOWUP",
                "PROMISED_TO",
                "CONTAINS",
                "SUPPORTS"
            ]
        );
        assert_eq!(
            expansion_rel_types(&["OWNS".into(), "BOGUS_TYPE".into()]),
            vec!["OWNS"]
        );
        assert_eq!(
            expansion_rel_types(&["ADVANCES".into(), "BOGUS_TYPE".into()]),
            vec!["ADVANCES"]
        );
        // A fully-unknown allowlist yields no rel types (expansion disabled),
        // never an injection vector.
        assert!(expansion_rel_types(&["DROP_ALL".into()]).is_empty());
    }

    #[test]
    fn parse_expansion_rows_extracts_origin_and_rel_type() {
        let result = json!({
            "rows": [
                expansion_row("l:ol:a", "RELATES_TO", 7,
                    json!({ "id": "g:health", "title": "Health goal" })),
                // Missing origin_id → skipped.
                json!({ "rel_type": "OWNS", "node": { "id": 8, "labels": ["Goal"], "properties": {} } })
            ]
        });
        let hits = parse_expansion_rows(&result);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].origin_id, "l:ol:a");
        assert_eq!(hits[0].rel_type, "RELATES_TO");
        assert_eq!(hits[0].hit.node_id(), "g:health");
        assert!(parse_expansion_rows(&json!({})).is_empty());
    }

    #[test]
    fn fold_expansion_hits_applies_decay_scoring() {
        let parents = vec![parent_hit("l:ol:a", 0.8)];
        let expansion = parse_expansion_rows(&json!({
            "rows": [expansion_row("l:ol:a", "RELATES_TO", 7,
                json!({ "id": "g:health", "title": "Health goal",
                         "confidence": 0.7, "validation_state": "proposed" }))]
        }));

        let folded = fold_expansion_hits(&parents, expansion, &[], EXPANSION_SCORE_DECAY, 32);

        assert_eq!(folded.len(), 1);
        assert!((folded[0].score - 0.8 * EXPANSION_SCORE_DECAY).abs() < 0.001);
        let origin = folded[0].expansion_origin.as_ref().unwrap();
        assert_eq!(origin.origin.id, "l:ol:a");
        assert_eq!(origin.origin.label, "OpenLoop");
        assert_eq!(origin.rel_type, "RELATES_TO");
    }

    #[test]
    fn fold_expansion_hits_policy_filters_expanded_nodes() {
        let parents = vec![parent_hit("l:ol:a", 0.8)];
        let expansion = parse_expansion_rows(&json!({
            "rows": [
                expansion_row("l:ol:a", "OWNS", 7,
                    json!({ "id": "g:retired", "title": "Retired goal",
                             "confidence": 0.9, "validation_state": "retired" })),
                expansion_row("l:ol:a", "OWNS", 8,
                    json!({ "id": "g:weak", "title": "Weak goal",
                             "confidence": 0.1, "validation_state": "inferred" })),
                expansion_row("l:ol:a", "OWNS", 9,
                    json!({ "id": "g:live", "title": "Live goal",
                             "confidence": 0.8, "validation_state": "confirmed" })),
            ]
        }));

        let folded = fold_expansion_hits(
            &parents,
            expansion,
            &[PolicyFilter::ExcludeRetired, PolicyFilter::RequireEvidence],
            EXPANSION_SCORE_DECAY,
            32,
        );

        assert_eq!(folded.len(), 1, "retired + low-confidence hits must drop");
        assert_eq!(folded[0].hit.node_id(), "g:live");
    }

    #[test]
    fn fold_expansion_hits_dedupes_and_caps_at_max_nodes() {
        let parents = vec![parent_hit("l:ol:a", 0.8), parent_hit("l:ol:b", 0.6)];
        let expansion = parse_expansion_rows(&json!({
            "rows": [
                // Duplicate of a parent → skipped.
                expansion_row("l:ol:a", "RELATES_TO", 5, json!({ "id": "l:ol:b" })),
                // Unknown origin → skipped.
                expansion_row("l:ol:missing", "OWNS", 6, json!({ "id": "g:orphan" })),
                expansion_row("l:ol:a", "OWNS", 7, json!({ "id": "g:one" })),
                // Same node reached twice → folded once.
                expansion_row("l:ol:b", "SHAPES", 7, json!({ "id": "g:one" })),
                expansion_row("l:ol:a", "SETS", 8, json!({ "id": "g:two" })),
                expansion_row("l:ol:b", "SPAWNS", 9, json!({ "id": "g:three" })),
            ]
        }));

        let folded = fold_expansion_hits(&parents, expansion, &[], EXPANSION_SCORE_DECAY, 2);

        assert_eq!(folded.len(), 2, "max_nodes must cap folded expansion hits");
        assert_eq!(folded[0].hit.node_id(), "g:one");
        assert_eq!(folded[1].hit.node_id(), "g:two");
    }

    #[test]
    fn project_context_packet_folds_expansion_into_multi_node_evidence_path() {
        let parent = parent_hit("l:ol:a", 0.8);
        let expansion = parse_expansion_rows(&json!({
            "rows": [expansion_row("l:ol:a", "RELATES_TO", 7,
                json!({ "id": "g:health", "title": "Health goal",
                         "confidence": 0.7, "validation_state": "proposed" }))]
        }));
        let mut hits = vec![parent];
        let folded = fold_expansion_hits(&hits, expansion, &[], EXPANSION_SCORE_DECAY, 32);
        hits.extend(folded);

        let packet = project_context_packet(
            "ctx:test",
            "q:test",
            RetrievalStrategy::MemoryAwareGraphRank,
            hits,
            vec![],
            10_000,
            "2026-07-06T20:00:00Z",
        );

        assert_eq!(packet.ranked_packets.len(), 2);
        // Parent keeps its single-node path and no expansion tags.
        let parent_packet = &packet.ranked_packets[0];
        assert_eq!(parent_packet.evidence_path.len(), 1);
        assert!(
            parent_packet
                .packet
                .metadata
                .get("expansion_origin")
                .is_none()
        );
        // Expansion hit carries the real multi-node path plus provenance tags.
        let expanded = &packet.ranked_packets[1];
        assert_eq!(expanded.evidence_path.len(), 2);
        assert_eq!(expanded.evidence_path[0].id, "l:ol:a");
        assert_eq!(expanded.evidence_path[0].label, "OpenLoop");
        assert_eq!(expanded.evidence_path[1].id, "g:health");
        assert_eq!(expanded.packet.metadata["expansion_origin"], "l:ol:a");
        assert_eq!(expanded.packet.metadata["expansion_rel_type"], "RELATES_TO");
        assert_eq!(expanded.packet.metadata["from_expansion"], true);
        assert!(expanded.score < parent_packet.score);
    }

    #[test]
    fn projected_packet_is_marked_from_vector_search() {
        let result = json!({
            "rows": [bolt_node_row(1, "Goal",
                json!({ "id": "g:health", "title": "Health goal", "confidence": 0.8,
                         "validation_state": "confirmed", "status": "active",
                         "source_membrane": "membrane:telegram", "provenance": "transcript",
                         "observed_at": "2026-06-04T10:00:00Z" }),
                0.93)]
        });
        let hit = parse_vector_search_rows(&result).pop().unwrap();
        let packet = project_hit_to_evidence_packet(&hit, "2026-06-04T20:00:00Z");

        assert_eq!(packet.metadata["from_vector_search"], true);
        assert!(packet.metadata["similarity"].as_f64().unwrap() > 0.9);
        assert_eq!(packet.claim_ref.label, "Goal");
        assert_eq!(packet.adjudication_status, AdjudicationStatus::NotNeeded);
    }

    #[test]
    fn project_context_packet_marks_fallback_origin_metadata() {
        let vector = parent_hit("l:ol:vec", 0.7);
        let mut fallback = parent_hit("l:ol:fb", 0.3);
        fallback.fallback_origin = true;

        let packet = project_context_packet(
            "ctx:test",
            "q:test",
            RetrievalStrategy::MemoryAwareGraphRank,
            vec![vector, fallback],
            vec![],
            10_000,
            "2026-07-07T00:00:00Z",
        );

        assert_eq!(packet.ranked_packets.len(), 2);
        // Vector hits carry no fallback marker at all.
        assert!(
            packet.ranked_packets[0]
                .packet
                .metadata
                .get("fallback_origin")
                .is_none()
        );
        // Fallback top-up rows are clearly marked for consumers.
        assert_eq!(
            packet.ranked_packets[1].packet.metadata["fallback_origin"],
            true
        );
        // They remain projection-synthesised evidence packets.
        assert_eq!(
            packet.ranked_packets[1].packet.metadata["from_vector_search"],
            true
        );
    }
}
