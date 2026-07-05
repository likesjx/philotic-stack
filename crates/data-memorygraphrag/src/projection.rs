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
/// Verified against Memgraph 3.10.1 on vps-jane: correct procedure name,
/// arg arity, and index name format. Empty DB returns 0 rows (not an error).
pub fn semantic_expand_cypher(
    index: &str,
    top_k: usize,
    vector: &[f32],
    min_similarity: f32,
) -> String {
    let vec_body: Vec<String> = vector.iter().map(|v| format!("{:.6}", v)).collect();
    format!(
        "CALL vector_search.search(\"{index}\", {top_k}, [{vec_body}]) \
         YIELD node, similarity \
         WHERE similarity >= {min_sim:.4} \
         RETURN node, similarity \
         ORDER BY similarity DESC",
        vec_body = vec_body.join(", "),
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
        let similarity = row.get("similarity").and_then(Value::as_f64).unwrap_or(0.0) as f32;

        hits.push(VectorHit {
            bolt_id,
            label,
            properties,
            similarity,
        });
    }
    hits
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

/// Composite ranking score. Returns a value in `[0.0, 1.0]`.
///
/// `age_secs`: seconds elapsed since `observed_at`, pre-computed by the caller
/// to keep this crate dependency-free (no chrono, no time parsing).
pub fn ranking_score(hit: &VectorHit, weights: &RankingWeights, age_secs: u64) -> f32 {
    let sim = hit.similarity.clamp(0.0, 1.0);

    // Exponential decay: ~14-day half-life.
    let age_days = age_secs as f32 / 86_400.0;
    let recency = (-age_days / 20.0_f32).exp().clamp(0.0, 1.0);

    // Confirmed nodes score 1.0; others use their stored confidence.
    let confirmation = match hit.validation_state() {
        ValidationState::Confirmed => 1.0_f32,
        _ => hit.confidence(),
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

    (weights.semantic_similarity * sim
        + weights.recency * recency
        + weights.confirmation * confirmation
        + weights.active_commitment * active_commitment
        + weights.graph_specificity * specificity)
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

/// Assemble a `RetrievalContextPacket` from scored, filtered hits.
///
/// `hits`: `(VectorHit, score, matched_policy_filters)` sorted descending by score.
/// Drops from the tail when `token_budget` would be exceeded.
pub fn project_context_packet(
    context_id: &str,
    query_id: &str,
    strategy: RetrievalStrategy,
    hits: Vec<(VectorHit, f32, Vec<PolicyFilter>)>,
    omitted_conflict_ids: Vec<String>,
    token_budget: usize,
    generated_at: &str,
) -> RetrievalContextPacket {
    let mut ranked_packets = Vec::new();
    let mut tokens_used = 0usize;

    for (hit, score, matched) in hits {
        let cost = token_estimate(&hit);
        if tokens_used + cost > token_budget && !ranked_packets.is_empty() {
            break;
        }
        let claim_ref = GraphRecordRef {
            id: hit.node_id().to_string(),
            label: hit.label.clone(),
            datasource: Some("life-graph".into()),
        };
        let packet = project_hit_to_evidence_packet(&hit, generated_at);
        ranked_packets.push(RankedEvidencePacket {
            packet,
            score,
            matched_policy_filters: matched,
            evidence_path: vec![claim_ref],
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
    fn semantic_expand_cypher_embeds_vector_inline() {
        let cypher =
            semantic_expand_cypher("life_event_semantic__OpenLoop", 5, &[0.1, 0.2, 0.3], 0.4);
        assert!(cypher.contains("CALL vector_search.search("));
        assert!(cypher.contains("\"life_event_semantic__OpenLoop\""));
        assert!(cypher.contains(", 5, ["));
        assert!(cypher.contains("0.100000"));
        assert!(cypher.contains("YIELD node, similarity"));
        assert!(cypher.contains("WHERE similarity >= 0.4000"));
        assert!(cypher.contains("ORDER BY similarity DESC"));
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
        let score_fresh = ranking_score(&hit, &weights, 0);
        // age = 60 days in seconds
        let score_stale = ranking_score(&hit, &weights, 60 * 86_400);

        assert!(score_fresh > score_stale, "fresh hit should rank higher");
        assert!(score_fresh > 0.5, "fresh confirmed hit should score well");
        assert!(score_stale >= 0.0 && score_stale <= 1.0);
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
        let specific = ranking_score(&make_hit("OpenLoop", 0.8), &weights, 0);
        let hub = ranking_score(&make_hit("Person", 0.8), &weights, 0);

        assert!(specific > hub, "specific label should rank higher than hub");
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

        let scored: Vec<(VectorHit, f32, Vec<PolicyFilter>)> = hits
            .into_iter()
            .map(|h| {
                let s = ranking_score(&h, &weights, 0);
                (h, s, vec![])
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
}
